use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use serde_json::Value;

use super::{
    bm25_hits, doc_lifecycle, query_tokens, search_term_hits, search_term_keys, superseded_by,
    QueryCandidate, QueryMatch, SearchIndex, CODE_INDEX, NATIVE_CODE_INDEX, SECTION_INDEX,
};
use crate::index::cell;
use crate::sqlite::SqliteConnection;
use crate::Result;

pub(crate) const DEFAULT_BUNDLE_LIMIT: usize = 5;
const RRF_K: f64 = 60.0;
const PROPAGATION: f64 = 0.35;
const TEST_CODE_WEIGHT: f64 = 0.3;
const RELEVANCE_FLOOR: f64 = 0.50;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum QuerySide {
    Mixed,
    Docs,
    Code,
}

impl QuerySide {
    fn accepts(self, kind: &str) -> bool {
        self == Self::Mixed || (self == Self::Docs) == matches!(kind, "doc" | "section")
    }
}

#[derive(Clone, Serialize, serde::Deserialize)]
pub(crate) struct BundleNode {
    pub(crate) id: String,
    pub(crate) kind: String,
    pub(crate) path: String,
    pub(crate) name: String,
    pub(crate) title: String,
    pub(crate) heading: String,
    pub(crate) role: String,
    pub(crate) status: String,
    pub(crate) lifecycle: String,
    pub(crate) span_start: usize,
    pub(crate) span_end: usize,
    pub(crate) lang: String,
    pub(crate) signature: String,
    pub(crate) summary: String,
    #[serde(default, skip_serializing)]
    is_test: bool,
    #[serde(rename = "match", skip_serializing_if = "Option::is_none")]
    pub(crate) match_info: Option<QueryMatch>,
}

impl BundleNode {
    fn is_code(&self) -> bool {
        matches!(self.kind.as_str(), "file" | "symbol")
    }

    fn retrieval_weight(&self) -> f64 {
        let lifecycle = if self.lifecycle == "historical" {
            0.25
        } else {
            1.0
        };
        lifecycle
            * if self.is_test_code() {
                TEST_CODE_WEIGHT
            } else {
                1.0
            }
    }

    fn related_priority(&self) -> u8 {
        if !self.is_code() {
            2
        } else {
            u8::from(self.is_test_code())
        }
    }

    fn is_test_code(&self) -> bool {
        self.is_test
    }
}

fn is_test_path(path: &str) -> bool {
    let path = path.replace('\\', "/").to_ascii_lowercase();
    let parts = path.split('/').collect::<Vec<_>>();
    let filename = parts.last().copied().unwrap_or_default();
    parts
        .iter()
        .any(|part| matches!(*part, "test" | "tests" | "__tests__" | "testing"))
        || filename.starts_with("test_")
        || filename.split('.').any(|part| {
            matches!(part, "test" | "tests" | "spec")
                || part.ends_with("_test")
                || part.ends_with("_tests")
        })
}

#[derive(Clone, Serialize, serde::Deserialize)]
pub(crate) struct BundleEdge {
    pub(crate) src: String,
    pub(crate) dst: String,
    pub(crate) kind: String,
    pub(crate) confidence: String,
    pub(crate) weight: f64,
    pub(crate) evidence: Value,
    /// File-level evidence also applies to contained symbols. Keep this step
    /// explicit rather than presenting it as a directly extracted symbol edge.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) via_file: Option<String>,
}

#[derive(Clone, Serialize, serde::Deserialize)]
pub(crate) struct Freshness {
    pub(crate) status: String,
    pub(crate) modified_at: Option<u64>,
    pub(crate) issues: Vec<Value>,
}

#[derive(Clone, Serialize, serde::Deserialize)]
pub(crate) struct RelatedLink {
    pub(crate) node: BundleNode,
    pub(crate) edge: BundleEdge,
    pub(crate) freshness: Freshness,
}

#[derive(Clone, Serialize, serde::Deserialize)]
pub(crate) struct RelatedNode {
    #[serde(flatten)]
    pub(crate) link: RelatedLink,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) additional_symbols: Vec<RelatedLink>,
}

