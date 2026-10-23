mod cards;
mod mixed;

pub(crate) use cards::refresh_cards;
pub(crate) use mixed::{query_bundles, BundleNode, QueryBundle, QuerySide, DEFAULT_BUNDLE_LIMIT};

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use crate::discover::Coverage;
use crate::index::{cell, scalar_count};
use crate::parse::parse_heading;
use crate::sqlite::SqliteConnection;
use crate::Result;

pub(crate) const DEFAULT_LIMIT: usize = 8;

#[derive(Clone, serde::Serialize)]
pub(crate) struct QueryCandidate {
    pub(crate) path: String,
    pub(crate) title: String,
    pub(crate) role: String,
    pub(crate) status: String,
    pub(crate) score: String,
    pub(crate) why: String,
    pub(crate) summary: String,
    #[serde(serialize_with = "crate::render::serialize_inline")]
    pub(crate) neighbors: Vec<String>,
    pub(crate) snippet_heading: String,
    pub(crate) snippet_location: String,
    pub(crate) snippet: String,
    #[serde(rename = "match", skip_serializing_if = "Option::is_none")]
    pub(crate) match_info: Option<QueryMatch>,
    pub(crate) lifecycle: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) superseded_by: Vec<String>,
}

#[derive(Clone, serde::Serialize)]
pub(crate) struct QueryMatch {
    pub(crate) matched_terms: Vec<String>,
    pub(crate) missing_terms: Vec<String>,
    pub(crate) mode: String,
}

#[derive(serde::Serialize)]
pub(crate) struct QueryDiagnostics {
    pub(crate) terms: Vec<TermDiagnostic>,
    pub(crate) target_dir: String,
    pub(crate) target_dir_empty: bool,
    pub(crate) indexed_docs: usize,
    pub(crate) index_empty: bool,
    pub(crate) query_empty: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) indexed_code_nodes: Option<usize>,
}

#[derive(serde::Serialize)]
pub(crate) struct TermDiagnostic {
    pub(crate) term: String,
    pub(crate) managed_hits: usize,
    pub(crate) unmanaged_hits: usize,
    pub(crate) unmanaged_paths: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) code_hits: Option<usize>,
}

#[derive(serde::Serialize)]
pub(crate) struct TitleSuggestion {
    pub(crate) path: String,
    pub(crate) title: String,
    pub(crate) similarity: f64,
}

fn query_tokens(query: &str) -> Vec<String> {
    let mut seen = BTreeSet::new();
    query
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .map(str::to_lowercase)
        .filter(|term| seen.insert(term.clone()))
        .collect()
}

fn contains_cjk(term: &str) -> bool {
    term.chars().any(|ch| {
        matches!(ch as u32,
        0x2e80..=0x9fff | 0xac00..=0xd7af | 0xf900..=0xfaff | 0x20000..=0x323af)
    })
}

// One posting list per term lets AND, OR, match explanations and zero-result
// diagnostics use exactly the same matching rules (including short CJK terms).
struct SearchIndex {
    fts: &'static str,
    trigram: &'static str,
    key: &'static str,
    fields: &'static [(&'static str, f64)],
}

const DOC_INDEX: SearchIndex = SearchIndex {
    fts: "docs_fts",
    trigram: "docs_trigram",
    key: "path",
    fields: &[
        ("path", 8.0),
        ("title", 12.0),
        ("headings_text", 5.0),
        ("summary", 3.0),
        ("body_tokens", 1.0),
    ],
};

const SECTION_INDEX: SearchIndex = SearchIndex {
    fts: "sections_fts",
    trigram: "sections_trigram",
    key: "node_id",
    fields: &[
        ("node_id", 0.0),
        ("path", 8.0),
        ("title", 12.0),
        ("heading", 5.0),
        ("summary", 3.0),
        ("body", 1.0),
    ],
};

const CODE_INDEX: SearchIndex = SearchIndex {
    fts: "code_fts",
    trigram: "code_trigram",
    key: "node_id",
    fields: &[
        ("node_id", 0.0),
        ("name", 12.0),
        ("split_name", 10.0),
        ("path", 8.0),
        ("signature", 4.0),
        ("doc_comment", 3.0),
        ("bridged", 2.0),
    ],
};

