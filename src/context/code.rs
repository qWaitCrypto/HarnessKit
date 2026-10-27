use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::Serialize;
use serde_json::{json, Value};

use crate::code::{indexed_regions, CodeRegion};
use crate::index::{cell, read_index_artifact_from_fact_store};
use crate::knowledge::{anchor_states, AnchorState, LockEntry};
use crate::lifecycle::{doc_lifecycle, is_active_plan, successor_map};
use crate::schema::Schema;
use crate::sqlite::SqliteConnection;
use crate::weave::{cochanged_files, CochangedFile};
use crate::Result;

pub(super) const DOC_LIMIT: usize = 8;
pub(super) const REGION_LIMIT: usize = 8;

#[derive(Clone, Serialize)]
pub(crate) struct CodeLink {
    pub(crate) kind: String,
    pub(crate) confidence: String,
    pub(crate) evidence: Value,
    pub(crate) code: CodeRegion,
}

#[derive(Clone, Serialize)]
pub(crate) struct CodeDocument {
    pub(crate) path: String,
    pub(crate) title: String,
    pub(crate) heading: String,
    pub(crate) role: String,
    pub(crate) status: String,
    pub(crate) lifecycle: String,
    pub(crate) summary: String,
    #[serde(skip_serializing)]
    body: String,
    pub(crate) constrains: bool,
    pub(crate) active_plan: bool,
    pub(crate) links: Vec<CodeLink>,
    pub(crate) links_total: usize,
}

impl CodeDocument {
    pub(super) fn priority(&self) -> (bool, u8, &str, &str) {
        (
            self.lifecycle == "historical",
            if self.constrains {
                0
            } else if self.active_plan {
                1
            } else {
                2
            },
            &self.path,
            &self.heading,
        )
    }

    pub(super) fn covers(&self) -> bool {
        self.lifecycle == "current" && self.links.iter().any(|link| link.kind != "cochange")
    }
}

pub(super) struct CodeGraph {
    pub(super) regions: Vec<CodeRegion>,
    pub(super) states: Vec<AnchorState>,
    documents: BTreeMap<String, CodeDocument>,
    links: Vec<(String, CodeLink)>,
}

