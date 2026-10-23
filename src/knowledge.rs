//! The durable document-to-code anchor seals used by Weave W3.
//!
//! The SQLite graph is a derived index.  `knowledge.lock` is deliberately a
//! small, line-oriented source file so that two agents can update it without
//! producing an opaque binary merge conflict.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::process::Command;

use pulldown_cmark::{Event, Parser};
use serde::Serialize;
use serde_json::{json, Value};

use crate::checks::{complete_check, IndexCheck};
use crate::fs_utils::{file_modified_parts, now_unix, read_text_lossy};
use crate::git::{find_host_git_dir, run_git_capture};
use crate::index::cell;
use crate::sqlite::SqliteConnection;
use crate::Result;

const LOCK_HEADER: &str = "# HarnessKit knowledge.lock: confirmed document anchors and code fingerprints.";

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct LockEntry {
    doc: String,
    anchor: String,
    fingerprint: String,
}

#[derive(Clone, Debug)]
struct TargetNode {
    kind: String,
    path: String,
    name: String,
    fingerprint: String,
}

#[derive(Clone, Debug)]
struct CurrentSeal {
    fingerprint: String,
    target_path: String,
    target_name: String,
}

#[derive(Clone, Serialize)]
pub(crate) struct AnchorState {
    pub(crate) doc: String,
    pub(crate) anchor: String,
    pub(crate) status: String,
    pub(crate) target_path: String,
    pub(crate) target_name: String,
    pub(crate) expected_fingerprint: String,
    pub(crate) actual_fingerprint: Option<String>,
}

pub(crate) fn read_anchor_lock(root: &Path) -> Result<Vec<LockEntry>> {
    Ok(read_lock(&root.join(".harnesskit/knowledge.lock"))?.0)
}

/// Compare with the caller's seals without re-sealing. Review packets capture
/// these before indexing, so automatic joint-review cannot hide a prediction.
pub(crate) fn anchor_states(
    db: &SqliteConnection,
    entries: &[LockEntry],
) -> Result<Vec<AnchorState>> {
    let current = current_seals(db)?;
    let doc_anchors = current_doc_anchors(db)?;
    let mut states = Vec::new();
    let mut seen = BTreeSet::new();
    for entry in entries {
        let key = (entry.doc.clone(), entry.anchor.clone());
        if !doc_anchors.contains(&key) {
            continue;
        }
        seen.insert(key.clone());
        let seal = current.get(&key);
        let matches = if seal.is_none() {
            fingerprint_matches(db, entry)?
        } else {
            Vec::new()
        };
        let moved = (matches.len() == 1).then(|| &matches[0]);
        states.push(AnchorState {
            doc: entry.doc.clone(),
            anchor: entry.anchor.clone(),
            status: match seal {
                Some(seal) if seal.fingerprint == entry.fingerprint => "current",
                Some(_) => "drifted",
                None if moved.is_some() => "moved",
                None if matches.len() > 1 => "ambiguous",
                None => "broken",
            }
            .to_string(),
            target_path: seal
                .map(|seal| seal.target_path.clone())
                .or_else(|| moved.as_ref().map(|node| node.path.clone()))
                .unwrap_or_else(|| anchor_path(&entry.anchor).to_string()),
            target_name: seal
                .map(|seal| seal.target_name.clone())
                .or_else(|| moved.as_ref().map(|node| node.name.clone()))
                .unwrap_or_else(|| {
                    entry
                        .anchor
                        .split_once('#')
                        .map(|(_, name)| name)
                        .unwrap_or(if entry.anchor.contains("::") {
                            &entry.anchor
                        } else {
                            ""
                        })
                        .to_string()
                }),
            expected_fingerprint: entry.fingerprint.clone(),
            actual_fingerprint: seal
                .map(|seal| seal.fingerprint.clone())
                .or_else(|| moved.map(|node| node.fingerprint.clone())),
        });
    }
    for ((doc, anchor), seal) in current {
        if seen.contains(&(doc.clone(), anchor.clone())) {
            continue;
        }
        states.push(AnchorState {
            doc,
            anchor,
            status: "current".to_string(),
            target_path: seal.target_path,
            target_name: seal.target_name,
            expected_fingerprint: seal.fingerprint.clone(),
            actual_fingerprint: Some(seal.fingerprint),
        });
    }
    states.sort_by(|a, b| (&a.doc, &a.anchor).cmp(&(&b.doc, &b.anchor)));
    Ok(states)
}

