//! W4 inferred edges: document/code cochange history and identifier mentions.
//!
//! The graph itself remains derived SQLite state.  Git history is consumed by
//! commit cursor, while the complete pair statistics stay in `cochange_stats`
//! so materialising the top eight edges never loses a lower-ranked candidate.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::{Command, Stdio};

use rusqlite::params;
use serde_json::json;

use crate::fs_utils::now_unix;
use crate::git::{git_repo_check, run_git_capture};
use crate::index::cell;
use crate::schema::Schema;
use crate::sqlite::SqliteConnection;
use crate::Result;

const DEFAULT_MAX_COMMIT_FILES: usize = 30;
const COCHANGE_TOP_K: usize = 8;
const HALF_LIFE_SECONDS: f64 = 90.0 * 24.0 * 60.0 * 60.0;
// Match basenames at any depth, and paths for generated/vendor directories.
// Defaults are case-insensitive; an explicit schema list replaces them.
const DEFAULT_COCHANGE_IGNORE: &[&str] = &[
    "*.lock",
    "package-lock.json",
    "pnpm-lock.yaml",
    "yarn.lock",
    "go.sum",
    "cargo.toml",
    "package.json",
    "plugin.json",
    "manifest.json",
    "deno.json",
    "deno.jsonc",
    "bower.json",
    "pyproject.toml",
    "go.mod",
    "composer.json",
    "gemfile",
    "pipfile",
    "requirements*.txt",
    "setup.py",
    "setup.cfg",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "*.csproj",
    "*.fsproj",
    "*.vbproj",
    "packages.config",
    "changelog*",
    "changes*",
    "history*",
    "release_notes*",
    "release-notes*",
    "*.min.js",
    "*.min.css",
    "*.map",
    "*.generated.*",
    "*.g.*",
    "**/{target,dist,build,generated,vendor,node_modules}/**",
];

#[derive(Clone, Default)]
struct PairStats {
    decayed_count: f64,
    count: u64,
    last_commit: String,
    last_timestamp: u64,
}

#[derive(Default)]
struct CochangeStats {
    pairs: BTreeMap<(String, String), PairStats>,
    files: BTreeMap<String, u64>,
    commits: u64,
}

struct CommitRecord {
    id: String,
    timestamp: u64,
    paths: Vec<String>,
}

#[derive(serde::Serialize)]
pub(crate) struct CochangedFile {
    pub(crate) path: String,
    pub(crate) count: u64,
    pub(crate) weight: f64,
    pub(crate) confidence: &'static str,
    pub(crate) last_commit: String,
}

/// The reverse entrypoint also needs code/code neighbours. Reuse W4's history,
/// scope, large-commit filter and decay rather than infer a second graph hop.
pub(crate) fn cochanged_files(
    db: &SqliteConnection,
    root: &Path,
    schema: &Schema,
    selected: &BTreeSet<String>,
) -> Result<Vec<CochangedFile>> {
    if selected.is_empty() || git_repo_check(root).0 != Some(true) {
        return Ok(Vec::new());
    }
    let Ok(head) = run_git_capture(root, &["rev-parse", "HEAD"]) else {
        return Ok(Vec::new());
    };
    let (docs, code) = indexed_paths(db, schema)?;
    let eligible = docs.union(&code).cloned().collect::<BTreeSet<_>>();
    let selected = selected
        .intersection(&eligible)
        .cloned()
        .collect::<BTreeSet<_>>();
    if selected.is_empty() {
        return Ok(Vec::new());
    }
    let max_files = schema
        .rules
        .cochange_max_files
        .unwrap_or(DEFAULT_MAX_COMMIT_FILES as u32)
        .max(1) as usize;
    let commits = read_commits(root, None, &head)?;
    let mut counts = BTreeMap::<String, u64>::new();
    let mut pairs = BTreeMap::<String, PairStats>::new();
    let mut selected_count = 0u64;
    let now = now_unix();
    for commit in commits {
        let changed = commit.paths.into_iter().collect::<BTreeSet<_>>();
        if changed.len() > max_files {
            continue;
        }
        for path in changed.intersection(&eligible) {
            *counts.entry(path.clone()).or_default() += 1;
        }
        if changed.is_disjoint(&selected) {
            continue;
        }
        selected_count += 1;
        for path in changed
            .intersection(&eligible)
            .filter(|path| !selected.contains(*path))
        {
            let pair = pairs.entry(path.clone()).or_default();
            pair.count += 1;
            pair.decayed_count +=
                0.5_f64.powf(now.saturating_sub(commit.timestamp) as f64 / HALF_LIFE_SECONDS);
            if commit.timestamp >= pair.last_timestamp {
                pair.last_timestamp = commit.timestamp;
                pair.last_commit = commit.id.clone();
            }
        }
    }
    let mut files = pairs
        .into_iter()
        .map(|(path, pair)| {
            let normalizer = (selected_count as f64 * counts[&path] as f64).sqrt();
            CochangedFile {
                path,
                count: pair.count,
                weight: pair.decayed_count / normalizer,
                confidence: "INFERRED",
                last_commit: pair.last_commit,
            }
        })
        .collect::<Vec<_>>();
    files.sort_by(|a, b| {
        b.weight
            .total_cmp(&a.weight)
            .then_with(|| a.path.cmp(&b.path))
    });
    files.truncate(5);
    Ok(files)
}

