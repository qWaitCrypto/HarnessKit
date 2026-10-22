use std::fs;
use std::path::Path;

use crate::index::{cell, split_pipe_list, sql_string_literal};
use crate::parse::parse_heading;
use crate::sqlite_cli::{QueryResult, SqliteConnection};
use crate::Result;

#[derive(Clone)]
pub(crate) struct QueryCandidate {
    pub(crate) path: String,
    pub(crate) title: String,
    pub(crate) role: String,
    pub(crate) status: String,
    pub(crate) score: String,
    pub(crate) why: String,
    pub(crate) summary: String,
    pub(crate) neighbors: Vec<String>,
    pub(crate) snippet_heading: String,
    pub(crate) snippet_location: String,
    pub(crate) snippet: String,
}

fn sanitize_fts_query(query: &str) -> String {
    let tokens = query
        .split(|ch: char| !ch.is_alphanumeric())
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    if tokens.is_empty() {
        String::new()
    } else {
        tokens.join(" ")
    }
}

pub(crate) fn query_candidates(
    db: &SqliteConnection,
    target_dir: &Path,
    query_terms: &str,
) -> Result<QueryResult> {
    let escaped_terms = sql_string_literal(query_terms);
    let sanitized_fts_terms = sanitize_fts_query(query_terms);
    let escaped_fts_terms = sql_string_literal(&sanitized_fts_terms);
    let fts_match_clause = if sanitized_fts_terms.is_empty() {
        "0".to_string()
    } else {
        format!("docs_fts MATCH {}", escaped_fts_terms)
    };
    db.query(&format!(
        "WITH lexical AS (
            SELECT d.path,
                   d.title,
                   d.role,
                   COALESCE(d.status, '') AS status,
                   d.summary,
                   d.incoming_links,
                   CASE
                       WHEN lower(d.title) LIKE '%' || lower({0}) || '%' THEN 4.0
                       WHEN lower(d.path) LIKE '%' || lower({0}) || '%' THEN 3.0
                       WHEN lower(d.headings_text) LIKE '%' || lower({0}) || '%' THEN 2.0
                       WHEN lower(d.summary) LIKE '%' || lower({0}) || '%' THEN 1.5
                       ELSE 0.0
                   END AS lexical_score,
                   CASE
                       WHEN lower(d.title) LIKE '%' || lower({0}) || '%' THEN 'title matched query'
                       WHEN lower(d.path) LIKE '%' || lower({0}) || '%' THEN 'path matched query'
                       WHEN lower(d.headings_text) LIKE '%' || lower({0}) || '%' THEN 'heading matched query'
                       WHEN lower(d.summary) LIKE '%' || lower({0}) || '%' THEN 'summary matched query'
                       ELSE ''
                   END AS why
            FROM docs d
            WHERE lower(d.path) LIKE '%' || lower({0}) || '%'
               OR lower(d.title) LIKE '%' || lower({0}) || '%'
               OR lower(d.headings_text) LIKE '%' || lower({0}) || '%'
               OR lower(d.summary) LIKE '%' || lower({0}) || '%'
        ),
        fts AS (
            SELECT d.path,
                   d.title,
                   d.role,
                   COALESCE(d.status, '') AS status,
                   d.summary,
                   d.incoming_links,
                   2.5 AS lexical_score,
                   'full-text matched query' AS why
            FROM docs_fts f
            JOIN docs d ON d.path = f.path
            WHERE {1}
        ),
        base AS (
            SELECT * FROM lexical
            UNION ALL
            SELECT * FROM fts
        ),
        merged AS (
            SELECT path,
                   max(title) AS title,
                   max(role) AS role,
                   max(status) AS status,
                   max(summary) AS summary,
                   max(incoming_links) AS incoming_links,
                   max(lexical_score) AS lexical_score,
                   group_concat(DISTINCT why) AS why
            FROM base
            GROUP BY path
        ),
        ranked AS (
            SELECT path,
                   title,
                   role,
                   status,
                   summary,
                   incoming_links,
                   lexical_score,
                   why,
                   (lexical_score + (incoming_links * 0.05)) AS score
            FROM merged
            ORDER BY score DESC, path ASC
            LIMIT 12
        ),
        expanded AS (
            SELECT rnk.path AS anchor_path,
                   r.dst_path AS neighbor_path
            FROM ranked rnk
            JOIN relations r ON r.src_path = rnk.path
            WHERE r.dst_path LIKE '%.md'
            UNION
            SELECT rnk.path AS anchor_path,
                   r.src_path AS neighbor_path
            FROM ranked rnk
            JOIN relations r ON r.dst_path = rnk.path
            WHERE r.src_path LIKE '%.md'
            UNION
            SELECT rnk.path AS anchor_path,
                   sibling.dst_path AS neighbor_path
            FROM ranked rnk
            JOIN relations parent ON parent.dst_path = rnk.path AND parent.relation_type = 'doc_indexes_doc'
            JOIN relations sibling ON sibling.src_path = parent.src_path AND sibling.relation_type = 'doc_indexes_doc'
            WHERE sibling.dst_path LIKE '%.md' AND sibling.dst_path != rnk.path
            UNION
            SELECT rnk.path AS anchor_path,
                   scoped.doc_path AS neighbor_path
            FROM ranked rnk
            JOIN scope_paths base_scope ON base_scope.doc_path = rnk.path
            JOIN scope_paths scoped ON scoped.scope_path = base_scope.scope_path AND scoped.doc_path != rnk.path
        ),
        neighbor_summary AS (
            SELECT anchor_path,
                   group_concat(neighbor_path, ' | ') AS neighbors
            FROM (
                SELECT anchor_path, neighbor_path
                FROM (
                    SELECT anchor_path,
                           neighbor_path,
                           row_number() OVER (PARTITION BY anchor_path ORDER BY neighbor_path) AS rn
                    FROM expanded
                )
                WHERE rn <= 8
                ORDER BY anchor_path, neighbor_path
            )
            GROUP BY anchor_path
        )
        SELECT rnk.path,
               rnk.title,
               rnk.role,
               rnk.status,
               printf('%.2f', rnk.score) AS score,
               rnk.why,
               rnk.summary,
               COALESCE(n.neighbors, '')
        FROM ranked rnk
        LEFT JOIN neighbor_summary n ON n.anchor_path = rnk.path
        ORDER BY rnk.score DESC, rnk.path ASC;",
        escaped_terms,
        fts_match_clause
    ))
    .map(|mut result| {
        for row in &mut result.rows {
            let path = cell(row, 0).to_string();
            let (heading, location, snippet) = snippet_for_doc(target_dir, &path, query_terms);
            row.push(Some(heading));
            row.push(Some(location));
            row.push(Some(snippet));
        }
        result
    })
}

