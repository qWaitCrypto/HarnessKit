mod inputs;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::params;
use serde::Serialize;

use crate::checks::{build_index_checks, IndexCheck};
use crate::discover::{
    collect_managed_doc_paths, collection_lookup, collection_name_for_path, default_authority,
    discover_docs, infer_index_role, is_anchor_candidate, is_entrypoint_path, Coverage,
};
use crate::fs_utils::{
    absolute_display_path, ensure_writable_fact_store_dir, fact_store_path_hint,
    file_modified_parts, now_unix, read_text_lossy, simple_content_hash,
};
use crate::git::{git_dirty_label, host_git_info, HostGitInfo, HostGitMode};
use crate::history::{compute_history_status, is_history_dirty, HistoryStatus};
use crate::lifecycle::{doc_lifecycle, infer_status, successor_map};
use crate::parse::parse_doc_text;
use crate::render::{render_index_json, render_index_summary};
use crate::schema::{core_file_rel_path, DocCollectionSpec, Schema};
use crate::sqlite::{QueryResult, SqliteConnection};
use crate::Result;

pub(crate) const FACT_SCHEMA_VERSION: &str = "13";

#[derive(Clone, Default, Serialize)]
pub(crate) struct IndexedDoc {
    pub(crate) path: String,
    pub(crate) title: String,
    pub(crate) summary: String,
    #[serde(skip_serializing)]
    pub(crate) frontmatter_keys: Vec<String>,
    #[serde(skip_serializing)]
    pub(crate) content_text: String,
    pub(crate) role: String,
    pub(crate) status: Option<String>,
    pub(crate) authority: Option<String>,
    pub(crate) collection: Option<String>,
    #[serde(serialize_with = "crate::render::serialize_inline")]
    pub(crate) headings: Vec<String>,
    #[serde(serialize_with = "crate::render::serialize_inline")]
    pub(crate) links: Vec<String>,
    #[serde(skip_serializing)]
    pub(crate) path_mentions: Vec<String>,
    #[serde(serialize_with = "crate::render::serialize_inline")]
    pub(crate) scope_paths: Vec<String>,
    #[serde(serialize_with = "crate::render::serialize_inline")]
    pub(crate) supersedes: Vec<String>,
    #[serde(serialize_with = "crate::render::serialize_inline")]
    pub(crate) superseded_by: Vec<String>,
    #[serde(serialize_with = "crate::render::serialize_inline")]
    pub(crate) supersedes_parts_of: Vec<String>,
    pub(crate) content_hash: String,
    pub(crate) file_size_bytes: u64,
    pub(crate) incoming_links: usize,
    pub(crate) outgoing_links: usize,
    pub(crate) is_entrypoint: bool,
    pub(crate) is_anchor_candidate: bool,
    pub(crate) is_generated: bool,
    pub(crate) is_reference: bool,
    pub(crate) mtime_unix: u64,
    #[serde(skip_serializing)]
    pub(crate) mtime_ns: u64,
}

/// Documents whose resolved explicit code references are durable contracts.
/// The policy is deliberately computed from the indexed lifecycle state so a
/// completed, archived, superseded or banner-marked document is downgraded to
/// a searchable mention even when its collection normally seals anchors.
pub(crate) fn sealed_doc_paths(schema: &Schema, docs: &[IndexedDoc]) -> BTreeSet<String> {
    let successors = successor_map(docs);
    docs.iter()
        .filter(|doc| {
            crate::discover::anchor_policy_for_path(schema, &doc.path)
                == crate::schema::AnchorPolicy::Seal
                && doc_lifecycle(
                    &doc.path,
                    doc.status.as_deref().unwrap_or(""),
                    &doc.summary,
                    successors.get(&doc.path).map(Vec::as_slice).unwrap_or(&[]),
                ) == "current"
        })
        .map(|doc| doc.path.clone())
        .collect()
}

#[derive(Clone, Serialize)]
pub(crate) struct DocRelation {
    pub(crate) src_path: String,
    pub(crate) dst_path: String,
    pub(crate) relation_type: String,
    pub(crate) reason: String,
    pub(crate) explicit: bool,
    pub(crate) confidence: f32,
    #[serde(serialize_with = "crate::render::serialize_evidence")]
    pub(crate) evidence_json: String,
}

#[derive(Default, Serialize)]
pub(crate) struct IndexArtifact {
    pub(crate) repo_root: String,
    pub(crate) managed_root: String,
    pub(crate) generated_at_unix: u64,
    pub(crate) history_latest_snapshot: Option<String>,
    pub(crate) history_dirty: bool,
    pub(crate) history_tracked_files_count: usize,
    pub(crate) host_git_head: Option<String>,
    pub(crate) host_git_branch: Option<String>,
    pub(crate) host_git_dirty: Option<bool>,
    pub(crate) docs: Vec<IndexedDoc>,
    pub(crate) relations: Vec<DocRelation>,
    pub(crate) checks: Vec<IndexCheck>,
    pub(crate) coverage: Coverage,
    pub(crate) hints: Vec<String>,
    pub(crate) external_refs: Vec<crate::external_refs::ExternalRef>,
}

pub(crate) struct EngineContext {
    pub(crate) target_dir: PathBuf,
    pub(crate) schema: Schema,
}

#[derive(Clone, Default)]
pub(crate) struct FileStateRecord {
    pub(crate) content_hash: String,
    pub(crate) file_size_bytes: u64,
    pub(crate) mtime_unix: u64,
    pub(crate) mtime_ns: u64,
}

#[derive(Default)]
pub(crate) struct FactStoreDelta {
    pub(crate) added_paths: Vec<String>,
    pub(crate) changed_paths: Vec<String>,
    pub(crate) removed_paths: Vec<String>,
}

pub(crate) fn build_index_artifact(target_dir: &Path, schema: &Schema) -> Result<IndexArtifact> {
    let repo_root = target_dir
        .canonicalize()
        .unwrap_or_else(|_| target_dir.to_path_buf());
    let mut doc_paths = collect_managed_doc_paths(&repo_root, schema)?;
    doc_paths.sort();

    let mut docs = Vec::new();
    let path_set = doc_paths.iter().cloned().collect::<BTreeSet<_>>();
    let collection_by_path = collection_lookup(schema);
    for rel_path in &doc_paths {
        docs.push(load_indexed_doc(
            &repo_root,
            schema,
            &collection_by_path,
            rel_path,
        )?);
    }

    let mut relations = build_doc_relations(schema, &docs, &path_set);
    annotate_link_counts(&mut docs, &relations);
    let checks = build_index_checks(schema, &repo_root, &docs, &relations)?;
    let history_status = compute_history_status(&repo_root, schema).unwrap_or_default();
    let host_git = host_git_info(&repo_root, HostGitMode::Full);
    relations.sort_by(|a, b| {
        a.src_path
            .cmp(&b.src_path)
            .then_with(|| a.dst_path.cmp(&b.dst_path))
            .then_with(|| a.relation_type.cmp(&b.relation_type))
    });

    let coverage = discover_docs(&repo_root, schema)?;
    Ok(IndexArtifact {
        hints: coverage.hints(),
        coverage,
        external_refs: Vec::new(),
        repo_root: absolute_display_path(&repo_root),
        managed_root: schema.managed_root.clone(),
        generated_at_unix: now_unix(),
        history_latest_snapshot: history_status.latest_snapshot.clone(),
        history_dirty: is_history_dirty(&history_status),
        history_tracked_files_count: history_status.tracked_files_count,
        host_git_head: host_git.head,
        host_git_branch: host_git.branch,
        host_git_dirty: host_git.dirty,
        docs,
        relations,
        checks,
    })
}

pub(crate) fn write_index_artifact(target_dir: &Path, artifact: &IndexArtifact) -> Result<()> {
    let state_dir = target_dir.join(".harnesskit").join("state");
    ensure_writable_fact_store_dir(target_dir, &state_dir, "HarnessKit state")?;
    fs::write(
        state_dir.join("doc-index.json"),
        render_index_json(artifact),
    )
    .map_err(|err| {
        format!(
            "cannot write HarnessKit index artifact: {}\n\n{}",
            err,
            fact_store_path_hint(target_dir, Some(&state_dir), None)
        )
    })?;
    fs::write(
        state_dir.join("doc-index-summary.md"),
        render_index_summary(artifact),
    )
    .map_err(|err| {
        format!(
            "cannot write HarnessKit index summary: {}\n\n{}",
            err,
            fact_store_path_hint(target_dir, Some(&state_dir), None)
        )
    })?;
    Ok(())
}