#[derive(Clone, Serialize, serde::Deserialize)]
pub(crate) struct QueryBundle {
    pub(crate) primary: BundleNode,
    pub(crate) related: Vec<RelatedNode>,
    pub(crate) score: String,
    pub(crate) freshness: Freshness,
}

struct Graph {
    nodes: BTreeMap<String, BundleNode>,
    edges: Vec<BundleEdge>,
    adjacency: BTreeMap<String, Vec<(String, usize)>>,
    freshness: BTreeMap<String, Freshness>,
}

impl Graph {
    fn load(db: &SqliteConnection) -> Result<Self> {
        Self::load_selected(db, None)
    }

    fn load_selected(db: &SqliteConnection, selected: Option<&str>) -> Result<Self> {
        let rows = if let Some(id) = selected {
            db.query(
                "SELECT id, kind, path, name, is_test FROM nodes WHERE id = ?1",
                [id],
            )?
        } else {
            db.query(
                "SELECT id, kind, path, name, is_test FROM nodes ORDER BY id",
                [],
            )?
        };
        let mut lifecycles = BTreeMap::new();
        for row in if selected.is_none() {
            db.query("SELECT path, status, summary FROM docs ORDER BY path", [])?
                .rows
        } else {
            Vec::new()
        } {
            let path = cell(&row, 0);
            lifecycles.insert(
                path.to_string(),
                doc_lifecycle(
                    path,
                    cell(&row, 1),
                    cell(&row, 2),
                    &superseded_by(db, path)?,
                ),
            );
        }
        let checks = db.query(
            "SELECT rule_id, subject_path, message, evidence_json FROM checks
             WHERE rule_id IN ('code-drift', 'anchor-moved', 'anchor-broken')
             ORDER BY subject_path, rule_id, evidence_json;",
            [],
        )?;
        let mut nodes = BTreeMap::new();
        let mut freshness = BTreeMap::new();
        for row in &rows.rows {
            let kind = cell(row, 1);
            let path = cell(row, 2);
            let doc = matches!(kind, "doc" | "section");
            // Ranking and propagation only need identity and lifecycle. Read
            // signatures and prose for the selected bundles after ranking.
            let node = BundleNode {
                id: cell(row, 0).to_string(),
                kind: kind.to_string(),
                path: path.to_string(),
                name: cell(row, 3).to_string(),
                title: String::new(),
                heading: String::new(),
                role: String::new(),
                status: String::new(),
                lifecycle: lifecycles
                    .get(path)
                    .cloned()
                    .unwrap_or_else(|| "current".to_string()),
                span_start: 0,
                span_end: 0,
                lang: String::new(),
                signature: String::new(),
                summary: String::new(),
                match_info: None,
                is_test: !doc && (cell(row, 4) == "1" || is_test_path(path)),
            };
            let mut issues = Vec::new();
            for check in &checks.rows {
                let evidence: Value = serde_json::from_str(cell(check, 3))?;
                let anchor = evidence["anchor"].as_str().unwrap_or_default();
                let (anchor_path, symbol) = anchor.split_once('#').unwrap_or((anchor, ""));
                let target = evidence["target_path"].as_str().unwrap_or(anchor_path);
                let affected = if doc {
                    cell(check, 1) == path
                } else {
                    (path == target
                        || (anchor_path.ends_with('/') && path.starts_with(anchor_path)))
                        && (kind == "file"
                            || symbol.is_empty()
                            || node.name == symbol
                            || node.name.ends_with(&format!("::{symbol}")))
                };
                if affected {
                    issues.push(serde_json::json!({"kind": cell(check, 0), "doc": cell(check, 1), "message": cell(check, 2), "evidence": evidence}));
                }
            }
            freshness.insert(
                node.id.clone(),
                Freshness {
                    status: if !issues.is_empty() {
                        "drifted"
                    } else {
                        &node.lifecycle
                    }
                    .to_string(),
                    modified_at: None,
                    issues,
                },
            );
            nodes.insert(node.id.clone(), node);
        }
        let rows = if selected.is_none() {
            db.query("SELECT src, dst, kind, confidence, weight, evidence_json FROM edges ORDER BY src, dst, kind, evidence_json;", [])?
        } else {
            crate::sqlite::QueryResult::default()
        };
        let mut edges = Vec::new();
        for row in &rows.rows {
            let (Some(source), Some(target)) = (nodes.get(cell(row, 0)), nodes.get(cell(row, 1)))
            else {
                continue;
            };
            let edge = BundleEdge {
                src: source.id.clone(),
                dst: target.id.clone(),
                kind: cell(row, 2).to_string(),
                confidence: cell(row, 3).to_string(),
                weight: cell(row, 4).parse()?,
                evidence: serde_json::from_str(cell(row, 5))?,
                via_file: None,
            };
            if target.kind == "file" {
                for symbol in nodes
                    .values()
                    .filter(|node| node.kind == "symbol" && node.path == target.path)
                {
                    let mut contained = edge.clone();
                    contained.dst = symbol.id.clone();
                    contained.via_file = Some(target.id.clone());
                    edges.push(contained);
                }
            }
            edges.push(edge);
        }
        let mut adjacency = BTreeMap::<String, Vec<(String, usize)>>::new();
        for (index, edge) in edges.iter().enumerate() {
            if edge.src != edge.dst {
                adjacency
                    .entry(edge.src.clone())
                    .or_default()
                    .push((edge.dst.clone(), index));
                adjacency
                    .entry(edge.dst.clone())
                    .or_default()
                    .push((edge.src.clone(), index));
            }
        }
        Ok(Self {
            nodes,
            edges,
            adjacency,
            freshness,
        })
    }