pub(crate) fn query_rows_to_candidates(
    result: &QueryResult,
    target_dir: &Path,
    query_terms: &str,
) -> Vec<QueryCandidate> {
    result
        .rows
        .iter()
        .map(|row| {
            let path = cell(row, 0).to_string();
            let mut fallback = None;
            let snippet_heading = cell(row, 8).to_string();
            let snippet_location = cell(row, 9).to_string();
            let snippet = cell(row, 10).to_string();
            let (snippet_heading, snippet_location, snippet) = if snippet_heading.is_empty()
                || snippet_location.is_empty()
                || snippet.is_empty()
            {
                let computed =
                    fallback.get_or_insert_with(|| snippet_for_doc(target_dir, &path, query_terms));
                (
                    if snippet_heading.is_empty() {
                        computed.0.clone()
                    } else {
                        snippet_heading
                    },
                    if snippet_location.is_empty() {
                        computed.1.clone()
                    } else {
                        snippet_location
                    },
                    if snippet.is_empty() {
                        computed.2.clone()
                    } else {
                        snippet
                    },
                )
            } else {
                (snippet_heading, snippet_location, snippet)
            };
            QueryCandidate {
                path,
                title: cell(row, 1).to_string(),
                role: cell(row, 2).to_string(),
                status: cell(row, 3).to_string(),
                score: cell(row, 4).to_string(),
                why: cell(row, 5).to_string(),
                summary: cell(row, 6).to_string(),
                neighbors: split_pipe_list(cell(row, 7)),
                snippet_heading,
                snippet_location,
                snippet,
            }
        })
        .collect()
}

pub(crate) fn snippet_for_doc(
    target_dir: &Path,
    doc_path: &str,
    query_terms: &str,
) -> (String, String, String) {
    let text = fs::read_to_string(target_dir.join(doc_path)).unwrap_or_default();
    if text.is_empty() {
        return (String::new(), String::new(), String::new());
    }
    let terms = sanitize_fts_query(query_terms)
        .split_whitespace()
        .map(|term| term.to_ascii_lowercase())
        .collect::<Vec<_>>();
    let mut current_heading = String::new();
    let mut fallback = String::new();
    let mut fallback_line = 0usize;
    let mut current_heading_line = 0usize;
    for (index, line) in text.lines().enumerate() {
        let line_number = index + 1;
        if let Some((_level, heading)) = parse_heading(line) {
            current_heading = heading;
            current_heading_line = line_number;
            continue;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("---") {
            continue;
        }
        if fallback.is_empty() && !trimmed.starts_with('#') {
            fallback = trimmed.to_string();
            fallback_line = line_number;
        }
        let lower = trimmed.to_ascii_lowercase();
        if terms.iter().any(|term| lower.contains(term)) {
            return (
                current_heading,
                format!("line {}", line_number),
                truncate_snippet(trimmed, 220),
            );
        }
    }
    let location = if fallback_line > 0 {
        format!("line {}", fallback_line)
    } else if current_heading_line > 0 {
        format!("line {}", current_heading_line)
    } else {
        String::new()
    };
    (current_heading, location, truncate_snippet(&fallback, 220))
}

fn truncate_snippet(value: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for ch in value.chars().take(max_chars) {
        out.push(ch);
    }
    if value.chars().count() > max_chars {
        out.push_str("...");
    }
    out
}