#[derive(Clone)]
struct SymbolTarget {
    id: String,
    path: String,
    name: String,
}

#[derive(Clone)]
struct MentionSpan {
    doc: String,
    src: String,
    section: String,
    line: usize,
    column: usize,
    text: String,
}

pub(crate) fn refresh(db: &SqliteConnection, root: &Path, schema: &Schema) -> Result<()> {
    let max_commit_files = schema
        .rules
        .cochange_max_files
        .unwrap_or(DEFAULT_MAX_COMMIT_FILES as u32)
        .max(1) as usize;
    let (doc_paths, code_paths) = indexed_paths(db, schema)?;
    let scope = blake3::hash(&serde_json::to_vec(&(
        &doc_paths,
        &code_paths,
        max_commit_files,
    ))?)
    .to_hex()
    .to_string();

    let git_status = git_repo_check(root).0;
    if git_status == Some(false) || (git_status.is_none() && !has_git_marker(root)) {
        clear_cochange_state(db, "non-git")?;
        refresh_mentions(db)?;
        return Ok(());
    }
    if git_status.is_none() {
        // Keep the last derived graph when Git itself is temporarily
        // unavailable.  This mirrors W1's cached-code behavior and avoids
        // turning an environment problem into data loss.
        set_cochange_scan_mode(db, "git-unavailable")?;
        return refresh_mentions(db);
    }

    let Ok(head) = run_git_capture(root, &["rev-parse", "HEAD"]) else {
        set_cochange_scan_mode(db, "git-unavailable")?;
        return refresh_mentions(db);
    };
    if head.is_empty() {
        clear_cochange_state(db, "git-empty")?;
        refresh_mentions(db)?;
        return Ok(());
    }

    let previous_head = crate::index::meta_value(db, "cochange_last_commit")?;
    let previous_indexed_at = crate::index::meta_value(db, "cochange_indexed_at")?
        .and_then(|value| value.parse::<u64>().ok());
    let indexed_at = now_unix();
    // Newly indexed files may already have history before the cursor. Replay
    // it when the eligible scope changes rather than silently losing evidence.
    let full_rebuild = crate::index::meta_value(db, "cochange_scope")?.as_deref() != Some(&scope)
        || match previous_head.as_deref().filter(|value| !value.is_empty()) {
            None => true,
            Some(previous) if previous == head => false,
            Some(previous) => !is_ancestor(root, previous, &head),
        };
    let commits = if full_rebuild {
        read_commits(root, None, &head)?
    } else if previous_head.as_deref() == Some(head.as_str()) {
        Vec::new()
    } else {
        read_commits(root, previous_head.as_deref(), &head)?
    };

    let mut stats = if full_rebuild {
        CochangeStats::default()
    } else {
        load_cochange_stats(db)?
    };
    if !full_rebuild {
        if let Some(previous_indexed_at) = previous_indexed_at {
            let elapsed = indexed_at.saturating_sub(previous_indexed_at) as f64;
            let decay = 0.5_f64.powf(elapsed / HALF_LIFE_SECONDS);
            for stat in stats.pairs.values_mut() {
                stat.decayed_count *= decay;
            }
        }
    }
    update_cochange_stats(
        &mut stats,
        &commits,
        &doc_paths,
        &code_paths,
        max_commit_files,
        indexed_at,
    );
    let history_changed = previous_head.as_deref() != Some(head.as_str());
    let time_changed = previous_indexed_at != Some(indexed_at);
    if !full_rebuild && !history_changed && !time_changed {
        return refresh_mentions(db);
    }
    write_cochange_state(
        db,
        &stats,
        &doc_paths,
        &code_paths,
        &head,
        if full_rebuild { "full" } else { "delta" },
        max_commit_files,
        indexed_at,
        &scope,
    )?;
    refresh_mentions(db)
}

