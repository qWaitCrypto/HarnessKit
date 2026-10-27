//! Cheap validation of the inputs to the derived graph. A warm command still
//! discovers files and stats source paths; it can reuse parsing, edges and
//! search cards when those inputs have not changed.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;

use crate::discover::discover_docs;
use crate::fs_utils::{file_modified_parts, now_unix, simple_content_hash};
use crate::git::{ignored_paths, run_git_capture};
use crate::index::{cell, EngineContext};
use crate::sqlite::SqliteConnection;
use crate::Result;

pub(super) fn fingerprint(context: &EngineContext, db: &SqliteConnection) -> Result<String> {
    let root = &context.target_dir;
    let mut hash = blake3::Hasher::new();
    let mut stale = false;
    let documents = db.query("SELECT path, content_hash, file_size_bytes, mtime_ns FROM file_states WHERE kind = 'doc'", [])?.rows
        .into_iter().map(|row| (cell(&row, 0).to_string(), row)).collect::<BTreeMap<_, _>>();
    let coverage = discover_docs(root, &context.schema)?;
    hash.update(&serde_json::to_vec(&coverage)?);
    let code_paths = if let (Ok(manifest), Ok(status)) = (
        git_records(root, &["ls-files", "--stage", "-v", "-z"]),
        git_records(
            root,
            &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        ),
    ) {
        hash.update(manifest.as_bytes());
        hash.update(status.as_bytes());
        let tracked = manifest
            .split('\0')
            .filter_map(|record| record.split_once('\t').map(|(_, path)| path.to_string()))
            .collect::<Vec<_>>();
        hash.update(&serde_json::to_vec(&ignored_paths(root, &tracked)?)?);
        // Git validates clean tracked files against its index. Re-hash dirty
        // files so a second edit to the same path also invalidates the graph.
        let mut changed = Vec::new();
        let mut records = status.split('\0');
        while let Some(record) = records.next() {
            if let Some(path) = record.get(3..) {
                changed.push(path.to_string());
                if record[..2].contains('R') || record[..2].contains('C') {
                    if let Some(previous) = records.next() {
                        changed.push(previous.to_string());
                    }
                }
            }
        }
        changed.extend(
            manifest
                .split('\0')
                .filter(|record| record.starts_with(char::is_lowercase) || record.starts_with('S'))
                .filter_map(|record| record.split_once('\t').map(|(_, path)| path.to_string())),
        );
        for path in &changed {
            let bytes = fs::read(root.join(path)).unwrap_or_default();
            hash.update(&bytes);
            let indexed = db.query(
                "SELECT content_hash FROM file_states WHERE path = ?1 AND kind = 'code'",
                [path],
            )?;
            if let Some(row) = indexed.rows.first() {
                stale |= cell(row, 0) != blake3::hash(&bytes).to_hex().as_str();
            }
        }
        Vec::new()
    } else {
        crate::code::input_paths(root, &context.schema)?
    };
    for path in code_paths
        .into_iter()
        .chain(coverage.mappings.iter().map(|mapping| mapping.path.clone()))
    {
        hash.update(path.as_bytes());
        hash.update(&[0]);
        if let Ok(metadata) = fs::symlink_metadata(root.join(&path)) {
            if let Some(row) = documents.get(&path) {
                if cell(row, 2).parse::<u64>()? != metadata.len()
                    || cell(row, 3).parse::<u64>()? != file_modified_parts(&metadata)?.1
                {
                    let bytes = fs::read(root.join(&path)).unwrap_or_default();
                    stale |= cell(row, 1) != simple_content_hash(&String::from_utf8_lossy(&bytes));
                }
            }
            hash.update(&metadata.len().to_le_bytes());
            hash.update(&file_modified_parts(&metadata)?.1.to_le_bytes());
            hash.update(&[u8::from(metadata.file_type().is_symlink())]);
        }
    }
    let args = vec!["rev-parse", "HEAD", "--symbolic-full-name", "HEAD"];
    hash.update(run_git_capture(root, &args).unwrap_or_default().as_bytes());
    for path in [
        ".harnesskit/knowledge.lock",
        ".harnesskit/history/refs/latest",
    ] {
        hash.update(&fs::read(root.join(path)).unwrap_or_default());
    }
    for row in db
        .query(
            "SELECT DISTINCT target_path FROM external_refs ORDER BY target_path",
            [],
        )?
        .rows
    {
        // External references have always used content hashes, independently
        // of their metadata. Retain that contract, including preserved mtimes.
        let path = cell(&row, 0);
        hash.update(path.as_bytes());
        hash.update(&fs::read(root.join(path)).unwrap_or_default());
    }
    let now = now_unix();
    let stale_after = u64::from(context.schema.rules.stale_active_plan_days.unwrap_or(30)) * 86400;
    for row in db
        .query(
            "SELECT path, mtime_unix FROM docs WHERE role LIKE 'exec_plans%' ORDER BY path",
            [],
        )?
        .rows
    {
        let age = now.saturating_sub(cell(&row, 1).parse::<u64>().unwrap_or(now));
        hash.update(cell(&row, 0).as_bytes());
        hash.update(&(age / 86400).to_le_bytes());
        hash.update(&[u8::from(age > stale_after)]);
    }
    let latest =
        fs::read_to_string(root.join(".harnesskit/history/refs/latest")).unwrap_or_default();
    if let Ok(bytes) = fs::read(
        root.join(".harnesskit/history/snapshots")
            .join(format!("{}.json", latest.trim())),
    ) {
        if let Ok(snapshot) = serde_json::from_slice::<serde_json::Value>(&bytes) {
            let age = now.saturating_sub(snapshot["created_at_unix"].as_u64().unwrap_or(now));
            hash.update(&(age / 86400).to_le_bytes());
            hash.update(&[u8::from(age > 7 * 86400)]);
        }
    }
    // Preserve W4's clock-sensitive decay when the repository has known pairs.
    // Empty histories need no time-driven graph rebuild.
    if !db
        .query("SELECT 1 FROM cochange_stats LIMIT 1", [])?
        .rows
        .is_empty()
    {
        hash.update(&now.to_le_bytes());
    }
    for row in db.query("SELECT key, value FROM meta WHERE key IN ('cochange_indexed_at', 'discovery_fingerprint', 'schema_fingerprint') ORDER BY key", [])?.rows {
        hash.update(cell(&row, 0).as_bytes());
        hash.update(cell(&row, 1).as_bytes());
    }
    add_directory_state(&mut hash, &root.join(".harnesskit/history/snapshots"))?;
    // An edit can race an index refresh. Never cache the current worktree as
    // processed when its content still disagrees with the indexed facts.
    Ok(if stale {
        String::new()
    } else {
        hash.finalize().to_hex().to_string()
    })
}

fn git_records(root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git").args(args).current_dir(root).output()?;
    if !output.status.success() {
        return Err("cannot read Git index/status records".into());
    }
    // Leading spaces are status columns; whitespace inside a filename is data.
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn add_directory_state(hash: &mut blake3::Hasher, path: &Path) -> Result<()> {
    if let Ok(metadata) = fs::metadata(path) {
        hash.update(&file_modified_parts(&metadata)?.1.to_le_bytes());
    }
    Ok(())
}