#[derive(Default)]
pub(crate) struct KnowledgeResult {
    pub(crate) checks: Vec<IndexCheck>,
    /// W2's unresolved diagnostic is superseded by a lock-aware W3 result.
    pub(crate) locked_unresolved: BTreeSet<(String, String)>,
}

pub(crate) fn refresh(
    root: &Path,
    db: &SqliteConnection,
) -> Result<KnowledgeResult> {
    let lock_path = root.join(".harnesskit/knowledge.lock");
    let (entries, had_valid_entry) = read_lock(&lock_path)?;
    let existing_entries = entries.clone();
    let current_anchors = current_seals(db)?;
    let current_doc_anchors = current_doc_anchors(db)?;
    let lock_mtime = lock_modified_time(&lock_path);
    let git_available = find_host_git_dir(root).is_some() && git_head(root).is_some();
    let changed_paths = if git_available {
        Some(worktree_changed_paths(root))
    } else {
        None
    };

    let mut checks = Vec::new();
    let mut locked_unresolved = BTreeSet::new();
    let mut next = BTreeMap::<(String, String), LockEntry>::new();

    // Existing entries are the authority for detecting a disappeared symbol.
    // Entries whose document no longer contains the anchor are intentionally
    // omitted, which also cleans stale lines after an ordinary index.
    for entry in &existing_entries {
        let key = (entry.doc.clone(), entry.anchor.clone());
        if is_line_anchor(&entry.anchor) || !current_doc_anchors.contains(&key) {
            continue;
        }
        locked_unresolved.insert(key.clone());
        let current = current_anchors.get(&key);
        match current {
            Some(current) if current.fingerprint == entry.fingerprint => {
                next.insert(key, entry.clone());
            }
            Some(current) => {
                if should_reseal(
                    root,
                    entry,
                    Some(current),
                    lock_mtime,
                    git_available,
                    changed_paths.as_ref(),
                ) {
                    next.insert(
                        key,
                        LockEntry {
                            doc: entry.doc.clone(),
                            anchor: entry.anchor.clone(),
                            fingerprint: current.fingerprint.clone(),
                        },
                    );
                } else {
                    next.insert(key.clone(), entry.clone());
                    checks.push(code_drift_check(root, entry, current)?);
                }
            }
            None => {
                let matches = fingerprint_matches(db, entry)?;
                if matches.len() == 1 {
                    checks.push(anchor_moved_check(entry, &matches[0]));
                } else if matches.is_empty() {
                    checks.push(anchor_broken_check(entry));
                }
                next.insert(key, entry.clone());
            }
        }
    }

    // A newly written anchor gets its first seal only after W2 resolved it.
    for ((doc, anchor), current) in &current_anchors {
        let key = (doc.clone(), anchor.clone());
        if !next.contains_key(&key) {
            next.insert(
                key,
                LockEntry {
                    doc: doc.clone(),
                    anchor: anchor.clone(),
                    fingerprint: current.fingerprint.clone(),
                },
            );
        }
    }

    let next_entries = next.into_values().collect::<Vec<_>>();
    write_lock(&lock_path, &next_entries, had_valid_entry)?;

    for check in &mut checks {
        complete_check(check);
    }
    Ok(KnowledgeResult {
        checks,
        locked_unresolved,
    })
}

pub(crate) fn acknowledge(
    root: &Path,
    db: &SqliteConnection,
    patterns: &[String],
) -> Result<bool> {
    if patterns.is_empty() {
        return Ok(false);
    }
    let lock_path = root.join(".harnesskit/knowledge.lock");
    let (entries, had_valid_entry) = read_lock(&lock_path)?;
    if !had_valid_entry {
        return Ok(false);
    }
    let current = current_seals(db)?;
    let mut changed = false;
    let mut next = entries
        .into_iter()
        .map(|entry| ((entry.doc.clone(), entry.anchor.clone()), entry))
        .collect::<BTreeMap<_, _>>();
    for ((doc, anchor), seal) in current {
        if !matches_any_pattern(&doc, patterns) {
            continue;
        }
        let key = (doc.clone(), anchor.clone());
        let replacement = LockEntry {
            doc,
            anchor,
            fingerprint: seal.fingerprint,
        };
        if next.get(&key) != Some(&replacement) {
            next.insert(key, replacement);
            changed = true;
        }
    }
    if changed {
        write_lock(&lock_path, &next.into_values().collect::<Vec<_>>(), true)?;
    }
    Ok(changed)
}