    fn bundle_node(&self, db: &SqliteConnection, id: &str, query: &str) -> Result<BundleNode> {
        let mut node = self.nodes[id].clone();
        let rows = db.query(
            "SELECT n.span_start, n.span_end, n.lang, n.signature, n.doc_comment,
                    COALESCE(d.title, ''), COALESCE(d.role, n.kind), COALESCE(d.status, ''),
                    COALESCE(s.heading, ''), COALESCE(s.summary, d.summary, ''),
                    COALESCE(s.body, d.body_tokens, '')
             FROM nodes n LEFT JOIN docs d ON d.path = n.path
             LEFT JOIN doc_sections s ON s.node_id = n.id WHERE n.id = ?1",
            [id],
        )?;
        if let Some(row) = rows.rows.first() {
            node.span_start = cell(row, 0).parse()?;
            node.span_end = cell(row, 1).parse()?;
            node.lang = cell(row, 2).to_string();
            node.signature = cell(row, 3).to_string();
            node.summary = if node.is_code() {
                cell(row, 4)
            } else {
                cell(row, 9)
            }
            .chars()
            .take(440)
            .collect();
            node.title = if node.kind == "file" {
                &node.path
            } else if node.is_code() {
                &node.name
            } else {
                cell(row, 5)
            }
            .to_string();
            node.role = cell(row, 6).to_string();
            node.status = cell(row, 7).to_string();
            node.heading = cell(row, 8).to_string();
            if !node.is_code() {
                node.summary = crate::render::preferred_excerpt(cell(row, 10), query, &[], &node.summary, 440);
            }
        }
        Ok(node)
    }

    fn freshness_for(&self, db: &SqliteConnection, id: &str) -> Result<Freshness> {
        let mut value = self.freshness[id].clone();
        value.modified_at = db
            .query(
                "SELECT mtime_unix FROM file_states WHERE path = ?1",
                [&self.nodes[id].path],
            )?
            .rows
            .first()
            .and_then(|row| cell(row, 0).parse().ok());
        Ok(value)
    }

