mod markdown;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};

use rusqlite::params;
use serde::Serialize;
use serde_json::{json, Value};

use crate::checks::{complete_check, IndexCheck};
use crate::fs_utils::read_text_lossy;
use crate::index::{cell, IndexedDoc};
use crate::sqlite::SqliteConnection;
use crate::Result;

#[derive(Serialize)]
struct Target {
    id: String,
    path: String,
    name: String,
    span_start: usize,
    span_end: usize,
}

impl Target {
    fn label(&self) -> String {
        if self.name.is_empty() {
            self.path.clone()
        } else {
            format!("{}#{} (L{})", self.path, self.name, self.span_start)
        }
    }
}

#[derive(Default)]
struct Targets {
    files: BTreeMap<String, Target>,
    symbols: BTreeMap<String, Vec<Target>>,
    aliases: BTreeMap<String, Vec<(String, usize)>>,
}

impl Targets {
    fn load(db: &SqliteConnection) -> Result<Self> {
        let rows = db.query(
            "SELECT id, kind, path, name, span_start, span_end FROM nodes
             WHERE kind IN ('file', 'symbol', 'doc') ORDER BY path, span_start, id;",
            [],
        )?;
        let mut targets = Self::default();
        for row in &rows.rows {
            let path = cell(row, 2).to_string();
            let target = Target {
                id: cell(row, 0).to_string(),
                path: path.clone(),
                name: if cell(row, 1) == "symbol" {
                    cell(row, 3).to_string()
                } else {
                    String::new()
                },
                span_start: cell(row, 4).parse().unwrap_or(1),
                span_end: cell(row, 5).parse().unwrap_or(1),
            };
            if cell(row, 1) == "symbol" {
                let symbols = targets.symbols.entry(path.clone()).or_default();
                let parts = target.name.split("::").collect::<Vec<_>>();
                for index in 0..parts.len() {
                    targets
                        .aliases
                        .entry(parts[index..].join("::"))
                        .or_default()
                        .push((path.clone(), symbols.len()));
                }
                symbols.push(target);
            } else {
                targets.files.insert(path, target);
            }
        }
        Ok(targets)
    }

    fn matching_symbols(&self, name: &str, path: Option<&str>) -> Vec<&Target> {
        self.aliases
            .get(name)
            .into_iter()
            .flatten()
            .filter(|(found, _)| path.map(|path| path == found).unwrap_or(true))
            .map(|(path, index)| &self.symbols[path][*index])
            .collect()
    }