const NATIVE_CODE_INDEX: SearchIndex = SearchIndex {
    fields: &[
        ("node_id", 0.0),
        ("name", 12.0),
        ("split_name", 10.0),
        ("path", 8.0),
        ("signature", 4.0),
        ("doc_comment", 3.0),
        ("bridged", 0.0),
    ],
    ..CODE_INDEX
};

fn bm25_hits(
    db: &SqliteConnection,
    index: &SearchIndex,
    phrase: &str,
    trigram: bool,
) -> Result<BTreeMap<String, f64>> {
    let table = if trigram { index.trigram } else { index.fts };
    let weights = index
        .fields
        .iter()
        .map(|(_, weight)| weight.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let columns = index
        .fields
        .iter()
        .filter(|(_, weight)| *weight > 0.0)
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(" ");
    let quoted = format!("{{{columns}}} : \"{}\"", phrase.replace('"', "\"\""));
    let rows = db.query(
        &format!(
            "SELECT {}, -bm25({table}, {weights}) FROM {table} WHERE {table} MATCH ?1;",
            index.key
        ),
        [&quoted],
    )?;
    rows.rows
        .iter()
        .map(|row| Ok((cell(row, 0).to_string(), cell(row, 1).parse()?)))
        .collect()
}

fn search_term_hits(
    db: &SqliteConnection,
    index: &SearchIndex,
    term: &str,
) -> Result<BTreeMap<String, f64>> {
    let mut hits = bm25_hits(db, index, term, false)?;
    if term.chars().count() >= 3 {
        for (key, score) in bm25_hits(db, index, term, true)? {
            // Exact unicode tokens keep their BM25 score; trigram adds substrings.
            hits.entry(key).or_insert(score);
        }
    } else if contains_cjk(term) {
        // SQLite's trigram tokenizer cannot index one- or two-character terms.
        // query_tokens removes LIKE metacharacters before this bound pattern.
        let mut fields = index
            .fields
            .iter()
            .filter(|(_, weight)| *weight > 0.0)
            .collect::<Vec<_>>();
        fields.sort_by(|a, b| b.1.total_cmp(&a.1));
        let cases = fields
            .iter()
            .map(|(field, weight)| format!("WHEN {field} LIKE ?1 THEN {weight}"))
            .collect::<Vec<_>>()
            .join(" ");
        let conditions = fields
            .iter()
            .map(|(field, _)| format!("{field} LIKE ?1"))
            .collect::<Vec<_>>()
            .join(" OR ");
        let rows = db.query(
            &format!(
                "SELECT {}, CASE {cases} ELSE 1.0 END FROM {} WHERE {conditions};",
                index.key, index.fts
            ),
            [format!("%{}%", term)],
        )?;
        for row in rows.rows {
            hits.entry(cell(&row, 0).to_string())
                .or_insert(cell(&row, 1).parse::<f64>()? * 0.000001);
        }
    }
    Ok(hits)
}

fn term_hits(db: &SqliteConnection, term: &str) -> Result<BTreeMap<String, f64>> {
    search_term_hits(db, &DOC_INDEX, term)
}

pub(crate) fn query_candidates(
    db: &SqliteConnection,
    target_dir: &Path,
    query_terms: &str,
    limit: usize,
) -> Result<Vec<QueryCandidate>> {
    let terms = query_tokens(query_terms);
    let mut matches: BTreeMap<String, (Vec<String>, f64)> = BTreeMap::new();
    for term in &terms {
        for (path, score) in term_hits(db, term)? {
            let entry = matches.entry(path).or_default();
            entry.0.push(term.clone());
            entry.1 += score;
        }
    }
    // Preserve phrase relevance when relaxing individual terms. A contiguous
    // phrase is stronger evidence than the same words scattered through a doc.
    if terms.len() > 1 {
        let rows = db.query(
            "SELECT path, -bm25(docs_fts, 8.0, 12.0, 5.0, 3.0, 1.0)
             FROM docs_fts WHERE docs_fts MATCH ?1;",
            [format!("\"{}\"", terms.join(" "))],
        )?;
        for row in rows.rows {
            if let Some((_, score)) = matches.get_mut(cell(&row, 0)) {
                *score += cell(&row, 1).parse::<f64>()?;
            }
        }
    }
    let strict_count = matches
        .values()
        .filter(|(found, _)| found.len() == terms.len())
        .count();
    let mut ranked = Vec::new();
    for (path, (matched_terms, score)) in matches {
        let strict = matched_terms.len() == terms.len();
        if strict_count >= 3 && !strict {
            continue;
        }
        let rows = db.query(
            "SELECT title, role, COALESCE(status, ''), summary, incoming_links FROM docs WHERE path = ?1;",
            [&path],
        )?;
        let Some(row) = rows.rows.first() else {
            continue;
        };
        // Keep the existing graph signal, bounded so link volume cannot swamp relevance.
        let incoming = cell(row, 4).parse::<usize>()?.min(5);
        let score = score * (1.0 + incoming as f64 * 0.05);
        let superseded_by = superseded_by(db, &path)?;
        let lifecycle = doc_lifecycle(&path, cell(row, 2), cell(row, 3), &superseded_by);
        let missing_terms = terms
            .iter()
            .filter(|term| !matched_terms.contains(term))
            .cloned()
            .collect();
        let candidate = QueryCandidate {
            path,
            title: cell(row, 0).to_string(),
            role: cell(row, 1).to_string(),
            status: cell(row, 2).to_string(),
            score: format!("{:.6}", score),
            why: if strict {
                "all query terms matched"
            } else {
                "relaxed to partial term matches"
            }
            .to_string(),
            summary: cell(row, 3).to_string(),
            neighbors: Vec::new(),
            snippet_heading: String::new(),
            snippet_location: String::new(),
            snippet: String::new(),
            match_info: Some(QueryMatch {
                matched_terms,
                missing_terms,
                mode: if strict { "strict" } else { "relaxed" }.to_string(),
            }),
            lifecycle,
            superseded_by,
        };
        ranked.push((candidate, score));
    }
    ranked.sort_by(|(a, a_score), (b, b_score)| {
        let count = |doc: &QueryCandidate| doc.match_info.as_ref().unwrap().matched_terms.len();
        count(b)
            .cmp(&count(a))
            // Prefer current evidence among documents with equal term coverage.
            .then_with(|| (a.lifecycle == "historical").cmp(&(b.lifecycle == "historical")))
            .then_with(|| b_score.total_cmp(a_score))
            .then_with(|| a.path.cmp(&b.path))
    });
    ranked.truncate(limit);
    let mut candidates = Vec::with_capacity(ranked.len());
    for (mut candidate, _) in ranked {
        candidate.neighbors = query_neighbors(db, &candidate.path)?;
        let (heading, location, snippet) =
            snippet_for_doc(target_dir, &candidate.path, query_terms);
        candidate.snippet_heading = heading;
        candidate.snippet_location = location;
        candidate.snippet = snippet;
        candidates.push(candidate);
    }
    Ok(candidates)
}

pub(crate) fn superseded_by(db: &SqliteConnection, path: &str) -> Result<Vec<String>> {
    Ok(db
        .query(
            "SELECT doc_path FROM supersedes WHERE superseded_path = ?1
         UNION SELECT successor_path FROM superseded_by WHERE doc_path = ?1
         ORDER BY 1;",
            [path],
        )?
        .rows
        .iter()
        .map(|row| cell(row, 0).to_string())
        .collect())
}

pub(crate) use crate::lifecycle::doc_lifecycle;

fn query_neighbors(db: &SqliteConnection, path: &str) -> Result<Vec<String>> {
    let rows = db.query(
        "WITH neighbors(path) AS (
            SELECT dst_path FROM relations WHERE src_path = ?1
            UNION SELECT src_path FROM relations WHERE dst_path = ?1
            UNION SELECT sibling.dst_path FROM relations parent
                JOIN relations sibling ON sibling.src_path = parent.src_path
                WHERE parent.dst_path = ?1 AND parent.relation_type = 'doc_indexes_doc'
                  AND sibling.relation_type = 'doc_indexes_doc'
            UNION SELECT scoped.doc_path FROM scope_paths base
                JOIN scope_paths scoped ON scoped.scope_path = base.scope_path
                WHERE base.doc_path = ?1
         )
         SELECT d.path FROM neighbors n JOIN docs d ON d.path = n.path
         WHERE d.path != ?1 AND d.is_entrypoint = 0
           AND d.role NOT IN ('manifest', 'collection-index')
           AND lower(d.path) NOT LIKE '%/index.md' AND lower(d.path) != 'index.md'
         ORDER BY d.path LIMIT 5;",
        [path],
    )?;
    Ok(rows
        .rows
        .iter()
        .map(|row| cell(row, 0).to_string())
        .collect())
}

