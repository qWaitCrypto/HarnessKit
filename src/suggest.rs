//! Document-to-code anchor suggestions and application helpers.
//!
//! Suggestions consume the already-built fact store. The CLI refreshes that
//! store before invoking this module.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use serde::Serialize;
use serde_json::{json, Value};

use crate::context::{build_diff_context, validate_diff_repository};
use crate::discover::Coverage;
use crate::fs_utils::{normalize_path, read_text_lossy};
use crate::index::cell;
use crate::knowledge::read_anchor_lock;
use crate::output::println;
use crate::render::{compact, cursor_for_offset, cursor_offset, paginate_lines, RenderLine, RenderOptions};
use crate::schema::Schema;
use crate::sqlite::SqliteConnection;
use crate::Result;

#[derive(Clone, Debug, Serialize)]
pub(crate) struct AnchorSuggestion {
    pub(crate) index: usize,
    pub(crate) doc: String,
    pub(crate) section: String,
    pub(crate) anchor: String,
    pub(crate) reason: String,
    pub(crate) evidence: Value,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct AppliedAnchor {
    pub(crate) index: usize,
    pub(crate) doc: String,
    pub(crate) section: String,
    pub(crate) anchor: String,
}

#[derive(Default)]
struct SuggestionAccumulator {
    doc: String,
    section: String,
    anchor: String,
    score: f64,
    mention: Option<Value>,
    cochange: Option<Value>,
}

pub(crate) fn suggest_anchors(
    db: &SqliteConnection,
    root: &Path,
    schema: &Schema,
    requested_doc: Option<&str>,
) -> Result<Vec<AnchorSuggestion>> {
    let mut doc_paths = BTreeMap::<String, BTreeSet<String>>::new();
    if let Some(doc) = requested_doc {
        let doc = if Path::new(doc).is_absolute() {
            Path::new(doc)
                .strip_prefix(root.canonicalize().unwrap_or_else(|_| root.to_path_buf()))
                .map(normalize_path)
                .unwrap_or_else(|_| normalize_path(Path::new(doc)))
        } else {
            normalize_path(Path::new(doc))
        };
        if db.query("SELECT path FROM docs WHERE path = ?1;", [&doc])?.rows.is_empty() {
            return Err(format!("document not found in fact store: {}", doc).into());
        }
        doc_paths.insert(doc, BTreeSet::new());
    } else {
        // Keep the no-document form aligned with context --diff: changed code
        // areas choose the documents already associated with those areas, then
        // fall back to same-commit pairs when the area has no explicit edge.
        if validate_diff_repository(root).is_err() {
            return Ok(Vec::new());
        }
        let diff = build_diff_context(
            db,
            root,
            schema,
            None,
            &read_anchor_lock(root)?,
        )?;
        for file in &diff.files {
            let preferred = file
                .documents
                .iter()
                .filter(|document| document.constrains || document.active_plan)
                .take(4)
                .collect::<Vec<_>>();
            let mut documents = preferred;
            for document in &file.documents {
                if documents.len() >= 4 {
                    break;
                }
                if !documents.iter().any(|item| item.path == document.path) {
                    documents.push(document);
                }
            }
            for document in documents {
                doc_paths
                    .entry(document.path.clone())
                    .or_default()
                    .insert(file.path.clone());
            }
            if file.documents.is_empty() {
                let rows = db.query(
                    "SELECT doc_path FROM cochange_stats WHERE code_path = ?1
                     ORDER BY weight DESC, doc_path ASC;",
                    [&file.path],
                )?;
                for row in &rows.rows {
                    doc_paths
                        .entry(cell(row, 0).to_string())
                        .or_default()
                        .insert(file.path.clone());
                }
            }
        }
    }

    let existing = existing_anchors(db)?;
    let mut accumulators = BTreeMap::<(String, String), SuggestionAccumulator>::new();

    // Identifier mentions carry the most precise chapter evidence.
    let mention_rows = db.query(
        "SELECT e.src, e.dst, e.weight, e.evidence_json, n.path, n.name, n.kind
         FROM edges e JOIN nodes n ON n.id = e.dst
         WHERE e.kind = 'mention' AND e.confidence = 'INFERRED'
         ORDER BY e.src, n.path, n.name, e.evidence_json;",
        [],
    )?;
    for row in &mention_rows.rows {
        let evidence: Value = serde_json::from_str(cell(row, 3)).unwrap_or_default();
        if evidence["ambiguous"].as_bool() == Some(true) {
            continue;
        }
        let Some(doc) = evidence["doc"].as_str() else { continue };
        let Some(paths) = doc_paths.get(doc) else { continue };
        let path = cell(row, 4).to_string();
        if !paths.is_empty() && !paths.contains(&path) {
            continue;
        }
        let name = cell(row, 5);
        let anchor = format!("{}#{}", path, name);
        if anchor_is_covered(&existing, doc, &path, name) {
            continue;
        }
        let key = (doc.to_string(), anchor.clone());
        let entry = accumulators.entry(key).or_insert_with(|| SuggestionAccumulator {
            doc: doc.to_string(),
            section: evidence["section"].as_str().unwrap_or_default().to_string(),
            anchor,
            ..SuggestionAccumulator::default()
        });
        if entry.section.is_empty() {
            entry.section = evidence["section"].as_str().unwrap_or_default().to_string();
        }
        entry.score = entry.score.max(2.0 + cell(row, 2).parse::<f64>().unwrap_or(0.0));
        entry.mention = Some(json!({
            "kind": "mention",
            "confidence": "INFERRED",
            "weight": cell(row, 2).parse::<f64>().unwrap_or(0.0),
            "source": cell(row, 0),
            "target": format!("{}#{}", path, name),
            "evidence": evidence,
        }));
    }

    // Cochange edges identify a file, so choose its first definition as a
    // deterministic symbol anchor when available; otherwise retain a file
    // anchor.  This keeps suggestions useful without pretending to infer a
    // call graph.
    let cochange_rows = db.query(
        "SELECT doc_path, code_path, weight, cochange_count, last_commit,
                last_timestamp
         FROM cochange_stats ORDER BY doc_path, weight DESC, code_path ASC;",
        [],
    )?;
    for row in &cochange_rows.rows {
        let doc = cell(row, 0);
        let Some(paths) = doc_paths.get(doc) else { continue };
        let path = cell(row, 1).to_string();
        if !paths.is_empty() && !paths.contains(&path) {
            continue;
        }
        let symbol = db.query(
            "SELECT name FROM nodes WHERE kind = 'symbol' AND is_definition = 1
             AND path = ?1 ORDER BY span_start, name LIMIT 1;",
            [&path],
        )?;
        let anchor = symbol
            .rows
            .first()
            .map(|item| format!("{}#{}", path, cell(item, 0)))
            .unwrap_or_else(|| path.clone());
        let name = anchor.split_once('#').map(|(_, name)| name).unwrap_or("");
        if anchor_is_covered(&existing, doc, &path, name) {
            continue;
        }
        let key = (doc.to_string(), anchor.clone());
        let entry = accumulators.entry(key).or_insert_with(|| SuggestionAccumulator {
            doc: doc.to_string(),
            anchor,
            ..SuggestionAccumulator::default()
        });
        entry.score = entry.score.max(
            1.0 + cell(row, 2).parse::<f64>().unwrap_or(0.0)
                + cell(row, 3).parse::<f64>().unwrap_or(0.0).min(8.0) * 0.05,
        );
        entry.cochange = Some(json!({
            "kind": "cochange",
            "confidence": "INFERRED",
            "weight": cell(row, 2).parse::<f64>().unwrap_or(0.0),
            "cochange_count": cell(row, 3).parse::<u64>().unwrap_or(0),
            "last_commit": cell(row, 4),
            "last_timestamp": cell(row, 5).parse::<u64>().unwrap_or(0),
            "doc": doc,
            "code": path,
        }));
    }

    let mut suggestions = accumulators
        .into_values()
        .map(|entry| {
            let section = if entry.section.is_empty() {
                document_fallback_section(db, &entry.doc)?
            } else {
                entry.section
            };
            let mut reasons = Vec::new();
            if let Some(evidence) = &entry.mention {
                let identifier = evidence["evidence"]["identifier"]
                    .as_str()
                    .unwrap_or(&entry.anchor);
                let line = evidence["evidence"]["line"].as_u64().unwrap_or(0);
                reasons.push(if line > 0 {
                    format!("Document mentions `{identifier}` (line {line})")
                } else {
                    format!("Document mentions `{identifier}`")
                });
            }
            if let Some(evidence) = &entry.cochange {
                reasons.push(format!(
                    "Same-commit history: {} occurrence(s), latest {}",
                    evidence["cochange_count"].as_u64().unwrap_or(0),
                    evidence["last_commit"]
                        .as_str()
                        .unwrap_or("unknown commit")
                        .chars()
                        .take(12)
                        .collect::<String>()
                ));
            }
            let evidence = json!({
                "mention": entry.mention,
                "cochange": entry.cochange,
            });
            Ok(AnchorSuggestion {
                index: 0,
                doc: entry.doc,
                section,
                anchor: entry.anchor,
                reason: if reasons.is_empty() {
                    "Code has an explainable historical association with this document.".to_string()
                } else {
                    reasons.join("; ")
                },
                evidence,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    suggestions.sort_by(|left, right| {
        let left_score = suggestion_score(left);
        let right_score = suggestion_score(right);
        right_score
            .total_cmp(&left_score)
            .then_with(|| left.doc.cmp(&right.doc))
            .then_with(|| left.section.cmp(&right.section))
            .then_with(|| left.anchor.cmp(&right.anchor))
    });
    for (index, suggestion) in suggestions.iter_mut().enumerate() {
        suggestion.index = index + 1;
    }
    Ok(suggestions)
}

fn suggestion_score(suggestion: &AnchorSuggestion) -> f64 {
    let mention = suggestion.evidence["mention"]["weight"].as_f64().unwrap_or(0.0);
    let cochange = suggestion.evidence["cochange"]["weight"].as_f64().unwrap_or(0.0);
    2.0 * f64::from(suggestion.evidence["mention"].is_object())
        + mention
        + cochange
}

fn document_fallback_section(db: &SqliteConnection, doc: &str) -> Result<String> {
    let rows = db.query(
        "SELECT heading FROM doc_sections WHERE path = ?1 AND heading <> ''
         ORDER BY node_id LIMIT 1;",
        [doc],
    )?;
    Ok(rows
        .rows
        .first()
        .map(|row| cell(row, 0).to_string())
        .unwrap_or_default())
}

fn existing_anchors(db: &SqliteConnection) -> Result<BTreeMap<String, Vec<String>>> {
    let rows = db.query(
        "SELECT evidence_json FROM edges WHERE kind = 'anchor' AND confidence = 'EXTRACTED';",
        [],
    )?;
    let mut anchors = BTreeMap::<String, Vec<String>>::new();
    for row in &rows.rows {
        let evidence: Value = serde_json::from_str(cell(row, 0)).unwrap_or_default();
        if let (Some(doc), Some(anchor)) = (evidence["doc"].as_str(), evidence["anchor"].as_str()) {
            anchors.entry(doc.to_string()).or_default().push(anchor.to_string());
        }
    }
    Ok(anchors)
}

fn anchor_is_covered(
    existing: &BTreeMap<String, Vec<String>>,
    doc: &str,
    path: &str,
    name: &str,
) -> bool {
    existing.get(doc).into_iter().flatten().any(|anchor| {
        let (anchor_path, fragment) = anchor.split_once('#').unwrap_or((anchor, ""));
        let path_match = anchor_path == path
            || (anchor_path.ends_with('/') && path.starts_with(anchor_path));
        path_match && (fragment.is_empty() || fragment == name || fragment.ends_with(&format!("::{name}")))
    })
}

pub(crate) fn apply_anchor_suggestions(
    root: &Path,
    suggestions: &[AnchorSuggestion],
    indexes: &[usize],
) -> Result<Vec<AppliedAnchor>> {
    let mut selected = indexes.to_vec();
    selected.sort_unstable();
    selected.dedup();
    if let Some(index) = selected.iter().find(|index| **index == 0 || **index > suggestions.len()) {
        return Err(format!("anchor suggestion index out of range: {}", index).into());
    }
    let mut documents = BTreeMap::<String, String>::new();
    let mut applied = Vec::new();
    for index in selected {
        let suggestion = &suggestions[index - 1];
        let text = documents
            .entry(suggestion.doc.clone())
            .or_insert(read_text_lossy(&root.join(&suggestion.doc))?);
        let code_line = format!("Code: `{}`", suggestion.anchor);
        if text.contains(&code_line) {
            continue;
        }
        append_code_line(text, &suggestion.section, &code_line);
        applied.push(AppliedAnchor {
            index: suggestion.index,
            doc: suggestion.doc.clone(),
            section: suggestion.section.clone(),
            anchor: suggestion.anchor.clone(),
        });
    }
    for (doc, text) in documents {
        fs::write(root.join(doc), text)?;
    }
    Ok(applied)
}

fn append_code_line(text: &mut String, section: &str, code_line: &str) {
    let insertion = section_end(text, section);
    let mut addition = String::new();
    let before = &text[..insertion];
    if before.is_empty() {
        addition.push_str(code_line);
        addition.push('\n');
    } else if before.ends_with("\n\n") {
        addition.push_str(code_line);
        addition.push('\n');
    } else if before.ends_with('\n') {
        addition.push('\n');
        addition.push_str(code_line);
        addition.push('\n');
    } else {
        addition.push('\n');
        addition.push('\n');
        addition.push_str(code_line);
        addition.push('\n');
    }
    text.insert_str(insertion, &addition);
}

fn section_end(text: &str, section: &str) -> usize {
    if section.is_empty() {
        return text.len();
    }
    let mut offset = 0usize;
    let mut start_level = None;
    for line in text.split_inclusive('\n') {
        let content = line.trim_end_matches(['\r', '\n']);
        if let Some(level) = heading_level(content) {
            let title = content[level..].trim();
            if start_level.is_none() && heading_matches(title, section) {
                start_level = Some(level);
            } else if let Some(start) = start_level {
                if level <= start {
                    return offset;
                }
            }
        }
        offset += line.len();
    }
    text.len()
}

fn heading_level(line: &str) -> Option<usize> {
    let level = line.chars().take_while(|ch| *ch == '#').count();
    (level > 0 && line.chars().nth(level) == Some(' ')).then_some(level)
}

fn heading_matches(raw: &str, expected: &str) -> bool {
    let normalize = |value: &str| {
        value
            .trim_end_matches('#')
            .replace('`', "")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    normalize(raw) == normalize(expected)
}

fn render_suggestion_lines(
    suggestions: &[AnchorSuggestion],
    applied: &[AppliedAnchor],
) -> Vec<RenderLine> {
    let mut lines = Vec::new();
    if suggestions.is_empty() {
        lines.push(RenderLine::content("No new anchor suggestions found."));
    }
    for suggestion in suggestions {
        lines.push(RenderLine::content(format!(
            "{}. {} §{} — Code: `{}`",
            suggestion.index,
            suggestion.doc,
            if suggestion.section.is_empty() { "document" } else { &suggestion.section },
            suggestion.anchor
        )));
        lines.push(RenderLine::content(format!("   {}", suggestion.reason)));
        lines.push(RenderLine::related(format!(
            "   evidence: {}",
            compact(&suggestion.evidence.to_string(), 360)
        )));
    }
    if !applied.is_empty() {
        lines.push(RenderLine::hint(format!("Applied {} anchor suggestion(s).", applied.len())));
    }
    lines
}

pub(crate) fn print_suggest_anchors(
    suggestions: &[AnchorSuggestion],
    applied: &[AppliedAnchor],
    options: &RenderOptions,
) {
    let page = paginate_lines(
        render_suggestion_lines(suggestions, applied),
        options,
        "suggest-anchors",
    );
    for line in page.lines {
        println!("{}", line);
    }
}

pub(crate) fn render_suggest_json(
    suggestions: &[AnchorSuggestion],
    coverage: &Coverage,
    hints: &[String],
    options: &RenderOptions,
    applied: &[AppliedAnchor],
) -> String {
    let offset = options
        .cursor
        .as_deref()
        .and_then(cursor_offset)
        .unwrap_or_else(|| options.page.saturating_sub(1) * 8);
    let mut used = 0usize;
    let mut end = offset.min(suggestions.len());
    while end < suggestions.len() {
        let cost = crate::render::token_cost(&format!(
            "{} {} {} {}",
            suggestions[end].doc, suggestions[end].section, suggestions[end].anchor, suggestions[end].reason
        ));
        if end > offset && used + cost > options.budget {
            break;
        }
        used += cost;
        end += 1;
    }
    let start = offset.min(suggestions.len());
    let selected = &suggestions[start..end];
    let truncated = suggestions.len().saturating_sub(end);
    let next = (end < suggestions.len()).then(|| cursor_for_offset(end, "suggest-anchors"));
    let report = json!({
        "suggestions": selected,
        "applied": applied,
        "applied_count": applied.len(),
        "truncated": truncated,
        "next_cursor": next,
    });
    crate::render::render_covered_json_page(
        &report,
        coverage,
        hints,
        next.as_deref(),
        truncated,
        options.full,
    )
}