impl CodeGraph {
    pub(super) fn load(db: &SqliteConnection, locks: &[LockEntry]) -> Result<Self> {
        let regions = indexed_regions(db)?;
        let states = anchor_states(db, locks)?;
        let artifact = read_index_artifact_from_fact_store(db)?;
        let successors = successor_map(&artifact.docs);
        let sections = db.query("SELECT n.id, n.path, n.name, COALESCE(s.summary, d.summary, ''), COALESCE(s.body, d.body_tokens, '')
            FROM nodes n LEFT JOIN doc_sections s ON s.node_id = n.id LEFT JOIN docs d ON d.path = n.path
            WHERE n.kind IN ('doc', 'section') ORDER BY n.id;", [])?;
        let mut documents = BTreeMap::new();
        for row in &sections.rows {
            let Some(doc) = artifact.docs.iter().find(|doc| doc.path == cell(row, 1)) else {
                continue;
            };
            let next = successors.get(&doc.path).map(Vec::as_slice).unwrap_or(&[]);
            let lifecycle = doc_lifecycle(
                &doc.path,
                doc.status.as_deref().unwrap_or(""),
                &doc.summary,
                next,
            );
            documents.insert(
                cell(row, 0).to_string(),
                CodeDocument {
                    path: doc.path.clone(),
                    title: doc.title.clone(),
                    heading: if cell(row, 0).starts_with("section:") {
                        cell(row, 2).to_string()
                    } else {
                        String::new()
                    },
                    role: doc.role.clone(),
                    status: doc.status.clone().unwrap_or_default(),
                    constrains: lifecycle == "current" && doc.role == "decisions",
                    active_plan: is_active_plan(doc, next),
                    lifecycle,
                    summary: cell(row, 3).chars().take(240).collect(),
                    body: cell(row, 4).to_string(),
                    links: Vec::new(),
                    links_total: 0,
                },
            );
        }
        let nodes = regions
            .iter()
            .map(|region| (region.id.as_str(), region))
            .collect::<BTreeMap<_, _>>();
        let rows = db.query("SELECT src, dst, kind, confidence, evidence_json FROM edges WHERE kind IN ('anchor', 'mention', 'cochange') ORDER BY src, dst, kind, evidence_json;", [])?;
        let mut links = Vec::new();
        for row in &rows.rows {
            let (Some(_), Some(region)) = (documents.get(cell(row, 0)), nodes.get(cell(row, 1)))
            else {
                continue;
            };
            let evidence: Value = serde_json::from_str(cell(row, 4))?;
            if evidence["ambiguous"].as_bool() == Some(true) {
                continue;
            }
            links.push((
                cell(row, 0).to_string(),
                CodeLink {
                    kind: cell(row, 2).to_string(),
                    confidence: cell(row, 3).to_string(),
                    evidence,
                    code: (*region).clone(),
                },
            ));
        }
        // Deleted/moved symbols have no live edge. Retain their exact chapter
        // using the durable lock and original inline-code evidence.
        let spans = db.query("SELECT src, doc_path, section, text FROM inline_code_spans ORDER BY doc_path, span_index;", [])?;
        for state in states
            .iter()
            .filter(|state| matches!(state.status.as_str(), "broken" | "moved" | "ambiguous"))
        {
            for row in &spans.rows {
                if cell(row, 1) != state.doc || cell(row, 3) != state.anchor {
                    continue;
                }
                let old_path = state
                    .anchor
                    .split_once('#')
                    .map(|(path, _)| path)
                    .unwrap_or(&state.target_path);
                links.push((cell(row, 0).to_string(), CodeLink {
                    kind: "anchor".to_string(), confidence: "EXTRACTED".to_string(),
                    evidence: json!({"doc": state.doc, "anchor": state.anchor, "section": cell(row, 2), "status": state.status, "target_path": old_path}),
                    code: CodeRegion { id: format!("missing:{}", state.anchor),
                        kind: if state.target_name.is_empty() { "file" } else { "symbol" }.to_string(),
                        path: old_path.to_string(), name: state.target_name.clone(), span_start: 0, span_end: 0,
                        fingerprint: state.expected_fingerprint.clone() },
                }));
                if state.status == "moved" {
                    let (source, mut link) = links.last().unwrap().clone();
                    link.code.path = state.target_path.clone();
                    links.push((source, link));
                }
            }
        }
        Ok(Self {
            regions,
            states,
            documents,
            links,
        })
    }

    pub(super) fn documents_for(
        &self,
        regions: &[CodeRegion],
        whole_file: bool,
    ) -> Vec<CodeDocument> {
        let mut found = BTreeMap::<String, CodeDocument>::new();
        for (source, link) in &self.links {
            if !regions.iter().any(|region| {
                link_matches(&link.code, region, whole_file)
                    || (link.evidence["anchor"]
                        .as_str()
                        .map(|anchor| anchor.ends_with('/'))
                        .unwrap_or(false)
                        && link.evidence["target_path"]
                            .as_str()
                            .map(|scope| path_in_scope(&region.path, scope))
                            .unwrap_or(false))
            }) {
                continue;
            }
            let Some(doc) = self.documents.get(source) else {
                continue;
            };
            let item = found.entry(source.clone()).or_insert_with(|| doc.clone());
            item.links_total += 1;
            if item.links.len() < 3 {
                item.links.push(link.clone());
            }
            // Coverage must not depend on the first three displayed links.
            if link.kind != "cochange" && !item.links.iter().any(|link| link.kind != "cochange") {
                item.links[0] = link.clone();
            }
        }
        let mut docs = found.into_values().collect::<Vec<_>>();
        for doc in &mut docs {
            let anchors = doc
                .links
                .iter()
                .flat_map(|link| [link.code.path.as_str(), link.code.name.as_str()])
                .collect::<Vec<_>>();
            doc.summary =
                crate::render::preferred_excerpt(&doc.body, "", &anchors, &doc.summary, 240);
        }
        docs.sort_by(|a, b| a.priority().cmp(&b.priority()));
        docs
    }
}

fn link_matches(target: &CodeRegion, region: &CodeRegion, whole_file: bool) -> bool {
    (target.path == region.path
        || (target.path.contains("::")
            && target.path == target.name
            && symbol_matches(&region.name, &target.name)))
        && (target.kind == "file"
            || whole_file
            || (region.kind == "symbol"
                && (target.name == region.name
                    || (target.span_start > 0
                        && target.span_start <= region.span_start
                        && target.span_end >= region.span_end))))
}

pub(super) fn symbol_matches(name: &str, input: &str) -> bool {
    name == input || name.ends_with(&format!("::{input}"))
}

pub(super) fn path_in_scope(path: &str, scope: &str) -> bool {
    scope.is_empty()
        || scope == "."
        || path == scope.trim_end_matches('/')
        || path.starts_with(&format!("{}/", scope.trim_end_matches('/')))
}

pub(super) fn state_affects(state: &AnchorState, region: &CodeRegion, whole_file: bool) -> bool {
    let (old_path, old_name) = state
        .anchor
        .split_once('#')
        .unwrap_or((state.anchor.as_str(), ""));
    let path_match =
        path_in_scope(&region.path, &state.target_path) || path_in_scope(&region.path, old_path);
    let name_match = state.target_name.is_empty()
        || symbol_matches(&region.name, &state.target_name)
        || region.name.starts_with(&format!("{}::", state.target_name))
        || (!old_name.is_empty() && symbol_matches(&region.name, old_name))
        || symbol_matches(&region.name, &state.anchor);
    let global = !state.anchor.contains('/')
        && !state.anchor.contains('#')
        && symbol_matches(&region.name, &state.anchor);
    global || (path_match && (whole_file || name_match))
}

#[derive(Serialize)]
pub(crate) struct CodeContext {
    pub(crate) kind: &'static str,
    pub(crate) input: String,
    pub(crate) ambiguous: bool,
    pub(crate) targets: Vec<CodeRegion>,
    pub(crate) documents: Vec<CodeDocument>,
    pub(crate) decisions: Vec<String>,
    pub(crate) active_plans: Vec<String>,
    pub(crate) cochanged_files: Vec<CochangedFile>,
    pub(crate) anchor_status: Vec<AnchorState>,
    pub(crate) anchor_counts: BTreeMap<String, usize>,
    pub(crate) message: Option<&'static str>,
    pub(crate) truncated: BTreeMap<String, usize>,
}

pub(crate) fn build_code_context(
    db: &SqliteConnection,
    root: &Path,
    schema: &Schema,
    input: &str,
    locks: &[LockEntry],
) -> Result<CodeContext> {
    let graph = CodeGraph::load(db, locks)?;
    let input = input.trim().trim_start_matches("./").replace('\\', "/");
    let (raw_path, fragment) = input.split_once('#').unwrap_or((&input, ""));
    let input = if Path::new(raw_path).is_absolute() {
        let root = root.canonicalize()?;
        match Path::new(raw_path).strip_prefix(&root) {
            Ok(relative) => format!(
                "{}{}",
                crate::fs_utils::normalize_path(relative),
                if fragment.is_empty() {
                    String::new()
                } else {
                    format!("#{fragment}")
                }
            ),
            Err(_) => input,
        }
    } else {
        input
    };
    let (path, name) = input.split_once('#').unwrap_or((&input, ""));
    let is_path = !name.is_empty()
        || graph
            .regions
            .iter()
            .any(|region| path_in_scope(&region.path, path))
        || root.join(path).exists()
        || path.contains('/');
    let whole_file = is_path && name.is_empty();
    let mut targets = graph
        .regions
        .iter()
        .filter(|region| {
            if is_path {
                if name.is_empty() {
                    region.kind == "file" && path_in_scope(&region.path, path)
                } else {
                    region.path == path
                        && region.kind == "symbol"
                        && symbol_matches(&region.name, name)
                }
            } else {
                region.kind == "symbol" && symbol_matches(&region.name, &input)
            }
        })
        .cloned()
        .collect::<Vec<_>>();
    let ambiguous = !whole_file && targets.len() > 1;
    let mut states = graph
        .states
        .iter()
        .filter(|state| {
            targets
                .iter()
                .any(|region| state_affects(state, region, whole_file))
                || (is_path
                    && name.is_empty()
                    && (path_in_scope(&state.target_path, path)
                        || (state.anchor.ends_with('/')
                            && path_in_scope(path, &state.target_path))
                        || path_in_scope(state.anchor.split('#').next().unwrap_or(""), path)))
                || (!whole_file
                    && (state.anchor == input
                        || (!is_path && symbol_matches(&state.target_name, &input))))
        })
        .cloned()
        .collect::<Vec<_>>();
    // A deleted file/symbol still has actionable locked documentation.
    if targets.is_empty() {
        for state in &states {
            let (old_path, old_name) = state
                .anchor
                .split_once('#')
                .unwrap_or((&state.target_path, &state.target_name));
            targets.push(CodeRegion {
                id: format!("missing:{}", state.anchor),
                kind: if whole_file { "file" } else { "symbol" }.to_string(),
                path: if whole_file && state.anchor.ends_with('/') {
                    path.to_string()
                } else {
                    old_path.to_string()
                },
                name: old_name.to_string(),
                span_start: 0,
                span_end: 0,
                fingerprint: state.expected_fingerprint.clone(),
            });
        }
    }
    let mut documents = graph.documents_for(&targets, whole_file);
    let decisions = document_paths(&documents, |doc| doc.constrains);
    let active_plans = document_paths(&documents, |doc| doc.active_plan);
    let cochanged_files = cochanged_files(
        db,
        root,
        schema,
        &targets
            .iter()
            .map(|region| region.path.clone())
            .collect::<BTreeSet<_>>(),
    )?;
    let message = documents.is_empty().then_some("No related documents");
    let mut anchor_counts = BTreeMap::new();
    for state in &states {
        *anchor_counts.entry(state.status.clone()).or_default() += 1;
    }
    let mut truncated = BTreeMap::new();
    cap(&mut documents, DOC_LIMIT, "documents", &mut truncated);
    // A full document page is intentionally treated as potentially bounded so
    // clients can offer a continuation even when the graph supplied exactly
    // the display limit.
    if documents.len() == DOC_LIMIT {
        truncated.entry("documents".to_string()).or_insert(1);
    }
    cap(&mut targets, REGION_LIMIT, "targets", &mut truncated);
    states.sort_by_key(|state| {
        (
            state.status == "current",
            state.doc.clone(),
            state.anchor.clone(),
        )
    });
    cap(&mut states, REGION_LIMIT, "anchor_status", &mut truncated);
    Ok(CodeContext {
        kind: "code",
        input,
        ambiguous,
        targets,
        documents,
        decisions,
        active_plans,
        cochanged_files,
        anchor_status: states,
        anchor_counts,
        message,
        truncated,
    })
}

pub(super) fn document_paths(
    docs: &[CodeDocument],
    predicate: impl Fn(&CodeDocument) -> bool,
) -> Vec<String> {
    docs.iter()
        .filter(|doc| predicate(doc))
        .map(|doc| doc.path.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .take(DOC_LIMIT)
        .collect()
}

pub(super) fn cap<T>(
    items: &mut Vec<T>,
    limit: usize,
    key: &str,
    truncated: &mut BTreeMap<String, usize>,
) {
    if items.len() > limit {
        truncated.insert(key.to_string(), items.len() - limit);
        items.truncate(limit);
    }
}
