mod support;

use std::fs;
use std::io::Write;
use std::process::Command;

use serde_json::Value;

use support::TestRepo;

fn write(repo: &TestRepo, path: &str, text: &str) {
    let path = repo.root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn json(repo: &TestRepo, args: &[&str]) -> Value {
    serde_json::from_str(&repo.run(args)).unwrap()
}

fn git(repo: &TestRepo, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(&repo.root)
        .output()
        .unwrap();
    support::assert_success(&output, args);
}

fn commit(repo: &TestRepo, message: &str) {
    git(repo, &["add", "."]);
    git(
        repo,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-qm",
            message,
        ],
    );
}

fn sparse_schema(repo: &TestRepo, version: u32, rules: &str) {
    write(repo, ".harnesskit/schema.yaml", &format!(
        "schema_version: {version}\nmanaged_root: docs\nentrypoints:\n  manifest: docs/index.md\n  project_map: docs/index.md\n{rules}"
    ));
    write(repo, "docs/index.md", "# Docs\n\nProject documentation.\n");
}

fn has_migration_hint(report: &Value) -> bool {
    report["hints"]
        .as_array()
        .unwrap()
        .iter()
        .any(|hint| hint.as_str().unwrap().contains("harnesskit migrate"))
}

#[test]
fn alpha6_style_project_plans_applies_safe_changes_without_touching_body_and_is_idempotent() {
    let repo = TestRepo::new(Some("compat"));
    fs::create_dir_all(repo.root.join(".harnesskit")).unwrap();
    fs::copy(
        repo.root.join("alpha6-schema.yaml"),
        repo.root.join(".harnesskit/schema.yaml"),
    )
    .unwrap();
    fs::OpenOptions::new()
        .append(true)
        .open(repo.root.join(".harnesskit/schema.yaml"))
        .unwrap()
        .write_all(
            b"\nsuppressions:\n  legacy:\n    rule_id: history-missing\n    path: .harnesskit/history\n    reason: legacy fixture\n",
        )
        .unwrap();
    fs::create_dir_all(repo.root.join("docs/design-docs")).unwrap();
    fs::create_dir_all(repo.root.join("docs/decisions")).unwrap();
    fs::write(repo.root.join("docs/index.md"), "# Docs\n\n").unwrap();
    fs::write(repo.root.join("docs/project.md"), "# Project\n\nMap.\n").unwrap();
    fs::write(
        repo.root.join("docs/design-docs/legacy.md"),
        b"---\r\nstatus: active\r\n---\r\n# Legacy\r\n\r\nSuperseded by the new route.\r\n\r\nKeep this body byte-for-byte.\r\n",
    )
    .unwrap();
    fs::write(
        repo.root.join("docs/decisions/current.md"),
        "# Current\n\nThe replacement route.\n",
    )
    .unwrap();
    write(&repo, "src/current.rs", "pub fn current_route() {}\n");
    write(
        &repo,
        "docs/decisions/current.md",
        "# Current\n\nThe replacement route uses `src/current.rs#current_route`.\n",
    );
    git(&repo, &["init", "-q"]);
    git(&repo, &["add", "src"]);
    let original_body = fs::read(repo.root.join("docs/design-docs/legacy.md")).unwrap();

    let plan: Value = serde_json::from_str(&repo.run(&["migrate", "--plan", "--json"])).unwrap();
    let items = plan["items"].as_array().unwrap();
    assert!(items.iter().any(|item| item["category"] == "schema"));
    assert!(items.iter().any(|item| item["category"] == "lifecycle"));
    assert!(items.iter().any(|item| item["category"] == "gitignore"));
    assert!(items
        .iter()
        .any(|item| item["category"] == "knowledge-lock"));
    assert!(items.iter().all(|item| item["evidence"].is_object()));
    assert!(items.iter().all(|item| item["action"].as_str().is_some()));

    let applied: Value =
        serde_json::from_str(&repo.run(&["migrate", "--apply", "--json"])).unwrap();
    assert!(applied["apply"]["applied_count"].as_u64().unwrap() > 0);
    assert!(applied["apply"]["snapshot_message"]
        .as_str()
        .unwrap()
        .contains("snapshot"));
    let schema = fs::read_to_string(repo.root.join(".harnesskit/schema.yaml")).unwrap();
    assert!(schema.contains("schema_version: 1"));
    assert!(schema.contains("rule: history-missing") || schema.contains("rule:"));
    assert!(schema.contains("superseded_by"));
    assert!(fs::read_to_string(repo.root.join(".gitignore"))
        .unwrap()
        .contains(".harnesskit/state/"));
    assert!(repo.root.join(".harnesskit/knowledge.lock").is_file());
    assert!(
        !fs::read_dir(repo.root.join(".harnesskit/history/snapshots"))
            .unwrap()
            .collect::<Vec<_>>()
            .is_empty()
    );
    assert_eq!(
        fs::read(repo.root.join("docs/design-docs/legacy.md")).unwrap(),
        original_body
    );

    let second: Value = serde_json::from_str(&repo.run(&["migrate", "--apply", "--json"])).unwrap();
    assert_eq!(second["apply"]["applied_count"], 0);
    assert!(!second["apply"]["changed"].as_bool().unwrap());
    assert!(second["apply"]["snapshot_skipped"].as_bool().unwrap());
    assert_eq!(
        fs::read(repo.root.join("docs/design-docs/legacy.md")).unwrap(),
        original_body
    );
}

