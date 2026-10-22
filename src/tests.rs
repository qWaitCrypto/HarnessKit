use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};
use std::{env, fs};

use crate::checks::{filter_and_suppress_checks, newest_existing_path_mtime, CheckView};
use crate::cli::{command_options, load_engine_context, CommandOptions, CommonArgs, TargetArgs};
use crate::context::context_packet_for_doc;
use crate::discover::infer_index_role;
use crate::git::{ensure_host_git_exclude, run_git_capture};
use crate::history::{
    apply_restore_plan, compute_history_status, create_history_snapshot, diff_snapshot_to_worktree,
    history_content_hash, history_object_path, parse_history_snapshot_json, restore_plan,
};
use crate::index::{
    build_index_artifact, cell, collect_current_file_states, compute_fact_store_delta, meta_value,
    open_fact_store, parse_file_state_rows, read_index_artifact_from_fact_store,
    refresh_fact_store, scalar_count, write_fact_store, write_index_artifact,
    EngineContext, FACT_SCHEMA_VERSION,
};
use crate::parse::{extract_inline_code_path_mentions, parse_doc_text};
use crate::query::{query_candidates, DEFAULT_LIMIT};
use crate::render::{
    render_check_json, render_context_packet_json, render_index_json, render_query_json,
};
use crate::schema::{
    parse_inline_list, parse_schema, render_schema_copy, rewrite_docs_root_bound_paths, Schema,
    BUNDLED_SCHEMA_TEXT,
};
use crate::sqlite::QueryResult;
use crate::templates::{init_plan_paths, materialize_from_schema};
use crate::update::write_archive_sha256_file;

fn bundled_schema() -> Schema {
    parse_schema(BUNDLED_SCHEMA_TEXT).unwrap()
}