    fn path_targets(&self, path: &str, directory: bool) -> Vec<&Target> {
        if !directory {
            if let Some(file) = self.files.get(path) {
                return vec![file];
            }
        }
        let prefix = format!("{path}/");
        self.files
            .range(prefix.clone()..)
            .take_while(|(path, _)| path.starts_with(&prefix))
            .map(|(_, target)| target)
            .collect()
    }
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Edge {
    src: String,
    dst: String,
    evidence: String,
}

pub(crate) fn refresh_anchor_facts(
    db: &SqliteConnection,
    root: &Path,
    docs: &[IndexedDoc],
) -> Result<Vec<IndexCheck>> {
    let mut checks = Vec::new();
    let suppressed_docs = docs
        .iter()
        .filter(|doc| suppress_diagnostics_for_doc(&doc.path))
        .map(|doc| doc.path.as_str())
        .collect::<BTreeSet<_>>();
    db.transaction(|db| {
        refresh_sources(db, root, docs)?;
        let targets = Targets::load(db)?;
        let spans = db.query(
            "SELECT doc_path, src, section, line, column_number, text FROM inline_code_spans
             ORDER BY doc_path, span_index;",
            [],
        )?;
        let mut edges = Vec::new();
        for row in &spans.rows {
            let evidence = json!({
                "doc": cell(row, 0), "section": cell(row, 2),
                "line": cell(row, 3).parse::<usize>()?,
                "column": cell(row, 4).parse::<usize>()?, "anchor": cell(row, 5),
            });
            resolve(
                &targets,
                cell(row, 1),
                evidence,
                suppressed_docs.contains(cell(row, 0)),
                &mut edges,
                &mut checks,
            );
        }
        edges.sort();
        edges.dedup();
        let previous = db
            .query(
                "SELECT src, dst, evidence_json FROM edges WHERE kind = 'anchor'
             AND confidence = 'EXTRACTED' ORDER BY src, dst, evidence_json;",
                [],
            )?
            .rows
            .iter()
            .map(|row| Edge {
                src: cell(row, 0).to_string(),
                dst: cell(row, 1).to_string(),
                evidence: cell(row, 2).to_string(),
            })
            .collect::<Vec<_>>();
        if previous != edges {
            db.exec("DELETE FROM edges WHERE kind = 'anchor' AND confidence = 'EXTRACTED';")?;
            for edge in edges {
                db.execute(
                    "INSERT INTO edges(src, dst, kind, confidence, weight, evidence_json)
                     VALUES (?1, ?2, 'anchor', 'EXTRACTED', 1.0, ?3);",
                    params![edge.src, edge.dst, edge.evidence],
                )?;
            }
        }
        Ok(())
    })?;
    Ok(checks)
}

fn refresh_sources(db: &SqliteConnection, root: &Path, docs: &[IndexedDoc]) -> Result<()> {
    let previous = db
        .query(
            "SELECT path, fingerprint FROM nodes WHERE kind = 'doc';",
            [],
        )?
        .rows
        .iter()
        .map(|row| (cell(row, 0).to_string(), cell(row, 1).to_string()))
        .collect::<BTreeMap<_, _>>();
    let current = docs
        .iter()
        .map(|doc| doc.path.as_str())
        .collect::<BTreeSet<_>>();
    for path in previous
        .keys()
        .filter(|path| !current.contains(path.as_str()))
    {
        delete_source(db, path)?;
    }
    for doc in docs {
        if previous.get(&doc.path) == Some(&doc.content_hash) {
            continue;
        }
        let text = read_text_lossy(&root.join(&doc.path))?;
        let parsed = markdown::parse(&doc.path, &text);
        delete_source(db, &doc.path)?;
        db.execute(
            "INSERT INTO nodes(id, kind, path, name, span_start, span_end, fingerprint, lang)
             VALUES (?1, 'doc', ?2, ?3, 1, ?4, ?5, 'markdown');",
            params![
                format!("doc:{}", doc.path),
                doc.path,
                doc.title,
                text.lines().count().max(1),
                doc.content_hash
            ],
        )?;
        for section in parsed.sections {
            db.execute(
                "INSERT INTO nodes(id, kind, path, name, span_start, span_end, fingerprint, lang)
                 VALUES (?1, 'section', ?2, ?3, ?4, ?5, ?6, 'markdown');",
                params![
                    section.id,
                    doc.path,
                    section.name,
                    section.span_start,
                    section.span_end,
                    section.fingerprint
                ],
            )?;
        }
        for (index, span) in parsed.spans.iter().enumerate() {
            db.execute(
                "INSERT INTO inline_code_spans(doc_path, span_index, src, section, line, column_number, text)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7);",
                params![doc.path, index, span.src, span.section, span.line, span.column, span.text],
            )?;
        }
    }
    Ok(())
}

fn delete_source(db: &SqliteConnection, path: &str) -> Result<()> {
    db.execute(
        "DELETE FROM nodes WHERE path = ?1 AND kind IN ('doc', 'section');",
        [path],
    )?;
    db.execute("DELETE FROM inline_code_spans WHERE doc_path = ?1;", [path])
}

fn resolve(
    targets: &Targets,
    src: &str,
    mut evidence: Value,
    suppress_diagnostics: bool,
    edges: &mut Vec<Edge>,
    checks: &mut Vec<IndexCheck>,
) {
    let raw = evidence["anchor"].as_str().unwrap().to_string();
    let doc = evidence["doc"].as_str().unwrap();
    let (path, fragment) = raw
        .split_once('#')
        .map(|(path, name)| (path, Some(name)))
        .unwrap_or((&raw, None));
    if path.is_empty() {
        return;
    }
    let global = fragment.is_none() && raw.contains("::");
    if global {
        // Plain identifier mentions belong to W4. Unknown qualified names can
        // also be ordinary code snippets, so they do not produce diagnostics.
        let matches = targets.matching_symbols(&raw, None);
        if matches.is_empty() {
            return;
        }
        if matches.len() > 1 {
            if !suppress_diagnostics && !is_placeholder_anchor(&raw) {
                report(
                    checks,
                    "anchor-ambiguous",
                    &evidence,
                    &matches,
                    "Use a path#fully::qualified::symbol anchor to select one candidate.",
                );
            }
            return;
        }
        for target in matches {
            edges.push(Edge {
                src: src.to_string(),
                dst: target.id.clone(),
                evidence: evidence.to_string(),
            });
        }
        return;
    }

    let paths = path_candidates(doc, path);
    let directory = path.ends_with('/');
    let resolved_path = paths
        .iter()
        .find(|path| !targets.path_targets(path, directory).is_empty());
    // An unqualified, missing path is ordinary prose. A qualified anchor is
    // only actionable after its file has been found in the indexed scope.
    let Some(path) = resolved_path else {
        return;
    };
    if fragment.is_some() && directory {
        return;
    }
    evidence["target_path"] = path.clone().into();
    let matches = if let Some(fragment) = fragment {
        if let Some((start, end)) = line_range(fragment) {
            evidence["target_span"] = json!({"start": start, "end": end});
            let candidates = targets
                .symbols
                .get(path)
                .into_iter()
                .flatten()
                .filter(|symbol| symbol.span_start <= end && symbol.span_end >= start)
                .collect::<Vec<_>>();
            if !suppress_diagnostics && !is_placeholder_anchor(&raw) {
                report(
                    checks,
                    "anchor-line-number",
                    &evidence,
                    &candidates,
                    "Use a path#symbol anchor instead of a line-number anchor.",
                );
            }
            targets
                .files
                .get(path)
                .filter(|file| end <= file.span_end)
                .into_iter()
                .collect()
        } else {
            targets.matching_symbols(fragment, Some(path))
        }
    } else {
        targets.path_targets(path, directory)
    };
    if matches.is_empty() {
        if fragment.is_some() && !suppress_diagnostics && !is_placeholder_anchor(&raw) {
            let candidates = {
                let global = targets.matching_symbols(fragment.unwrap(), None);
                if global.is_empty() {
                    targets
                        .symbols
                        .get(path)
                        .into_iter()
                        .flatten()
                        .collect()
                } else {
                    global
                }
            };
            report(
                checks,
                "anchor-unresolved",
                &evidence,
                &candidates,
                "Correct the path or symbol, or ensure the target is included in the code/document index.",
            );
        }
    } else if matches.len() > 1 && fragment.is_some() {
        if !suppress_diagnostics && !is_placeholder_anchor(&raw) {
            report(
                checks,
                "anchor-ambiguous",
                &evidence,
                &matches,
                "Use a path#fully::qualified::symbol anchor to select one candidate.",
            );
        }
    } else {
        for target in matches {
            edges.push(Edge {
                src: src.to_string(),
                dst: target.id.clone(),
                evidence: evidence.to_string(),
            });
        }
    }
}

fn suppress_diagnostics_for_doc(path: &str) -> bool {
    let components = Path::new(path)
        .components()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(name.to_string_lossy()),
            _ => None,
        })
        .collect::<Vec<_>>();
    components.iter().any(|name| *name == "examples" || *name == "templates")
        || components
            .windows(2)
            .any(|parts| parts[0] == "tests" && parts[1] == "fixtures")
}