/// Rewrite every uniquely recoverable moved anchor and update its lock entry.
pub(crate) fn fix_moved(root: &Path, db: &SqliteConnection) -> Result<bool> {
    let lock_path = root.join(".harnesskit/knowledge.lock");
    let (entries, had_valid_entry) = read_lock(&lock_path)?;
    if !had_valid_entry {
        return Ok(false);
    }
    let current = current_seals(db)?;
    let current_docs = current_doc_anchors(db)?;
    let mut replacements = BTreeMap::<String, BTreeMap<String, String>>::new();
    let mut next = entries
        .iter()
        .cloned()
        .map(|entry| ((entry.doc.clone(), entry.anchor.clone()), entry))
        .collect::<BTreeMap<_, _>>();
    let mut changed = false;
    for entry in &entries {
        let key = (entry.doc.clone(), entry.anchor.clone());
        if !current_docs.contains(&key) || current.contains_key(&key) {
            continue;
        }
        let Some(target) = unique_fingerprint_match(db, entry)? else {
            continue;
        };
        let Some(new_anchor) = moved_anchor(&entry.anchor, &target) else {
            continue;
        };
        replacements
            .entry(entry.doc.clone())
            .or_default()
            .insert(entry.anchor.clone(), new_anchor.clone());
        next.remove(&key);
        next.insert(
            (entry.doc.clone(), new_anchor.clone()),
            LockEntry {
                doc: entry.doc.clone(),
                anchor: new_anchor,
                fingerprint: target.fingerprint,
            },
        );
        changed = true;
    }

    if !changed {
        return Ok(false);
    }
    for (doc, edits) in replacements {
        let path = root.join(&doc);
        let text = read_text_lossy(&path)?;
        let updated = replace_anchor_code_spans(&text, &edits);
        if updated != text {
            fs::write(path, updated)?;
        }
    }
    write_lock(&lock_path, &next.into_values().collect::<Vec<_>>(), true)?;
    Ok(true)
}

fn current_seals(db: &SqliteConnection) -> Result<BTreeMap<(String, String), CurrentSeal>> {
    let rows = db.query(
        "SELECT e.evidence_json, n.kind, n.path, n.name, n.fingerprint
         FROM edges e JOIN nodes n ON n.id = e.dst
         WHERE e.kind = 'anchor' AND e.confidence = 'EXTRACTED'
         ORDER BY e.src, e.dst, e.evidence_json;",
        [],
    )?;
    let mut grouped = BTreeMap::<(String, String), Vec<TargetNode>>::new();
    for row in &rows.rows {
        let evidence: Value = serde_json::from_str(cell(row, 0))?;
        let Some(doc) = evidence.get("doc").and_then(Value::as_str) else {
            continue;
        };
        let Some(anchor) = evidence.get("anchor").and_then(Value::as_str) else {
            continue;
        };
        if is_line_anchor(anchor) {
            continue;
        }
        grouped
            .entry((doc.to_string(), anchor.to_string()))
            .or_default()
            .push(TargetNode {
                kind: cell(row, 1).to_string(),
                path: cell(row, 2).to_string(),
                name: cell(row, 3).to_string(),
                fingerprint: cell(row, 4).to_string(),
            });
    }

    let mut seals = BTreeMap::new();
    for (key, mut nodes) in grouped {
        nodes.sort_by(|left, right| {
            (&left.kind, &left.path, &left.name, &left.fingerprint).cmp(&(
                &right.kind,
                &right.path,
                &right.name,
                &right.fingerprint,
            ))
        });
        nodes.dedup_by(|left, right| {
            left.kind == right.kind
                && left.path == right.path
                && left.name == right.name
                && left.fingerprint == right.fingerprint
        });
        let anchor = &key.1;
        let is_directory = anchor.split_once('#').map(|(path, _)| path).unwrap_or(anchor).ends_with('/');
        let fingerprint = if is_directory {
            let leaves = nodes
                .iter()
                .filter(|node| node.kind == "file")
                .map(|node| (node.path.clone(), node.fingerprint.clone()))
                .collect::<Vec<_>>();
            if leaves.is_empty() {
                continue;
            }
            merkle_fingerprint(&leaves)
        } else {
            let Some(node) = nodes.first() else {
                continue;
            };
            if node.kind != "file" && node.kind != "symbol" {
                continue;
            }
            node.fingerprint.clone()
        };
        let target = nodes.first().unwrap();
        let target_path = if is_directory {
            anchor
                .split_once('#')
                .map(|(path, _)| path)
                .unwrap_or(anchor)
                .to_string()
        } else {
            target.path.clone()
        };
        seals.insert(
            key,
            CurrentSeal {
                fingerprint,
                target_path,
                target_name: if is_directory {
                    String::new()
                } else {
                    target.name.clone()
                },
            },
        );
    }
    Ok(seals)
}

