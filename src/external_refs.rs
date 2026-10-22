use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use rusqlite::params;

use crate::checks::IndexCheck;
use crate::fs_utils::normalize_path;
use crate::history::history_content_hash;
use crate::index::{cell, IndexedDoc};
use crate::schema::Schema;
use crate::sqlite::SqliteConnection;
use crate::Result;

#[derive(Clone, serde::Serialize)]
pub(crate) struct ExternalRef {
    pub(crate) source_path: String,
    pub(crate) target_path: String,
    pub(crate) content_hash: String,
    pub(crate) observed_hash: String,
}

// Inline paths are commonly repo-relative; explicit ./ and ../ are doc-relative.
pub(crate) fn resolve_mention(root: &Path, source: &str, mention: &str) -> Option<String> {
    let relative = Path::new(source)
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .join(mention);
    let candidates = if mention.starts_with("./") || mention.starts_with("../") {
        vec![relative]
    } else {
        vec![mention.into(), relative]
    };
    candidates.into_iter().find_map(|path| {
        let absolute = root.join(&path).canonicalize().ok()?;
        let root = root.canonicalize().ok()?;
        if absolute.is_file() && absolute.starts_with(&root) {
            Some(normalize_path(absolute.strip_prefix(&root).ok()?))
        } else {
            None
        }
    })
}

pub(crate) fn refresh_external_refs(
    db: &SqliteConnection,
    root: &Path,
    schema: &Schema,
    docs: &[IndexedDoc],
) -> Result<(Vec<ExternalRef>, Vec<IndexCheck>)> {
    let old = db.query(
        "SELECT source_path, target_path, source_hash, content_hash FROM external_refs;",
        [],
    )?;
    let old = old
        .rows
        .iter()
        .map(|row| {
            (
                (cell(row, 0).to_string(), cell(row, 1).to_string()),
                (cell(row, 2).to_string(), cell(row, 3).to_string()),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let managed = docs
        .iter()
        .map(|doc| doc.path.as_str())
        .collect::<BTreeSet<_>>();
    let docs_root = root.join(&schema.managed_root).canonicalize().ok();
    let repo_root = root.canonicalize()?;
    let mut refs = Vec::new();
    let mut checks = Vec::new();
    let mut hashes = BTreeMap::<String, String>::new();
    db.transaction(|db| {
        db.exec("DELETE FROM external_refs;")?;
        for doc in docs {
            let targets = doc.links.iter().cloned().chain(
                doc.path_mentions.iter().filter_map(|mention| resolve_mention(root, &doc.path, mention))
            ).collect::<BTreeSet<_>>();
            for target in targets {
                let path = root.join(&target);
                let Ok(absolute) = path.canonicalize() else { continue; };
                if !absolute.is_file() || !absolute.starts_with(&repo_root)
                    || absolute.starts_with(repo_root.join(".harnesskit"))
                    || absolute.starts_with(repo_root.join(".git"))
                    || docs_root.as_ref().map(|dir| absolute.starts_with(dir)).unwrap_or(false)
                    || managed.contains(target.as_str())
                {
                    continue;
                }
                let observed_hash = if let Some(hash) = hashes.get(&target) {
                    hash.clone()
                } else {
                    let hash = history_content_hash(&fs::read(&absolute)?);
                    hashes.insert(target.clone(), hash.clone());
                    hash
                };
                // Keep a changed reference visible across index/query/check until its
                // referring document changes, even if another command ran first.
                let content_hash = old.get(&(doc.path.clone(), target.clone()))
                    .filter(|(source_hash, _)| source_hash == &doc.content_hash)
                    .map(|(_, hash)| hash.clone()).unwrap_or_else(|| observed_hash.clone());
                db.execute("INSERT INTO external_refs(source_path, target_path, source_hash, content_hash, observed_hash) VALUES (?1, ?2, ?3, ?4, ?5);",
                    params![doc.path, target, doc.content_hash, content_hash, observed_hash])?;
                if content_hash != observed_hash {
                    checks.push(IndexCheck {
                        rule_id: "external-ref-changed".to_string(), severity: "info".to_string(),
                        subject_path: doc.path.clone(),
                        hint: None,
                        message: format!("{} changed since {} recorded it; review the evidence and update the referring document.", target, doc.path),
                        evidence_json: serde_json::json!({"source": doc.path, "target": target, "recorded_hash": content_hash, "current_hash": observed_hash}).to_string(),
                    });
                }
                refs.push(ExternalRef { source_path: doc.path.clone(), target_path: target, content_hash, observed_hash });
            }
        }
        Ok(())
    })?;
    Ok((refs, checks))
}