fn is_placeholder_anchor(anchor: &str) -> bool {
    anchor.contains('*')
        || (anchor.contains('<') && anchor.contains('>'))
        || (anchor.contains('{') && anchor.contains('}'))
}

fn report(
    checks: &mut Vec<IndexCheck>,
    rule: &str,
    evidence: &Value,
    candidates: &[&Target],
    hint: &str,
) {
    let mut evidence = evidence.clone();
    evidence["candidates"] = json!(candidates);
    let labels = candidates
        .iter()
        .map(|target| target.label())
        .collect::<Vec<_>>();
    let mut check = IndexCheck {
        rule_id: rule.to_string(),
        severity: "info".to_string(),
        subject_path: evidence["doc"].as_str().unwrap().to_string(),
        message: format!(
            "{}:{}: `{}`: {} Candidates: {}.",
            evidence["doc"].as_str().unwrap(),
            evidence["line"],
            evidence["anchor"].as_str().unwrap(),
            match rule {
                "anchor-ambiguous" => "ambiguous anchor",
                "anchor-line-number" => "line-number anchor",
                _ => "unresolved anchor",
            },
            if labels.is_empty() {
                "none".to_string()
            } else {
                labels.join(", ")
            }
        ),
        hint: Some(hint.to_string()),
        fixable: false,
        evidence_json: evidence.to_string(),
    };
    complete_check(&mut check);
    checks.push(check);
}

fn path_candidates(doc: &str, raw: &str) -> Vec<String> {
    let relative = Path::new(doc)
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .join(raw);
    let candidates = if raw.starts_with("./") || raw.starts_with("../") {
        vec![relative]
    } else {
        vec![relative, raw.into()]
    };
    candidates
        .into_iter()
        .filter_map(|path| {
            let mut parts = Vec::new();
            for part in path.components() {
                match part {
                    Component::Normal(part) => parts.push(part.to_string_lossy().to_string()),
                    Component::CurDir => {}
                    Component::ParentDir => {
                        parts.pop()?;
                    }
                    _ => return None,
                }
            }
            (!parts.is_empty()).then(|| parts.join("/"))
        })
        .collect()
}

fn line_range(fragment: &str) -> Option<(usize, usize)> {
    let fragment = fragment.strip_prefix('L')?;
    let (start, end) = fragment.split_once("-L").unwrap_or((fragment, fragment));
    let start = start.parse::<usize>().ok()?;
    let end = end.parse::<usize>().ok()?;
    (start > 0 && start <= end).then_some((start, end))
}