fn current_doc_anchors(db: &SqliteConnection) -> Result<BTreeSet<(String, String)>> {
    let rows = db.query(
        "SELECT doc_path, text FROM inline_code_spans ORDER BY doc_path, span_index;",
        [],
    )?;
    Ok(rows
        .rows
        .iter()
        .map(|row| (cell(row, 0).to_string(), cell(row, 1).to_string()))
        .collect())
}

fn unique_fingerprint_match(db: &SqliteConnection, entry: &LockEntry) -> Result<Option<TargetNode>> {
    let matches = fingerprint_matches(db, entry)?;
    Ok((matches.len() == 1).then(|| matches[0].clone()))
}

fn fingerprint_matches(db: &SqliteConnection, entry: &LockEntry) -> Result<Vec<TargetNode>> {
    let expected_kind = anchor_kind(&entry.anchor);
    let rows = db.query(
        "SELECT kind, path, name, fingerprint FROM nodes
         WHERE fingerprint = ?1 AND kind IN ('file', 'symbol')
         ORDER BY kind, path, name, id;",
        [&entry.fingerprint],
    )?;
    let matches = rows
        .rows
        .iter()
        .filter(|row| expected_kind.map(|kind| kind == cell(row, 0)).unwrap_or(true))
        .map(|row| TargetNode {
            kind: cell(row, 0).to_string(),
            path: cell(row, 1).to_string(),
            name: cell(row, 2).to_string(),
            fingerprint: cell(row, 3).to_string(),
        })
        .collect::<Vec<_>>();
    Ok(matches)
}

fn anchor_kind(anchor: &str) -> Option<&'static str> {
    let (path, fragment) = anchor.split_once('#').map_or((anchor, None), |(p, f)| (p, Some(f)));
    if path.ends_with('/') {
        None
    } else if fragment.is_some() || (!path.contains('/') && path.contains("::")) {
        Some("symbol")
    } else if anchor.contains("::") && fragment.is_none() {
        Some("symbol")
    } else {
        Some("file")
    }
}

fn moved_anchor(anchor: &str, target: &TargetNode) -> Option<String> {
    match anchor_kind(anchor) {
        Some("symbol") if !target.name.is_empty() => Some(format!("{}#{}", target.path, target.name)),
        Some("file") => Some(target.path.clone()),
        _ => None,
    }
}

fn is_line_anchor(anchor: &str) -> bool {
    anchor
        .split_once('#')
        .map(|(_, fragment)| {
            let fragment = fragment.strip_prefix('L').unwrap_or("");
            let (start, end) = fragment.split_once("-L").unwrap_or((fragment, fragment));
            start.parse::<usize>().is_ok() && end.parse::<usize>().is_ok()
        })
        .unwrap_or(false)
}

