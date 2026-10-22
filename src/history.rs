use serde::{Deserialize, Serialize};

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::discover::collect_managed_doc_paths;
use crate::fs_utils::{file_modified_parts, now_unix};
use crate::git::{host_git_info, HostGitMode};
use crate::render::render_history_snapshot_json;
use crate::schema::Schema;
use crate::Result;

static SNAPSHOT_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct HistoryFileRecord {
    pub(crate) path: String,
    pub(crate) hash: String,
    pub(crate) size: u64,
    pub(crate) mtime_unix: u64,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct HistorySnapshot {
    pub(crate) id: String,
    pub(crate) created_at_unix: u64,
    pub(crate) message: String,
    pub(crate) docs_root: String,
    pub(crate) schema_hash: String,
    pub(crate) host_git_head: Option<String>,
    pub(crate) host_git_branch: Option<String>,
    pub(crate) host_git_dirty: bool,
    pub(crate) files: Vec<HistoryFileRecord>,
}

#[derive(Default)]
pub(crate) struct HistoryStatus {
    pub(crate) latest_snapshot: Option<String>,
    pub(crate) tracked_files_count: usize,
    pub(crate) added: Vec<String>,
    pub(crate) changed: Vec<String>,
    pub(crate) removed: Vec<String>,
    pub(crate) schema_changed: bool,
}

fn harnesskit_dir(target_dir: &Path) -> PathBuf {
    target_dir.join(".harnesskit")
}

pub(crate) fn history_dir(target_dir: &Path) -> PathBuf {
    harnesskit_dir(target_dir).join("history")
}

fn history_objects_dir(target_dir: &Path) -> PathBuf {
    history_dir(target_dir).join("objects")
}

fn history_snapshots_dir(target_dir: &Path) -> PathBuf {
    history_dir(target_dir).join("snapshots")
}

fn history_refs_dir(target_dir: &Path) -> PathBuf {
    history_dir(target_dir).join("refs")
}

fn ensure_history_dirs(target_dir: &Path) -> Result<()> {
    fs::create_dir_all(history_objects_dir(target_dir))?;
    fs::create_dir_all(history_snapshots_dir(target_dir))?;
    fs::create_dir_all(history_refs_dir(target_dir))?;
    Ok(())
}

pub(crate) fn create_history_snapshot(
    target_dir: &Path,
    schema: &Schema,
    message: &str,
) -> Result<HistorySnapshot> {
    ensure_history_dirs(target_dir)?;
    let created_at_unix = now_unix();
    let id = unique_snapshot_id(created_at_unix);
    let schema_hash = schema_file_hash(target_dir);
    let files = collect_history_file_records(target_dir, schema)?;
    for file in &files {
        store_history_object(target_dir, &file.path, &file.hash)?;
    }
    let git = host_git_info(target_dir, HostGitMode::Full);
    let snapshot = HistorySnapshot {
        id,
        created_at_unix,
        message: message.to_string(),
        docs_root: schema.managed_root.clone(),
        schema_hash,
        files,
        host_git_head: git.head,
        host_git_branch: git.branch,
        host_git_dirty: git.dirty,
    };
    write_history_snapshot(target_dir, &snapshot)?;
    fs::write(history_refs_dir(target_dir).join("latest"), &snapshot.id)?;
    Ok(snapshot)
}

fn unique_snapshot_id(created_at_unix: u64) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.subsec_nanos())
        .unwrap_or(0);
    let counter = SNAPSHOT_COUNTER.fetch_add(1, Ordering::Relaxed) % 1000;
    format!("{}-{:09}-{:03}", created_at_unix, nanos, counter)
}

fn schema_file_hash(target_dir: &Path) -> String {
    fs::read(target_dir.join(".harnesskit").join("schema.yaml"))
        .map(|bytes| history_content_hash(&bytes))
        .unwrap_or_default()
}