    fn neighbors(
        &self,
        id: &str,
        side: QuerySide,
        include_ambiguous: bool,
    ) -> BTreeMap<String, usize> {
        let mut neighbors = BTreeMap::<String, usize>::new();
        for (neighbor, index) in self.adjacency.get(id).into_iter().flatten() {
            let edge = &self.edges[*index];
            if !side.accepts(&self.nodes[neighbor].kind)
                || (!include_ambiguous && edge.evidence["ambiguous"].as_bool() == Some(true))
            {
                continue;
            }
            let replace = neighbors
                .get(neighbor)
                .map(|previous| {
                    let previous = &self.edges[*previous];
                    (
                        edge.src == id,
                        edge.confidence == "EXTRACTED",
                        edge_strength(edge),
                    ) > (
                        previous.src == id,
                        previous.confidence == "EXTRACTED",
                        edge_strength(previous),
                    )
                })
                .unwrap_or(true);
            if replace {
                neighbors.insert(neighbor.clone(), *index);
            }
        }
        neighbors
    }
}

struct RecallHit {
    id: String,
    score: f64,
    matched: Vec<String>,
    weight: f64,
    native: bool,
    historical: bool,
    retrieval_weight: f64,
}

fn recall(
    db: &SqliteConnection,
    graph: &Graph,
    terms: &[String],
    index: &SearchIndex,
    limit: usize,
) -> Result<Vec<RecallHit>> {
    let mut hits = BTreeMap::<String, (Vec<String>, f64)>::new();
    let mut native = BTreeSet::new();
    for term in terms {
        for (id, score) in search_term_hits(db, index, term)? {
            let entry = hits.entry(id).or_default();
            entry.0.push(term.clone());
            entry.1 += score;
        }
        if index.fts == CODE_INDEX.fts {
            native.extend(search_term_keys(db, &NATIVE_CODE_INDEX, term)?);
        }
    }
    if terms.len() > 1 {
        for (id, score) in bm25_hits(db, index, &terms.join(" "), false)? {
            if let Some(hit) = hits.get_mut(&id) {
                hit.1 += score;
            }
        }
    }
    let strict = hits
        .values()
        .filter(|(matched, _)| matched.len() == terms.len())
        .count();
    let mut hits = hits
        .into_iter()
        .filter(|(id, (matched, _))| {
            graph.nodes.contains_key(id) && (strict < 3 || matched.len() == terms.len())
        })
        .map(|(id, (matched, score))| {
            // Explicit chapter anchors support native-strength vocabulary
            // bridges. Pure cochange vocabulary is less specific than native
            // names, signatures/comments or the matching document section.
            let extracted = graph
                .adjacency
                .get(&id)
                .into_iter()
                .flatten()
                .any(|(_, index)| graph.edges[*index].confidence == "EXTRACTED");
            let weight = if graph.nodes[&id].is_code() && !native.contains(&id) && !extracted {
                0.5
            } else {
                1.0
            };
            RecallHit {
                historical: graph.nodes[&id].lifecycle == "historical",
                retrieval_weight: graph.nodes[&id].retrieval_weight(),
                native: native.contains(&id),
                id,
                score,
                matched,
                weight,
            }
        })
        .collect::<Vec<_>>();
    hits.sort_by(|a, b| {
        b.matched
            .len()
            .cmp(&a.matched.len())
            .then_with(|| a.historical.cmp(&b.historical))
            .then_with(|| {
                (b.score * b.weight * b.retrieval_weight)
                    .total_cmp(&(a.score * a.weight * a.retrieval_weight))
            })
            .then_with(|| a.id.cmp(&b.id))
    });
    hits.truncate(limit);
    Ok(hits)
}

fn edge_strength(edge: &BundleEdge) -> f64 {
    let weight = edge.weight.clamp(0.0, 1.0);
    if edge.confidence == "EXTRACTED" {
        weight
    } else if edge.kind == "mention" {
        weight * 0.5
    } else {
        weight
    }
}

