//! Derived retrieval cards. Both tokenizers share one content table per side.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use rusqlite::params;

use super::{SearchIndex, CODE_INDEX, SECTION_INDEX};
use crate::fs_utils::read_text_lossy;
use crate::index::cell;
use crate::parse::{parse_doc_text, split_frontmatter};
use crate::sqlite::SqliteConnection;
use crate::Result;

pub(crate) const BRIDGE_THRESHOLD: f64 = 0.2;

pub(crate) fn refresh_cards(db: &SqliteConnection, root: &Path) -> Result<()> {
    let code = db.query(
        "SELECT id, kind, path, name, signature, doc_comment FROM nodes
         WHERE kind IN ('file', 'symbol') ORDER BY id;",
        [],
    )?;
    let sections = if code.rows.is_empty() {
        BTreeMap::new()
    } else {
        section_cards(db, root)?
    };
    let mut by_path = BTreeMap::<String, Vec<String>>::new();
    for row in &code.rows {
        by_path
            .entry(cell(row, 2).to_string())
            .or_default()
            .push(cell(row, 0).to_string());
    }
    let mut bridges = BTreeMap::<String, BTreeSet<String>>::new();
    let edges = db.query(
        "SELECT e.src, t.id, t.kind, t.path, s.kind, s.path FROM edges e
         JOIN nodes s ON s.id = e.src JOIN nodes t ON t.id = e.dst
         WHERE s.kind IN ('doc', 'section') AND t.kind IN ('file', 'symbol')
           AND (e.confidence = 'EXTRACTED' OR (e.kind = 'cochange' AND e.weight >= ?1))
         ORDER BY e.src, e.dst;",
        [BRIDGE_THRESHOLD],
    )?;
    for edge in &edges.rows {
        let source = cell(edge, 0);
        let mut texts = BTreeSet::new();
        for (id, card) in &sections {
            // A file-level cochange edge carries the document's chapter titles
            // and summaries. Section anchors carry only their own chapter.
            if id == source || (cell(edge, 4) == "doc" && card[1] == cell(edge, 5)) {
                texts.insert(bridge_text(card));
            }
        }
        let targets = if cell(edge, 2) == "file" {
            // File/directory anchors and W4 cochange edges also describe the
            // symbols contained in that file, without inventing inferred edges.
            by_path.get(cell(edge, 3)).cloned().unwrap_or_default()
        } else {
            vec![cell(edge, 1).to_string()]
        };
        for target in targets {
            bridges
                .entry(target)
                .or_default()
                .extend(texts.iter().cloned());
        }
    }
    let mut cards = BTreeMap::new();
    for row in &code.rows {
        let id = cell(row, 0);
        let path = cell(row, 2);
        let name = if cell(row, 1) == "file" {
            Path::new(path)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(path)
        } else {
            cell(row, 3)
        };
        cards.insert(
            id.to_string(),
            vec![
                id.to_string(),
                name.to_string(),
                split_identifier(name),
                format!("{} {}", path, split_identifier(path)),
                cell(row, 4).to_string(),
                cell(row, 5).to_string(),
                bridges
                    .remove(id)
                    .unwrap_or_default()
                    .into_iter()
                    .collect::<Vec<_>>()
                    .join("\n"),
            ],
        );
    }
    db.transaction(|db| {
        sync_cards(db, "doc_sections", &SECTION_INDEX, &sections)?;
        sync_cards(db, "code_cards", &CODE_INDEX, &cards)
    })
}