fn collect_history_file_records(
    target_dir: &Path,
    schema: &Schema,
) -> Result<Vec<HistoryFileRecord>> {
    let mut paths = collect_managed_doc_paths(target_dir, schema)?;
    paths.push(".harnesskit/schema.yaml".to_string());
    paths.sort();
    paths.dedup();

    let mut records = Vec::new();
    for path in paths {
        let abs = target_dir.join(&path);
        if !abs.is_file() {
            continue;
        }
        let bytes = fs::read(&abs)?;
        let metadata = fs::metadata(&abs)?;
        let (mtime_unix, _) = file_modified_parts(&metadata)?;
        records.push(HistoryFileRecord {
            path,
            hash: history_content_hash(&bytes),
            size: metadata.len(),
            mtime_unix,
        });
    }
    Ok(records)
}

fn store_history_object(target_dir: &Path, rel_path: &str, hash: &str) -> Result<()> {
    let object_path = history_object_path(target_dir, hash);
    if object_path.exists() {
        return Ok(());
    }
    if let Some(parent) = object_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let content = fs::read(target_dir.join(rel_path))?;
    fs::write(&object_path, content).map_err(|err| {
        format!(
            "failed to store history object for {} at {}: {}",
            rel_path,
            object_path.display(),
            err
        )
    })?;
    Ok(())
}

pub(crate) fn history_object_path(target_dir: &Path, hash: &str) -> PathBuf {
    let prefix_len = hash.len().min(2);
    let (prefix, suffix) = hash.split_at(prefix_len);
    history_objects_dir(target_dir).join(prefix).join(suffix)
}

fn write_history_snapshot(target_dir: &Path, snapshot: &HistorySnapshot) -> Result<()> {
    let path = history_snapshots_dir(target_dir).join(format!("{}.json", snapshot.id));
    fs::write(path, render_history_snapshot_json(snapshot))?;
    Ok(())
}

pub(crate) fn history_content_hash(bytes: &[u8]) -> String {
    let mut left: u64 = 0xcbf29ce484222325;
    let mut right: u64 = 0x84222325cbf29ce4;
    for byte in bytes {
        left ^= *byte as u64;
        left = left.wrapping_mul(0x100000001b3);
        right ^= (*byte as u64).wrapping_add(0x9e3779b97f4a7c15);
        right = right.rotate_left(5).wrapping_mul(0x100000001b3);
    }
    format!("{:016x}{:016x}", left, right)
}