fn indexed_paths(
    db: &SqliteConnection,
    schema: &Schema,
) -> Result<(BTreeSet<String>, BTreeSet<String>)> {
    let patterns = schema.cochange_ignore.clone().unwrap_or_else(|| {
        DEFAULT_COCHANGE_IGNORE
            .iter()
            .map(|pattern| pattern.to_string())
            .collect()
    });
    let matchers = patterns
        .iter()
        .map(|pattern| crate::schema::glob_matcher(pattern))
        .collect::<Result<Vec<_>>>()?;
    let eligible = |path: &String| {
        if path.is_empty() {
            return false;
        }
        let candidate = if schema.cochange_ignore.is_none() {
            path.to_ascii_lowercase()
        } else {
            path.clone()
        };
        let basename = Path::new(&candidate).file_name().unwrap_or_default();
        !matchers
            .iter()
            .any(|matcher| matcher.is_match(&candidate) || matcher.is_match(basename))
    };
    let docs = db.query("SELECT path FROM docs ORDER BY path;", [])?;
    let code = db.query(
        "SELECT path FROM nodes WHERE kind = 'file' ORDER BY path;",
        [],
    )?;
    Ok((
        docs.rows
            .iter()
            .map(|row| cell(row, 0).to_string())
            .filter(eligible)
            .collect(),
        code.rows
            .iter()
            .map(|row| cell(row, 0).to_string())
            .filter(eligible)
            .collect(),
    ))
}

fn clear_cochange_state(db: &SqliteConnection, mode: &str) -> Result<()> {
    db.transaction(|db| {
        db.exec(
            "DELETE FROM edges WHERE kind = 'cochange'; DELETE FROM cochange_stats;
                 DELETE FROM cochange_files;",
        )?;
        for (key, value) in [
            ("cochange_last_commit", String::new()),
            ("cochange_head", String::new()),
            ("cochange_indexed_at", String::new()),
            ("cochange_scan_mode", mode.to_string()),
            ("cochange_scope", String::new()),
            ("cochange_accepted_commits", "0".to_string()),
        ] {
            db.execute(
                "INSERT OR REPLACE INTO meta(key, value) VALUES (?1, ?2);",
                params![key, value],
            )?;
        }
        Ok(())
    })
}

fn set_cochange_scan_mode(db: &SqliteConnection, mode: &str) -> Result<()> {
    db.execute(
        "INSERT OR REPLACE INTO meta(key, value) VALUES ('cochange_scan_mode', ?1);",
        [mode],
    )
}

fn has_git_marker(root: &Path) -> bool {
    root.ancestors().any(|ancestor| {
        let marker = ancestor.join(".git");
        marker.join("HEAD").is_file()
            || std::fs::read_to_string(marker)
                .map(|text| text.starts_with("gitdir: "))
                .unwrap_or(false)
    })
}

fn load_cochange_stats(db: &SqliteConnection) -> Result<CochangeStats> {
    let rows = db.query(
        "SELECT doc_path, code_path, decayed_count, cochange_count, last_commit, last_timestamp
         FROM cochange_stats ORDER BY doc_path, code_path;",
        [],
    )?;
    let mut stats = CochangeStats {
        commits: crate::index::meta_value(db, "cochange_accepted_commits")?
            .and_then(|value| value.parse().ok())
            .unwrap_or(0),
        ..CochangeStats::default()
    };
    for row in &rows.rows {
        let doc = cell(row, 0).to_string();
        let code = cell(row, 1).to_string();
        if doc.is_empty() || code.is_empty() {
            continue;
        }
        stats.pairs.insert(
            (doc, code),
            PairStats {
                decayed_count: cell(row, 2).parse().unwrap_or(0.0),
                count: cell(row, 3).parse().unwrap_or(0),
                last_commit: cell(row, 4).to_string(),
                last_timestamp: cell(row, 5).parse().unwrap_or(0),
            },
        );
    }
    for row in &db
        .query(
            "SELECT path, commit_count FROM cochange_files ORDER BY path;",
            [],
        )?
        .rows
    {
        stats
            .files
            .insert(cell(row, 0).to_string(), cell(row, 1).parse().unwrap_or(0));
    }
    Ok(stats)
}