pub(crate) fn diagnose_query(
    db: &SqliteConnection,
    target_dir: &Path,
    query_terms: &str,
    coverage: &Coverage,
    side: QuerySide,
) -> Result<QueryDiagnostics> {
    let mut terms = Vec::new();
    for term in query_tokens(query_terms) {
        let mut unmanaged_paths = Vec::new();
        let mut unmanaged_hits = 0;
        for mapping in coverage
            .mappings
            .iter()
            .filter(|mapping| mapping.status == "unmanaged")
        {
            let content = fs::read(target_dir.join(&mapping.path))?;
            if String::from_utf8_lossy(&content)
                .to_lowercase()
                .contains(&term)
            {
                unmanaged_hits += 1;
                if unmanaged_paths.len() < 5 {
                    unmanaged_paths.push(mapping.path.clone());
                }
            }
        }
        terms.push(TermDiagnostic {
            managed_hits: if side == QuerySide::Code {
                0
            } else {
                term_hits(db, &term)?.len()
            },
            code_hits: if side == QuerySide::Docs {
                None
            } else {
                Some(search_term_hits(db, &CODE_INDEX, &term)?.len())
            },
            term,
            unmanaged_hits,
            unmanaged_paths,
        });
    }
    let indexed_docs = scalar_count(db, "SELECT COUNT(*) FROM docs;")?;
    let indexed_code_nodes = scalar_count(db, "SELECT COUNT(*) FROM code_cards;")?;
    let target_dir_empty = fs::read_dir(target_dir)?.all(|entry| {
        entry
            .map(|entry| matches!(entry.file_name().to_str(), Some(".harnesskit" | ".git")))
            .unwrap_or(false)
    });
    Ok(QueryDiagnostics {
        query_empty: terms.is_empty(),
        terms,
        target_dir: target_dir.canonicalize()?.display().to_string(),
        target_dir_empty,
        indexed_docs,
        index_empty: match side {
            QuerySide::Docs => indexed_docs == 0,
            QuerySide::Code => indexed_code_nodes == 0,
            QuerySide::Mixed => indexed_docs + indexed_code_nodes == 0,
        },
        indexed_code_nodes: (side != QuerySide::Docs).then_some(indexed_code_nodes),
    })
}