fn propagate(
    graph: &Graph,
    seeds: &BTreeMap<String, f64>,
    side: QuerySide,
) -> BTreeMap<String, f64> {
    let mut scores = seeds.clone();
    let mut boosts = BTreeMap::<String, f64>::new();
    let max_seed = seeds.values().copied().fold(0.0, f64::max);
    // Read exclusively from the immutable RRF seeds: newly reached nodes do
    // not relay scores. Ambiguous mentions can boost lexical hits, but cannot
    // introduce a candidate whose identity is still unresolved.
    for (id, score) in seeds {
        let neighbors = graph.neighbors(id, side, true);
        let normalizer = neighbors
            .values()
            .map(|index| edge_strength(&graph.edges[*index]))
            .sum::<f64>()
            .max(1.0);
        for (neighbor, index) in neighbors {
            if graph.edges[index].evidence["ambiguous"].as_bool() == Some(true)
                && !seeds.contains_key(&neighbor)
            {
                continue;
            }
            *boosts.entry(neighbor).or_default() +=
                PROPAGATION * score * edge_strength(&graph.edges[index]) / normalizer;
        }
    }
    // Directory anchors and heavily linked documents are hubs. Hundreds of
    // contained symbols must not outvote a direct lexical match in one hop.
    for (id, boost) in boosts {
        *scores.entry(id.clone()).or_default() +=
            boost.min(PROPAGATION * max_seed) * graph.nodes[&id].retrieval_weight();
    }
    scores
}