#[test]
fn text_plan_keeps_each_evidence_and_action_with_its_item() {
    let repo = TestRepo::new(None);
    sparse_schema(&repo, 0, "");
    let text = repo.run(&["migrate", "--plan", "--budget", "100000"]);
    let lines = text.lines().collect::<Vec<_>>();
    let mut count = 0;
    for (index, line) in lines.iter().enumerate() {
        if line.starts_with("- [") {
            assert!(lines[index + 1].starts_with("  Evidence: "), "{text}");
            assert!(lines[index + 2].starts_with("  Action: "), "{text}");
            count += 1;
        }
    }
    assert!(count >= 3, "{text}");
}

#[test]
fn pagination_keeps_whole_items_and_page_and_cursor_agree_in_text_and_json() {
    let repo = TestRepo::new(None);
    sparse_schema(&repo, 0, "");
    let plan = json(&repo, &["migrate", "--plan", "--only", "schema", "--json"]);
    let mut cursor: Option<String> = None;
    for (index, item) in plan["items"].as_array().unwrap().iter().enumerate() {
        let page = (index + 1).to_string();
        let text = repo.run(&[
            "migrate", "--plan", "--only", "schema", "--budget", "1", "--page", &page,
        ]);
        let lines = text.lines().collect::<Vec<_>>();
        let head = lines
            .iter()
            .position(|line| line.starts_with("- ["))
            .unwrap();
        assert_eq!(
            lines.iter().filter(|line| line.starts_with("- [")).count(),
            1,
            "{text}"
        );
        assert!(lines[head].ends_with(item["message"].as_str().unwrap()));
        assert!(lines[head + 1].starts_with("  Evidence: "), "{text}");
        assert_eq!(
            lines[head + 2],
            format!("  Action: {}", item["action"].as_str().unwrap())
        );
        let paged = json(
            &repo,
            &[
                "migrate", "--plan", "--only", "schema", "--budget", "1", "--page", &page, "--json",
            ],
        );
        assert_eq!(paged["items"], serde_json::json!([item]));
        if let Some(previous) = &cursor {
            let via_cursor = json(
                &repo,
                &[
                    "migrate", "--plan", "--only", "schema", "--budget", "1", "--cursor", previous,
                    "--json",
                ],
            );
            assert_eq!(via_cursor["items"], paged["items"]);
            let via_cursor = repo.run(&[
                "migrate", "--plan", "--only", "schema", "--budget", "1", "--cursor", previous,
            ]);
            assert_eq!(
                via_cursor.lines().find(|line| line.starts_with("- [")),
                Some(lines[head])
            );
        }
        cursor = paged["next_cursor"].as_str().map(str::to_string);
    }
    assert!(cursor.is_none());
}