fn update_cochange_stats(
    stats: &mut CochangeStats,
    commits: &[CommitRecord],
    docs: &BTreeSet<String>,
    code: &BTreeSet<String>,
    max_commit_files: usize,
    now: u64,
) {
    for commit in commits {
        let changed = commit.paths.iter().cloned().collect::<BTreeSet<_>>();
        if changed.len() > max_commit_files {
            continue;
        }
        stats.commits += 1;
        let changed_docs = changed.intersection(docs).cloned().collect::<Vec<_>>();
        let changed_code = changed.intersection(code).cloned().collect::<Vec<_>>();
        // Count doc-only and code-only commits as well: a frequently changed
        // utility must not look specific to every document it ever touched.
        for path in changed_docs.iter().chain(&changed_code) {
            *stats.files.entry(path.clone()).or_default() += 1;
        }
        let age = now.saturating_sub(commit.timestamp) as f64;
        let decay = 0.5_f64.powf(age / HALF_LIFE_SECONDS);
        for doc in &changed_docs {
            for path in &changed_code {
                let entry = stats.pairs.entry((doc.clone(), path.clone())).or_default();
                entry.decayed_count += decay;
                entry.count += 1;
                if commit.timestamp > entry.last_timestamp
                    || (commit.timestamp == entry.last_timestamp && commit.id > entry.last_commit)
                {
                    entry.last_timestamp = commit.timestamp;
                    entry.last_commit = commit.id.clone();
                }
            }
        }
    }
}

fn cochange_factors(stats: &CochangeStats, doc: &str, code: &str, pair: &PairStats) -> (f64, f64) {
    let doc_count = stats.files.get(doc).copied().unwrap_or(0) as f64;
    let code_count = stats.files.get(code).copied().unwrap_or(0) as f64;
    let normalizer = (doc_count * code_count).sqrt();
    let cosine = if normalizer > 0.0 {
        pair.decayed_count / normalizer
    } else {
        0.0
    };
    // Smoothed, bounded IDF keeps even an always-changed file nonzero. Since
    // each decay <= 1, the decayed dot product divided by the full occurrence
    // norms is <= 1 (Cauchy-Schwarz). Clamp only floating-point roundoff.
    let total = stats.commits as f64 + 1.0;
    let idf = (1.0 + (total / (code_count + 1.0)).ln()) / (1.0 + total.ln());
    (cosine.clamp(0.0, 1.0), idf.clamp(0.0, 1.0))
}