fn merkle_fingerprint(leaves: &[(String, String)]) -> String {
    let mut sorted = leaves.to_vec();
    sorted.sort();
    let mut level = sorted
        .iter()
        .map(|(path, fingerprint)| {
            let mut hasher = blake3::Hasher::new();
            hasher.update(path.as_bytes());
            hasher.update(&[0]);
            hasher.update(fingerprint.as_bytes());
            *hasher.finalize().as_bytes()
        })
        .collect::<Vec<_>>();
    while level.len() > 1 {
        let mut next = Vec::with_capacity(level.len().div_ceil(2));
        for pair in level.chunks(2) {
            let mut hasher = blake3::Hasher::new();
            hasher.update(&pair[0]);
            hasher.update(pair.get(1).unwrap_or(&pair[0]));
            next.push(*hasher.finalize().as_bytes());
        }
        level = next;
    }
    hex12(&level[0])
}

fn hex12(bytes: &[u8; 32]) -> String {
    let mut output = String::with_capacity(12);
    for byte in bytes.iter().take(6) {
        output.push_str(&format!("{:02x}", byte));
    }
    output
}

fn read_lock(path: &Path) -> Result<(Vec<LockEntry>, bool)> {
    let Ok(text) = fs::read_to_string(path) else {
        return Ok((Vec::new(), false));
    };
    let mut entries = BTreeSet::new();
    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields = if line.contains('\t') {
            line.split('\t').map(str::trim).collect::<Vec<_>>()
        } else {
            line.split_whitespace().collect::<Vec<_>>()
        };
        if fields.len() != 3 {
            continue;
        }
        if fields.iter().any(|field| field.is_empty() || field.contains(char::is_whitespace))
            || fields[2].len() != 12
            || !fields[2].bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            continue;
        }
        entries.insert(LockEntry {
            doc: fields[0].to_string(),
            anchor: fields[1].to_string(),
            fingerprint: fields[2].to_ascii_lowercase(),
        });
    }
    let had_valid_entry = !entries.is_empty();
    Ok((entries.into_iter().collect(), had_valid_entry))
}