pub(crate) fn query_bundles(
    db: &SqliteConnection,
    query: &str,
    side: QuerySide,
    limit: usize,
    legacy: Option<&[QueryCandidate]>,
) -> Result<Vec<QueryBundle>> {
    // A unique, explicitly named code symbol is already a complete lexical
    // answer. Avoid constructing an unrelated graph or filling the result with
    // partial identifier matches. Mixed/document searches retain graph recall.
    if side == QuerySide::Code {
        let rows = db.query(
            "SELECT id FROM nodes WHERE kind = 'symbol' AND name = ?1 COLLATE NOCASE LIMIT 2",
            [query.trim()],
        )?;
        if rows.rows.len() == 1 {
            let id = cell(&rows.rows[0], 0);
            let graph = Graph::load_selected(db, Some(id))?;
            let mut primary = graph.bundle_node(db, id, query)?;
            primary.match_info = Some(QueryMatch {
                matched_terms: query_tokens(query),
                missing_terms: Vec::new(),
                mode: "strict".to_string(),
            });
            return Ok(vec![QueryBundle {
                primary,
                related: Vec::new(),
                score: format!("{:.6}", 1.0 / (RRF_K + 1.0)),
                freshness: graph.freshness_for(db, id)?,
            }]);
        }
    }
    let mut graph = Graph::load(db)?;
    if let Some(candidates) = legacy {
        let mut bundles = Vec::new();
        for candidate in candidates.iter().take(limit) {
            let id = format!("doc:{}", candidate.path);
            if !graph.nodes.contains_key(&id) {
                continue;
            }
            let mut primary = graph.bundle_node(db, &id, query)?;
            primary.summary = crate::render::preferred_excerpt(
                &candidate.snippet, query, &[], &candidate.summary, 440,
            );
            primary.match_info = candidate.match_info.clone();
            bundles.push(QueryBundle {
                primary,
                related: Vec::new(),
                score: candidate.score.clone(),
                freshness: graph.freshness_for(db, &id)?,
            });
        }
        return Ok(bundles);
    }
    let terms = query_tokens(query);
    let recall_limit = limit.saturating_mul(4).max(64);
    let mut rankings = Vec::new();
    if side != QuerySide::Code {
        rankings.push(recall(db, &graph, &terms, &SECTION_INDEX, recall_limit)?);
    }
    if side != QuerySide::Docs {
        rankings.push(recall(db, &graph, &terms, &CODE_INDEX, recall_limit)?);
    }
    let mut seeds = BTreeMap::new();
    let mut native_hits = BTreeSet::new();
    for ranking in rankings {
        for (rank, hit) in ranking.into_iter().enumerate() {
            if hit.native {
                native_hits.insert(hit.id.clone());
            }
            let node = graph.nodes.get_mut(&hit.id).unwrap();
            node.match_info = Some(QueryMatch {
                missing_terms: terms
                    .iter()
                    .filter(|term| !hit.matched.contains(term))
                    .cloned()
                    .collect(),
                mode: if hit.matched.len() == terms.len() {
                    "strict"
                } else {
                    "relaxed"
                }
                .to_string(),
                matched_terms: hit.matched,
            });
            *seeds.entry(hit.id).or_default() +=
                node.retrieval_weight() * hit.weight / (RRF_K + rank as f64 + 1.0);
        }
    }
    let scores = propagate(&graph, &seeds, side);
    let mut ranked = scores.iter().collect::<Vec<_>>();
    ranked.sort_by(|(a, a_score), (b, b_score)| {
        let priority = |id: &String| match graph.nodes[id].kind.as_str() {
            "section" | "doc" => 0,
            "symbol" => 1,
            _ => 2,
        };
        b_score
            .total_cmp(a_score)
            .then_with(|| priority(a).cmp(&priority(b)))
            .then_with(|| a.cmp(b))
    });
    if let Some((_, best_score)) = ranked.first() {
        let floor = *best_score * RELEVANCE_FLOOR;
        let has_strong_document = ranked.iter().any(|(id, score)| {
            **score >= floor && !graph.nodes[id.as_str()].is_code()
        });
        ranked.retain(|(id, score)| {
            if **score >= floor {
                return true;
            }
            let node = &graph.nodes[id.as_str()];
            // Keep inline tests when they document the same production file;
            // standalone test suites that merely repeat a query term are weak
            // related results and must not fill the bundle cache.
            node.is_test_code() && (side == QuerySide::Code || !has_strong_document)
        });
    }
    let mut covered = BTreeSet::new();
    let mut bundles = Vec::new();
    for (id, score) in ranked {
        if covered.contains(id) || *score <= 0.0 {
            continue;
        }
        let mut neighbors = graph
            .neighbors(id, side, false)
            .into_iter()
            .collect::<Vec<_>>();
        neighbors.sort_by(|(a, a_edge), (b, b_edge)| {
            let a_edge = &graph.edges[*a_edge];
            let b_edge = &graph.edges[*b_edge];
            let a_node = &graph.nodes[a];
            let b_node = &graph.nodes[b];
            a_node
                .related_priority()
                .cmp(&b_node.related_priority())
                // A section only sees its own edges, never all anchors from
                // the containing document. Prefer its outgoing evidence over
                // incoming references from other sections/documents.
                .then_with(|| (b_edge.src == *id).cmp(&(a_edge.src == *id)))
                .then_with(|| {
                    (b_edge.confidence == "EXTRACTED").cmp(&(a_edge.confidence == "EXTRACTED"))
                })
                .then_with(|| {
                    (b_node.kind == "symbol" && b_edge.via_file.is_none())
                        .cmp(&(a_node.kind == "symbol" && a_edge.via_file.is_none()))
                })
                .then_with(|| {
                    scores
                        .get(b)
                        .unwrap_or(&0.0)
                        .total_cmp(scores.get(a).unwrap_or(&0.0))
                })
                .then_with(|| edge_strength(b_edge).total_cmp(&edge_strength(a_edge)))
                .then_with(|| (b_node.kind == "symbol").cmp(&(a_node.kind == "symbol")))
                .then_with(|| a.cmp(b))
        });
        let mut related = Vec::<RelatedNode>::new();
        let mut code_slots = BTreeMap::<String, usize>::new();
        for (neighbor, edge) in neighbors {
            let node = &graph.nodes[&neighbor];
            if (node.is_code() && graph.nodes[id].is_code() && node.path == graph.nodes[id].path)
                // A directory/file edge proves the file's relevance. Only a
                // native hit can narrow it to a symbol; vocabulary bridged by
                // some other chapter must not masquerade as a local anchor.
                || (graph.edges[edge].via_file.is_some() && !native_hits.contains(&neighbor))
            {
                continue;
            }
            let slot = code_slots
                .get(&node.path)
                .copied()
                .filter(|_| node.is_code());
            if slot.is_none() && related.len() == 3 {
                continue;
            }
            covered.insert(neighbor.clone());
            let link = RelatedLink {
                node: graph.bundle_node(db, &neighbor, query)?,
                edge: graph.edges[edge].clone(),
                freshness: graph.freshness_for(db, &neighbor)?,
            };
            if let Some(slot) = slot {
                if node.kind == "symbol" {
                    related[slot].additional_symbols.push(link);
                }
            } else {
                if node.is_code() {
                    code_slots.insert(node.path.clone(), related.len());
                }
                related.push(RelatedNode {
                    link,
                    additional_symbols: Vec::new(),
                });
            }
        }
        covered.insert(id.clone());
        bundles.push(QueryBundle {
            primary: graph.bundle_node(db, id, query)?,
            related,
            score: format!("{score:.6}"),
            freshness: graph.freshness_for(db, id)?,
        });
        if bundles.len() == limit {
            break;
        }
    }
    Ok(bundles)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rrf_propagation_is_weighted_bounded_and_exactly_one_hop() {
        let db = SqliteConnection::open(std::path::Path::new(":memory:")).unwrap();
        // Use a real fixture-sized graph, without coupling the assertion to SQL
        // retrieval scores or accidentally letting a second hop hide a miss.
        db.exec("CREATE TABLE nodes(id TEXT, kind TEXT, path TEXT, name TEXT, span_start INT, span_end INT, lang TEXT, signature TEXT, doc_comment TEXT, is_test INT);
            CREATE TABLE docs(path TEXT, title TEXT, role TEXT, status TEXT, summary TEXT);
            CREATE TABLE doc_sections(node_id TEXT, heading TEXT, summary TEXT);
            CREATE TABLE file_states(path TEXT, mtime_unix INT);
            CREATE TABLE checks(rule_id TEXT, subject_path TEXT, message TEXT, evidence_json TEXT);
            CREATE TABLE edges(src TEXT, dst TEXT, kind TEXT, confidence TEXT, weight REAL, evidence_json TEXT);
            INSERT INTO nodes VALUES ('a', 'symbol', 'a.rs', 'a', 1, 1, 'rust', '', '', 0), ('b', 'symbol', 'b.rs', 'b', 1, 1, 'rust', '', '', 0), ('c', 'symbol', 'c.rs', 'c', 1, 1, 'rust', '', '', 0);
            INSERT INTO edges VALUES ('a', 'b', 'anchor', 'EXTRACTED', 0.5, '{}'), ('b', 'c', 'anchor', 'EXTRACTED', 1.0, '{}');").unwrap();
        let graph = Graph::load(&db).unwrap();
        let seed = 1.0 / (RRF_K + 1.0);
        let scores = propagate(
            &graph,
            &BTreeMap::from([("a".to_string(), seed)]),
            QuerySide::Mixed,
        );
        assert_eq!(scores["a"], seed);
        assert!((scores["b"] - PROPAGATION * seed * 0.5).abs() < 1e-12);
        assert!(!scores.contains_key("c"));
        db.exec("INSERT INTO edges VALUES ('a', 'c', 'mention', 'INFERRED', 0.1, '{\"ambiguous\":true}');").unwrap();
        let graph = Graph::load(&db).unwrap();
        let scores = propagate(
            &graph,
            &BTreeMap::from([("a".to_string(), seed)]),
            QuerySide::Mixed,
        );
        assert!(!scores.contains_key("c"));
        assert!(!graph
            .neighbors("a", QuerySide::Mixed, false)
            .contains_key("c"));
        let scores = propagate(
            &graph,
            &BTreeMap::from([("a".to_string(), seed), ("c".to_string(), seed)]),
            QuerySide::Mixed,
        );
        assert!(scores["c"] > seed);
    }
}
