use std::collections::BTreeSet;
use std::path::Path;

use crate::index::{cell, DocRelation};
use crate::query::{snippet_for_doc, QueryCandidate};
use crate::sqlite::SqliteConnection;
use crate::Result;

pub(crate) struct ContextPacket {
    pub(crate) anchor: QueryCandidate,
    pub(crate) incoming: Vec<DocRelation>,
    pub(crate) outgoing: Vec<DocRelation>,
    pub(crate) collection_siblings: Vec<String>,
    pub(crate) same_scope_docs: Vec<String>,
    pub(crate) recommended_reading_order: Vec<String>,
}

pub(crate) fn doc_exists(db: &SqliteConnection, doc_path: &str) -> Result<bool> {
    Ok(!db
        .query("SELECT path FROM docs WHERE path = ?1;", [doc_path])?
        .rows
        .is_empty())
}

pub(crate) fn context_packet_for_doc(
    db: &SqliteConnection,
    target_dir: &Path,
    doc_path: &str,
) -> Result<ContextPacket> {
    let doc = db.query(
        "SELECT path, title, role, COALESCE(status, ''), summary, incoming_links
         FROM docs WHERE path = ?1;",
        [doc_path],
    )?;
    let Some(row) = doc.rows.first() else {
        return Err(format!("doc not found in fact store: {}", doc_path).into());
    };
    let snippet = snippet_for_doc(target_dir, doc_path, doc_path);
    let anchor = QueryCandidate {
        path: cell(row, 0).to_string(),
        title: cell(row, 1).to_string(),
        role: cell(row, 2).to_string(),
        status: cell(row, 3).to_string(),
        score: format!("{:.2}", cell(row, 5).parse::<f32>().unwrap_or(0.0) * 0.05),
        why: "context anchor".to_string(),
        summary: cell(row, 4).to_string(),
        neighbors: Vec::new(),
        snippet_heading: snippet.0,
        snippet_location: snippet.1,
        snippet: snippet.2,
    };

    let outgoing = relation_rows(db, "src_path", doc_path)?;
    let incoming = relation_rows(db, "dst_path", doc_path)?;
    let collection_siblings = collection_siblings(db, doc_path)?;
    let same_scope_docs = same_scope_docs(db, doc_path)?;
    let recommended_reading_order = recommended_reading_order(
        &anchor,
        &incoming,
        &outgoing,
        &collection_siblings,
        &same_scope_docs,
    );

    Ok(ContextPacket {
        anchor,
        incoming,
        outgoing,
        collection_siblings,
        same_scope_docs,
        recommended_reading_order,
    })
}

fn relation_rows(db: &SqliteConnection, side: &str, doc_path: &str) -> Result<Vec<DocRelation>> {
    let sql = format!(
        "SELECT src_path, dst_path, relation_type, reason, explicit, confidence, evidence_json
         FROM relations
         WHERE {side} = ?1
         ORDER BY relation_type ASC, src_path ASC, dst_path ASC;",
        side = side
    );
    let rows = db.query(&sql, [doc_path])?;
    Ok(rows
        .rows
        .iter()
        .map(|row| DocRelation {
            src_path: cell(row, 0).to_string(),
            dst_path: cell(row, 1).to_string(),
            relation_type: cell(row, 2).to_string(),
            reason: cell(row, 3).to_string(),
            explicit: cell(row, 4) == "1",
            confidence: cell(row, 5).parse::<f32>().unwrap_or(0.0),
            evidence_json: cell(row, 6).to_string(),
        })
        .collect())
}

fn collection_siblings(db: &SqliteConnection, doc_path: &str) -> Result<Vec<String>> {
    let rows = db.query(
        "SELECT sibling.path
         FROM docs target
         JOIN docs sibling ON sibling.collection_name = target.collection_name
         WHERE target.path = ?1
           AND sibling.path != target.path
           AND sibling.path NOT LIKE '%/index.md'
         ORDER BY sibling.path ASC
         LIMIT 12;",
        [doc_path],
    )?;
    Ok(rows
        .rows
        .iter()
        .map(|row| cell(row, 0).to_string())
        .collect())
}

fn same_scope_docs(db: &SqliteConnection, doc_path: &str) -> Result<Vec<String>> {
    let rows = db.query(
        "SELECT DISTINCT scoped.doc_path
         FROM scope_paths base
         JOIN scope_paths scoped ON scoped.scope_path = base.scope_path
         WHERE base.doc_path = ?1
           AND scoped.doc_path != base.doc_path
         ORDER BY scoped.doc_path ASC
         LIMIT 12;",
        [doc_path],
    )?;
    Ok(rows
        .rows
        .iter()
        .map(|row| cell(row, 0).to_string())
        .collect())
}

fn recommended_reading_order(
    anchor: &QueryCandidate,
    incoming: &[DocRelation],
    outgoing: &[DocRelation],
    collection_siblings: &[String],
    same_scope_docs: &[String],
) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut order = Vec::new();
    push_unique_path(&mut order, &mut seen, &anchor.path);
    for relation in incoming
        .iter()
        .filter(|relation| relation.relation_type == "doc_indexes_doc")
    {
        push_unique_path(&mut order, &mut seen, &relation.src_path);
    }
    for relation in outgoing
        .iter()
        .filter(|relation| relation.dst_path.ends_with(".md"))
    {
        push_unique_path(&mut order, &mut seen, &relation.dst_path);
    }
    for path in same_scope_docs {
        push_unique_path(&mut order, &mut seen, path);
    }
    for path in collection_siblings {
        push_unique_path(&mut order, &mut seen, path);
    }
    order.truncate(12);
    order
}

fn push_unique_path(out: &mut Vec<String>, seen: &mut BTreeSet<String>, path: &str) {
    if !path.is_empty() && seen.insert(path.to_string()) {
        out.push(path.to_string());
    }
}