fn write_lock(path: &Path, entries: &[LockEntry], had_valid_entry: bool) -> Result<()> {
    if entries.is_empty() {
        if had_valid_entry && path.exists() {
            fs::remove_file(path)?;
        }
        return Ok(());
    }
    // Do not silently destroy an explicitly supplied, non-lock file.  This
    // also keeps recovery state visible for a human to repair before the next
    // index.  A header-only file remains eligible for its first seal.
    if path.exists() && !had_valid_entry {
        let opaque = fs::read_to_string(path)
            .map(|text| text.lines().any(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#')))
            .unwrap_or(false);
        if opaque {
            return Ok(());
        }
    }
    let mut sorted = entries.to_vec();
    sorted.sort();
    sorted.dedup();
    let mut output = String::new();
    output.push_str(LOCK_HEADER);
    output.push('\n');
    for entry in sorted {
        output.push_str(&entry.doc);
        output.push('\t');
        output.push_str(&entry.anchor);
        output.push('\t');
        output.push_str(&entry.fingerprint);
        output.push('\n');
    }
    if fs::read_to_string(path).ok().as_deref() != Some(output.as_str()) {
        fs::create_dir_all(path.parent().unwrap())?;
        fs::write(path, output)?;
    }
    Ok(())
}

fn lock_modified_time(path: &Path) -> Option<(u64, u64)> {
    fs::metadata(path)
        .ok()
        .and_then(|metadata| file_modified_parts(&metadata).ok())
}

fn should_reseal(
    root: &Path,
    entry: &LockEntry,
    current: Option<&CurrentSeal>,
    lock_mtime: Option<(u64, u64)>,
    git_available: bool,
    changed_paths: Option<&BTreeSet<String>>,
) -> bool {
    if !git_available {
        let Some(doc_mtime) = document_mtime(root, &entry.doc) else {
            return false;
        };
        return lock_mtime.map(|sealed_at| doc_mtime > sealed_at).unwrap_or(false);
    }
    let target_path = if anchor_path(entry.anchor.as_str()).ends_with('/') {
        anchor_path(entry.anchor.as_str())
    } else {
        current
            .map(|target| target.target_path.as_str())
            .unwrap_or_else(|| anchor_path(entry.anchor.as_str()))
    };
    let code_changed = changed_paths
        .map(|paths| paths.iter().any(|path| path_matches_scope(path, target_path)))
        .unwrap_or(false);
    let doc_changed = changed_paths
        .map(|paths| paths.iter().any(|path| path == &entry.doc))
        .unwrap_or(false);
    // A previous joint commit cannot review a newer code-only worktree edit.
    if code_changed {
        return doc_changed;
    }
    let Some(doc_commit) = latest_commit(root, &entry.doc) else {
        return false;
    };
    let Some(code_commit) = latest_commit(root, target_path) else {
        return false;
    };
    !doc_commit.is_empty() && doc_commit == code_commit
}

fn anchor_path(anchor: &str) -> &str {
    anchor.split_once('#').map(|(path, _)| path).unwrap_or(anchor)
}

fn path_matches_scope(path: &str, scope: &str) -> bool {
    path == scope || (scope.ends_with('/') && path.starts_with(scope))
}

fn document_mtime(root: &Path, path: &str) -> Option<(u64, u64)> {
    fs::metadata(root.join(path))
        .ok()
        .and_then(|metadata| file_modified_parts(&metadata).ok())
}

fn latest_commit(root: &Path, path: &str) -> Option<String> {
    run_git_capture(root, &["log", "-1", "--format=%H", "--", path]).ok()
}

fn git_head(root: &Path) -> Option<String> {
    run_git_capture(root, &["rev-parse", "HEAD"]).ok()
}

fn worktree_changed_paths(root: &Path) -> BTreeSet<String> {
    let Ok(output) = Command::new("git")
        .args(["status", "--porcelain=v1", "-z", "--untracked-files=all"])
        .current_dir(root)
        .output()
    else {
        return BTreeSet::new();
    };
    if !output.status.success() {
        return BTreeSet::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .split('\0')
        .filter_map(|record| {
            let path = record.get(3..)?.trim();
            let path = path.split(" -> ").last()?.trim();
            (!path.is_empty()).then(|| path.to_string())
        })
        .collect()
}

fn change_info(root: &Path, path: &str) -> (String, String, String) {
    let changed_paths = worktree_changed_paths(root);
    if changed_paths.iter().any(|changed| path_matches_scope(changed, path)) {
        return (
            "working-tree".to_string(),
            now_unix().to_string(),
            format!("git diff -- {}", path),
        );
    }
    if let Ok(value) = run_git_capture(root, &["log", "-1", "--format=%H%x00%ct%x00%cI", "--", path]) {
        let mut fields = value.split('\0');
        let commit = fields.next().unwrap_or_default().to_string();
        let timestamp = fields.next().unwrap_or_default().to_string();
        let iso = fields.next().unwrap_or_default().to_string();
        if !commit.is_empty() {
            return (
                commit.clone(),
                if iso.is_empty() { timestamp } else { iso },
                format!("git diff {} -- {}", commit, path),
            );
        }
    }
    let timestamp = now_unix().to_string();
    ("working-tree".to_string(), timestamp, format!("git diff -- {}", path))
}

fn code_drift_check(root: &Path, entry: &LockEntry, current: &CurrentSeal) -> Result<IndexCheck> {
    let (commit, changed_at, git_diff) = change_info(root, &current.target_path);
    let hint = format!("Review the changed code with `{}`; if the document remains correct, run `harnesskit check --ack {}`.", git_diff, entry.doc);
    let mut check = IndexCheck {
        rule_id: "code-drift".to_string(),
        severity: "warning".to_string(),
        subject_path: entry.doc.clone(),
        message: format!(
            "{} anchor `{}` drifted ({} at {}).",
            entry.doc, entry.anchor, commit, changed_at
        ),
        hint: Some(hint.clone()),
        fixable: false,
        evidence_json: json!({
            "doc": entry.doc,
            "anchor": entry.anchor,
            "expected_fingerprint": entry.fingerprint,
            "actual_fingerprint": current.fingerprint,
            "target_path": current.target_path,
            "commit": commit,
            "changed_commit": commit,
            "changed_at": changed_at,
            "time": changed_at,
            "git_diff": git_diff,
            "git_diff_command": git_diff,
            "hint": hint,
            "fixable": false,
        })
        .to_string(),
    };
    complete_check(&mut check);
    Ok(check)
}

fn anchor_moved_check(entry: &LockEntry, target: &TargetNode) -> IndexCheck {
    let target_anchor = moved_anchor(&entry.anchor, target).unwrap_or_else(|| target.path.clone());
    let hint = format!("Run `harnesskit check --fix` to rewrite this anchor to `{}`.", target_anchor);
    let mut check = IndexCheck {
        rule_id: "anchor-moved".to_string(),
        severity: "warning".to_string(),
        subject_path: entry.doc.clone(),
        message: format!(
            "{} anchor `{}` moved to `{}`.",
            entry.doc, entry.anchor, target_anchor
        ),
        hint: Some(hint.clone()),
        fixable: true,
        evidence_json: json!({
            "doc": entry.doc,
            "anchor": entry.anchor,
            "target": target_anchor,
            "target_path": target.path,
            "target_name": target.name,
            "fingerprint": entry.fingerprint,
            "old_fingerprint": entry.fingerprint,
            "new_fingerprint": target.fingerprint,
            "hint": hint,
            "fixable": true,
        })
        .to_string(),
    };
    complete_check(&mut check);
    check
}

fn anchor_broken_check(entry: &LockEntry) -> IndexCheck {
    let hint = format!("Restore the target for `{}` or remove/update the anchor in {}; `check --ack` only confirms an existing target.", entry.anchor, entry.doc);
    let mut check = IndexCheck {
        rule_id: "anchor-broken".to_string(),
        severity: "warning".to_string(),
        subject_path: entry.doc.clone(),
        message: format!("{} anchor `{}` is broken; no matching code fingerprint was found.", entry.doc, entry.anchor),
        hint: Some(hint.clone()),
        fixable: false,
        evidence_json: json!({
            "doc": entry.doc,
            "anchor": entry.anchor,
            "fingerprint": entry.fingerprint,
            "search": "full-repository structural fingerprint search",
            "hint": hint,
            "fixable": false,
        })
        .to_string(),
    };
    complete_check(&mut check);
    check
}

fn matches_any_pattern(path: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|pattern| {
        if pattern == path {
            return true;
        }
        globset::Glob::new(pattern)
            .ok()
            .map(|glob| glob.compile_matcher().is_match(path))
            .unwrap_or(false)
    })
}

fn replace_anchor_code_spans(text: &str, replacements: &BTreeMap<String, String>) -> String {
    if replacements.is_empty() {
        return text.to_string();
    }
    let (frontmatter, body) = crate::parse::split_frontmatter(text);
    let body_offset = text.len() - body.len();
    let mut ranges = Vec::<(usize, usize, String)>::new();
    for (event, range) in Parser::new(body).into_offset_iter() {
        let Event::Code(value) = event else {
            continue;
        };
        let value = value.as_ref();
        let Some(replacement) = replacements.get(value) else {
            continue;
        };
        let source = &body[range.clone()];
        let Some(relative) = source.find(value) else {
            continue;
        };
        ranges.push((
            body_offset + range.start + relative,
            body_offset + range.start + relative + value.len(),
            replacement.clone(),
        ));
    }
    let mut output = text.to_string();
    for (start, end, replacement) in ranges.into_iter().rev() {
        output.replace_range(start..end, &replacement);
    }
    let _ = frontmatter;
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_merkle_is_ordered_and_format_stable() {
        let first = merkle_fingerprint(&[("a.rs".into(), "111111111111".into()), ("b.rs".into(), "222222222222".into())]);
        let second = merkle_fingerprint(&[("b.rs".into(), "222222222222".into()), ("a.rs".into(), "111111111111".into())]);
        assert_eq!(first, second);
        assert_ne!(first, merkle_fingerprint(&[("a.rs".into(), "111111111111".into()), ("b.rs".into(), "333333333333".into())]));
    }

    #[test]
    fn replacement_only_touches_real_inline_code_spans() {
        let text = "`old#f`\n\n```md\n`old#f`\n```\n";
        assert_eq!(replace_anchor_code_spans(text, &BTreeMap::from([("old#f".into(), "new#f".into())])), "`new#f`\n\n```md\n`old#f`\n```\n");
    }
}