pub(crate) fn write_fact_store(target_dir: &Path, artifact: &IndexArtifact) -> Result<()> {
    let state_dir = target_dir.join(".harnesskit").join("state");
    ensure_writable_fact_store_dir(target_dir, &state_dir, "HarnessKit fact store")?;
    let db_path = state_dir.join("facts.sqlite");
    let tmp_path = state_dir.join(format!(
        "facts.sqlite.rebuild-{}-{}",
        std::process::id(),
        now_unix()
    ));
    let _ = fs::remove_file(&tmp_path);
    let db = SqliteConnection::open(&tmp_path).map_err(|err| {
        format!(
            "cannot create HarnessKit fact store at {}: {}\n\n{}",
            tmp_path.display(),
            err,
            fact_store_path_hint(target_dir, Some(&state_dir), Some(&db_path))
        )
    })?;
    initialize_fact_store(&db)?;
    replace_fact_store_contents(&db, artifact)?;
    db.exec("PRAGMA wal_checkpoint(TRUNCATE);")?;
    drop(db);
    replace_fact_store_file(&db_path, &tmp_path)?;
    Ok(())
}

fn replace_fact_store_file(db_path: &Path, tmp_path: &Path) -> Result<()> {
    for suffix in ["-wal", "-shm"] {
        let _ = fs::remove_file(PathBuf::from(format!("{}{}", tmp_path.display(), suffix)));
        let _ = fs::remove_file(PathBuf::from(format!("{}{}", db_path.display(), suffix)));
    }
    fs::rename(tmp_path, db_path).map_err(|err| {
        let target_dir = db_path
            .parent()
            .and_then(|state| state.parent())
            .and_then(|harnesskit| harnesskit.parent())
            .unwrap_or_else(|| Path::new("."));
        format!(
            "cannot replace HarnessKit fact store at {}: {}\n\n{}",
            db_path.display(),
            err,
            fact_store_path_hint(target_dir, db_path.parent(), Some(db_path))
        )
    })?;
    Ok(())
}

pub(crate) fn open_fact_store(target_dir: &Path) -> Result<SqliteConnection> {
    let state_dir = target_dir.join(".harnesskit").join("state");
    ensure_writable_fact_store_dir(target_dir, &state_dir, "HarnessKit fact store")?;
    let db_path = state_dir.join("facts.sqlite");
    if !db_path.exists() {
        return Err(format!(
            "fact store not found at {}. Run `harnesskit index` first.\n\n{}",
            db_path.display(),
            fact_store_path_hint(target_dir, Some(&state_dir), Some(&db_path))
        )
        .into());
    }
    SqliteConnection::open(&db_path).map_err(|err| {
        let err_text = err.to_string();
        format!(
            "cannot open HarnessKit fact store at {}: {}. Check that the project path and `.harnesskit/state` are writable.\n\n{}",
            db_path.display(),
            err_text,
            fact_store_path_hint(target_dir, Some(&state_dir), Some(&db_path))
        )
        .into()
    })
}

pub(crate) fn scalar_count(db: &SqliteConnection, sql: &str) -> Result<usize> {
    let result = db.query(sql, [])?;
    Ok(result
        .rows
        .first()
        .map(|row| cell(row, 0).parse::<usize>().unwrap_or(0))
        .unwrap_or(0))
}

pub(crate) fn refresh_fact_store(context: &EngineContext) -> Result<()> {
    refresh_fact_store_with_knowledge(context, true)
}

/// Migrate snapshots before writing any durable knowledge.lock changes.
pub(crate) fn refresh_fact_store_for_migration(context: &EngineContext) -> Result<()> {
    refresh_fact_store_with_knowledge(context, false)
}

fn refresh_fact_store_with_knowledge(context: &EngineContext, persist: bool) -> Result<()> {
    let schema_fingerprint = simple_content_hash(&format!(
        "{}{:?}{}:checks-s7",
        crate::schema::render_schema_copy(&context.schema),
        context.schema.inferred_root,
        context.schema.inferred_layout
    ));
    let database = context.target_dir.join(".harnesskit/state/facts.sqlite");
    if database.exists() {
        let db = open_fact_store(&context.target_dir)?;
        if !fact_store_is_compatible(&db)?
            || meta_value(&db, "schema_fingerprint")?
                .map(|value| value != schema_fingerprint)
                .unwrap_or(false)
        {
            drop(db);
            rebuild_fact_store_full(context)?;
        } else if persist
            && meta_value(&db, "refresh_inputs")?
                .filter(|value| !value.is_empty())
                .as_deref()
                == Some(&inputs::fingerprint(context, &db)?)
        {
            return Ok(());
        }
    }
    refresh_doc_facts(context)?;
    let db = open_fact_store(&context.target_dir)?;
    crate::code::refresh_code_facts(&db, &context.target_dir, &context.schema)?;
    let mut artifact = read_index_artifact_from_fact_store(&db)?;
    let sealed_docs = sealed_doc_paths(&context.schema, &artifact.docs);
    let mut anchor_checks = crate::anchors::refresh_anchor_facts(
        &db,
        &context.target_dir,
        &artifact.docs,
        &sealed_docs,
    )?;
    let knowledge = if persist {
        crate::knowledge::refresh(&context.target_dir, &db, &sealed_docs)?
    } else {
        crate::knowledge::preview(&context.target_dir, &db, &sealed_docs)?
    };
    anchor_checks.retain(|check| {
        if check.rule_id != "anchor-unresolved" {
            return true;
        }
        let evidence: serde_json::Value =
            serde_json::from_str(&check.evidence_json).unwrap_or_default();
        let key = evidence
            .get("doc")
            .and_then(serde_json::Value::as_str)
            .zip(evidence.get("anchor").and_then(serde_json::Value::as_str))
            .map(|(doc, anchor)| (doc.to_string(), anchor.to_string()));
        key.map(|key| !knowledge.locked_unresolved.contains(&key))
            .unwrap_or(true)
    });
    crate::weave::refresh(&db, &context.target_dir, &context.schema)?;
    crate::query::refresh_cards(&db, &context.target_dir)?;
    let coverage = discover_docs(&context.target_dir, &context.schema)?;
    let (external_refs, external_checks) = crate::external_refs::refresh_external_refs(
        &db,
        &context.target_dir,
        &context.schema,
        &artifact.docs,
    )?;
    // Discovery and linked-file existence can change without a managed doc edit.
    let link_states = artifact
        .docs
        .iter()
        .flat_map(|doc| doc.links.iter())
        .map(|path| (path, context.target_dir.join(path).exists()))
        .collect::<Vec<_>>();
    let stale_after = u64::from(context.schema.rules.stale_active_plan_days.unwrap_or(30)) * 86400;
    let now = now_unix();
    let stale_plan_states = artifact
        .docs
        .iter()
        .filter(|doc| doc.role.starts_with("exec_plans"))
        .map(|doc| {
            let age = now.saturating_sub(doc.mtime_unix);
            (&doc.path, age / 86400, age > stale_after)
        })
        .collect::<Vec<_>>();
    let fingerprint = simple_content_hash(&serde_json::to_string(&(
        &coverage,
        &external_refs,
        link_states,
        stale_plan_states,
    ))?);
    // Git commits, ignore rules and scoped code may change without a doc edit.
    let mut checks = build_index_checks(
        &context.schema,
        &context.target_dir,
        &artifact.docs,
        &artifact.relations,
    )?;
    crate::checks::add_undocumented_hotspot_checks(&db, &context.schema, &mut checks)?;
    checks.extend(external_checks);
    checks.extend(anchor_checks);
    checks.extend(knowledge.checks);
    // `check` refreshes the same fact store more than once while assembling
    // its filtered report. Keep the one aggregate cleanup notice visible for
    // the remainder of that derived-state generation.
    if knowledge.pruned_docs.is_empty() {
        checks.extend(
            artifact
                .checks
                .iter()
                .filter(|check| check.rule_id == "knowledge-lock-pruned")
                .cloned(),
        );
    }
    if !knowledge.pruned_docs.is_empty() {
        let docs = knowledge.pruned_docs.iter().cloned().collect::<Vec<_>>();
        let mut check = IndexCheck {
            rule_id: "knowledge-lock-pruned".to_string(),
            severity: "info".to_string(),
            subject_path: ".harnesskit/knowledge.lock".to_string(),
            message: format!(
                "Removed {} mention-only document(s) from knowledge.lock: {}.",
                docs.len(),
                docs.join(", ")
            ),
            hint: Some(
                "Historical, release and completed documents remain searchable through mention edges; no acknowledgement is required."
                    .to_string(),
            ),
            fixable: false,
            evidence_json: serde_json::json!({
                "docs": docs,
                "reason": "document anchor policy changed to mention",
            })
            .to_string(),
        };
        crate::checks::complete_check(&mut check);
        checks.push(check);
    }
    checks.sort_by(|left, right| {
        (&left.subject_path, &left.rule_id).cmp(&(&right.subject_path, &right.rule_id))
    });
    if meta_value(&db, "discovery_fingerprint")?.as_deref() != Some(&fingerprint)
        || artifact.checks != checks
    {
        artifact.checks = checks;
        artifact.coverage = coverage;
        artifact.hints = artifact.coverage.hints();
        for check in artifact
            .checks
            .iter()
            .filter(|check| check.rule_id == "unmanaged-docs")
        {
            if !artifact
                .hints
                .iter()
                .any(|hint| hint.contains(&check.message))
            {
                artifact.hints.push(check.message.clone());
            }
        }
        artifact.hints.push(format!("{} external file reference(s) hashed outside managed_root; these files are not included in history. A changed-reference notice persists until its referring document is updated. Rebuilding derived state resets this hash baseline.", external_refs.len()));
        artifact.external_refs = external_refs;
        db.transaction(|db| {
            db.exec("DELETE FROM checks;")?;
            for check in &artifact.checks {
                insert_check(db, check)?;
            }
            for (key, value) in [
                ("coverage", serde_json::to_string(&artifact.coverage)?),
                ("coverage_hints", serde_json::to_string(&artifact.hints)?),
                ("discovery_fingerprint", fingerprint),
            ] {
                db.execute(
                    "INSERT OR REPLACE INTO meta(key, value) VALUES (?1, ?2);",
                    params![key, value],
                )?;
            }
            Ok(())
        })?;
        write_index_artifact(&context.target_dir, &artifact)?;
    }
    db.execute(
        "INSERT OR REPLACE INTO meta(key, value) VALUES ('schema_fingerprint', ?1);",
        [&schema_fingerprint],
    )?;
    if persist {
        db.execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES ('refresh_inputs', ?1)",
            [inputs::fingerprint(context, &db)?],
        )?;
    }
    Ok(())
}