pub(crate) fn list_history_snapshots(target_dir: &Path) -> Result<Vec<HistorySnapshot>> {
    let dir = history_snapshots_dir(target_dir);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut snapshots = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let text = fs::read_to_string(path)?;
        snapshots.push(parse_history_snapshot_json(&text)?);
    }
    snapshots.sort_by(|a, b| {
        a.created_at_unix
            .cmp(&b.created_at_unix)
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(snapshots)
}

pub(crate) fn parse_history_snapshot_json(text: &str) -> Result<HistorySnapshot> {
    let snapshot: HistorySnapshot = serde_json::from_str(text)?;
    if snapshot.id.is_empty() {
        return Err("invalid history snapshot: missing id".into());
    }
    Ok(snapshot)
}

pub(crate) fn latest_history_snapshot_id(target_dir: &Path) -> Option<String> {
    fs::read_to_string(history_refs_dir(target_dir).join("latest"))
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub(crate) fn resolve_snapshot_arg(target_dir: &Path, value: &str) -> Result<HistorySnapshot> {
    let id = if value == "latest" {
        latest_history_snapshot_id(target_dir).ok_or("no latest history snapshot found")?
    } else {
        value.to_string()
    };
    let path = history_snapshots_dir(target_dir).join(format!("{}.json", id));
    let text = fs::read_to_string(path)?;
    parse_history_snapshot_json(&text)
}

fn latest_history_snapshot(target_dir: &Path) -> Result<Option<HistorySnapshot>> {
    match latest_history_snapshot_id(target_dir) {
        Some(id) => Ok(Some(resolve_snapshot_arg(target_dir, &id)?)),
        None => Ok(None),
    }
}

pub(crate) fn compute_history_status(target_dir: &Path, schema: &Schema) -> Result<HistoryStatus> {
    let current = collect_history_file_records(target_dir, schema)?;
    let current_map = current
        .iter()
        .map(|file| (file.path.clone(), file.hash.clone()))
        .collect::<BTreeMap<_, _>>();
    let latest = latest_history_snapshot(target_dir)?;
    let Some(snapshot) = latest else {
        return Ok(HistoryStatus {
            latest_snapshot: None,
            tracked_files_count: current.len(),
            added: current.iter().map(|file| file.path.clone()).collect(),
            changed: Vec::new(),
            removed: Vec::new(),
            schema_changed: !current.is_empty(),
        });
    };
    let snapshot_map = snapshot
        .files
        .iter()
        .map(|file| (file.path.clone(), file.hash.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut added = Vec::new();
    let mut changed = Vec::new();
    let mut removed = Vec::new();
    for (path, hash) in &current_map {
        match snapshot_map.get(path) {
            None => added.push(path.clone()),
            Some(previous) if previous != hash => changed.push(path.clone()),
            Some(_) => {}
        }
    }
    for path in snapshot_map.keys() {
        if !current_map.contains_key(path) {
            removed.push(path.clone());
        }
    }
    added.sort();
    changed.sort();
    removed.sort();
    let schema_changed = current_map
        .get(".harnesskit/schema.yaml")
        .map(|hash| hash != &snapshot.schema_hash)
        .unwrap_or(!snapshot.schema_hash.is_empty());
    Ok(HistoryStatus {
        latest_snapshot: Some(snapshot.id),
        tracked_files_count: current.len(),
        added,
        changed,
        removed,
        schema_changed,
    })
}

pub(crate) fn is_history_dirty(status: &HistoryStatus) -> bool {
    has_history_doc_changes(status) || status.schema_changed
}

pub(crate) fn has_history_doc_changes(status: &HistoryStatus) -> bool {
    status
        .added
        .iter()
        .any(|path| path != ".harnesskit/schema.yaml")
        || status
            .changed
            .iter()
            .any(|path| path != ".harnesskit/schema.yaml")
        || status
            .removed
            .iter()
            .any(|path| path != ".harnesskit/schema.yaml")
}

#[derive(Default)]
pub(crate) struct HistoryDiff {
    pub(crate) added: Vec<String>,
    pub(crate) changed: Vec<String>,
    pub(crate) removed: Vec<String>,
}

pub(crate) fn diff_snapshots(a: &HistorySnapshot, b: &HistorySnapshot) -> HistoryDiff {
    let a_map = a
        .files
        .iter()
        .map(|file| (file.path.clone(), file.hash.clone()))
        .collect::<BTreeMap<_, _>>();
    let b_map = b
        .files
        .iter()
        .map(|file| (file.path.clone(), file.hash.clone()))
        .collect::<BTreeMap<_, _>>();
    diff_hash_maps(&a_map, &b_map)
}

pub(crate) fn diff_snapshot_to_worktree(
    target_dir: &Path,
    schema: &Schema,
    snapshot: &HistorySnapshot,
) -> Result<HistoryDiff> {
    let snapshot_map = snapshot
        .files
        .iter()
        .map(|file| (file.path.clone(), file.hash.clone()))
        .collect::<BTreeMap<_, _>>();
    let current_map = collect_history_file_records(target_dir, schema)?
        .iter()
        .map(|file| (file.path.clone(), file.hash.clone()))
        .collect::<BTreeMap<_, _>>();
    Ok(diff_hash_maps(&snapshot_map, &current_map))
}

fn diff_hash_maps(a: &BTreeMap<String, String>, b: &BTreeMap<String, String>) -> HistoryDiff {
    let mut diff = HistoryDiff::default();
    for (path, hash) in b {
        match a.get(path) {
            None => diff.added.push(path.clone()),
            Some(previous) if previous != hash => diff.changed.push(path.clone()),
            Some(_) => {}
        }
    }
    for path in a.keys() {
        if !b.contains_key(path) {
            diff.removed.push(path.clone());
        }
    }
    diff
}

#[derive(Default)]
pub(crate) struct RestorePlan {
    pub(crate) restore_files: Vec<HistoryFileRecord>,
    pub(crate) remove_paths: Vec<String>,
}

pub(crate) fn restore_plan(
    target_dir: &Path,
    schema: &Schema,
    snapshot: &HistorySnapshot,
) -> Result<RestorePlan> {
    let current = collect_history_file_records(target_dir, schema)?;
    let snapshot_paths = snapshot
        .files
        .iter()
        .map(|file| file.path.clone())
        .collect::<BTreeSet<_>>();
    let current_paths = current
        .iter()
        .map(|file| file.path.clone())
        .collect::<BTreeSet<_>>();
    let remove_paths = current_paths
        .difference(&snapshot_paths)
        .cloned()
        .collect::<Vec<_>>();
    Ok(RestorePlan {
        restore_files: snapshot.files.clone(),
        remove_paths,
    })
}

pub(crate) fn apply_restore_plan(target_dir: &Path, plan: &RestorePlan) -> Result<()> {
    for file in &plan.restore_files {
        let object_path = history_object_path(target_dir, &file.hash);
        if !object_path.is_file() {
            return Err(format!(
                "missing history object for {}: {}",
                file.path,
                object_path.display()
            )
            .into());
        }
    }

    for file in &plan.restore_files {
        let object_path = history_object_path(target_dir, &file.hash);
        let target_path = target_dir.join(&file.path);
        if let Some(parent) = target_path.parent() {
            fs::create_dir_all(parent)?;
        }
        atomic_copy_file(&object_path, &target_path)?;
    }
    for path in &plan.remove_paths {
        let target_path = target_dir.join(path);
        if target_path.exists() {
            fs::remove_file(target_path)?;
        }
    }
    Ok(())
}

fn atomic_copy_file(src: &Path, dst: &Path) -> Result<()> {
    let content = fs::read(src)
        .map_err(|err| format!("failed to read restore object {}: {}", src.display(), err))?;
    let file_name = dst
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("restore-target");
    let tmp_path = dst.with_file_name(format!(
        ".{}.harnesskit-restore-tmp-{}",
        file_name,
        std::process::id()
    ));
    if let Err(err) = fs::write(&tmp_path, content) {
        let _ = fs::remove_file(&tmp_path);
        return Err(format!(
            "failed to write restore temp {}: {}",
            tmp_path.display(),
            err
        )
        .into());
    }
    if let Err(err) = fs::rename(&tmp_path, dst) {
        let _ = fs::remove_file(&tmp_path);
        return Err(format!(
            "failed to replace {} from restore temp: {}",
            dst.display(),
            err
        )
        .into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frozen_alpha6_snapshot_roundtrips_byte_for_byte() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/compat/alpha6-history/snapshots");
        for entry in fs::read_dir(dir).unwrap() {
            let text = fs::read_to_string(entry.unwrap().path()).unwrap();
            let snapshot = parse_history_snapshot_json(&text).unwrap();
            assert_eq!(render_history_snapshot_json(&snapshot), text);
            assert_eq!(
                parse_history_snapshot_json(&render_history_snapshot_json(&snapshot)).unwrap(),
                snapshot
            );
        }
    }

    #[test]
    fn snapshot_roundtrip_preserves_unicode_controls_optional_fields_and_u64() {
        let raw = r#"{"files":[],"host_git_dirty":false,"unknown_future_field":{"nested":[1,2]},"schema_hash":"","docs_root":"资料","message":"emoji \uD83D\uDE00 \u0000 \u001f \"quoted\" \\ \b\f\n\r\t","created_at_unix":18446744073709551615,"id":"snapshot"}"#;
        let snapshot = parse_history_snapshot_json(raw).unwrap();
        assert_eq!(snapshot.created_at_unix, u64::MAX);
        assert_eq!(snapshot.host_git_head, None);
        assert_eq!(snapshot.host_git_branch, None);
        assert!(snapshot.message.contains('😀'));
        assert_eq!(
            parse_history_snapshot_json(&render_history_snapshot_json(&snapshot)).unwrap(),
            snapshot
        );
        assert_eq!(
            parse_history_snapshot_json(&serde_json::to_string(&snapshot).unwrap()).unwrap(),
            snapshot
        );
    }
}