fn unique_temp_dir(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = env::temp_dir().join(format!(
        "harnesskit-{}-{}-{}",
        name,
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn bundled_schema_exposes_architecture_and_collections() {
    let schema = bundled_schema();

    assert_eq!(schema.managed_root, "docs");
    assert_eq!(
        schema.entrypoints.architecture.as_deref(),
        Some("ARCHITECTURE.md")
    );
    assert!(schema
        .core_files
        .iter()
        .any(|spec| spec.template == "architecture"
            && spec.root_path.as_deref() == Some("ARCHITECTURE.md")));
    assert!(schema
        .doc_collections
        .iter()
        .any(|spec| spec.name == "product_specs" && spec.path == "product-specs"));
    assert!(schema
        .doc_collections
        .iter()
        .any(|spec| spec.name == "exec_plans_active" && spec.path == "exec-plans/active"));
}

#[test]
fn update_checksum_file_is_scoped_to_downloaded_archive() {
    let target = unique_temp_dir("archive-checksum");
    fs::write(
            target.join("SHA256SUMS"),
            "aaaa  harnesskit-aarch64-apple-darwin.tar.gz\nbbbb  harnesskit-x86_64-apple-darwin.tar.gz\ncccc  harnesskit-x86_64-unknown-linux-gnu.tar.gz\n",
        )
        .unwrap();

    let scoped = write_archive_sha256_file(
        &target,
        "SHA256SUMS",
        "harnesskit-x86_64-unknown-linux-gnu.tar.gz",
    )
    .unwrap();

    assert_eq!(scoped, "harnesskit-x86_64-unknown-linux-gnu.tar.gz.sha256");
    assert_eq!(
        fs::read_to_string(target.join(scoped)).unwrap(),
        "cccc  harnesskit-x86_64-unknown-linux-gnu.tar.gz\n"
    );
    fs::remove_dir_all(target).unwrap();
}

#[test]
fn update_checksum_file_requires_downloaded_archive_entry() {
    let target = unique_temp_dir("archive-checksum-missing");
    fs::write(target.join("SHA256SUMS"), "aaaa  other.tar.gz\n").unwrap();

    let err = write_archive_sha256_file(
        &target,
        "SHA256SUMS",
        "harnesskit-x86_64-unknown-linux-gnu.tar.gz",
    )
    .unwrap_err()
    .to_string();

    assert!(err.contains("did not contain checksum"));
    fs::remove_dir_all(target).unwrap();
}

#[test]
fn init_materializes_progressive_disclosure_scaffold() {
    let target = unique_temp_dir("default-init");
    let schema = bundled_schema();
    let schema_copy = render_schema_copy(&schema);

    let stats = materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();

    assert!(stats.created_files >= 20);
    assert!(target.join("AGENTS.md").exists());
    assert!(target.join("CLAUDE.md").exists());
    assert!(target.join("ARCHITECTURE.md").exists());
    assert!(target.join("docs/index.md").exists());
    assert!(target.join("docs/product-specs/index.md").exists());
    assert!(target.join("docs/design-docs/index.md").exists());
    assert!(target.join("docs/decisions/index.md").exists());
    assert!(target.join("docs/exec-plans/active/index.md").exists());
    assert!(target.join("docs/worklog/active/index.md").exists());
    assert!(target.join("docs/templates/design-doc.md").exists());
    assert!(target.join(".harnesskit/schema.yaml").exists());

    let agents = fs::read_to_string(target.join("AGENTS.md")).unwrap();
    assert!(agents.contains("ARCHITECTURE.md"));
    assert!(agents.contains("docs/product-specs/index.md"));

    let docs_index = fs::read_to_string(target.join("docs/index.md")).unwrap();
    assert!(docs_index.contains("docs/design-docs/index.md"));
    assert!(docs_index.contains("docs/templates/design-doc.md"));

    fs::remove_dir_all(target).unwrap();
}

#[test]
fn docs_root_override_rewrites_generated_paths() {
    let target = unique_temp_dir("docs-root-override");
    let mut schema = bundled_schema();
    let old_root = schema.managed_root.clone();
    schema.managed_root = "repo-docs".to_string();
    rewrite_docs_root_bound_paths(&mut schema, &old_root, "repo-docs");
    let schema_copy = render_schema_copy(&schema);

    materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();

    assert!(target.join("repo-docs/index.md").exists());
    assert!(target.join("repo-docs/product-specs/index.md").exists());
    assert!(target.join("repo-docs/templates/product-spec.md").exists());

    let agents = fs::read_to_string(target.join("AGENTS.md")).unwrap();
    assert!(agents.contains("repo-docs/index.md"));
    assert!(agents.contains("repo-docs/product-specs/index.md"));

    let copied_schema = fs::read_to_string(target.join(".harnesskit/schema.yaml")).unwrap();
    assert!(copied_schema.contains("managed_root: repo-docs"));

    fs::remove_dir_all(target).unwrap();
}

#[test]
fn parse_schema_rejects_invalid_boolean_values() {
    let raw = r#"
schema_version: 0
managed_root: docs

entrypoints:
  agents: AGENTS.md
  manifest: docs/index.md
  project_map: docs/project.md

core_files:
  index:
    path: index.md
    required: maybe
    template: docs-index

doc_collections:
  product_specs:
    path: product-specs
    authority: canonical
    anchor_candidate: true
    template: product-spec
    allowed_frontmatter: []

rules:
  path_defines_doc_type: true
"#;

    match parse_schema(raw) {
        Ok(_) => panic!("expected parse_schema to reject invalid boolean"),
        Err(err) => assert!(err.to_string().contains("expected boolean")),
    }
}

#[test]
fn parse_schema_rejects_tabs_with_clear_message() {
    let raw = "schema_version: 0\nmanaged_root: docs\nentrypoints:\n\tagents: AGENTS.md\n";

    match parse_schema(raw) {
        Ok(_) => panic!("expected parse_schema to reject tab indentation"),
        Err(err) => assert!(err.to_string().contains("tab indentation")),
    }
}

#[test]
fn parse_inline_list_respects_quoted_commas() {
    let values = parse_inline_list("[status, \"owner, team\", authority]");

    assert_eq!(
        values,
        vec![
            "status".to_string(),
            "owner, team".to_string(),
            "authority".to_string()
        ]
    );
}

#[test]
fn command_options_no_strict_overrides_schema_strict() {
    let mut schema = Schema::default();
    schema.rules.strict_checks = Some(true);
    let mut args = CommonArgs::default();
    args.no_strict = true;

    let options = command_options(&args, Some(&schema));

    assert!(!options.strict);
}

#[test]
fn index_builds_docs_relations_and_checks() {
    let target = unique_temp_dir("index-artifact");
    let schema = bundled_schema();
    let schema_copy = render_schema_copy(&schema);

    materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();

    fs::write(
            target.join("docs/product-specs/example.md"),
            "---\nstatus: active\nauthority: canonical\nscope_paths:\n  - src/example/\n---\n\n# Product Spec: Example\n\nThis spec references `docs/design-docs/example-design.md`.\n",
        )
        .unwrap();
    fs::write(
            target.join("docs/design-docs/example-design.md"),
            "---\nstatus: active\nauthority: canonical\nsupersedes:\n  - docs/design-docs/older.md\n---\n\n# Design Doc: Example Design\n\nImplements the product spec in `docs/product-specs/example.md`.\n",
        )
        .unwrap();
    let artifact = build_index_artifact(&target, &schema).unwrap();

    assert!(artifact
        .docs
        .iter()
        .any(|doc| doc.path == "docs/product-specs/example.md"));
    assert!(artifact
        .docs
        .iter()
        .any(|doc| doc.path == "docs/design-docs/example-design.md"));
    assert!(artifact
        .relations
        .iter()
        .any(|relation| relation.relation_type == "doc_indexes_doc"
            && relation.dst_path == "docs/product-specs/example.md"));
    assert!(artifact
        .relations
        .iter()
        .any(|relation| relation.relation_type == "doc_scopes_code"
            && relation.dst_path == "src/example"));
    assert!(artifact
        .relations
        .iter()
        .any(|relation| relation.relation_type == "supersedes"
            && relation.dst_path == "docs/design-docs/older.md"));
    assert!(artifact
        .docs
        .iter()
        .any(|doc| doc.path == "AGENTS.md" && doc.is_entrypoint));
    assert!(artifact
        .docs
        .iter()
        .any(|doc| doc.path == "docs/index.md" && doc.role == "manifest"));

    let json = render_index_json(&artifact);
    assert!(json.contains("\"docs\""));
    assert!(json.contains("\"relations\""));
    assert!(json.contains("\"checks\""));

    fs::remove_dir_all(target).unwrap();
}

#[test]
fn fenced_markdown_links_do_not_create_doc_relations() {
    let target = unique_temp_dir("fenced-markdown-link");
    let schema = bundled_schema();
    let schema_copy = render_schema_copy(&schema);

    materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();
    fs::write(
            target.join("docs/design-docs/source.md"),
            "---\nstatus: active\nauthority: canonical\n---\n\n# Design Doc: Source\n\n```md\n[Target](target.md)\n```\n",
        )
        .unwrap();
    fs::write(
        target.join("docs/design-docs/target.md"),
        "---\nstatus: active\nauthority: canonical\n---\n\n# Design Doc: Target\n",
    )
    .unwrap();

    let artifact = build_index_artifact(&target, &schema).unwrap();

    assert!(!artifact.relations.iter().any(|relation| {
        relation.src_path == "docs/design-docs/source.md"
            && relation.dst_path == "docs/design-docs/target.md"
            && relation.reason == "markdown-link"
    }));

    fs::remove_dir_all(target).unwrap();
}

#[test]
fn fenced_headings_do_not_enter_doc_title_or_headings() {
    let parsed = parse_doc_text(
            "docs/design-docs/fenced-heading.md",
            "---\nstatus: active\nauthority: canonical\n---\n\n```md\n# Fake Title\n## Fake Section\n```\n\n# Real Title\n\n## Real Section\n",
        );

    assert_eq!(parsed.title.as_deref(), Some("Real Title"));
    assert_eq!(
        parsed.headings,
        vec!["Real Title".to_string(), "Real Section".to_string()]
    );
}

#[test]
fn short_backtick_lines_are_not_treated_as_fences() {
    let parsed = parse_doc_text(
        "docs/design-docs/source.md",
        "---\nstatus: active\nauthority: canonical\n---\n\n# Source\n\n`` [Target](target.md)\n",
    );

    assert_eq!(parsed.links, vec!["docs/design-docs/target.md".to_string()]);
}

#[test]
fn summary_ignores_fenced_code_blocks() {
    let parsed = parse_doc_text(
            "docs/design-docs/summary.md",
            "---\nstatus: active\nauthority: canonical\n---\n\n```md\nThis fenced example should not become the summary.\n```\n\nReal summary starts here.\n\nMore detail.\n",
        );

    assert_eq!(parsed.summary, "Real summary starts here.");
}

#[test]
fn split_frontmatter_accepts_closing_marker_at_eof() {
    let parsed = parse_doc_text(
        "docs/design-docs/frontmatter-only.md",
        "---\nstatus: active\nauthority: canonical\n---",
    );

    assert_eq!(
        parsed.frontmatter.get("status").map(String::as_str),
        Some("active")
    );
    assert_eq!(
        parsed.frontmatter.get("authority").map(String::as_str),
        Some("canonical")
    );
    assert!(parsed.body_text.is_empty());
}

#[test]
fn parse_doc_text_recognizes_crlf_frontmatter() {
    let parsed = parse_doc_text(
            "docs/design-docs/crlf.md",
            "---\r\nstatus: active\r\nauthority: canonical\r\nscope_paths:\r\n  - src/example\r\n---\r\n\r\n# CRLF Doc\r\n\r\nSummary line.\r\n",
        );

    assert_eq!(
        parsed.frontmatter.get("status").map(String::as_str),
        Some("active")
    );
    assert_eq!(
        parsed.frontmatter.get("authority").map(String::as_str),
        Some("canonical")
    );
    assert_eq!(parsed.scope_paths, vec!["src/example".to_string()]);
    assert_eq!(parsed.title.as_deref(), Some("CRLF Doc"));
}

#[test]
fn project_map_is_not_architecture_when_architecture_is_unconfigured() {
    let raw = r#"
schema_version: 0
managed_root: docs

entrypoints:
  agents: AGENTS.md
  manifest: docs/index.md
  project_map: docs/project.md

core_files:
  project_map:
    path: project.md
    required: true
    template: project-map

doc_collections:
  design_docs:
    path: design-docs
    authority: canonical
    anchor_candidate: true
    template: design-doc
    allowed_frontmatter: [status, authority]

rules:
  index_required: true
"#;
    let schema = parse_schema(raw).unwrap();

    assert_eq!(infer_index_role(&schema, "docs/project.md", None), "doc");
}

#[test]
fn collection_authority_marks_generated_and_reference_docs() {
    let raw = r#"
schema_version: 0
managed_root: docs

entrypoints:
  agents: AGENTS.md
  manifest: docs/index.md
  project_map: docs/project.md

core_files:
  index:
    path: index.md
    required: true
    template: docs-index

doc_collections:
  machine_outputs:
    path: machine-outputs
    authority: generated
    anchor_candidate: false
    template: generated
    allowed_frontmatter: [status, authority]
  source_notes:
    path: source-notes
    authority: reference
    anchor_candidate: false
    template: reference
    allowed_frontmatter: [status, authority]

rules:
  index_required: true
"#;
    let target = unique_temp_dir("authority-flags");
    let schema = parse_schema(raw).unwrap();
    fs::create_dir_all(target.join("docs/machine-outputs")).unwrap();
    fs::create_dir_all(target.join("docs/source-notes")).unwrap();
    fs::write(target.join(".harnesskit-schema-placeholder"), "").unwrap();
    fs::write(target.join("AGENTS.md"), "# Agents\n").unwrap();
    fs::write(target.join("docs/index.md"), "# Index\n").unwrap();
    fs::write(target.join("docs/project.md"), "# Project\n").unwrap();
    fs::write(target.join("docs/machine-outputs/report.md"), "# Report\n").unwrap();
    fs::write(target.join("docs/source-notes/source.md"), "# Source\n").unwrap();
    fs::create_dir_all(target.join(".harnesskit")).unwrap();
    fs::write(target.join(".harnesskit/schema.yaml"), raw).unwrap();

    let artifact = build_index_artifact(&target, &schema).unwrap();
    let generated = artifact
        .docs
        .iter()
        .find(|doc| doc.path == "docs/machine-outputs/report.md")
        .unwrap();
    let reference = artifact
        .docs
        .iter()
        .find(|doc| doc.path == "docs/source-notes/source.md")
        .unwrap();

    assert!(generated.is_generated);
    assert!(!generated.is_reference);
    assert!(reference.is_reference);
    assert!(!reference.is_generated);

    fs::remove_dir_all(target).unwrap();
}

#[test]
fn index_reads_non_utf8_markdown_lossily() {
    let target = unique_temp_dir("non-utf8-markdown");
    let schema = bundled_schema();
    let schema_copy = render_schema_copy(&schema);

    materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();
    fs::write(
        target.join("docs/design-docs/non-utf8.md"),
        b"---\nstatus: active\nauthority: canonical\n---\n\n# Non UTF8\n\nInvalid byte: \xff\n",
    )
    .unwrap();

    let artifact = build_index_artifact(&target, &schema).unwrap();

    assert!(artifact
        .docs
        .iter()
        .any(|doc| doc.path == "docs/design-docs/non-utf8.md"));

    fs::remove_dir_all(target).unwrap();
}

#[test]
fn index_only_extracts_path_mentions_from_inline_code() {
    let target = unique_temp_dir("index-inline-code");
    let schema = bundled_schema();
    let schema_copy = render_schema_copy(&schema);

    materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();

    fs::write(
            target.join("docs/product-specs/example.md"),
            "---\nstatus: active\nauthority: canonical\n---\n\n# Product Spec: Example\n\nPlain text mention docs/design-docs/example-design.md should stay weak and ignored.\n\nInline code mention `docs/design-docs/example-design.md` should become a relation.\n\n```md\nfenced docs/design-docs/example-design.md should not count\n```\n",
        )
        .unwrap();
    fs::write(
        target.join("docs/design-docs/example-design.md"),
        "---\nstatus: active\nauthority: canonical\n---\n\n# Design Doc: Example Design\n",
    )
    .unwrap();

    let artifact = build_index_artifact(&target, &schema).unwrap();
    let matching_relations = artifact
        .relations
        .iter()
        .filter(|relation| {
            relation.src_path == "docs/product-specs/example.md"
                && relation.dst_path == "docs/design-docs/example-design.md"
        })
        .collect::<Vec<_>>();

    assert_eq!(matching_relations.len(), 1);
    assert_eq!(
        matching_relations[0].relation_type,
        "doc_mentions_code_path"
    );
    assert_eq!(matching_relations[0].reason, "inline-code-path");

    fs::remove_dir_all(target).unwrap();
}

#[test]
fn inline_code_triple_backticks_do_not_toggle_fence() {
    let mentions =
        extract_inline_code_path_mentions("Inline `src/lib.rs ``` docs/project.md` mention.\n");

    assert!(mentions.contains(&"src/lib.rs".to_string()));
    assert!(mentions.contains(&"docs/project.md".to_string()));
}

#[test]
fn index_emits_missing_doc_checks_when_required_docs_are_absent() {
    let target = unique_temp_dir("index-missing-docs");
    let schema = bundled_schema();
    let schema_copy = render_schema_copy(&schema);

    materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();

    fs::remove_file(target.join("CLAUDE.md")).unwrap();
    fs::remove_file(target.join("docs/commands.md")).unwrap();
    fs::remove_file(target.join("docs/generated/index.md")).unwrap();

    let artifact = build_index_artifact(&target, &schema).unwrap();

    assert!(artifact.checks.iter().any(
        |check| check.rule_id == "missing-entrypoint-doc" && check.subject_path == "CLAUDE.md"
    ));
    assert!(artifact
        .checks
        .iter()
        .any(|check| check.rule_id == "missing-required-doc"
            && check.subject_path == "docs/commands.md"));
    assert!(artifact
        .checks
        .iter()
        .any(|check| check.rule_id == "missing-collection-index"
            && check.subject_path == "docs/generated/index.md"));

    fs::remove_dir_all(target).unwrap();
}

#[test]
fn fact_store_persists_file_states_and_relations() {
    let target = unique_temp_dir("fact-store");
    let schema = bundled_schema();
    let schema_copy = render_schema_copy(&schema);

    materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();
    fs::write(
            target.join("docs/design-docs/example-design.md"),
            "---\nstatus: active\nauthority: canonical\n---\n\n# Design Doc: Example Design\n\nLinks to [project map](../project.md).\n",
        )
        .unwrap();

    let artifact = build_index_artifact(&target, &schema).unwrap();
    write_fact_store(&target, &artifact).unwrap();

    let db = open_fact_store(&target).unwrap();
    let file_states = db
            .query("SELECT path, content_hash, file_size_bytes, mtime_unix FROM file_states WHERE path = 'docs/design-docs/example-design.md';", [])
            .unwrap();
    assert_eq!(file_states.rows.len(), 1);
    assert_eq!(
        cell(&file_states.rows[0], 0),
        "docs/design-docs/example-design.md"
    );
    assert!(!cell(&file_states.rows[0], 1).is_empty());
    assert!(cell(&file_states.rows[0], 2).parse::<u64>().unwrap() > 0);

    let relations = db
            .query("SELECT relation_type, confidence, evidence_json FROM relations WHERE src_path = 'docs/design-docs/example-design.md';", [])
            .unwrap();
    assert!(relations
        .rows
        .iter()
        .any(|row| cell(row, 0) == "doc_links_doc" || cell(row, 0) == "doc_indexes_doc"));
    assert!(relations
        .rows
        .iter()
        .all(|row| !cell(row, 1).is_empty() && cell(row, 2).contains("\"reason\"")));

    fs::remove_dir_all(target).unwrap();
}

#[test]
fn fact_store_read_preserves_exact_mtime_ns_from_file_states() {
    let target = unique_temp_dir("fact-store-mtime-ns");
    let schema = bundled_schema();
    let schema_copy = render_schema_copy(&schema);

    materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();
    let artifact = build_index_artifact(&target, &schema).unwrap();
    write_fact_store(&target, &artifact).unwrap();

    let db = open_fact_store(&target).unwrap();
    db.exec(
        "UPDATE file_states SET mtime_ns = 1234567890123456789 WHERE path = 'docs/project.md';",
    )
    .unwrap();
    let read_back = read_index_artifact_from_fact_store(&db).unwrap();
    let project = read_back
        .docs
        .iter()
        .find(|doc| doc.path == "docs/project.md")
        .unwrap();

    assert_eq!(project.mtime_ns, 1234567890123456789);

    fs::remove_dir_all(target).unwrap();
}

#[test]
fn fact_store_state_path_errors_include_recovery_hint() {
    let target = unique_temp_dir("fact-store-state-path-error");
    fs::create_dir_all(target.join(".harnesskit")).unwrap();
    fs::write(target.join(".harnesskit/state"), "not a directory").unwrap();

    let err = match open_fact_store(&target) {
        Ok(_) => panic!("expected fact store open to fail"),
        Err(err) => err.to_string(),
    };

    assert!(err.contains("HarnessKit fact store"));
    assert!(err.contains(".harnesskit"));
    assert!(err.contains("target_dir:"));
    assert!(err.contains("current_dir:"));
    assert!(err.contains("PWD:"));
    assert!(err.contains("facts_path:"));
    assert!(err.contains("intended writable project root"));
    assert!(err.contains("Do not delete `.harnesskit/history`"));

    fs::remove_dir_all(target).unwrap();
}

#[test]
fn fact_store_refresh_detects_file_changes() {
    let target = unique_temp_dir("fact-refresh");
    let schema = bundled_schema();
    let schema_copy = render_schema_copy(&schema);

    materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();
    let artifact = build_index_artifact(&target, &schema).unwrap();
    write_fact_store(&target, &artifact).unwrap();

    let context = EngineContext {
        target_dir: target.clone(),
        schema: schema.clone(),
    };
    let delta = compute_fact_store_delta(&context).unwrap();
    assert!(delta.added_paths.is_empty());
    assert!(delta.changed_paths.is_empty());
    assert!(delta.removed_paths.is_empty());

    fs::write(
        target.join("docs/project.md"),
        "---\nstatus: active\nauthority: canonical\n---\n\n# Project Map\n\nChanged summary.\n",
    )
    .unwrap();
    let delta = compute_fact_store_delta(&context).unwrap();
    assert!(!delta.changed_paths.is_empty());

    fs::remove_dir_all(target).unwrap();
}

#[test]
fn query_sql_returns_scores_and_neighbors() {
    let target = unique_temp_dir("query-surface");
    let schema = bundled_schema();
    let schema_copy = render_schema_copy(&schema);

    materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();
    fs::write(
            target.join("docs/design-docs/schema-engine.md"),
            "---\nstatus: active\nauthority: canonical\n---\n\n# Design Doc: Schema Engine\n\nThis document explains schema parsing and query behavior.\n\nLinks to [project map](../project.md).\n",
        )
        .unwrap();
    let artifact = build_index_artifact(&target, &schema).unwrap();
    write_fact_store(&target, &artifact).unwrap();
    let db = open_fact_store(&target).unwrap();

    let result = db
            .query(
                "WITH lexical AS (
                    SELECT d.path,
                           d.title,
                           d.role,
                           COALESCE(d.status, '') AS status,
                           d.summary,
                           d.incoming_links,
                           CASE
                               WHEN lower(d.title) LIKE '%' || lower(?1) || '%' THEN 4.0
                               WHEN lower(d.path) LIKE '%' || lower(?1) || '%' THEN 3.0
                               WHEN lower(d.headings_text) LIKE '%' || lower(?1) || '%' THEN 2.0
                               WHEN lower(d.summary) LIKE '%' || lower(?1) || '%' THEN 1.5
                               ELSE 0.0
                           END AS lexical_score,
                           CASE
                               WHEN lower(d.title) LIKE '%' || lower(?1) || '%' THEN 'title matched query'
                               WHEN lower(d.path) LIKE '%' || lower(?1) || '%' THEN 'path matched query'
                               WHEN lower(d.headings_text) LIKE '%' || lower(?1) || '%' THEN 'heading matched query'
                               WHEN lower(d.summary) LIKE '%' || lower(?1) || '%' THEN 'summary matched query'
                               ELSE ''
                           END AS why
                    FROM docs d
                    WHERE lower(d.path) LIKE '%' || lower(?1) || '%'
                       OR lower(d.title) LIKE '%' || lower(?1) || '%'
                       OR lower(d.headings_text) LIKE '%' || lower(?1) || '%'
                       OR lower(d.summary) LIKE '%' || lower(?1) || '%'
                ),
                fts AS (
                    SELECT d.path,
                           d.title,
                           d.role,
                           COALESCE(d.status, '') AS status,
                           d.summary,
                           d.incoming_links,
                           2.5 AS lexical_score,
                           'full-text matched query' AS why
                    FROM docs_fts f
                    JOIN docs d ON d.path = f.path
                    WHERE docs_fts MATCH ?1
                ),
                base AS (
                    SELECT * FROM lexical
                    UNION ALL
                    SELECT * FROM fts
                ),
                merged AS (
                    SELECT path,
                           max(title) AS title,
                           max(role) AS role,
                           max(status) AS status,
                           max(summary) AS summary,
                           max(incoming_links) AS incoming_links,
                           max(lexical_score) AS lexical_score,
                           group_concat(DISTINCT why) AS why
                    FROM base
                    GROUP BY path
                ),
                expanded AS (
                    SELECT m.path AS anchor_path,
                           r.dst_path AS neighbor_path
                    FROM merged m
                    JOIN relations r ON r.src_path = m.path
                    WHERE r.dst_path LIKE '%.md'
                    UNION
                    SELECT m.path AS anchor_path,
                           r.src_path AS neighbor_path
                    FROM merged m
                    JOIN relations r ON r.dst_path = m.path
                    WHERE r.src_path LIKE '%.md'
                ),
                neighbor_summary AS (
                    SELECT anchor_path,
                           group_concat(neighbor_path, ' | ') AS neighbors
                    FROM (
                        SELECT anchor_path, neighbor_path
                        FROM expanded
                        ORDER BY anchor_path, neighbor_path
                    )
                    GROUP BY anchor_path
                )
                SELECT m.path,
                       m.title,
                       m.role,
                       m.status,
                       printf('%.2f', m.lexical_score + (m.incoming_links * 0.05)) AS score,
                       m.why,
                       m.summary,
                       COALESCE(n.neighbors, '')
                FROM merged m
                LEFT JOIN neighbor_summary n ON n.anchor_path = m.path
                ORDER BY (m.lexical_score + (m.incoming_links * 0.05)) DESC, m.path ASC
                LIMIT 12;",
                ["schema"],
            )
            .unwrap();

    assert!(result
        .rows
        .iter()
        .any(|row| cell(row, 0) == "docs/design-docs/schema-engine.md"));
    let row = result
        .rows
        .iter()
        .find(|row| cell(row, 0) == "docs/design-docs/schema-engine.md")
        .unwrap();
    assert!(!cell(row, 4).is_empty());
    assert!(!cell(row, 5).is_empty());
    assert!(cell(row, 7).contains("docs/project.md"));

    fs::remove_dir_all(target).unwrap();
}

#[test]
fn compute_fact_store_delta_detects_added_changed_and_removed_paths() {
    let target = unique_temp_dir("delta-detect");
    let schema = bundled_schema();
    let schema_copy = render_schema_copy(&schema);

    materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();
    let artifact = build_index_artifact(&target, &schema).unwrap();
    write_fact_store(&target, &artifact).unwrap();

    fs::write(
        target.join("docs/project.md"),
        "---\nstatus: active\nauthority: canonical\n---\n\n# Project\n\nChanged body.\n",
    )
    .unwrap();
    fs::write(
        target.join("docs/design-docs/new-one.md"),
        "---\nstatus: active\nauthority: canonical\n---\n\n# Design Doc: New One\n",
    )
    .unwrap();
    fs::remove_file(target.join("docs/commands.md")).unwrap();

    let context = EngineContext {
        target_dir: target.clone(),
        schema: schema.clone(),
    };
    let delta = compute_fact_store_delta(&context).unwrap();

    assert!(delta.changed_paths.contains(&"docs/project.md".to_string()));
    assert!(delta
        .added_paths
        .contains(&"docs/design-docs/new-one.md".to_string()));
    assert!(delta
        .removed_paths
        .contains(&"docs/commands.md".to_string()));

    fs::remove_dir_all(target).unwrap();
}

#[test]
fn incremental_refresh_updates_fact_store_without_dropping_unchanged_docs() {
    let target = unique_temp_dir("incremental-refresh");
    let schema = bundled_schema();
    let schema_copy = render_schema_copy(&schema);

    materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();
    let artifact = build_index_artifact(&target, &schema).unwrap();
    write_index_artifact(&target, &artifact).unwrap();
    write_fact_store(&target, &artifact).unwrap();

    let unchanged_before = fs::read_to_string(target.join("docs/project.md")).unwrap();
    fs::write(
            target.join("docs/DOCUMENTATION_SYSTEM.md"),
            "---\nstatus: stable\nauthority: canonical\n---\n\n# Documentation System\n\nIncrementally updated summary.\n",
        )
        .unwrap();
    fs::remove_file(target.join("docs/commands.md")).unwrap();
    fs::write(
            target.join("docs/design-docs/incremental-added.md"),
            "---\nstatus: active\nauthority: canonical\n---\n\n# Design Doc: Incremental Added\n\nLinked from `docs/index.md`.\n",
        )
        .unwrap();

    let context = EngineContext {
        target_dir: target.clone(),
        schema: schema.clone(),
    };
    refresh_fact_store(&context).unwrap();

    let db = open_fact_store(&target).unwrap();
    assert_eq!(
        scalar_count(
            &db,
            "SELECT COUNT(*) FROM docs WHERE path = 'docs/DOCUMENTATION_SYSTEM.md';"
        )
        .unwrap(),
        1
    );
    assert_eq!(
        scalar_count(
            &db,
            "SELECT COUNT(*) FROM docs WHERE path = 'docs/commands.md';"
        )
        .unwrap(),
        0
    );
    assert_eq!(
        scalar_count(
            &db,
            "SELECT COUNT(*) FROM docs WHERE path = 'docs/design-docs/incremental-added.md';"
        )
        .unwrap(),
        1
    );
    let project_row = db
        .query(
            "SELECT summary FROM docs WHERE path = 'docs/DOCUMENTATION_SYSTEM.md';",
            [],
        )
        .unwrap();
    assert!(cell(&project_row.rows[0], 0).contains("Incrementally updated summary"));

    let meta_rows = db
            .query("SELECT key, value FROM meta WHERE key IN ('last_refresh_mode', 'last_delta_changed') ORDER BY key ASC;", [])
            .unwrap();
    assert!(meta_rows
        .rows
        .iter()
        .any(|row| cell(row, 0) == "last_refresh_mode" && cell(row, 1) == "delta"));
    assert!(meta_rows
        .rows
        .iter()
        .any(|row| cell(row, 0) == "last_delta_changed" && cell(row, 1) == "1"));

    let commands_row = db
        .query(
            "SELECT summary FROM docs WHERE path = 'docs/project.md';",
            [],
        )
        .unwrap();
    assert!(!cell(&commands_row.rows[0], 0).is_empty());
    assert_eq!(
        unchanged_before,
        fs::read_to_string(target.join("docs/project.md")).unwrap()
    );

    fs::remove_dir_all(target).unwrap();
}

#[test]
fn delta_refresh_records_real_host_git_dirty() {
    let target = unique_temp_dir("delta-host-git-dirty");
    let schema = bundled_schema();
    let schema_copy = render_schema_copy(&schema);

    materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();
    Command::new("git")
        .arg("init")
        .current_dir(&target)
        .output()
        .unwrap();
    Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(&target)
        .output()
        .unwrap();
    Command::new("git")
        .args(["config", "user.name", "HarnessKit Test"])
        .current_dir(&target)
        .output()
        .unwrap();
    Command::new("git")
        .args(["add", "."])
        .current_dir(&target)
        .output()
        .unwrap();
    Command::new("git")
        .args(["commit", "-m", "baseline"])
        .current_dir(&target)
        .output()
        .unwrap();

    let artifact = build_index_artifact(&target, &schema).unwrap();
    assert!(!artifact.host_git_dirty);
    write_index_artifact(&target, &artifact).unwrap();
    write_fact_store(&target, &artifact).unwrap();

    fs::write(
        target.join("docs/project.md"),
        "---\nstatus: active\nauthority: canonical\n---\n\n# Project\n\nDirty delta body.\n",
    )
    .unwrap();
    let context = EngineContext {
        target_dir: target.clone(),
        schema: schema.clone(),
    };
    refresh_fact_store(&context).unwrap();

    let db = open_fact_store(&target).unwrap();
    let dirty = meta_value(&db, "host_git_dirty").unwrap();
    let mode = meta_value(&db, "last_refresh_mode").unwrap();
    assert_eq!(dirty.as_deref(), Some("true"));
    assert_eq!(mode.as_deref(), Some("delta"));

    fs::remove_dir_all(target).unwrap();
}

#[test]
fn delta_quick_check_reuses_hash_when_file_metadata_is_unchanged() {
    let target = unique_temp_dir("delta-quick-check");
    let schema = bundled_schema();
    let schema_copy = render_schema_copy(&schema);

    materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();
    let artifact = build_index_artifact(&target, &schema).unwrap();
    write_fact_store(&target, &artifact).unwrap();

    let db = open_fact_store(&target).unwrap();
    let rows = db
            .query("SELECT path, content_hash, file_size_bytes, mtime_unix, mtime_ns FROM file_states WHERE path = 'docs/project.md';", [])
            .unwrap();
    let previous = parse_file_state_rows(&rows);
    let current = collect_current_file_states(&target, &schema, &previous).unwrap();

    assert_eq!(
        previous.get("docs/project.md").unwrap().content_hash,
        current.get("docs/project.md").unwrap().content_hash
    );

    fs::remove_dir_all(target).unwrap();
}

#[test]
fn refresh_upgrades_older_fact_store_schema() {
    let target = unique_temp_dir("schema-upgrade");
    let schema = bundled_schema();
    let schema_copy = render_schema_copy(&schema);

    materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();
    let artifact = build_index_artifact(&target, &schema).unwrap();
    write_fact_store(&target, &artifact).unwrap();

    let db = open_fact_store(&target).unwrap();
    db.exec("DROP TABLE path_mentions;").unwrap();

    fs::write(
        target.join("docs/project.md"),
        "---\nstatus: active\nauthority: canonical\n---\n\n# Project\n\nUpgrade path check.\n",
    )
    .unwrap();

    let context = EngineContext {
        target_dir: target.clone(),
        schema: schema.clone(),
    };
    refresh_fact_store(&context).unwrap();

    let db = open_fact_store(&target).unwrap();
    let table_rows = db
        .query(
            "SELECT name FROM sqlite_master WHERE type='table' AND name='path_mentions';",
            [],
        )
        .unwrap();
    assert_eq!(table_rows.rows.len(), 1);
    let version_rows = db
        .query(
            "SELECT value FROM meta WHERE key='fact_schema_version';",
            [],
        )
        .unwrap();
    assert_eq!(cell(&version_rows.rows[0], 0), FACT_SCHEMA_VERSION);

    fs::remove_dir_all(target).unwrap();
}

#[test]
fn full_build_writes_fact_schema_version_meta() {
    let target = unique_temp_dir("fact-schema-version");
    let schema = bundled_schema();
    let schema_copy = render_schema_copy(&schema);

    materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();
    let artifact = build_index_artifact(&target, &schema).unwrap();
    write_fact_store(&target, &artifact).unwrap();

    let db = open_fact_store(&target).unwrap();
    let version_rows = db
        .query(
            "SELECT value FROM meta WHERE key='fact_schema_version';",
            [],
        )
        .unwrap();
    assert_eq!(cell(&version_rows.rows[0], 0), FACT_SCHEMA_VERSION);

    fs::remove_dir_all(target).unwrap();
}

#[test]
fn delta_refresh_recovers_relations_when_new_target_doc_appears() {
    let target = unique_temp_dir("delta-link-recovery");
    let schema = bundled_schema();
    let schema_copy = render_schema_copy(&schema);

    materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();
    fs::write(
            target.join("docs/design-docs/source.md"),
            "---\nstatus: active\nauthority: canonical\n---\n\n# Design Doc: Source\n\nLinks to `docs/design-docs/target.md`.\n",
        )
        .unwrap();

    let artifact = build_index_artifact(&target, &schema).unwrap();
    write_index_artifact(&target, &artifact).unwrap();
    write_fact_store(&target, &artifact).unwrap();

    let db = open_fact_store(&target).unwrap();
    let before = scalar_count(
            &db,
            "SELECT COUNT(*) FROM relations WHERE src_path = 'docs/design-docs/source.md' AND dst_path = 'docs/design-docs/target.md';",
        )
        .unwrap();
    assert_eq!(before, 0);

    fs::write(
        target.join("docs/design-docs/target.md"),
        "---\nstatus: active\nauthority: canonical\n---\n\n# Design Doc: Target\n",
    )
    .unwrap();

    let context = EngineContext {
        target_dir: target.clone(),
        schema: schema.clone(),
    };
    refresh_fact_store(&context).unwrap();

    let db = open_fact_store(&target).unwrap();
    let after = scalar_count(
            &db,
            "SELECT COUNT(*) FROM relations WHERE src_path = 'docs/design-docs/source.md' AND dst_path = 'docs/design-docs/target.md';",
        )
        .unwrap();
    assert_eq!(after, 1);

    fs::remove_dir_all(target).unwrap();
}

#[test]
fn delta_refresh_removes_stale_reverse_scope_relations() {
    let target = unique_temp_dir("delta-scope-reverse");
    let schema = bundled_schema();
    let schema_copy = render_schema_copy(&schema);

    materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();
    fs::create_dir_all(target.join("src/old")).unwrap();
    fs::create_dir_all(target.join("src/new")).unwrap();
    fs::write(target.join("src/old/lib.rs"), "pub fn old() {}\n").unwrap();
    fs::write(target.join("src/new/lib.rs"), "pub fn new() {}\n").unwrap();
    fs::write(
            target.join("docs/design-docs/scope.md"),
            "---\nstatus: active\nauthority: canonical\nscope_paths:\n  - src/old\n---\n\n# Design Doc: Scope\n\nOld scope.\n",
        )
        .unwrap();

    let artifact = build_index_artifact(&target, &schema).unwrap();
    write_index_artifact(&target, &artifact).unwrap();
    write_fact_store(&target, &artifact).unwrap();

    let db = open_fact_store(&target).unwrap();
    assert_eq!(
            scalar_count(
                &db,
                "SELECT COUNT(*) FROM relations WHERE src_path = 'src/old' AND dst_path = 'docs/design-docs/scope.md' AND relation_type = 'code_mentions_doc';"
            )
            .unwrap(),
            1
        );

    fs::write(
            target.join("docs/design-docs/scope.md"),
            "---\nstatus: active\nauthority: canonical\nscope_paths:\n  - src/new\n---\n\n# Design Doc: Scope\n\nNew scope.\n",
        )
        .unwrap();

    let context = EngineContext {
        target_dir: target.clone(),
        schema: schema.clone(),
    };
    refresh_fact_store(&context).unwrap();

    let db = open_fact_store(&target).unwrap();
    assert_eq!(
            scalar_count(
                &db,
                "SELECT COUNT(*) FROM relations WHERE src_path = 'src/old' AND dst_path = 'docs/design-docs/scope.md' AND relation_type = 'code_mentions_doc';"
            )
            .unwrap(),
            0
        );
    assert_eq!(
            scalar_count(
                &db,
                "SELECT COUNT(*) FROM relations WHERE src_path = 'src/new' AND dst_path = 'docs/design-docs/scope.md' AND relation_type = 'code_mentions_doc';"
            )
            .unwrap(),
            1
        );

    fs::remove_dir_all(target).unwrap();
}

#[test]
fn phase2_checks_cover_manifest_stale_duplicate_contamination_and_frontmatter() {
    let target = unique_temp_dir("phase2-checks");
    let schema = bundled_schema();
    let schema_copy = render_schema_copy(&schema);

    materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();
    let docs_index = fs::read_to_string(target.join("docs/index.md")).unwrap();
    fs::write(
        target.join("docs/index.md"),
        docs_index.replace("`docs/operations/index.md`", "`docs/operations/README.md`"),
    )
    .unwrap();
    fs::create_dir_all(target.join("src/fresh")).unwrap();
    fs::write(target.join("src/fresh/lib.rs"), "pub fn fresh() {}\n").unwrap();
    fs::write(
            target.join("docs/design-docs/stale.md"),
            "---\nstatus: active\nauthority: canonical\nunknown_field: value\nscope_paths:\n  - src/fresh\n---\n\n# Design Doc: Stale\n\nCanonical source doc links to [generated](../generated/report.md).\n",
        )
        .unwrap();
    let initial_artifact = build_index_artifact(&target, &schema).unwrap();
    let stale_doc_mtime = initial_artifact
        .docs
        .iter()
        .find(|doc| doc.path == "docs/design-docs/stale.md")
        .unwrap()
        .mtime_unix;
    fs::write(
            target.join("docs/design-docs/dup-a.md"),
            "---\nstatus: active\nauthority: canonical\n---\n\n# Duplicate Shape\n\nBody A.\n\n## Shared\n\n## Details\n",
        )
        .unwrap();
    fs::write(
            target.join("docs/design-docs/dup-b.md"),
            "---\nstatus: active\nauthority: canonical\n---\n\n# Duplicate Shape\n\nBody B.\n\n## Shared\n\n## Details\n",
        )
        .unwrap();
    fs::write(
        target.join("docs/generated/report.md"),
        "# Generated Report\n\nMachine output.\n",
    )
    .unwrap();
    for attempt in 0..5 {
        std::thread::sleep(std::time::Duration::from_millis(1100));
        fs::write(
            target.join("src/fresh/lib.rs"),
            format!("pub fn fresher_{}() {{}}\n", attempt),
        )
        .unwrap();
        if newest_existing_path_mtime(&target.join("src/fresh")).unwrap_or(0) > stale_doc_mtime {
            break;
        }
    }

    let artifact = build_index_artifact(&target, &schema).unwrap();
    let rule_ids = artifact
        .checks
        .iter()
        .map(|check| check.rule_id.as_str())
        .collect::<BTreeSet<_>>();

    assert!(artifact
        .relations
        .iter()
        .any(|relation| relation.relation_type == "code_mentions_doc"
            && relation.dst_path == "docs/design-docs/stale.md"));
    assert!(rule_ids.contains("manifest-missing-doc"));
    assert!(rule_ids.contains("stale-explicit-anchor"));
    assert!(rule_ids.contains("overlap-heading-fingerprint"));
    assert!(rule_ids.contains("canonical-links-generated"));
    assert!(rule_ids.contains("frontmatter-field-not-allowed"));
    assert!(artifact
        .checks
        .iter()
        .all(|check| !check.evidence_json.is_empty()));

    fs::remove_dir_all(target).unwrap();
}

#[test]
fn schema_parses_suppressions_and_check_filter_applies_them() {
    let raw = r#"
schema_version: 0
managed_root: docs

entrypoints:
  agents: AGENTS.md
  manifest: docs/index.md
  project_map: docs/project.md

core_files:
  index:
    path: index.md
    required: true
    template: docs-index

doc_collections:
  design_docs:
    path: design-docs
    authority: canonical
    anchor_candidate: true
    template: design-doc
    allowed_frontmatter: [status, authority]

rules:
  index_required: true

suppressions:
  known_missing:
    rule_id: manifest-missing-doc
    path: docs/design-docs/index.md
    reason: accepted during migration
"#;
    let schema = parse_schema(raw).unwrap();
    assert_eq!(schema.suppressions.len(), 1);

    let result = QueryResult {
        rows: vec![
            vec![
                Some("warning".to_string()),
                Some("manifest-missing-doc".to_string()),
                Some("docs/design-docs/index.md".to_string()),
                Some("not linked".to_string()),
                Some("{}".to_string()),
            ],
            vec![
                Some("info".to_string()),
                Some("frontmatter-field-not-allowed".to_string()),
                Some("docs/design-docs/x.md".to_string()),
                Some("extra key".to_string()),
                Some("{}".to_string()),
            ],
        ],
    };
    let options = CommandOptions {
        json: false,
        strict: false,
        rules: Some(["manifest".to_string()].into_iter().collect()),
    };
    let checks = filter_and_suppress_checks(&result, &schema, &options);

    assert_eq!(checks.len(), 1);
    assert!(checks[0].suppressed);
    assert_eq!(
        checks[0].suppression_reason.as_deref(),
        Some("accepted during migration")
    );
}

#[test]
fn engine_context_prefers_project_schema_after_init() {
    let target = unique_temp_dir("project-schema-default");
    let schema = bundled_schema();
    let mut schema_copy = render_schema_copy(&schema);
    schema_copy.push_str("\nsuppressions:\n  local_only:\n    rule_id: missing-required-doc\n    path: docs/commands.md\n    reason: local project config\n");

    materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();
    let args = TargetArgs {
        target_dir: Some(target.to_string_lossy().to_string()),
        ..Default::default()
    };
    let context = load_engine_context(&args).unwrap();

    assert_eq!(context.schema.suppressions.len(), 1);
    assert_eq!(
        context.schema.suppressions[0].reason,
        "local project config"
    );

    fs::remove_dir_all(target).unwrap();
}

#[test]
fn query_candidates_include_snippet_heading_location_and_json() {
    let target = unique_temp_dir("query-snippet");
    let schema = bundled_schema();
    let schema_copy = render_schema_copy(&schema);

    materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();
    fs::write(
            target.join("docs/design-docs/semantic-layer.md"),
            "---\nstatus: active\nauthority: canonical\n---\n\n# Design Doc: Semantic Layer\n\nOpening summary.\n\n## Query Semantics\n\nThe semantic layer supports phrase search and context packet retrieval.\n",
        )
        .unwrap();
    let artifact = build_index_artifact(&target, &schema).unwrap();
    write_fact_store(&target, &artifact).unwrap();
    let db = open_fact_store(&target).unwrap();

    let candidates = query_candidates(&db, &target, "phrase search", DEFAULT_LIMIT).unwrap();
    let candidate = candidates
        .iter()
        .find(|candidate| candidate.path == "docs/design-docs/semantic-layer.md")
        .unwrap();

    assert_eq!(candidate.snippet_heading, "Query Semantics");
    assert!(candidate.snippet_location.starts_with("line "));
    assert!(candidate.snippet.contains("phrase search"));
    let json = render_query_json(
        "phrase search", &candidates, &Default::default(), &[], None, &[],
    );
    assert!(json.contains("\"snippet_location\""));

    fs::remove_dir_all(target).unwrap();
}

#[test]
fn query_candidates_handles_punctuation_only_query() {
    let target = unique_temp_dir("query-punctuation");
    let schema = bundled_schema();
    let schema_copy = render_schema_copy(&schema);

    materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();
    let artifact = build_index_artifact(&target, &schema).unwrap();
    write_fact_store(&target, &artifact).unwrap();
    let db = open_fact_store(&target).unwrap();

    let candidates = query_candidates(&db, &target, "--- ?! ::", DEFAULT_LIMIT).unwrap();
    assert!(candidates.is_empty());

    fs::remove_dir_all(target).unwrap();
}

#[test]
fn query_candidates_limits_before_neighbor_expansion() {
    let target = unique_temp_dir("query-neighbor-limit");
    let schema = bundled_schema();
    let schema_copy = render_schema_copy(&schema);

    materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();
    for idx in 0..30 {
        fs::write(
                target
                    .join("docs/references")
                    .join(format!("sample-{:02}.md", idx)),
                format!(
                    "---\nstatus: active\nauthority: reference\n---\n\n# Sample {:02}\n\nShared sample body.\n",
                    idx
                ),
            )
            .unwrap();
    }
    let artifact = build_index_artifact(&target, &schema).unwrap();
    write_fact_store(&target, &artifact).unwrap();
    let db = open_fact_store(&target).unwrap();

    let candidates = query_candidates(&db, &target, "sample", DEFAULT_LIMIT).unwrap();

    assert_eq!(candidates.len(), 8);
    assert!(candidates
        .iter()
        .all(|candidate| candidate.neighbors.len() <= 5));

    fs::remove_dir_all(target).unwrap();
}

#[test]
fn history_hash_is_wide_for_history_objects() {
    let hash = history_content_hash(b"history content");
    assert_eq!(hash.len(), 32);
    assert!(hash.chars().all(|ch| ch.is_ascii_hexdigit()));
}

#[test]
fn parse_history_snapshot_json_accepts_compact_json_layout() {
    let raw = r#"{"id":"snap-1","created_at_unix":123,"message":"baseline","docs_root":"docs","schema_hash":"abcd","host_git_head":null,"host_git_branch":"main","host_git_dirty":false,"files":[{"path":"docs/index.md","hash":"beef","size":10,"mtime_unix":456}]}"#;

    let snapshot = parse_history_snapshot_json(raw).unwrap();

    assert_eq!(snapshot.id, "snap-1");
    assert_eq!(snapshot.host_git_branch.as_deref(), Some("main"));
    assert_eq!(snapshot.files.len(), 1);
    assert_eq!(snapshot.files[0].path, "docs/index.md");
}

#[test]
fn json_parser_decodes_surrogate_pairs() {
    let raw = r#"{"id":"snap-1","created_at_unix":123,"message":"emoji \uD83D\uDE00","docs_root":"docs","schema_hash":"","host_git_dirty":false,"files":[]}"#;
    let snapshot = parse_history_snapshot_json(raw).unwrap();

    assert_eq!(snapshot.message, "emoji \u{1F600}");
}

#[test]
fn json_string_escapes_all_control_characters() {
    let rendered = render_query_json(
        "a\u{0001}\u{0008}\u{000c}z", &[], &Default::default(), &[], None, &[],
    );
    assert!(rendered.contains("\"query\": \"a\\u0001\\b\\fz\""));
}

#[test]
fn render_check_json_emits_evidence_as_json_value() {
    let checks = vec![CheckView {
        severity: "warning".to_string(),
        rule_id: "history-dirty-since-snapshot".to_string(),
        subject_path: ".harnesskit/history".to_string(),
        message: "dirty".to_string(),
        evidence_json: "{\"changed\":\"1\"}".to_string(),
        suppressed: false,
        suppression_reason: None,
    }];

    let rendered = render_check_json(Path::new("/tmp/demo"), 1, 2, &checks, &Default::default(), &[]);

    assert!(rendered.contains("\"evidence_json\": {\"changed\":\"1\"}"));
}

#[test]
fn context_packet_collects_graph_neighbors_and_reading_order() {
    let target = unique_temp_dir("context-packet");
    let schema = bundled_schema();
    let schema_copy = render_schema_copy(&schema);

    materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();
    fs::create_dir_all(target.join("src/shared")).unwrap();
    fs::write(target.join("src/shared/lib.rs"), "pub fn shared() {}\n").unwrap();
    fs::write(
            target.join("docs/design-docs/semantic-layer.md"),
            "---\nstatus: active\nauthority: canonical\nscope_paths:\n  - src/shared\n---\n\n# Design Doc: Semantic Layer\n\nThe semantic layer anchors query context.\n\nLinks to [aggregation](aggregation-layer.md).\n",
        )
        .unwrap();
    fs::write(
            target.join("docs/design-docs/aggregation-layer.md"),
            "---\nstatus: active\nauthority: canonical\nscope_paths:\n  - src/shared\n---\n\n# Design Doc: Aggregation Layer\n\nSibling design doc.\n",
        )
        .unwrap();
    let artifact = build_index_artifact(&target, &schema).unwrap();
    write_fact_store(&target, &artifact).unwrap();
    let db = open_fact_store(&target).unwrap();

    let packet =
        context_packet_for_doc(&db, &target, "docs/design-docs/semantic-layer.md").unwrap();

    assert!(packet
        .incoming
        .iter()
        .any(|relation| relation.src_path == "docs/design-docs/index.md"));
    assert!(packet
        .outgoing
        .iter()
        .any(|relation| relation.dst_path == "docs/design-docs/aggregation-layer.md"));
    assert!(packet
        .collection_siblings
        .contains(&"docs/design-docs/aggregation-layer.md".to_string()));
    assert!(packet
        .same_scope_docs
        .contains(&"docs/design-docs/aggregation-layer.md".to_string()));
    assert_eq!(
        packet.recommended_reading_order.first().map(String::as_str),
        Some("docs/design-docs/semantic-layer.md")
    );
    assert!(render_context_packet_json("context", "semantic", &packet, &Default::default(), &[])
        .contains("\"recommended_reading_order\""));

    fs::remove_dir_all(target).unwrap();
}

#[test]
fn init_writes_host_git_exclude_and_history_dirs() {
    let target = unique_temp_dir("git-exclude");
    run_git_capture(&target, &["init"]).unwrap();
    let schema = bundled_schema();
    let schema_copy = render_schema_copy(&schema);

    materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();
    let stats = ensure_host_git_exclude(&target, &schema).unwrap().unwrap();

    assert!(stats.0 >= 5);
    let exclude = fs::read_to_string(target.join(".git/info/exclude")).unwrap();
    assert!(exclude.contains("docs/"));
    assert!(exclude.contains("AGENTS.md"));
    assert!(exclude.contains("CLAUDE.md"));
    assert!(exclude.contains("ARCHITECTURE.md"));
    assert!(exclude.contains(".harnesskit/"));
    assert!(target.join(".harnesskit/history/objects").is_dir());
    assert!(target.join(".harnesskit/history/snapshots").is_dir());
    assert!(target.join(".harnesskit/history/refs").is_dir());

    fs::remove_dir_all(target).unwrap();
}

#[test]
fn init_does_not_treat_stray_git_directory_as_repo() {
    let target = unique_temp_dir("stray-git-dir");
    fs::create_dir_all(target.join(".git/info")).unwrap();
    let schema = bundled_schema();
    let schema_copy = render_schema_copy(&schema);

    materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();

    assert!(ensure_host_git_exclude(&target, &schema).unwrap().is_none());
    assert!(!target.join(".git/info/exclude").exists());

    fs::remove_dir_all(target).unwrap();
}

#[test]
fn init_tracked_mode_leaves_git_exclude_unchanged() {
    let target = unique_temp_dir("git-tracked");
    run_git_capture(&target, &["init"]).unwrap();
    fs::write(target.join(".git/info/exclude"), "# local excludes\n").unwrap();
    let schema = bundled_schema();
    let schema_copy = render_schema_copy(&schema);

    materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();

    let exclude = fs::read_to_string(target.join(".git/info/exclude")).unwrap();
    assert_eq!(exclude, "# local excludes\n");
    assert!(target.join("AGENTS.md").exists());
    assert!(target.join("docs/index.md").exists());

    fs::remove_dir_all(target).unwrap();
}

#[test]
fn init_preview_paths_do_not_write_files() {
    let parent = unique_temp_dir("init-preview-plan");
    let target = parent.join("preview-target");
    let schema = bundled_schema();
    let paths = init_plan_paths(&schema);

    assert!(paths.contains(&"AGENTS.md".to_string()));
    assert!(paths.contains(&"CLAUDE.md".to_string()));
    assert!(paths.contains(&"ARCHITECTURE.md".to_string()));
    assert!(paths.contains(&"docs/index.md".to_string()));
    assert!(paths.contains(&".harnesskit/state/".to_string()));
    assert!(paths.contains(&".harnesskit/history/".to_string()));
    assert!(!target.exists());
    fs::remove_dir_all(parent).unwrap();
}

#[test]
fn history_snapshot_status_diff_and_restore_roundtrip() {
    let target = unique_temp_dir("history-roundtrip");
    let schema = bundled_schema();
    let schema_copy = render_schema_copy(&schema);

    materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();
    let first = create_history_snapshot(&target, &schema, "baseline").unwrap();
    let original_project = fs::read_to_string(target.join("docs/project.md")).unwrap();

    fs::write(
        target.join("docs/project.md"),
        "---\nstatus: active\nauthority: canonical\n---\n\n# Project\n\nChanged for history.\n",
    )
    .unwrap();
    fs::write(
        target.join("docs/design-docs/history-added.md"),
        "---\nstatus: active\nauthority: canonical\n---\n\n# Design Doc: Added\n",
    )
    .unwrap();

    let status = compute_history_status(&target, &schema).unwrap();
    assert_eq!(status.latest_snapshot.as_deref(), Some(first.id.as_str()));
    assert!(status.changed.contains(&"docs/project.md".to_string()));
    assert!(status
        .added
        .contains(&"docs/design-docs/history-added.md".to_string()));

    let diff = diff_snapshot_to_worktree(&target, &schema, &first).unwrap();
    assert!(diff.changed.contains(&"docs/project.md".to_string()));
    assert!(diff
        .added
        .contains(&"docs/design-docs/history-added.md".to_string()));

    let plan = restore_plan(&target, &schema, &first).unwrap();
    assert!(plan
        .remove_paths
        .contains(&"docs/design-docs/history-added.md".to_string()));
    apply_restore_plan(&target, &plan).unwrap();

    assert_eq!(
        fs::read_to_string(target.join("docs/project.md")).unwrap(),
        original_project
    );
    assert!(!target.join("docs/design-docs/history-added.md").exists());

    fs::remove_dir_all(target).unwrap();
}

#[test]
fn history_restore_preflight_rejects_missing_objects_without_writing() {
    let target = unique_temp_dir("history-restore-preflight");
    let schema = bundled_schema();
    let schema_copy = render_schema_copy(&schema);

    materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();
    let snapshot = create_history_snapshot(&target, &schema, "baseline").unwrap();
    let project_path = target.join("docs/project.md");
    let changed_project = "---\nstatus: active\nauthority: canonical\n---\n\n# Project\n\nChanged before failed restore.\n";
    fs::write(&project_path, changed_project).unwrap();

    let missing_object = history_object_path(&target, &snapshot.files[0].hash);
    fs::remove_file(missing_object).unwrap();
    let plan = restore_plan(&target, &schema, &snapshot).unwrap();
    let err = apply_restore_plan(&target, &plan).unwrap_err();

    assert!(err.to_string().contains("missing history object"));
    assert_eq!(fs::read_to_string(project_path).unwrap(), changed_project);

    fs::remove_dir_all(target).unwrap();
}

#[test]
fn history_schema_only_change_has_specific_check() {
    let target = unique_temp_dir("history-schema-only");
    fs::create_dir_all(target.join(".git/info")).unwrap();
    let schema = bundled_schema();
    let mut schema_copy = render_schema_copy(&schema);

    materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();
    ensure_host_git_exclude(&target, &schema).unwrap();
    create_history_snapshot(&target, &schema, "baseline").unwrap();
    schema_copy.push_str("\n# local schema comment\n");
    fs::write(target.join(".harnesskit/schema.yaml"), schema_copy).unwrap();

    let checks = build_index_artifact(&target, &schema).unwrap().checks;

    assert!(checks
        .iter()
        .any(|check| check.rule_id == "history-schema-not-snapshotted"));
    assert!(!checks
        .iter()
        .any(|check| check.rule_id == "history-dirty-since-snapshot"));

    fs::remove_dir_all(target).unwrap();
}

#[test]
fn history_checks_and_index_meta_reflect_snapshot_state() {
    let target = unique_temp_dir("history-checks");
    fs::create_dir_all(target.join(".git/info")).unwrap();
    let schema = bundled_schema();
    let schema_copy = render_schema_copy(&schema);

    materialize_from_schema(&target, &schema, &schema_copy, false).unwrap();
    let artifact_without_snapshot = build_index_artifact(&target, &schema).unwrap();
    assert!(artifact_without_snapshot
        .checks
        .iter()
        .any(|check| check.rule_id == "history-missing"));

    ensure_host_git_exclude(&target, &schema).unwrap();
    let snapshot = create_history_snapshot(&target, &schema, "tracked").unwrap();
    fs::write(
        target.join("docs/project.md"),
        "---\nstatus: active\nauthority: canonical\n---\n\n# Project\n\nDirty after snapshot.\n",
    )
    .unwrap();

    let artifact = build_index_artifact(&target, &schema).unwrap();
    assert_eq!(
        artifact.history_latest_snapshot.as_deref(),
        Some(snapshot.id.as_str())
    );
    assert!(artifact.history_dirty);
    assert!(artifact.history_tracked_files_count > 0);
    assert!(artifact
        .checks
        .iter()
        .any(|check| check.rule_id == "history-dirty-since-snapshot"));

    write_fact_store(&target, &artifact).unwrap();
    let db = open_fact_store(&target).unwrap();
    let meta = db
        .query(
            "SELECT value FROM meta WHERE key = 'history_latest_snapshot';",
            [],
        )
        .unwrap();
    assert_eq!(cell(&meta.rows[0], 0), snapshot.id);

    fs::remove_dir_all(target).unwrap();
}