fn refresh_doc_facts(context: &EngineContext) -> Result<()> {
    let db_path = context
        .target_dir
        .join(".harnesskit")
        .join("state")
        .join("facts.sqlite");
    if !db_path.exists() {
        rebuild_fact_store_full(context)?;
        return Ok(());
    }

    let db = open_fact_store(&context.target_dir)?;
    if !fact_store_is_compatible(&db)? {
        drop(db);
        rebuild_fact_store_full(context)?;
        return Ok(());
    }

    let delta = compute_fact_store_delta(context)?;
    if delta.added_paths.is_empty()
        && delta.changed_paths.is_empty()
        && delta.removed_paths.is_empty()
    {
        if let Some((history, git)) = history_or_host_git_state_changed(context, &db)? {
            refresh_fact_store_metadata(context, &db, "metadata", &history, &git)?;
        }
        return Ok(());
    }

    apply_fact_store_delta(context, &delta)?;
    Ok(())
}

fn history_or_host_git_state_changed(
    context: &EngineContext,
    db: &SqliteConnection,
) -> Result<Option<(HistoryStatus, HostGitInfo)>> {
    let history_status =
        compute_history_status(&context.target_dir, &context.schema).unwrap_or_default();
    let host_git = host_git_info(&context.target_dir, HostGitMode::Full);
    let current = [
        (
            "history_latest_snapshot",
            history_status.latest_snapshot.clone().unwrap_or_default(),
        ),
        (
            "history_dirty",
            if is_history_dirty(&history_status) {
                "true"
            } else {
                "false"
            }
            .to_string(),
        ),
        (
            "history_tracked_files_count",
            history_status.tracked_files_count.to_string(),
        ),
        ("host_git_head", host_git.head.clone().unwrap_or_default()),
        (
            "host_git_branch",
            host_git.branch.clone().unwrap_or_default(),
        ),
        (
            "host_git_dirty",
            git_dirty_label(host_git.dirty).to_string(),
        ),
    ];
    for (key, value) in current {
        if meta_value(db, key)?.unwrap_or_default() != value {
            return Ok(Some((history_status, host_git)));
        }
    }
    Ok(None)
}

fn refresh_fact_store_metadata(
    context: &EngineContext,
    db: &SqliteConnection,
    mode: &str,
    history_status: &HistoryStatus,
    host_git: &HostGitInfo,
) -> Result<()> {
    let artifact = read_index_artifact_from_fact_store(db)?;
    let repo_root = context
        .target_dir
        .canonicalize()
        .unwrap_or_else(|_| context.target_dir.clone());
    let checks = build_index_checks(
        &context.schema,
        &repo_root,
        &artifact.docs,
        &artifact.relations,
    )?;
    replace_checks_and_meta(
        db,
        &absolute_display_path(&repo_root),
        &context.schema.managed_root,
        now_unix(),
        mode,
        &FactStoreDelta::default(),
        history_status,
        host_git,
        &checks,
    )
}

fn rebuild_fact_store_full(context: &EngineContext) -> Result<()> {
    let artifact = build_index_artifact(&context.target_dir, &context.schema)?;
    write_index_artifact(&context.target_dir, &artifact)?;
    write_fact_store(&context.target_dir, &artifact)?;
    Ok(())
}

fn fact_store_is_compatible(db: &SqliteConnection) -> Result<bool> {
    // Derived stores from before test-symbol classification are rebuilt;
    // repository schemas and durable history formats remain unchanged.
    if !db
        .query("PRAGMA table_info(nodes);", [])?
        .rows
        .iter()
        .any(|row| cell(row, 1) == "is_test")
    {
        return Ok(false);
    }
    let required_tables = [
        "meta",
        "docs",
        "file_states",
        "nodes",
        "edges",
        "cochange_stats",
        "cochange_files",
        "inline_code_spans",
        "headings",
        "raw_links",
        "path_mentions",
        "relations",
        "checks",
        "scope_paths",
        "supersedes",
        "superseded_by",
        "supersedes_parts_of",
        "external_refs",
        "docs_fts",
        "docs_trigram",
        "code_cards",
        "code_fts",
        "code_trigram",
        "doc_sections",
        "sections_fts",
        "sections_trigram",
    ];
    for table in required_tables {
        let exists = db.query(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1;",
            [table],
        )?;
        if exists
            .rows
            .first()
            .map(|row| cell(row, 0) != "1")
            .unwrap_or(true)
        {
            return Ok(false);
        }
    }

    let version = meta_value(db, "fact_schema_version")?;
    if version.as_deref() != Some(FACT_SCHEMA_VERSION) {
        return Ok(false);
    }

    let file_state_columns = db.query("PRAGMA table_info(file_states);", [])?;
    let has_mtime_ns = file_state_columns
        .rows
        .iter()
        .any(|row| cell(row, 1) == "mtime_ns");
    let has_kind = file_state_columns
        .rows
        .iter()
        .any(|row| cell(row, 1) == "kind");
    if !has_mtime_ns || !has_kind {
        return Ok(false);
    }

    Ok(true)
}

pub(crate) fn meta_value(db: &SqliteConnection, key: &str) -> Result<Option<String>> {
    let result = db.query("SELECT value FROM meta WHERE key = ?1;", [key])?;
    Ok(result.rows.first().map(|row| cell(row, 0).to_string()))
}

pub(crate) fn compute_fact_store_delta(context: &EngineContext) -> Result<FactStoreDelta> {
    let db = open_fact_store(&context.target_dir)?;
    let rows = db.query("SELECT path, content_hash, file_size_bytes, mtime_unix, mtime_ns FROM file_states WHERE kind = 'doc' ORDER BY path ASC;", [])?;
    let previous_states = parse_file_state_rows(&rows);
    let current_states =
        collect_current_file_states(&context.target_dir, &context.schema, &previous_states)?;

    let mut added_paths = Vec::new();
    let mut changed_paths = Vec::new();
    let mut removed_paths = Vec::new();

    for (path, current) in &current_states {
        match previous_states.get(path) {
            None => added_paths.push(path.clone()),
            Some(previous) => {
                if previous.content_hash != current.content_hash
                    || previous.file_size_bytes != current.file_size_bytes
                    || previous.mtime_unix != current.mtime_unix
                    || previous.mtime_ns != current.mtime_ns
                {
                    changed_paths.push(path.clone());
                }
            }
        }
    }

    for path in previous_states.keys() {
        if !current_states.contains_key(path) {
            removed_paths.push(path.clone());
        }
    }

    added_paths.sort();
    changed_paths.sort();
    removed_paths.sort();

    Ok(FactStoreDelta {
        added_paths,
        changed_paths,
        removed_paths,
    })
}

pub(crate) fn parse_file_state_rows(result: &QueryResult) -> BTreeMap<String, FileStateRecord> {
    let mut states = BTreeMap::new();
    for row in &result.rows {
        let path = cell(row, 0).to_string();
        if path.is_empty() {
            continue;
        }
        states.insert(
            path.clone(),
            FileStateRecord {
                content_hash: cell(row, 1).to_string(),
                file_size_bytes: cell(row, 2).parse::<u64>().unwrap_or(0),
                mtime_unix: cell(row, 3).parse::<u64>().unwrap_or(0),
                mtime_ns: cell(row, 4).parse::<u64>().unwrap_or(0),
            },
        );
    }
    states
}