pub(crate) fn diagnostic_hints(diagnostics: &QueryDiagnostics) -> Vec<String> {
    let mut hints = Vec::new();
    let mut terms = diagnostics.terms.iter().collect::<Vec<_>>();
    terms.sort_by_key(|term| {
        let hits = term.managed_hits + term.code_hits.unwrap_or(0);
        (
            hits == 0,
            hits,
            term.unmanaged_hits == 0,
            term.unmanaged_hits,
            std::cmp::Reverse(term.term.chars().count()),
        )
    });
    if terms.is_empty() {
        hints.push(
            "Provide one or two searchable words; punctuation alone is not a query.".to_string(),
        );
    } else {
        hints.push(format!(
            "Try `{}` (the most distinctive available terms).",
            terms
                .iter()
                .take(2)
                .map(|term| term.term.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        ));
    }
    if diagnostics.index_empty {
        if diagnostics.indexed_code_nodes.is_some() {
            hints.push(format!("No searchable code cards at {}. Review tracked files, ignore rules and `harnesskit doctor --code`.", diagnostics.target_dir));
        } else {
            hints.push(format!("The index has no managed documents at {}. Review target_dir and `harnesskit doctor --layout`.", diagnostics.target_dir));
        }
    }
    for term in terms.iter().filter(|term| term.unmanaged_hits > 0) {
        hints.push(format!("`{}` occurs in {} unmanaged file(s): {}. Review coverage and add a schema include mapping.", term.term, term.unmanaged_hits, term.unmanaged_paths.join(", ")));
    }
    hints
}

fn trigrams(text: &str) -> BTreeSet<String> {
    let chars = text.to_lowercase().chars().collect::<Vec<_>>();
    chars
        .windows(3)
        .map(|window| window.iter().collect())
        .collect()
}

fn trigram_similarity(a: &BTreeSet<String>, b: &BTreeSet<String>) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    2.0 * a.intersection(b).count() as f64 / (a.len() + b.len()) as f64
}