#[allow(clippy::too_many_arguments)]
fn write_cochange_state(
    db: &SqliteConnection,
    stats: &CochangeStats,
    docs: &BTreeSet<String>,
    code: &BTreeSet<String>,
    head: &str,
    mode: &str,
    max_commit_files: usize,
    indexed_at: u64,
    scope: &str,
) -> Result<()> {
    db.transaction(|db| {
        db.exec("DELETE FROM edges WHERE kind = 'cochange'; DELETE FROM cochange_stats;
                 DELETE FROM cochange_files;")?;
        for (path, count) in &stats.files {
            db.execute("INSERT INTO cochange_files(path, commit_count) VALUES (?1, ?2);", params![path, count])?;
        }
        for ((doc, path), stat) in &stats.pairs {
            let (cosine, idf) = cochange_factors(stats, doc, path, stat);
            db.execute(
                "INSERT INTO cochange_stats(doc_path, code_path, weight, decayed_count, cochange_count, last_commit, last_timestamp)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7);",
                params![
                    doc,
                    path,
                    cosine * idf,
                    stat.decayed_count,
                    stat.count,
                    stat.last_commit,
                    stat.last_timestamp
                ],
            )?;
        }

        for doc in docs {
            let rows = db.query(
                "SELECT code_path, weight, cochange_count, last_commit, last_timestamp
                 FROM cochange_stats WHERE doc_path = ?1
                 ORDER BY weight DESC, code_path ASC LIMIT ?2;",
                params![doc, COCHANGE_TOP_K as i64],
            )?;
            for row in &rows.rows {
                let path = cell(row, 0);
                if !code.contains(path) {
                    continue;
                }
                let pair = &stats.pairs[&(doc.clone(), path.to_string())];
                let (cosine, idf) = cochange_factors(stats, doc, path, pair);
                let evidence = json!({
                    "doc": doc,
                    "code": path,
                    "cochange_count": cell(row, 2).parse::<u64>().unwrap_or(0),
                    "count": cell(row, 2).parse::<u64>().unwrap_or(0),
                    "last_commit": cell(row, 3),
                    "recent_commit": cell(row, 3),
                    "last_commit_time": cell(row, 4).parse::<u64>().unwrap_or(0),
                    "max_commit_files": max_commit_files,
                    "doc_commits": stats.files.get(doc).copied().unwrap_or(0),
                    "code_commits": stats.files.get(path).copied().unwrap_or(0),
                    "history_commits": stats.commits,
                    "half_life_days": 90,
                    "cosine": cosine,
                    "idf": idf,
                });
                db.execute(
                    "INSERT INTO edges(src, dst, kind, confidence, weight, evidence_json)
                     VALUES (?1, ?2, 'cochange', 'INFERRED', ?3, ?4);",
                    params![
                        format!("doc:{doc}"),
                        format!("file:{path}"),
                        cell(row, 1).parse::<f64>().unwrap_or(0.0),
                        evidence.to_string()
                    ],
                )?;
            }
        }
        for (key, value) in [
            ("cochange_last_commit", head.to_string()),
            ("cochange_head", head.to_string()),
            ("cochange_indexed_at", indexed_at.to_string()),
            ("cochange_scan_mode", mode.to_string()),
            ("cochange_scope", scope.to_string()),
            ("cochange_accepted_commits", stats.commits.to_string()),
        ] {
            db.execute(
                "INSERT OR REPLACE INTO meta(key, value) VALUES (?1, ?2);",
                params![key, value],
            )?;
        }
        Ok(())
    })
}

fn is_ancestor(root: &Path, ancestor: &str, descendant: &str) -> bool {
    Command::new("git")
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .current_dir(root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn read_commits(root: &Path, since: Option<&str>, head: &str) -> Result<Vec<CommitRecord>> {
    let mut args = vec![
        "--no-pager".to_string(),
        "log".to_string(),
        "--no-merges".to_string(),
        "--format=%H%x00%ct%x00".to_string(),
        "--name-only".to_string(),
        "-z".to_string(),
        "--no-renames".to_string(),
    ];
    if let Some(since) = since {
        args.push(format!("{since}..{head}"));
    } else {
        args.push(head.to_string());
    }
    args.push("--".to_string());
    let output = Command::new("git").args(&args).current_dir(root).output()?;
    if !output.status.success() {
        return Err(format!(
            "git log failed while reading cochange history: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }

    let mut records = Vec::new();
    let mut current: Option<CommitRecord> = None;
    let mut expecting_timestamp = false;
    for raw in output.stdout.split(|byte| *byte == 0) {
        let mut part = String::from_utf8_lossy(raw).to_string();
        if part.is_empty() {
            continue;
        }
        part = part.trim_start_matches('\n').to_string();
        if current.is_none() && looks_like_commit(&part) {
            current = Some(CommitRecord {
                id: part,
                timestamp: 0,
                paths: Vec::new(),
            });
            expecting_timestamp = true;
            continue;
        }
        if expecting_timestamp {
            if let Some(record) = current.as_mut() {
                record.timestamp = part.parse().unwrap_or(0);
            }
            expecting_timestamp = false;
            continue;
        }
        if looks_like_commit(&part) {
            if let Some(record) = current.take() {
                records.push(record);
            }
            current = Some(CommitRecord {
                id: part,
                timestamp: 0,
                paths: Vec::new(),
            });
            expecting_timestamp = true;
            continue;
        }
        if let Some(record) = current.as_mut() {
            record.paths.push(part);
        }
    }
    if let Some(record) = current {
        records.push(record);
    }
    Ok(records)
}

fn looks_like_commit(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn refresh_mentions(db: &SqliteConnection) -> Result<()> {
    let spans = db.query(
        "SELECT doc_path, src, section, line, column_number, text
         FROM inline_code_spans WHERE prose = 0 ORDER BY doc_path, span_index;",
        [],
    )?;
    let anchor_spans = db
        .query(
            "SELECT evidence_json FROM edges
             WHERE kind IN ('anchor', 'mention') AND confidence = 'EXTRACTED';",
            [],
        )?
        .rows
        .iter()
        .filter_map(|row| serde_json::from_str::<serde_json::Value>(cell(row, 0)).ok())
        .filter_map(|value| {
            Some((
                value.get("doc")?.as_str()?.to_string(),
                value.get("line")?.as_u64()? as usize,
                value.get("column")?.as_u64()? as usize,
            ))
        })
        .collect::<BTreeSet<_>>();
    if spans.rows.iter().all(|row| {
        !is_identifier(cell(row, 5).trim())
            || anchor_spans.contains(&(
                cell(row, 0).to_string(),
                cell(row, 3).parse().unwrap_or(0),
                cell(row, 4).parse().unwrap_or(0),
            ))
    }) {
        return db.exec("DELETE FROM edges WHERE kind = 'mention' AND confidence = 'INFERRED'");
    }
    let symbols = db.query(
        "SELECT id, path, name FROM nodes WHERE kind = 'symbol' AND is_definition = 1
         ORDER BY path, name, id;",
        [],
    )?;
    let targets = symbols
        .rows
        .iter()
        .map(|row| SymbolTarget {
            id: cell(row, 0).to_string(),
            path: cell(row, 1).to_string(),
            name: cell(row, 2).to_string(),
        })
        .collect::<Vec<_>>();
    let cochange = db.query(
        "SELECT doc_path, code_path, weight FROM cochange_stats ORDER BY doc_path, code_path;",
        [],
    )?;
    let cochange = cochange
        .rows
        .iter()
        .map(|row| {
            (
                (cell(row, 0).to_string(), cell(row, 1).to_string()),
                cell(row, 2).parse::<f64>().unwrap_or(0.0),
            )
        })
        .collect::<BTreeMap<_, _>>();

    let mut mentions = Vec::new();
    for row in &spans.rows {
        let span = MentionSpan {
            doc: cell(row, 0).to_string(),
            src: cell(row, 1).to_string(),
            section: cell(row, 2).to_string(),
            line: cell(row, 3).parse().unwrap_or(0),
            column: cell(row, 4).parse().unwrap_or(0),
            text: cell(row, 5).trim().to_string(),
        };
        if !is_identifier(&span.text)
            || is_explicit_anchor(&span.text, &targets)
            || anchor_spans.contains(&(span.doc.clone(), span.line, span.column))
        {
            continue;
        }
        let mut candidates = targets
            .iter()
            .filter(|target| {
                target.name == span.text
                    || target
                        .name
                        .rsplit_once("::")
                        .map(|(_, name)| name == span.text)
                        .unwrap_or(false)
            })
            .map(|target| {
                let proximity = path_proximity(&span.doc, &target.path);
                let cochange_weight = cochange
                    .get(&(span.doc.clone(), target.path.clone()))
                    .copied()
                    .unwrap_or(0.0);
                (target.clone(), proximity, cochange_weight)
            })
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            continue;
        }
        candidates.sort_by(|left, right| {
            right
                .1
                .total_cmp(&left.1)
                .then_with(|| right.2.total_cmp(&left.2))
                .then_with(|| left.0.id.cmp(&right.0.id))
        });
        let ambiguous = candidates.len() > 1
            && (candidates[0].1 - candidates[1].1).abs() < f64::EPSILON
            && (candidates[0].2 - candidates[1].2).abs() < f64::EPSILON;
        let selected = if ambiguous {
            candidates
                .iter()
                .filter(|candidate| {
                    (candidate.1 - candidates[0].1).abs() < f64::EPSILON
                        && (candidate.2 - candidates[0].2).abs() < f64::EPSILON
                })
                .collect::<Vec<_>>()
        } else {
            vec![&candidates[0]]
        };
        let candidate_evidence = candidates
            .iter()
            .map(|(target, proximity, cochange_weight)| {
                json!({
                    "id": target.id,
                    "path": target.path,
                    "name": target.name,
                    "path_proximity": proximity,
                    "cochange_weight": cochange_weight,
                })
            })
            .collect::<Vec<_>>();
        for (target, proximity, cochange_weight) in selected {
            mentions.push((
                span.clone(),
                target.clone(),
                *proximity,
                *cochange_weight,
                ambiguous,
                candidate_evidence.clone(),
            ));
        }
    }

    let mut next_edges = mentions
        .into_iter()
        .map(
            |(span, target, proximity, cochange_weight, ambiguous, candidates)| {
                let evidence = json!({
                    "doc": span.doc,
                    "section": span.section,
                    "line": span.line,
                    "column": span.column,
                    "identifier": span.text,
                    "ambiguous": ambiguous,
                    "path_proximity": proximity,
                    "cochange_weight": cochange_weight,
                    "candidates": candidates,
                });
                (
                    span.src,
                    target.id,
                    if ambiguous { 0.1_f64 } else { 1.0_f64 },
                    evidence.to_string(),
                )
            },
        )
        .collect::<Vec<_>>();
    next_edges
        .sort_by(|left, right| (&left.0, &left.1, &left.3).cmp(&(&right.0, &right.1, &right.3)));
    let previous = db
        .query(
            "SELECT src, dst, weight, evidence_json FROM edges
             WHERE kind = 'mention' AND confidence = 'INFERRED'
             ORDER BY src, dst, evidence_json;",
            [],
        )?
        .rows
        .iter()
        .map(|row| {
            (
                cell(row, 0).to_string(),
                cell(row, 1).to_string(),
                cell(row, 2).parse::<f64>().unwrap_or(0.0),
                cell(row, 3).to_string(),
            )
        })
        .collect::<Vec<_>>();
    if previous.len() == next_edges.len()
        && previous.iter().zip(&next_edges).all(|(left, right)| {
            left.0 == right.0
                && left.1 == right.1
                && (left.2 - right.2).abs() < f64::EPSILON
                && left.3 == right.3
        })
    {
        return Ok(());
    }

    db.transaction(|db| {
        // W9c stores resolved mention-only document references as EXTRACTED
        // edges. Rebuild only W4's inferred mentions here.
        db.exec("DELETE FROM edges WHERE kind = 'mention' AND confidence = 'INFERRED';")?;
        for (src, dst, weight, evidence) in next_edges {
            db.execute(
                "INSERT INTO edges(src, dst, kind, confidence, weight, evidence_json)
                 VALUES (?1, ?2, 'mention', 'INFERRED', ?3, ?4);",
                params![src, dst, weight, evidence],
            )?;
        }
        Ok(())
    })
}

fn is_identifier(value: &str) -> bool {
    if value.chars().count() < 4
        || (!value.contains('_') && !value.chars().any(|ch| ch.is_uppercase()))
    {
        return false;
    }
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first == '$' || first.is_alphabetic())
        && chars.all(|ch| ch == '_' || ch == '$' || ch.is_alphanumeric())
}

fn is_explicit_anchor(value: &str, targets: &[SymbolTarget]) -> bool {
    value.contains('#')
        || value.contains("::")
        || value.contains('/')
        || value.contains('\\')
        || value.contains('.')
        || targets.iter().any(|target| target.path == value)
}

fn path_proximity(doc: &str, code: &str) -> f64 {
    let doc_parts = path_tokens(doc);
    let code_parts = path_tokens(code);
    if doc_parts.is_empty() || code_parts.is_empty() {
        return 0.0;
    }
    let common = doc_parts
        .iter()
        .filter(|part| code_parts.contains(part))
        .count();
    common as f64 / doc_parts.len().max(code_parts.len()) as f64
}

fn path_tokens(path: &str) -> Vec<String> {
    Path::new(path)
        .components()
        .filter_map(|part| part.as_os_str().to_str())
        .flat_map(|part| {
            let part = part.rsplit_once('.').map(|(stem, _)| stem).unwrap_or(part);
            part.split(|ch: char| !ch.is_alphanumeric() && ch != '_')
                .filter(|token| !token.is_empty())
                .map(|token| token.to_ascii_lowercase())
                .collect::<Vec<_>>()
        })
        .collect()
}