pub(crate) fn collect_current_file_states(
    target_dir: &Path,
    schema: &Schema,
    previous_states: &BTreeMap<String, FileStateRecord>,
) -> Result<BTreeMap<String, FileStateRecord>> {
    let mut states = BTreeMap::new();
    for rel_path in collect_managed_doc_paths(target_dir, schema)? {
        let abs = target_dir.join(&rel_path);
        let metadata = fs::metadata(&abs)?;
        let (mtime_unix, mtime_ns) = file_modified_parts(&metadata)?;
        if let Some(previous) = previous_states.get(&rel_path) {
            if previous.file_size_bytes == metadata.len() && previous.mtime_ns == mtime_ns {
                states.insert(rel_path.clone(), previous.clone());
                continue;
            }
        }
        let text = read_text_lossy(&abs)?;
        states.insert(
            rel_path.clone(),
            FileStateRecord {
                content_hash: simple_content_hash(&text),
                file_size_bytes: metadata.len(),
                mtime_unix,
                mtime_ns,
            },
        );
    }
    Ok(states)
}

fn apply_fact_store_delta(context: &EngineContext, delta: &FactStoreDelta) -> Result<()> {
    let db = open_fact_store(&context.target_dir)?;
    let mut artifact = read_index_artifact_from_fact_store(&db)?;
    let previous_docs = artifact.docs.clone();
    let changed_set = delta
        .added_paths
        .iter()
        .chain(delta.changed_paths.iter())
        .cloned()
        .collect::<BTreeSet<_>>();
    let repo_root = context
        .target_dir
        .canonicalize()
        .unwrap_or_else(|_| context.target_dir.clone());
    let collection_by_path = collection_lookup(&context.schema);
    let changed_docs = changed_set
        .iter()
        .map(|path| {
            load_indexed_doc(
                &context.target_dir,
                &context.schema,
                &collection_by_path,
                path,
            )
        })
        .collect::<Result<Vec<_>>>()?;

    apply_doc_fact_delta(
        &db,
        &delta
            .added_paths
            .iter()
            .chain(delta.changed_paths.iter())
            .chain(delta.removed_paths.iter())
            .cloned()
            .collect::<Vec<_>>(),
        &changed_docs,
    )?;

    let generated_at_unix = now_unix();
    apply_doc_delta_to_artifact_docs(&mut artifact.docs, &changed_docs, &delta.removed_paths);
    let path_set = artifact
        .docs
        .iter()
        .map(|doc| doc.path.clone())
        .collect::<BTreeSet<_>>();
    let impacted_sources = collect_impacted_relation_sources(
        &db,
        &context.schema,
        delta,
        &changed_docs,
        &previous_docs,
        &path_set,
    )?;
    let impacted_docs = artifact
        .docs
        .iter()
        .filter(|doc| impacted_sources.contains(&doc.path))
        .cloned()
        .collect::<Vec<_>>();
    let rebuilt_relations = build_doc_relations(&context.schema, &impacted_docs, &path_set);
    replace_relations_for_sources(&db, &impacted_sources, &rebuilt_relations)?;

    artifact = read_index_artifact_from_fact_store(&db)?;
    artifact.repo_root = absolute_display_path(&repo_root);
    artifact.managed_root = context.schema.managed_root.clone();
    artifact.generated_at_unix = generated_at_unix;
    let history_status = compute_history_status(&repo_root, &context.schema).unwrap_or_default();
    let host_git = host_git_info(&repo_root, HostGitMode::Full);
    artifact.history_latest_snapshot = history_status.latest_snapshot.clone();
    artifact.history_dirty = is_history_dirty(&history_status);
    artifact.history_tracked_files_count = history_status.tracked_files_count;
    artifact.host_git_head = host_git.head.clone();
    artifact.host_git_branch = host_git.branch.clone();
    artifact.host_git_dirty = host_git.dirty;
    let mut relations = artifact.relations.clone();
    relations.sort_by(|a, b| {
        a.src_path
            .cmp(&b.src_path)
            .then_with(|| a.dst_path.cmp(&b.dst_path))
            .then_with(|| a.relation_type.cmp(&b.relation_type))
    });
    annotate_link_counts(&mut artifact.docs, &relations);
    update_doc_link_counts_in_db(&db, &artifact.docs)?;
    let checks = build_index_checks(&context.schema, &repo_root, &artifact.docs, &relations)?;
    replace_checks_and_meta(
        &db,
        &artifact.repo_root,
        &artifact.managed_root,
        generated_at_unix,
        "delta",
        delta,
        &history_status,
        &host_git,
        &checks,
    )?;
    artifact.coverage = discover_docs(&repo_root, &context.schema)?;
    artifact.hints = artifact.coverage.hints();
    artifact.relations = relations;
    artifact.checks = checks;
    write_index_artifact(&context.target_dir, &artifact)?;
    Ok(())
}