pub(crate) fn title_suggestions(
    db: &SqliteConnection,
    query: &str,
) -> Result<Vec<TitleSuggestion>> {
    let query_grams = trigrams(&query_tokens(query).join(" "));
    if query_grams.is_empty() {
        return Ok(Vec::new());
    }
    let rows = db.query("SELECT path, title FROM docs WHERE is_entrypoint = 0 AND role NOT IN ('manifest', 'collection-index') ORDER BY path;", [])?;
    let mut suggestions = Vec::new();
    for row in rows.rows {
        let title = cell(&row, 1);
        let similarity = query_tokens(title)
            .iter()
            .map(|word| trigram_similarity(&query_grams, &trigrams(word)))
            .fold(trigram_similarity(&query_grams, &trigrams(title)), f64::max);
        if similarity >= 0.3 {
            suggestions.push(TitleSuggestion {
                path: cell(&row, 0).to_string(),
                title: title.to_string(),
                similarity,
            });
        }
    }
    suggestions.sort_by(|a, b| {
        b.similarity
            .total_cmp(&a.similarity)
            .then_with(|| a.path.cmp(&b.path))
    });
    suggestions.truncate(3);
    Ok(suggestions)
}

pub(crate) fn snippet_for_doc(
    target_dir: &Path,
    doc_path: &str,
    query_terms: &str,
) -> (String, String, String) {
    let text = fs::read_to_string(target_dir.join(doc_path)).unwrap_or_default();
    let terms = query_tokens(query_terms);
    let mut current_heading = String::new();
    let mut frontmatter = false;
    let mut best = (String::new(), String::new(), String::new());
    let mut best_count = 0;
    for (index, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if index == 0 && trimmed == "---" {
            frontmatter = true;
            continue;
        }
        if frontmatter {
            if trimmed == "---" {
                frontmatter = false;
            }
            continue;
        }
        if let Some((_level, heading)) = parse_heading(line) {
            current_heading = heading;
        }
        let lower = trimmed.to_lowercase();
        let offsets = terms
            .iter()
            .filter_map(|term| matching_offset(&lower, term))
            .collect::<Vec<_>>();
        let count = offsets.len();
        if count > best_count {
            let first_match = *offsets.iter().min().unwrap();
            // Lowercasing can expand a character (e.g. İ -> i + combining dot).
            // Translate the match back to the original line before slicing it.
            let mut lower_offset = 0;
            let char_offset = trimmed
                .chars()
                .take_while(|ch| {
                    let before = lower_offset;
                    lower_offset += ch.to_lowercase().map(char::len_utf8).sum::<usize>();
                    before < first_match
                })
                .count();
            best = (
                current_heading.clone(),
                format!("line {}", index + 1),
                truncate_snippet(trimmed, char_offset, 220),
            );
            best_count = count;
        }
    }
    best
}

fn matching_offset(line: &str, term: &str) -> Option<usize> {
    line.match_indices(term).find_map(|(offset, _)| {
        let substring = term.chars().count() >= 3 || contains_cjk(term);
        let boundary = !line[..offset]
            .chars()
            .next_back()
            .map(char::is_alphanumeric)
            .unwrap_or(false)
            && !line[offset + term.len()..]
                .chars()
                .next()
                .map(char::is_alphanumeric)
                .unwrap_or(false);
        (substring || boundary).then_some(offset)
    })
}

fn truncate_snippet(value: &str, match_offset: usize, max_chars: usize) -> String {
    let chars = value.chars().collect::<Vec<_>>();
    let start = match_offset.saturating_sub(60).min(chars.len());
    let end = (start + max_chars).min(chars.len());
    let mut out = if start > 0 {
        "...".to_string()
    } else {
        String::new()
    };
    out.extend(&chars[start..end]);
    if end < chars.len() {
        out.push_str("...");
    }
    out
}