#[test]
fn zero_config_projects_do_not_need_schema_or_lock_or_repeat_migration_hints() {
    let repo = TestRepo::new(None);
    write(
        &repo,
        "README.md",
        "# Project\n\nWorks without initialization.\n",
    );
    for _ in 0..2 {
        assert!(!has_migration_hint(&json(&repo, &["doctor", "--json"])));
        let plan = json(&repo, &["migrate", "--plan", "--json"]);
        assert!(plan["items"].as_array().unwrap().iter().all(|item| {
            item["category"] != "schema"
                && item["category"] != "knowledge-lock"
                && item["id"] != "schema-file-missing"
        }));
        assert!(!has_migration_hint(&json(
            &repo,
            &["context", "--current", "--json"]
        )));
        repo.run(&["migrate", "--apply"]);
        assert!(!repo.root.join(".harnesskit/schema.yaml").exists());
        assert!(!repo.root.join(".harnesskit/knowledge.lock").exists());
    }
    assert_eq!(
        json(&repo, &["migrate", "--apply", "--json"])["apply"]["applied_count"],
        0
    );
}

#[test]
fn optional_layout_mappings_do_not_trigger_doctor_and_apply_idempotently() {
    let repo = TestRepo::new(None);
    write(&repo, "docs/index.md", "# Docs\n\n[Notes](misc/note.md)\n");
    write(&repo, "docs/misc/note.md", "# Note\n\nOptional mapping.\n");
    write(&repo, ".gitignore", ".harnesskit/state/\n");
    for _ in 0..2 {
        let plan = json(&repo, &["migrate", "--plan", "--json"]);
        assert!(plan["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["category"] == "layout"));
        assert!(!plan["items"].as_array().unwrap().is_empty());
        assert!(!has_migration_hint(&json(&repo, &["doctor", "--json"])));
        assert!(!repo.root.join(".harnesskit/schema.yaml").exists());
    }
    repo.run(&["migrate", "--apply", "--only", "layout"]);
    assert!(!has_migration_hint(&json(&repo, &["doctor", "--json"])));
    assert_eq!(
        json(&repo, &["migrate", "--apply", "--json"])["apply"]["applied_count"],
        0
    );
}

#[test]
fn knowledge_lock_requires_resolved_explicit_anchors_and_only_apply_writes_it() {
    for (anchor, expected) in [
        ("No inline code", false),
        ("`known_symbol`", false),
        ("`src/missing.rs#unknown`", false),
        ("`src/code.rs#unknown`", false),
        ("`src/code.rs#L1-L2`", false),
        ("`src/code.rs#known_symbol`", true),
        ("`src/code.rs`", true),
        ("`src/`", true),
    ] {
        let repo = TestRepo::new(None);
        write(&repo, "src/code.rs", "pub fn known_symbol() {}\n");
        write(
            &repo,
            "docs/index.md",
            &format!("# Docs\n\nExplicit anchor test: {anchor}.\n"),
        );
        write(&repo, ".gitignore", ".harnesskit/state/\n");
        git(&repo, &["init", "-q"]);
        git(&repo, &["add", "src"]);
        let plan = json(&repo, &["migrate", "--plan", "--json"]);
        assert_eq!(
            plan["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["category"] == "knowledge-lock"),
            expected,
            "{anchor}: {plan}"
        );
        let lock = repo.root.join(".harnesskit/knowledge.lock");
        assert!(!lock.exists(), "plan wrote {anchor}");
        let applied = json(
            &repo,
            &["migrate", "--apply", "--only", "knowledge-lock", "--json"],
        );
        assert_eq!(
            applied["apply"]["applied_count"],
            usize::from(expected),
            "{anchor}: {applied}"
        );
        assert_eq!(lock.exists(), expected, "{anchor}");
        let before = fs::read(&lock).ok();
        assert_eq!(
            json(
                &repo,
                &["migrate", "--apply", "--only", "knowledge-lock", "--json"]
            )["apply"]["applied_count"],
            0
        );
        assert_eq!(fs::read(&lock).ok(), before);
        if expected {
            assert!(String::from_utf8(before.unwrap())
                .unwrap()
                .contains("docs/index.md\t"));
        }
    }
}

#[test]
fn apply_edits_only_target_schema_bytes_preserving_comments_quotes_order_and_crlf() {
    let repo = TestRepo::new(None);
    let raw = concat!(
        "# Keep this preamble.\nmanaged_root: docs\nschema_version : 0  # legacy\n",
        "entrypoints:\n  project_map: docs/index.md\n  manifest: docs/index.md\n\n",
        "# Keep release and agent documentation in place.\ninclude:\n  'docs/mapped/*.md': notes # existing glob\n\n",
        "rules: # maintenance\n  minimal_frontmatter_only: true\n  index_required: false # keep ordering\n\n",
        "doc_collections:\n  decisions:\n    template: decision\n    path: decisions\n    allowed_frontmatter: [status, 'authority', supersedes]  # existing quotes\n",
        "  exec_plans_active:\n    path: exec-plans/active\n    template: exec-plan\n    allowed_frontmatter: [] # empty list\n",
        "  references:\n    path: references\n    template: reference\n    # No allowlist yet.\n\n",
        "suppressions:\n  legacy:\n    reason: 'Keep quoted # text: yes'\n    path: .harnesskit/history\n    rule_id: history-schema-not-snapshotted # keep inline comment\n",
    ).replace('\n', "\r\n");
    write(&repo, ".harnesskit/schema.yaml", &raw);
    write(
        &repo,
        "docs/index.md",
        "# Docs\n\n[Details](misc/note.md)\n",
    );
    write(
        &repo,
        "docs/misc/note.md",
        "# Note\n\nPreserve document bodies.\n",
    );
    let expected = raw
        .replace("schema_version : 0", "schema_version : 1")
        .replace("  'docs/mapped/*.md': notes # existing glob\r\n", "  'docs/mapped/*.md': notes # existing glob\r\n  \"docs/misc/**/*.md\": notes\r\n")
        .replace("  index_required: false # keep ordering\r\n", "  index_required: false # keep ordering\r\n  entrypoint_max_lines: 120\r\n  stale_active_plan_days: 30\r\n")
        .replace("[status, 'authority', supersedes]", "[status, 'authority', supersedes, superseded_by, supersedes_parts_of]")
        .replace("allowed_frontmatter: []", "allowed_frontmatter: [superseded_by, supersedes_parts_of, status]")
        .replace("    template: reference\r\n", "    template: reference\r\n    allowed_frontmatter: [superseded_by, supersedes_parts_of]\r\n")
        .replace("    rule_id:", "    rule:");
    let report = json(&repo, &["migrate", "--apply", "--json"]);
    assert!(report["apply"]["changed"].as_bool().unwrap());
    assert_eq!(
        fs::read_to_string(repo.root.join(".harnesskit/schema.yaml")).unwrap(),
        expected
    );
    assert_eq!(
        repo.run(&["history", "show", "latest", ".harnesskit/schema.yaml"]),
        raw
    );
    repo.run(&["history", "snapshot", "-m", "reviewed migration"]);
    let check = json(&repo, &["check", "--strict", "--all", "--json"]);
    assert_eq!(check["summary"]["error"], 0);
    assert_eq!(check["summary"]["warning"], 0);
    let second = json(&repo, &["migrate", "--apply", "--json"]);
    assert_eq!(second["apply"]["applied_count"], 0);
    assert_eq!(
        fs::read_to_string(repo.root.join(".harnesskit/schema.yaml")).unwrap(),
        expected
    );
}

#[test]
fn missing_schema_fields_and_sequence_suppressions_preserve_other_bytes_and_no_final_newline() {
    let repo = TestRepo::new(None);
    let raw = "# schema_version: example only\nmanaged_root: docs\nentrypoints:\n  manifest: docs/index.md\n  project_map: docs/index.md\nsuppressions:\n  - rule_id: history-missing # legacy list form\n    path: .harnesskit/history\n    reason: 'Keep rule_id: and # inside a quote'";
    write(&repo, ".harnesskit/schema.yaml", raw);
    write(
        &repo,
        "docs/index.md",
        "# Docs\n\nNothing to migrate in prose.\n",
    );
    repo.run(&["migrate", "--apply", "--only", "schema"]);
    let expected = format!(
        "schema_version: 1\n{}\nrules:\n  entrypoint_max_lines: 120\n  stale_active_plan_days: 30",
        raw.replace("  - rule_id:", "  - rule:")
    );
    assert_eq!(
        fs::read_to_string(repo.root.join(".harnesskit/schema.yaml")).unwrap(),
        expected
    );
    assert!(!expected.contains("core_files:"));
    assert!(!expected.contains("doc_collections:"));
    assert_eq!(
        json(&repo, &["migrate", "--apply", "--only", "schema", "--json"])["apply"]
            ["applied_count"],
        0
    );
    assert_eq!(json(&repo, &["check", "--json"])["summary"]["error"], 0);
}

#[test]
fn context_has_no_migration_nudge_without_session_id_while_doctor_still_reports_it() {
    let repo = TestRepo::new(None);
    sparse_schema(&repo, 0, "");
    for _ in 0..2 {
        let doctor = json(&repo, &["doctor", "--json"]);
        assert!(has_migration_hint(&doctor));
        assert_eq!(
            doctor["hints"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|hint| hint.as_str().unwrap() == "Run harnesskit migrate --plan")
                .count(),
            1
        );
        assert!(!has_migration_hint(&json(
            &repo,
            &["context", "--current", "--json"]
        )));
        assert!(!repo
            .run(&["context", "--current"])
            .contains("harnesskit migrate"));
    }
    repo.run(&["migrate", "--apply", "--only", "schema"]);
    assert!(!has_migration_hint(&json(&repo, &["doctor", "--json"])));
}

#[test]
fn applying_one_category_preserves_other_pending_schema_and_lock_work() {
    let repo = TestRepo::new(None);
    sparse_schema(&repo, 0, "");
    write(&repo, ".gitignore", ".harnesskit/state/\n");
    write(&repo, "src/code.rs", "pub fn known_symbol() {}\n");
    write(
        &repo,
        "docs/index.md",
        "# Docs\n\n[Note](misc/note.md) uses `src/code.rs#known_symbol`.\n",
    );
    write(
        &repo,
        "docs/misc/note.md",
        "# Note\n\nThe body stays unchanged.\n",
    );
    git(&repo, &["init", "-q"]);
    git(&repo, &["add", "src"]);
    let schema_path = repo.root.join(".harnesskit/schema.yaml");
    let raw = fs::read_to_string(&schema_path).unwrap();

    let layout = json(&repo, &["migrate", "--apply", "--only", "layout", "--json"]);
    assert_eq!(layout["apply"]["applied_count"], 1);
    assert_eq!(
        fs::read_to_string(&schema_path).unwrap(),
        format!("{raw}include:\n  \"docs/misc/**/*.md\": notes\n")
    );
    assert!(!repo.root.join(".harnesskit/knowledge.lock").exists());
    assert!(has_migration_hint(&json(&repo, &["doctor", "--json"])));

    repo.run(&["migrate", "--apply", "--only", "schema"]);
    assert!(!repo.root.join(".harnesskit/knowledge.lock").exists());
    assert!(!has_migration_hint(&json(&repo, &["doctor", "--json"])));
    let schema = fs::read(&schema_path).unwrap();
    assert_eq!(
        json(&repo, &["migrate", "--apply", "--only", "schema", "--json"])["apply"]
            ["applied_count"],
        0
    );
    assert_eq!(fs::read(&schema_path).unwrap(), schema);
    assert_eq!(
        json(
            &repo,
            &["migrate", "--plan", "--only", "knowledge-lock", "--json"]
        )["total"],
        1
    );
}

#[test]
fn layout_appends_a_quoted_literal_mapping_for_paths_with_glob_and_yaml_characters() {
    let repo = TestRepo::new(None);
    sparse_schema(
        &repo,
        1,
        "rules:\n  entrypoint_max_lines: 120\n  stale_active_plan_days: 30\n",
    );
    let path = "docs/misc#topic: [v1]/note.md";
    write(&repo, path, "# Note\n\nLiteral directory name.\n");
    let schema_path = repo.root.join(".harnesskit/schema.yaml");
    let before = fs::read_to_string(&schema_path).unwrap();
    let applied = json(&repo, &["migrate", "--apply", "--only", "layout", "--json"]);
    assert_eq!(applied["apply"]["applied_count"], 1);
    assert!(fs::read_to_string(&schema_path)
        .unwrap()
        .starts_with(&before));
    let check = json(&repo, &["check", "--json", "--rules", "unmanaged-docs"]);
    assert_eq!(check["coverage"]["unmanaged"], 0);
    assert!(check["coverage"]["mappings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|mapping| { mapping["path"] == path && mapping["collection"] == "notes" }));
    assert_eq!(
        json(&repo, &["migrate", "--apply", "--only", "layout", "--json"])["apply"]
            ["applied_count"],
        0
    );
}

#[test]
fn anchor_migrations_group_candidates_by_document_and_preview_only_three() {
    let repo = TestRepo::new(None);
    write(&repo, ".gitignore", ".harnesskit/state/\n");
    git(&repo, &["init", "-q"]);
    commit(&repo, "ignore derived state");
    write(&repo, "src/code.rs", "pub fn first_operation() {}\npub fn second_operation() {}\npub fn third_operation() {}\npub fn fourth_operation() {}\n");
    write(&repo, "docs/index.md", "# Docs\n\n[Policy](policy.md)\n");
    write(&repo, "docs/policy.md", "# Policy\n\nThe policy mentions `first_operation`, `second_operation`, `third_operation` and `fourth_operation`.\n");
    commit(&repo, "code and policy");
    write(&repo, "src/code.rs", "pub fn first_operation() { let _value = 1; }\npub fn second_operation() {}\npub fn third_operation() {}\npub fn fourth_operation() {}\n");
    let suggestions = json(
        &repo,
        &[
            "suggest-anchors",
            "docs/policy.md",
            "--json",
            "--budget",
            "100000",
        ],
    );
    assert_eq!(suggestions["suggestions"].as_array().unwrap().len(), 4);
    let plan = json(
        &repo,
        &[
            "migrate", "--plan", "--only", "anchors", "--json", "--budget", "100000",
        ],
    );
    let items = plan["items"].as_array().unwrap();
    let policy = items
        .iter()
        .filter(|item| item["path"] == "docs/policy.md")
        .collect::<Vec<_>>();
    assert_eq!(policy.len(), 1, "{plan}");
    assert_eq!(policy[0]["evidence"]["candidate_count"], 4);
    assert_eq!(
        policy[0]["evidence"]["candidates"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(policy[0]["evidence"]["omitted_candidates"], 1);
    assert!(policy[0]["action"]
        .as_str()
        .unwrap()
        .contains("harnesskit suggest-anchors docs/policy.md"));
    for candidate in policy[0]["evidence"]["candidates"].as_array().unwrap() {
        assert!(candidate["summary"]
            .as_str()
            .unwrap()
            .contains("Document mentions"));
        assert!(suggestions["suggestions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|suggestion| suggestion["anchor"] == candidate["anchor"]));
    }
    let text = repo.run(&[
        "migrate", "--plan", "--only", "anchors", "--budget", "100000",
    ]);
    let lines = text.lines().collect::<Vec<_>>();
    let policy_line = lines
        .iter()
        .position(|line| line.starts_with("- [anchors] docs/policy.md:"))
        .unwrap();
    for candidate in policy[0]["evidence"]["candidates"].as_array().unwrap() {
        let symbol = candidate["anchor"]
            .as_str()
            .unwrap()
            .split_once('#')
            .unwrap()
            .1;
        assert!(lines[policy_line + 1].contains(symbol), "{text}");
    }
}

#[test]
fn weak_cochange_history_is_filtered_by_both_suggest_anchors_and_migrate() {
    let repo = TestRepo::new(None);
    write(&repo, ".gitignore", ".harnesskit/state/\n");
    git(&repo, &["init", "-q"]);
    commit(&repo, "ignore derived state");
    write(
        &repo,
        "docs/policy.md",
        "# Policy\n\nKeep processing stable.\n",
    );
    write(&repo, "src/code.rs", "pub fn process() -> u32 { 0 }\n");
    commit(&repo, "one shared change");
    for value in 1..=5 {
        write(
            &repo,
            "src/code.rs",
            &format!("pub fn process() -> u32 {{ {value} }}\n"),
        );
        commit(&repo, "code only");
    }
    write(&repo, "src/code.rs", "pub fn process() -> u32 { 99 }\n");
    let suggestions = json(&repo, &["suggest-anchors", "docs/policy.md", "--json"]);
    assert!(
        suggestions["suggestions"].as_array().unwrap().is_empty(),
        "{suggestions}"
    );
    let db = rusqlite::Connection::open(repo.root.join(".harnesskit/state/facts.sqlite")).unwrap();
    let weight: f64 = db.query_row("SELECT weight FROM cochange_stats WHERE doc_path = 'docs/policy.md' AND code_path = 'src/code.rs'", [], |row| row.get(0)).unwrap();
    assert!(weight > 0.0 && weight < 0.2, "{weight}");
    let plan = json(&repo, &["migrate", "--plan", "--only", "anchors", "--json"]);
    assert!(plan["items"].as_array().unwrap().is_empty(), "{plan}");
    assert!(!repo.root.join(".harnesskit/knowledge.lock").exists());
}