fn apply_doc_delta_to_artifact_docs(
    docs: &mut Vec<IndexedDoc>,
    changed_docs: &[IndexedDoc],
    removed_paths: &[String],
) {
    let removed = removed_paths.iter().cloned().collect::<BTreeSet<_>>();
    let mut changed_by_path = changed_docs
        .iter()
        .map(|doc| (doc.path.clone(), doc.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut next = Vec::with_capacity(docs.len() + changed_docs.len());
    for doc in docs.drain(..) {
        if removed.contains(&doc.path) {
            continue;
        }
        if let Some(changed) = changed_by_path.remove(&doc.path) {
            next.push(changed);
        } else {
            next.push(doc);
        }
    }
    next.extend(changed_by_path.into_values());
    next.sort_by(|a, b| a.path.cmp(&b.path));
    *docs = next;
}

fn initialize_fact_store(db: &SqliteConnection) -> Result<()> {
    db.exec(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         CREATE TABLE IF NOT EXISTS meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS docs (
            path TEXT PRIMARY KEY,
            title TEXT NOT NULL,
            role TEXT NOT NULL,
            status TEXT,
            authority TEXT,
            collection_name TEXT,
            summary TEXT NOT NULL,
            frontmatter_keys_text TEXT NOT NULL DEFAULT '',
            headings_text TEXT NOT NULL,
            body_tokens TEXT NOT NULL,
            content_hash TEXT NOT NULL,
            file_size_bytes INTEGER NOT NULL,
            incoming_links INTEGER NOT NULL,
            outgoing_links INTEGER NOT NULL,
            is_entrypoint INTEGER NOT NULL,
            is_anchor_candidate INTEGER NOT NULL,
            is_generated INTEGER NOT NULL,
            is_reference INTEGER NOT NULL,
            mtime_unix INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS file_states (
            path TEXT PRIMARY KEY,
            kind TEXT NOT NULL DEFAULT 'doc',
            content_hash TEXT NOT NULL,
            file_size_bytes INTEGER NOT NULL,
            mtime_unix INTEGER NOT NULL,
            mtime_ns INTEGER NOT NULL DEFAULT 0
         );
         CREATE TABLE IF NOT EXISTS nodes (
            id TEXT PRIMARY KEY,
            kind TEXT NOT NULL,
            path TEXT NOT NULL,
            name TEXT NOT NULL,
            span_start INTEGER NOT NULL,
            span_end INTEGER NOT NULL,
            fingerprint TEXT NOT NULL,
            lang TEXT NOT NULL,
            is_definition INTEGER NOT NULL DEFAULT 0,
            is_test INTEGER NOT NULL DEFAULT 0,
            signature TEXT NOT NULL DEFAULT '',
            doc_comment TEXT NOT NULL DEFAULT ''
         ) WITHOUT ROWID;
         CREATE TABLE IF NOT EXISTS edges (
            src TEXT NOT NULL,
            dst TEXT NOT NULL,
            kind TEXT NOT NULL,
            confidence TEXT NOT NULL,
            weight REAL NOT NULL,
            evidence_json TEXT NOT NULL,
            PRIMARY KEY (src, dst, kind, evidence_json)
         ) WITHOUT ROWID;
         CREATE TABLE IF NOT EXISTS cochange_stats (
            doc_path TEXT NOT NULL,
            code_path TEXT NOT NULL,
            weight REAL NOT NULL,
            decayed_count REAL NOT NULL,
            cochange_count INTEGER NOT NULL,
            last_commit TEXT NOT NULL,
            last_timestamp INTEGER NOT NULL,
            PRIMARY KEY (doc_path, code_path)
         ) WITHOUT ROWID;
         CREATE TABLE IF NOT EXISTS cochange_files (
            path TEXT PRIMARY KEY,
            commit_count INTEGER NOT NULL
         ) WITHOUT ROWID;
         CREATE TABLE IF NOT EXISTS inline_code_spans (
            doc_path TEXT NOT NULL,
            span_index INTEGER NOT NULL,
            src TEXT NOT NULL,
            section TEXT NOT NULL,
            line INTEGER NOT NULL,
            column_number INTEGER NOT NULL,
            text TEXT NOT NULL,
            prose INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (doc_path, span_index)
         ) WITHOUT ROWID;
         CREATE TABLE IF NOT EXISTS headings (
            doc_path TEXT NOT NULL,
            heading_index INTEGER NOT NULL,
            heading_text TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS raw_links (
            src_doc TEXT NOT NULL,
            link_index INTEGER NOT NULL,
            normalized_target TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS path_mentions (
            doc_path TEXT NOT NULL,
            mention_index INTEGER NOT NULL,
            mentioned_path TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS relations (
            src_path TEXT NOT NULL,
            dst_path TEXT NOT NULL,
            relation_type TEXT NOT NULL,
            reason TEXT NOT NULL,
            explicit INTEGER NOT NULL,
            confidence REAL NOT NULL,
            evidence_json TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS checks (
            rule_id TEXT NOT NULL,
            severity TEXT NOT NULL,
            subject_path TEXT NOT NULL,
            message TEXT NOT NULL,
            evidence_json TEXT NOT NULL DEFAULT '{}'
         );
         CREATE TABLE IF NOT EXISTS external_refs (
            source_path TEXT NOT NULL,
            target_path TEXT NOT NULL,
            source_hash TEXT NOT NULL,
            content_hash TEXT NOT NULL,
            observed_hash TEXT NOT NULL,
            PRIMARY KEY (source_path, target_path)
         );
         CREATE TABLE IF NOT EXISTS scope_paths (
            doc_path TEXT NOT NULL,
            scope_path TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS supersedes (
            doc_path TEXT NOT NULL,
            superseded_path TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS superseded_by (
            doc_path TEXT NOT NULL,
            successor_path TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS supersedes_parts_of (
            doc_path TEXT NOT NULL,
            superseded_path TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_docs_role ON docs(role);
         CREATE INDEX IF NOT EXISTS idx_docs_collection ON docs(collection_name);
         CREATE INDEX IF NOT EXISTS idx_docs_status ON docs(status);
         CREATE INDEX IF NOT EXISTS idx_docs_mtime ON docs(mtime_unix);
         CREATE INDEX IF NOT EXISTS idx_nodes_path ON nodes(path, kind);
         CREATE INDEX IF NOT EXISTS idx_edges_dst ON edges(dst, kind);
         CREATE INDEX IF NOT EXISTS idx_cochange_stats_doc ON cochange_stats(doc_path, weight DESC);
         CREATE INDEX IF NOT EXISTS idx_relations_src ON relations(src_path, relation_type);
         CREATE INDEX IF NOT EXISTS idx_relations_dst ON relations(dst_path, relation_type);
         CREATE INDEX IF NOT EXISTS idx_checks_subject ON checks(subject_path, severity);
         CREATE VIRTUAL TABLE IF NOT EXISTS docs_fts USING fts5(
            path,
            title,
            headings_text,
            summary,
            body_tokens,
            tokenize = 'unicode61',
            content = 'docs',
            content_rowid = 'rowid'
         );
         CREATE VIRTUAL TABLE IF NOT EXISTS docs_trigram USING fts5(
            path,
            title,
            headings_text,
            summary,
            body_tokens,
            tokenize = 'trigram',
            content = 'docs',
            content_rowid = 'rowid'
         );
         CREATE TABLE IF NOT EXISTS code_cards (
            node_id TEXT NOT NULL UNIQUE,
            name TEXT NOT NULL,
            split_name TEXT NOT NULL,
            path TEXT NOT NULL,
            signature TEXT NOT NULL,
            doc_comment TEXT NOT NULL,
            bridged TEXT NOT NULL
         );
         CREATE VIRTUAL TABLE IF NOT EXISTS code_fts USING fts5(
            node_id UNINDEXED, name, split_name, path, signature, doc_comment, bridged,
            tokenize = 'unicode61', content = 'code_cards', content_rowid = 'rowid'
         );
         CREATE VIRTUAL TABLE IF NOT EXISTS code_trigram USING fts5(
            node_id UNINDEXED, name, split_name, path, signature, doc_comment, bridged,
            tokenize = 'trigram', content = 'code_cards', content_rowid = 'rowid'
         );
         CREATE TABLE IF NOT EXISTS doc_sections (
            node_id TEXT NOT NULL UNIQUE,
            path TEXT NOT NULL,
            title TEXT NOT NULL,
            heading TEXT NOT NULL,
            summary TEXT NOT NULL,
            body TEXT NOT NULL
         );
         CREATE VIRTUAL TABLE IF NOT EXISTS sections_fts USING fts5(
            node_id UNINDEXED, path, title, heading, summary, body,
            tokenize = 'unicode61', content = 'doc_sections', content_rowid = 'rowid'
         );
         CREATE VIRTUAL TABLE IF NOT EXISTS sections_trigram USING fts5(
            node_id UNINDEXED, path, title, heading, summary, body,
            tokenize = 'trigram', content = 'doc_sections', content_rowid = 'rowid'
         );",
    )?;
    Ok(())
}

fn load_indexed_doc(
    target_dir: &Path,
    schema: &Schema,
    collection_by_path: &BTreeMap<String, &DocCollectionSpec>,
    rel_path: &str,
) -> Result<IndexedDoc> {
    let abs = target_dir.join(rel_path);
    let text = read_text_lossy(&abs)?;
    let parsed = parse_doc_text(rel_path, &text);
    let metadata = fs::metadata(&abs)?;
    let (mtime_unix, mtime_ns) = file_modified_parts(&metadata)?;
    let collection = collection_name_for_path(schema, rel_path);
    let authority = parsed.frontmatter.get("authority").cloned().or_else(|| {
        collection.as_ref().and_then(|name| {
            collection_by_path
                .get(name)
                .and_then(|spec| spec.authority.clone())
                .or_else(|| default_authority(name))
        })
    });
    let default_status = schema
        .core_files
        .iter()
        .find(|spec| core_file_rel_path(schema, spec) == rel_path)
        .and_then(|spec| spec.status.clone())
        .or_else(|| {
            collection.as_ref().and_then(|name| {
                collection_by_path
                    .get(name)
                    .and_then(|spec| spec.status.clone())
            })
        });
    let role = infer_index_role(schema, rel_path, collection.as_deref());
    let default_status = default_status.as_deref().or_else(|| {
        if role.starts_with("exec_plans") || role.starts_with("worklog") {
            Some("active")
        } else {
            None
        }
    });
    let status = infer_status(
        parsed.frontmatter.get("status").map(String::as_str),
        rel_path,
        &parsed.summary,
        default_status,
    );
    let is_generated = authority.as_deref() == Some("generated");
    let is_reference = authority.as_deref() == Some("reference");

    Ok(IndexedDoc {
        path: rel_path.to_string(),
        title: parsed.title.clone().unwrap_or_else(|| rel_path.to_string()),
        summary: parsed.summary.clone(),
        frontmatter_keys: parsed.frontmatter.keys().cloned().collect(),
        content_text: parsed.body_text.clone(),
        role,
        status,
        authority,
        collection,
        headings: parsed.headings.clone(),
        links: parsed.links.clone(),
        path_mentions: parsed.path_mentions.clone(),
        scope_paths: parsed.scope_paths.clone(),
        supersedes: parsed.supersedes.clone(),
        superseded_by: parsed.superseded_by.clone(),
        supersedes_parts_of: parsed.supersedes_parts_of.clone(),
        incoming_links: 0,
        outgoing_links: 0,
        is_entrypoint: is_entrypoint_path(schema, rel_path),
        is_anchor_candidate: is_anchor_candidate(schema, rel_path),
        is_generated,
        is_reference,
        content_hash: simple_content_hash(&text),
        file_size_bytes: metadata.len(),
        mtime_unix,
        mtime_ns,
    })
}

fn apply_doc_fact_delta(
    db: &SqliteConnection,
    touched_paths: &[String],
    changed_docs: &[IndexedDoc],
) -> Result<()> {
    db.transaction(|db| {
        for path in touched_paths {
            // External-content FTS deletes need the old text before it is removed.
            for table in ["docs_fts", "docs_trigram"] {
                db.execute(
                    &format!("INSERT INTO {table}({table}, rowid, path, title, headings_text, summary, body_tokens)
                     SELECT 'delete', rowid, path, title, headings_text, summary, body_tokens
                     FROM docs WHERE path = ?1;"),
                    [path],
                )?;
            }
            for sql in [
                "DELETE FROM docs WHERE path = ?1;",
                "DELETE FROM file_states WHERE path = ?1;",
                "DELETE FROM headings WHERE doc_path = ?1;",
                "DELETE FROM raw_links WHERE src_doc = ?1;",
                "DELETE FROM path_mentions WHERE doc_path = ?1;",
                "DELETE FROM scope_paths WHERE doc_path = ?1;",
                "DELETE FROM supersedes WHERE doc_path = ?1;",
                "DELETE FROM superseded_by WHERE doc_path = ?1;",
                "DELETE FROM supersedes_parts_of WHERE doc_path = ?1;",
            ] {
                db.execute(sql, [path])?;
            }
        }
        for doc in changed_docs {
            insert_doc_facts(db, doc)?;
        }
        Ok(())
    })
}

fn collect_impacted_relation_sources(
    db: &SqliteConnection,
    schema: &Schema,
    delta: &FactStoreDelta,
    changed_docs: &[IndexedDoc],
    previous_docs: &[IndexedDoc],
    path_set: &BTreeSet<String>,
) -> Result<BTreeSet<String>> {
    // Deleted documents still own relations that must be removed from the graph.
    let mut impacted = delta.removed_paths.iter().cloned().collect::<BTreeSet<_>>();
    let touched_paths = delta
        .added_paths
        .iter()
        .chain(delta.changed_paths.iter())
        .chain(delta.removed_paths.iter())
        .cloned()
        .collect::<BTreeSet<_>>();
    let removed_paths = delta.removed_paths.iter().cloned().collect::<BTreeSet<_>>();

    for doc in changed_docs {
        impacted.insert(doc.path.clone());
        if let Some(index_path) = collection_index_for_doc(schema, doc) {
            impacted.insert(index_path);
        }
    }

    for doc in previous_docs {
        if removed_paths.contains(&doc.path) {
            if let Some(index_path) = collection_index_for_doc(schema, doc) {
                impacted.insert(index_path);
            }
            continue;
        }
        let depends_on_touched = doc.links.iter().any(|link| touched_paths.contains(link))
            || doc
                .path_mentions
                .iter()
                .any(|path| touched_paths.contains(path))
            || doc
                .scope_paths
                .iter()
                .any(|path| touched_paths.contains(path))
            || doc
                .supersedes
                .iter()
                .chain(doc.superseded_by.iter())
                .chain(doc.supersedes_parts_of.iter())
                .any(|path| touched_paths.contains(path));
        if depends_on_touched {
            impacted.insert(doc.path.clone());
        }
    }

    for removed in &removed_paths {
        let rows = db.query(
            "SELECT DISTINCT src_path FROM relations WHERE dst_path = ?1 OR src_path = ?1;",
            [removed],
        )?;
        for row in &rows.rows {
            let path = cell(row, 0).to_string();
            if !path.is_empty() && path_set.contains(&path) {
                impacted.insert(path);
            }
        }
    }

    let manifest_path = schema.entrypoints.manifest.as_deref().unwrap_or("");
    if !manifest_path.is_empty() {
        impacted.insert(manifest_path.to_string());
    }
    if let Some(path) = &schema.entrypoints.agents {
        impacted.insert(path.clone());
    }
    if let Some(path) = &schema.entrypoints.claude {
        impacted.insert(path.clone());
    }
    if let Some(path) = &schema.entrypoints.architecture {
        impacted.insert(path.clone());
    }

    Ok(impacted)
}

fn collection_index_for_doc(schema: &Schema, doc: &IndexedDoc) -> Option<String> {
    let collection = doc.collection.as_deref()?;
    let spec = schema
        .doc_collections
        .iter()
        .find(|item| item.name == collection)?;
    let prefix = format!(
        "{}/",
        crate::fs_utils::normalize_path(&Path::new(&schema.managed_root).join(&spec.path))
    );
    doc.path
        .starts_with(&prefix)
        .then(|| format!("{}index.md", prefix))
}

fn replace_relations_for_sources(
    db: &SqliteConnection,
    source_paths: &BTreeSet<String>,
    relations: &[DocRelation],
) -> Result<()> {
    db.transaction(|db| {
        for path in source_paths {
            db.execute("DELETE FROM relations WHERE src_path = ?1;", [path])?;
            db.execute(
                "DELETE FROM relations WHERE dst_path = ?1 AND relation_type = 'code_mentions_doc';",
                [path],
            )?;
        }
        for relation in relations {
            insert_relation(db, relation)?;
        }
        Ok(())
    })
}

fn update_doc_link_counts_in_db(db: &SqliteConnection, docs: &[IndexedDoc]) -> Result<()> {
    db.transaction(|db| {
        for doc in docs {
            db.execute(
                "UPDATE docs SET incoming_links = ?1, outgoing_links = ?2 WHERE path = ?3;",
                params![doc.incoming_links, doc.outgoing_links, doc.path],
            )?;
        }
        Ok(())
    })
}

#[allow(clippy::too_many_arguments)]
fn replace_checks_and_meta(
    db: &SqliteConnection,
    repo_root: &str,
    managed_root: &str,
    generated_at_unix: u64,
    mode: &str,
    delta: &FactStoreDelta,
    history_status: &HistoryStatus,
    host_git: &HostGitInfo,
    checks: &[IndexCheck],
) -> Result<()> {
    // W4 keeps its Git cursor and pair statistics across document metadata
    // refreshes.  This function rebuilds the generic meta table on every
    // delta, so carry those derived cursor values forward explicitly.
    let mut preserved_weave_meta = Vec::new();
    for key in [
        "cochange_last_commit",
        "cochange_head",
        "cochange_indexed_at",
        "cochange_scan_mode",
        "cochange_scope",
        "cochange_accepted_commits",
        "card_inputs",
        "anchor_inputs",
        "anchor_checks",
    ] {
        if let Some(value) = meta_value(db, key)? {
            preserved_weave_meta.push((key, value));
        }
    }
    db.transaction(|db| {
        db.exec("DELETE FROM checks; DELETE FROM meta;")?;
        insert_meta(
            db,
            &[
                ("repo_root", repo_root.to_string()),
                ("managed_root", managed_root.to_string()),
                ("generated_at_unix", generated_at_unix.to_string()),
                ("fact_schema_version", FACT_SCHEMA_VERSION.to_string()),
                ("last_refresh_mode", mode.to_string()),
                ("last_delta_added", delta.added_paths.len().to_string()),
                ("last_delta_changed", delta.changed_paths.len().to_string()),
                ("last_delta_removed", delta.removed_paths.len().to_string()),
                (
                    "history_latest_snapshot",
                    history_status.latest_snapshot.clone().unwrap_or_default(),
                ),
                (
                    "history_dirty",
                    is_history_dirty(history_status).to_string(),
                ),
                (
                    "history_tracked_files_count",
                    history_status.tracked_files_count.to_string(),
                ),
                ("host_git_head", host_git.head.clone().unwrap_or_default()),
                (
                    "host_git_branch",
                    host_git.branch.clone().unwrap_or_default(),
                ),
                (
                    "host_git_dirty",
                    git_dirty_label(host_git.dirty).to_string(),
                ),
            ],
        )?;
        for (key, value) in &preserved_weave_meta {
            db.execute(
                "INSERT INTO meta(key, value) VALUES (?1, ?2);",
                params![key, value],
            )?;
        }
        for check in checks {
            insert_check(db, check)?;
        }
        Ok(())
    })
}

fn insert_meta(db: &SqliteConnection, entries: &[(&str, String)]) -> Result<()> {
    for (key, value) in entries {
        db.execute(
            "INSERT INTO meta(key, value) VALUES (?1, ?2);",
            params![key, value],
        )?;
    }
    Ok(())
}

fn insert_doc_facts(db: &SqliteConnection, doc: &IndexedDoc) -> Result<()> {
    db.execute(
        "INSERT INTO docs(
            path, title, role, status, authority, collection_name, summary, frontmatter_keys_text, headings_text, content_hash, file_size_bytes,
            incoming_links, outgoing_links, is_entrypoint, is_anchor_candidate, is_generated, is_reference, mtime_unix, body_tokens
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19);",
        params![
            doc.path, doc.title, doc.role, doc.status, doc.authority, doc.collection,
            doc.summary, doc.frontmatter_keys.join(" | "), doc.headings.join(" | "),
            doc.content_hash, doc.file_size_bytes, doc.incoming_links, doc.outgoing_links,
            doc.is_entrypoint, doc.is_anchor_candidate, doc.is_generated, doc.is_reference,
            doc.mtime_unix, doc.content_text,
        ],
    )?;
    db.execute(
        "INSERT INTO file_states(path, content_hash, file_size_bytes, mtime_unix, mtime_ns) VALUES (?1, ?2, ?3, ?4, ?5);",
        params![doc.path, doc.content_hash, doc.file_size_bytes, doc.mtime_unix, doc.mtime_ns],
    )?;
    // Both FTS indexes share the docs row; neither stores another copy of the text.
    for table in ["docs_fts", "docs_trigram"] {
        db.execute(
            &format!("INSERT INTO {table}(rowid, path, title, headings_text, summary, body_tokens)
             SELECT rowid, path, title, headings_text, summary, body_tokens FROM docs WHERE path = ?1;"),
            [&doc.path],
        )?;
    }
    for (sql, values) in [
        ("INSERT INTO headings(doc_path, heading_index, heading_text) VALUES (?1, ?2, ?3);", &doc.headings),
        ("INSERT INTO raw_links(src_doc, link_index, normalized_target) VALUES (?1, ?2, ?3);", &doc.links),
        ("INSERT INTO path_mentions(doc_path, mention_index, mentioned_path) VALUES (?1, ?2, ?3);", &doc.path_mentions),
    ] {
        for (index, value) in values.iter().enumerate() {
            db.execute(sql, params![doc.path, index, value])?;
        }
    }
    for (sql, values) in [
        (
            "INSERT INTO scope_paths(doc_path, scope_path) VALUES (?1, ?2);",
            &doc.scope_paths,
        ),
        (
            "INSERT INTO supersedes(doc_path, superseded_path) VALUES (?1, ?2);",
            &doc.supersedes,
        ),
        (
            "INSERT INTO superseded_by(doc_path, successor_path) VALUES (?1, ?2);",
            &doc.superseded_by,
        ),
        (
            "INSERT INTO supersedes_parts_of(doc_path, superseded_path) VALUES (?1, ?2);",
            &doc.supersedes_parts_of,
        ),
    ] {
        for value in values {
            db.execute(sql, params![doc.path, value])?;
        }
    }
    Ok(())
}

fn insert_relation(db: &SqliteConnection, relation: &DocRelation) -> Result<()> {
    db.execute(
        "INSERT INTO relations(src_path, dst_path, relation_type, reason, explicit, confidence, evidence_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7);",
        params![
            relation.src_path, relation.dst_path, relation.relation_type, relation.reason,
            relation.explicit,
            // Preserve the decimal f32 value stored by the previous SQL writer.
            relation.confidence.to_string(), relation.evidence_json,
        ],
    )
}

fn insert_check(db: &SqliteConnection, check: &IndexCheck) -> Result<()> {
    db.execute(
        "INSERT INTO checks(rule_id, severity, subject_path, message, evidence_json) VALUES (?1, ?2, ?3, ?4, ?5);",
        params![check.rule_id, check.severity, check.subject_path, check.message, check.evidence_json],
    )
}

fn replace_fact_store_contents(db: &SqliteConnection, artifact: &IndexArtifact) -> Result<()> {
    db.transaction(|db| {
        db.exec(
            "INSERT INTO docs_fts(docs_fts) VALUES ('delete-all');
             INSERT INTO docs_trigram(docs_trigram) VALUES ('delete-all');
             INSERT INTO code_fts(code_fts) VALUES ('delete-all');
             INSERT INTO code_trigram(code_trigram) VALUES ('delete-all');
             INSERT INTO sections_fts(sections_fts) VALUES ('delete-all');
             INSERT INTO sections_trigram(sections_trigram) VALUES ('delete-all');
             DELETE FROM code_cards;
             DELETE FROM doc_sections;
             DELETE FROM meta;
             DELETE FROM docs;
             DELETE FROM file_states;
             DELETE FROM nodes;
             DELETE FROM edges;
             DELETE FROM cochange_stats;
             DELETE FROM cochange_files;
             DELETE FROM inline_code_spans;
             DELETE FROM headings;
             DELETE FROM raw_links;
             DELETE FROM path_mentions;
             DELETE FROM relations;
             DELETE FROM checks;
             DELETE FROM scope_paths;
             DELETE FROM supersedes;
             DELETE FROM superseded_by;
             DELETE FROM supersedes_parts_of;",
        )?;
        insert_meta(
            db,
            &[
                ("repo_root", artifact.repo_root.clone()),
                ("managed_root", artifact.managed_root.clone()),
                ("generated_at_unix", artifact.generated_at_unix.to_string()),
                ("fact_schema_version", FACT_SCHEMA_VERSION.to_string()),
                ("last_refresh_mode", "full".to_string()),
                ("last_delta_added", "0".to_string()),
                ("last_delta_changed", "0".to_string()),
                ("last_delta_removed", "0".to_string()),
                (
                    "history_latest_snapshot",
                    artifact.history_latest_snapshot.clone().unwrap_or_default(),
                ),
                ("history_dirty", artifact.history_dirty.to_string()),
                (
                    "history_tracked_files_count",
                    artifact.history_tracked_files_count.to_string(),
                ),
                (
                    "host_git_head",
                    artifact.host_git_head.clone().unwrap_or_default(),
                ),
                (
                    "host_git_branch",
                    artifact.host_git_branch.clone().unwrap_or_default(),
                ),
                (
                    "host_git_dirty",
                    git_dirty_label(artifact.host_git_dirty).to_string(),
                ),
            ],
        )?;
        for doc in &artifact.docs {
            insert_doc_facts(db, doc)?;
        }
        for relation in &artifact.relations {
            insert_relation(db, relation)?;
        }
        for check in &artifact.checks {
            insert_check(db, check)?;
        }
        Ok(())
    })
}

pub(crate) fn read_index_artifact_from_fact_store(db: &SqliteConnection) -> Result<IndexArtifact> {
    let meta = db.query("SELECT key, value FROM meta ORDER BY key ASC;", [])?;
    let docs_rows = db.query(
        "SELECT path, title, role, COALESCE(status, ''), COALESCE(authority, ''), COALESCE(collection_name, ''),
                summary, frontmatter_keys_text, headings_text, content_hash, file_size_bytes, incoming_links, outgoing_links,
                is_entrypoint, is_anchor_candidate, is_generated, is_reference, mtime_unix, body_tokens
         FROM docs
         ORDER BY path ASC;", [])?;
    let headings_rows = db.query(
        "SELECT doc_path, heading_text FROM headings ORDER BY doc_path ASC, heading_index ASC;",
        [],
    )?;
    let links_rows = db.query(
        "SELECT src_doc, normalized_target FROM raw_links ORDER BY src_doc ASC, link_index ASC;",
        [],
    )?;
    let path_mentions_rows = db.query(
        "SELECT doc_path, mentioned_path FROM path_mentions ORDER BY doc_path ASC, mention_index ASC;", [])?;
    let file_state_rows = db.query(
        "SELECT path, mtime_ns FROM file_states ORDER BY path ASC;",
        [],
    )?;
    let scope_rows = db.query(
        "SELECT doc_path, scope_path FROM scope_paths ORDER BY doc_path ASC, scope_path ASC;",
        [],
    )?;
    let supersedes_rows = db.query(
        "SELECT doc_path, superseded_path FROM supersedes ORDER BY doc_path ASC, superseded_path ASC;", [])?;
    let superseded_by_rows = db.query(
        "SELECT doc_path, successor_path FROM superseded_by ORDER BY doc_path, successor_path;",
        [],
    )?;
    let partial_rows = db.query(
        "SELECT doc_path, superseded_path FROM supersedes_parts_of ORDER BY doc_path, superseded_path;", [])?;
    let relations_rows = db.query(
        "SELECT src_path, dst_path, relation_type, reason, explicit, confidence, evidence_json
         FROM relations
         ORDER BY src_path ASC, dst_path ASC, relation_type ASC;",
        [],
    )?;
    let checks_rows = db.query(
        "SELECT rule_id, severity, subject_path, message, evidence_json FROM checks ORDER BY subject_path ASC, rule_id ASC;", [])?;

    let mut meta_map = BTreeMap::new();
    for row in &meta.rows {
        meta_map.insert(cell(row, 0).to_string(), cell(row, 1).to_string());
    }

    let headings_by_doc = group_second_column(&headings_rows);
    let links_by_doc = group_second_column(&links_rows);
    let path_mentions_by_doc = group_second_column(&path_mentions_rows);
    let scope_by_doc = group_second_column(&scope_rows);
    let supersedes_by_doc = group_second_column(&supersedes_rows);
    let successors_by_doc = group_second_column(&superseded_by_rows);
    let partial_by_doc = group_second_column(&partial_rows);
    let mut mtime_ns_by_path = BTreeMap::new();
    for row in &file_state_rows.rows {
        mtime_ns_by_path.insert(
            cell(row, 0).to_string(),
            cell(row, 1).parse::<u64>().unwrap_or(0),
        );
    }

    let docs = docs_rows
        .rows
        .iter()
        .map(|row| IndexedDoc {
            path: cell(row, 0).to_string(),
            title: cell(row, 1).to_string(),
            summary: cell(row, 6).to_string(),
            frontmatter_keys: split_pipe_list(cell(row, 7)),
            content_text: cell(row, 18).to_string(),
            role: cell(row, 2).to_string(),
            status: empty_as_none(cell(row, 3)),
            authority: empty_as_none(cell(row, 4)),
            collection: empty_as_none(cell(row, 5)),
            headings: headings_by_doc
                .get(cell(row, 0))
                .cloned()
                .unwrap_or_default(),
            links: links_by_doc.get(cell(row, 0)).cloned().unwrap_or_default(),
            path_mentions: path_mentions_by_doc
                .get(cell(row, 0))
                .cloned()
                .unwrap_or_default(),
            scope_paths: scope_by_doc.get(cell(row, 0)).cloned().unwrap_or_default(),
            supersedes: supersedes_by_doc
                .get(cell(row, 0))
                .cloned()
                .unwrap_or_default(),
            superseded_by: successors_by_doc
                .get(cell(row, 0))
                .cloned()
                .unwrap_or_default(),
            supersedes_parts_of: partial_by_doc
                .get(cell(row, 0))
                .cloned()
                .unwrap_or_default(),
            incoming_links: cell(row, 11).parse::<usize>().unwrap_or(0),
            outgoing_links: cell(row, 12).parse::<usize>().unwrap_or(0),
            is_entrypoint: cell(row, 13) == "1",
            is_anchor_candidate: cell(row, 14) == "1",
            is_generated: cell(row, 15) == "1",
            is_reference: cell(row, 16) == "1",
            content_hash: cell(row, 9).to_string(),
            file_size_bytes: cell(row, 10).parse::<u64>().unwrap_or(0),
            mtime_unix: cell(row, 17).parse::<u64>().unwrap_or(0),
            mtime_ns: mtime_ns_by_path
                .get(cell(row, 0))
                .cloned()
                .unwrap_or_else(|| {
                    cell(row, 17)
                        .parse::<u64>()
                        .unwrap_or(0)
                        .saturating_mul(1_000_000_000)
                }),
        })
        .collect::<Vec<_>>();

    let relations = relations_rows
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
        .collect::<Vec<_>>();

    let checks = checks_rows
        .rows
        .iter()
        .map(|row| IndexCheck {
            rule_id: cell(row, 0).to_string(),
            severity: cell(row, 1).to_string(),
            subject_path: cell(row, 2).to_string(),
            message: cell(row, 3).to_string(),
            hint: crate::checks::hint_from_evidence(cell(row, 4)),
            fixable: false,
            evidence_json: cell(row, 4).to_string(),
        })
        .collect::<Vec<_>>();

    Ok(IndexArtifact {
        coverage: meta_map.get("coverage").and_then(|text| serde_json::from_str(text).ok()).unwrap_or_default(),
        hints: meta_map.get("coverage_hints").and_then(|text| serde_json::from_str(text).ok()).unwrap_or_default(),
        external_refs: db.query("SELECT source_path, target_path, content_hash, observed_hash FROM external_refs ORDER BY source_path, target_path;", [])?.rows.iter().map(|row| crate::external_refs::ExternalRef {
            source_path: cell(row, 0).to_string(), target_path: cell(row, 1).to_string(),
            content_hash: cell(row, 2).to_string(), observed_hash: cell(row, 3).to_string(),
        }).collect(),
        repo_root: meta_map.get("repo_root").cloned().unwrap_or_default(),
        managed_root: meta_map.get("managed_root").cloned().unwrap_or_default(),
        generated_at_unix: meta_map
            .get("generated_at_unix")
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0),
        history_latest_snapshot: meta_map
            .get("history_latest_snapshot")
            .cloned()
            .filter(|value| !value.is_empty()),
        history_dirty: meta_map
            .get("history_dirty")
            .map(|value| value == "true")
            .unwrap_or(false),
        history_tracked_files_count: meta_map
            .get("history_tracked_files_count")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(0),
        host_git_head: meta_map
            .get("host_git_head")
            .cloned()
            .filter(|value| !value.is_empty()),
        host_git_branch: meta_map
            .get("host_git_branch")
            .cloned()
            .filter(|value| !value.is_empty()),
        host_git_dirty: meta_map
            .get("host_git_dirty")
            .and_then(|value| value.parse::<bool>().ok()),
        docs,
        relations,
        checks,
    })
}

fn group_second_column(result: &QueryResult) -> BTreeMap<String, Vec<String>> {
    let mut grouped = BTreeMap::<String, Vec<String>>::new();
    for row in &result.rows {
        let key = cell(row, 0).to_string();
        let value = cell(row, 1).to_string();
        if !key.is_empty() && !value.is_empty() {
            grouped.entry(key).or_default().push(value);
        }
    }
    grouped
}

pub(crate) fn split_pipe_list(value: &str) -> Vec<String> {
    value
        .split('|')
        .map(|item| item.trim())
        .filter(|item| !item.is_empty())
        .map(|item| item.to_string())
        .collect()
}

fn empty_as_none(value: &str) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn build_doc_relations(
    schema: &Schema,
    docs: &[IndexedDoc],
    path_set: &BTreeSet<String>,
) -> Vec<DocRelation> {
    let manifest_path = schema
        .entrypoints
        .manifest
        .clone()
        .unwrap_or_else(|| format!("{}/index.md", schema.managed_root));
    let mut relations = Vec::new();
    let mut seen = BTreeSet::new();

    for doc in docs {
        if let Some(collection_index) = collection_index_for_doc(schema, doc) {
            if doc.path != collection_index && path_set.contains(&collection_index) {
                push_relation(
                    &mut relations,
                    &mut seen,
                    collection_index,
                    doc.path.clone(),
                    "doc_indexes_doc",
                    "collection-membership",
                    true,
                );
            }
        }

        for link in &doc.links {
            if path_set.contains(link) {
                let relation_type = if doc.path == manifest_path
                    || is_entrypoint_path(schema, &doc.path)
                    || doc.path.ends_with("/index.md")
                {
                    "doc_indexes_doc"
                } else {
                    "doc_links_doc"
                };
                push_relation(
                    &mut relations,
                    &mut seen,
                    doc.path.clone(),
                    link.clone(),
                    relation_type,
                    "markdown-link",
                    true,
                );
            }
        }

        for mentioned in &doc.path_mentions {
            if path_set.contains(mentioned) {
                let relation_type = if is_navigation_doc(doc) {
                    "doc_indexes_doc"
                } else {
                    "doc_mentions_code_path"
                };
                push_relation(
                    &mut relations,
                    &mut seen,
                    doc.path.clone(),
                    mentioned.clone(),
                    relation_type,
                    "inline-code-path",
                    true,
                );
            }
        }

        for scope_path in &doc.scope_paths {
            push_relation(
                &mut relations,
                &mut seen,
                doc.path.clone(),
                scope_path.clone(),
                "doc_scopes_code",
                "frontmatter.scope_paths",
                true,
            );
            push_relation(
                &mut relations,
                &mut seen,
                scope_path.clone(),
                doc.path.clone(),
                "code_mentions_doc",
                "frontmatter.scope_paths.reverse",
                false,
            );
        }

        for superseded in &doc.supersedes {
            push_relation(
                &mut relations,
                &mut seen,
                doc.path.clone(),
                superseded.clone(),
                "supersedes",
                "frontmatter.supersedes",
                true,
            );
        }
        for (targets, relation_type, reason) in [
            (
                &doc.superseded_by,
                "superseded_by",
                "frontmatter.superseded_by",
            ),
            (
                &doc.supersedes_parts_of,
                "supersedes_parts_of",
                "frontmatter.supersedes_parts_of",
            ),
        ] {
            for target in targets {
                push_relation(
                    &mut relations,
                    &mut seen,
                    doc.path.clone(),
                    target.clone(),
                    relation_type,
                    reason,
                    true,
                );
            }
        }
    }

    relations
}

pub(crate) fn is_navigation_doc(doc: &IndexedDoc) -> bool {
    doc.is_entrypoint
        || matches!(
            doc.role.as_str(),
            "manifest" | "collection-index" | "architecture"
        )
}

fn push_relation(
    relations: &mut Vec<DocRelation>,
    seen: &mut BTreeSet<(String, String, String)>,
    src_path: String,
    dst_path: String,
    relation_type: &str,
    reason: &str,
    explicit: bool,
) {
    let key = (
        src_path.clone(),
        dst_path.clone(),
        relation_type.to_string(),
    );
    if seen.insert(key) {
        relations.push(DocRelation {
            src_path,
            dst_path,
            relation_type: relation_type.to_string(),
            reason: reason.to_string(),
            explicit,
            confidence: relation_confidence(relation_type, explicit),
            evidence_json: relation_evidence_json(reason),
        });
    }
}

fn relation_confidence(relation_type: &str, explicit: bool) -> f32 {
    if explicit {
        match relation_type {
            "doc_scopes_code" | "supersedes" | "superseded_by" | "supersedes_parts_of" => 1.0,
            "doc_indexes_doc" => 0.95,
            "doc_links_doc" => 0.9,
            "doc_mentions_code_path" => 0.75,
            "code_mentions_doc" => 0.55,
            _ => 0.8,
        }
    } else {
        0.5
    }
}

fn relation_evidence_json(reason: &str) -> String {
    serde_json::to_string(&serde_json::json!({ "reason": reason }))
        .expect("serialize relation evidence")
}

fn annotate_link_counts(docs: &mut [IndexedDoc], relations: &[DocRelation]) {
    let mut incoming = BTreeMap::<String, usize>::new();
    let mut outgoing = BTreeMap::<String, usize>::new();

    for relation in relations {
        if !counts_for_doc_navigation(relation) {
            continue;
        }
        if relation.dst_path.ends_with(".md") {
            *incoming.entry(relation.dst_path.clone()).or_insert(0) += 1;
        }
        if relation.src_path.ends_with(".md") {
            *outgoing.entry(relation.src_path.clone()).or_insert(0) += 1;
        }
    }

    for doc in docs {
        doc.incoming_links = incoming.get(&doc.path).copied().unwrap_or(0);
        doc.outgoing_links = outgoing.get(&doc.path).copied().unwrap_or(0);
    }
}

fn counts_for_doc_navigation(relation: &DocRelation) -> bool {
    matches!(
        relation.relation_type.as_str(),
        "doc_indexes_doc"
            | "doc_links_doc"
            | "supersedes"
            | "superseded_by"
            | "supersedes_parts_of"
    )
}

pub(crate) fn cell(row: &[Option<String>], index: usize) -> &str {
    row.get(index)
        .and_then(|value| value.as_deref())
        .unwrap_or("")
}