fn bridge_text(card: &[String]) -> String {
    // Never bridge the body: unrelated prose or another chapter must not become
    // a symbol's natural-language description.
    [&card[2], &card[3], &card[4]]
        .into_iter()
        .filter(|text| !text.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join("\n")
}

fn section_cards(db: &SqliteConnection, root: &Path) -> Result<BTreeMap<String, Vec<String>>> {
    let docs = db.query("SELECT path, title, summary FROM docs ORDER BY path;", [])?;
    let mut cards = BTreeMap::new();
    for doc in &docs.rows {
        let path = cell(doc, 0);
        let text = read_text_lossy(&root.join(path))?.replace("\r\n", "\n");
        let lines = text.lines().collect::<Vec<_>>();
        let sections = db.query(
            "SELECT id, name, span_start, span_end FROM nodes
             WHERE path = ?1 AND kind = 'section' ORDER BY span_start, id;",
            [path],
        )?;
        let first_heading = sections
            .rows
            .first()
            .map(|row| cell(row, 2).parse::<usize>())
            .transpose()?
            .unwrap_or(lines.len() + 1);
        let preamble = lines[..first_heading.saturating_sub(1).min(lines.len())].join("\n");
        let (_, preamble) = split_frontmatter(&preamble);
        let id = format!("doc:{path}");
        if sections.rows.is_empty() || !preamble.trim().is_empty() {
            cards.insert(
                id.clone(),
                vec![
                    id,
                    path.to_string(),
                    cell(doc, 1).to_string(),
                    String::new(),
                    cell(doc, 2).to_string(),
                    preamble.to_string(),
                ],
            );
        }
        for (index, section) in sections.rows.iter().enumerate() {
            let start = cell(section, 2)
                .parse::<usize>()?
                .saturating_sub(1)
                .min(lines.len());
            // Parent node spans include children for anchors; retrieval cards
            // stop at the next heading so sibling vocabulary stays local.
            let next = sections
                .rows
                .get(index + 1)
                .map(|row| cell(row, 2).parse::<usize>())
                .transpose()?
                .unwrap_or(lines.len() + 1);
            let end = cell(section, 3)
                .parse::<usize>()?
                .min(next.saturating_sub(1))
                .min(lines.len())
                .max(start);
            let body = lines[start..end].join("\n");
            let summary = parse_doc_text(path, &body).summary;
            let id = cell(section, 0).to_string();
            cards.insert(
                id.clone(),
                vec![
                    id,
                    path.to_string(),
                    cell(doc, 1).to_string(),
                    cell(section, 1).to_string(),
                    summary,
                    body,
                ],
            );
        }
    }
    Ok(cards)
}

fn sync_cards(
    db: &SqliteConnection,
    table: &str,
    index: &SearchIndex,
    next: &BTreeMap<String, Vec<String>>,
) -> Result<()> {
    let columns = index
        .fields
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(", ");
    let previous = db
        .query(
            &format!("SELECT {columns} FROM {table} ORDER BY node_id;"),
            [],
        )?
        .rows
        .into_iter()
        .map(|row| {
            let values = row
                .into_iter()
                .map(Option::unwrap_or_default)
                .collect::<Vec<_>>();
            (values[0].clone(), values)
        })
        .collect::<BTreeMap<_, _>>();
    let changed = previous
        .keys()
        .chain(next.keys())
        .filter(|id| previous.get(*id) != next.get(*id))
        .cloned()
        .collect::<BTreeSet<_>>();
    for id in changed {
        if previous.contains_key(&id) {
            for fts in [index.fts, index.trigram] {
                db.execute(&format!("INSERT INTO {fts}({fts}, rowid, {columns}) SELECT 'delete', rowid, {columns} FROM {table} WHERE node_id = ?1;"), [&id])?;
            }
            db.execute(&format!("DELETE FROM {table} WHERE node_id = ?1;"), [&id])?;
        }
        if let Some(values) = next.get(&id) {
            let placeholders = (1..=values.len())
                .map(|index| format!("?{index}"))
                .collect::<Vec<_>>()
                .join(", ");
            db.execute(
                &format!("INSERT INTO {table}({columns}) VALUES ({placeholders});"),
                rusqlite::params_from_iter(values),
            )?;
            for fts in [index.fts, index.trigram] {
                db.execute(&format!("INSERT INTO {fts}(rowid, {columns}) SELECT rowid, {columns} FROM {table} WHERE node_id = ?1;"), params![id])?;
            }
        }
    }
    Ok(())
}

fn split_identifier(value: &str) -> String {
    let chars = value.chars().collect::<Vec<_>>();
    let mut out = String::new();
    for (index, ch) in chars.iter().enumerate() {
        if !ch.is_alphanumeric() {
            out.push(' ');
            continue;
        }
        let previous = index.checked_sub(1).map(|index| chars[index]);
        let next = chars.get(index + 1);
        if ch.is_uppercase()
            && previous
                .map(|previous| {
                    previous.is_lowercase()
                        || previous.is_numeric()
                        || (previous.is_uppercase()
                            && next.map(|next| next.is_lowercase()).unwrap_or(false))
                })
                .unwrap_or(false)
        {
            out.push(' ');
        }
        out.extend(ch.to_lowercase());
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_split_acronyms_case_separators_and_path_segments() {
        assert_eq!(
            split_identifier("src/HTTPServer/cacheTtl_v2.rs"),
            "src http server cache ttl v2 rs"
        );
        assert_eq!(
            split_identifier("Session::renew_token"),
            "session renew token"
        );
    }
}
