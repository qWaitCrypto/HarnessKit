mod support;

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use serde_json::Value;
use support::{copy_tree, TestRepo};

fn json(repo: &TestRepo, args: &[&str]) -> Value {
    serde_json::from_str(&repo.run(args)).unwrap()
}

fn write(repo: &TestRepo, path: &str, text: &str) {
    let path = repo.root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn schema(repo: &TestRepo, extra: &str) {
    write(
        repo,
        ".harnesskit/schema.yaml",
        &format!(
            "{}\n{}",
            include_str!("fixtures/compat/alpha6-schema.yaml"),
            extra
        ),
    );
}

fn paths(report: &Value) -> BTreeSet<&str> {
    report["docs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|doc| doc["path"].as_str().unwrap())
        .collect()
}

fn mapping<'a>(report: &'a Value, path: &str) -> &'a Value {
    report["coverage"]["mappings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["path"] == path)
        .unwrap()
}

#[test]
fn all_five_fixtures_share_coverage_across_index_check_list_and_history() {
    for (fixture, before, expected) in [
        ("standard", 25, 25),
        ("flat", 3, 8),
        ("cjk", 5, 5),
        ("lifecycle", 9, 9),
        ("external", 3, 3),
    ] {
        let repo = TestRepo::new(Some(fixture));
        let index = json(&repo, &["index", "--json"]);
        let coverage = &index["coverage"];
        assert_eq!(coverage["markdown_total"], expected, "{}", fixture);
        assert_eq!(coverage["managed"], expected);
        assert_eq!(coverage["unmanaged"], 0);
        assert_eq!(coverage["ratio"], 1.0);
        assert!(index["hints"].is_array());
        for command in [
            vec!["check", "--json"],
            vec!["list-docs", "--json"],
            vec!["history", "snapshot", "--json"],
        ] {
            let report = json(&repo, &command);
            assert_eq!(&report["coverage"], coverage, "{} {:?}", fixture, command);
            assert!(!report["hints"].as_array().unwrap().is_empty());
            if command[0] == "history" {
                let files = report["files"].as_array().unwrap();
                assert_eq!(files.len(), expected);
                assert!(!files
                    .iter()
                    .any(|file| file["path"].as_str().unwrap().starts_with("research_runs/")));
            }
        }
        if fixture == "flat" {
            assert!(expected > before * 2);
        }
        if fixture == "external" {
            assert_eq!(index["external_refs"].as_array().unwrap().len(), 1);
            assert_eq!(
                index["external_refs"][0]["target_path"],
                "research_runs/x/result.json"
            );
        }
        for args in [
            vec!["index"],
            vec!["check"],
            vec!["list-docs"],
            vec!["history", "snapshot"],
        ] {
            assert!(repo.run(&args).contains("Coverage:"));
        }
    }
}

#[test]
fn flat_roles_authorities_queries_and_history_roundtrip_are_adaptive() {
    let repo = TestRepo::new(Some("flat"));
    let report = json(&repo, &["list-docs", "--json"]);
    for (path, role, authority, reason) in [
        ("docs/principles.md", "notes", "supporting", "flat Markdown"),
        (
            "docs/decisions.md",
            "decisions",
            "canonical",
            "filename heuristic",
        ),
        (
            "docs/execution-plan.md",
            "exec_plans",
            "supporting",
            "filename heuristic",
        ),
        (
            "docs/reference/relay.md",
            "references",
            "reference",
            "directory alias",
        ),
    ] {
        let doc = report["docs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|doc| doc["path"] == path)
            .unwrap();
        assert_eq!(doc["role"], role);
        assert_eq!(doc["authority"], authority);
        assert!(mapping(&report, path)["reason"]
            .as_str()
            .unwrap()
            .contains(reason));
    }
    write(
        &repo,
        "docs/principles.md",
        "# Principles\n\nUniqueadaptivetoken survives a restore.\n",
    );
    let query = json(&repo, &["query", "uniqueadaptivetoken", "--json"]);
    assert_eq!(query["candidates"][0]["path"], "docs/principles.md");
    repo.run(&["history", "snapshot"]);
    write(&repo, "docs/principles.md", "# Changed\n");
    assert!(repo
        .run(&["history", "diff", "latest"])
        .contains("docs/principles.md"));
    repo.run(&["history", "restore", "latest", "--apply", "--force"]);
    assert!(fs::read_to_string(repo.root.join("docs/principles.md"))
        .unwrap()
        .contains("Uniqueadaptivetoken"));
    let checks = json(&repo, &["check", "--json"]);
    assert!(!checks["checks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|check| check["rule_id"] == "missing-collection-index"
            && check["subject_path"].as_str().unwrap().contains("notes")));
}

#[test]
fn aliases_and_filename_heuristics_are_explained_and_overridable() {
    let repo = TestRepo::new(None);
    for (directory, role) in [
        ("reference", "references"),
        ("adr", "decisions"),
        ("adrs", "decisions"),
        ("plans", "exec_plans"),
        ("design", "design_docs"),
        ("specs", "product_specs"),
        ("runbooks", "operations"),
    ] {
        write(
            &repo,
            &format!("docs/{}/nested/topic.md", directory),
            "# Topic\n",
        );
        let report = json(&repo, &["doctor", "--layout", "--json"]);
        let path = format!("docs/{}/nested/topic.md", directory);
        assert_eq!(mapping(&report, &path)["collection"], role);
        assert!(mapping(&report, &path)["reason"]
            .as_str()
            .unwrap()
            .contains("directory alias"));
    }
    for (name, role) in [
        ("architecture-overview.md", "design_docs"),
        ("some-decision.md", "decisions"),
        ("release-plan.md", "exec_plans"),
    ] {
        write(&repo, &format!("docs/{}", name), "# Inferred\n");
        let report = json(&repo, &["list-docs", "--json"]);
        assert_eq!(mapping(&report, &format!("docs/{}", name))["role"], role);
    }
    schema(
        &repo,
        "include:\n  \"docs/adr/**/*.md\": notes\n  \"docs/release-plan.md\": references\n",
    );
    let report = json(&repo, &["list-docs", "--json"]);
    assert_eq!(
        mapping(&report, "docs/adr/nested/topic.md")["role"],
        "notes"
    );
    assert_eq!(
        mapping(&report, "docs/release-plan.md")["role"],
        "references"
    );
}

#[test]
fn ignore_include_globs_and_schema_edits_refresh_without_document_edits() {
    let repo = TestRepo::new(Some("flat"));
    write(&repo, "docs/misc/a.md", "# A\n");
    write(&repo, "docs/misc/nested/b.md", "# B\n");
    write(&repo, "docs/misc/skip.md", "# Ignored\n");
    write(&repo, "research/evidence.md", "# Evidence\n");
    schema(&repo, "include:\n  'docs/misc/**/*.md': notes\n  'research/*.md': references\nignore:\n  - 'docs/misc/skip.md'\n  - 'docs/decisions.md'\n");
    let report = json(&repo, &["index", "--json"]);
    assert!(paths(&report).contains("docs/misc/a.md"));
    assert!(paths(&report).contains("docs/misc/nested/b.md"));
    assert!(paths(&report).contains("research/evidence.md"));
    assert!(!paths(&report).contains("docs/misc/skip.md"));
    assert_eq!(report["coverage"]["excluded"], 2);
    assert_eq!(report["coverage"]["ratio"], 1.0);
    schema(
        &repo,
        "include:\n  'docs/misc/**/*.md': references\nignore: ['docs/principles.md']\n",
    );
    let report = json(&repo, &["index", "--json"]);
    assert_eq!(
        mapping(&report, "docs/misc/a.md")["collection"],
        "references"
    );
    assert_eq!(mapping(&report, "docs/principles.md")["status"], "excluded");
    assert!(!paths(&report).contains("research/evidence.md"));
    assert_eq!(report["coverage"]["excluded"], 1);
}

#[test]
fn unknown_directories_are_aggregated_live_and_hint_can_be_pasted() {
    let repo = TestRepo::new(Some("standard"));
    repo.run(&["index"]);
    for name in ["a.md", "b.MARKDOWN"] {
        write(&repo, &format!("docs/misc/{}", name), "# Unknown\n");
    }
    let report = json(&repo, &["check", "--json", "--rules", "unmanaged-docs"]);
    let checks = report["checks"].as_array().unwrap();
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0]["subject_path"], "docs/misc");
    assert_eq!(
        checks[0]["evidence_json"]["paths"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(report["coverage"]["unmanaged"], 2);
    assert_eq!(report["coverage"]["ratio"].as_f64().unwrap(), 25.0 / 27.0);
    for args in [
        vec!["index", "--json"],
        vec!["list-docs", "--json"],
        vec!["history", "snapshot", "--json"],
        vec!["doctor", "--layout", "--json"],
    ] {
        assert_eq!(
            json(&repo, &args)["coverage"],
            report["coverage"],
            "{:?}",
            args
        );
    }

    let hint = checks[0]["evidence_json"]["hint"].as_str().unwrap();
    schema(&repo, hint);
    let report = json(&repo, &["check", "--json", "--rules", "unmanaged-docs"]);
    assert!(report["checks"].as_array().unwrap().is_empty());
    assert_eq!(report["coverage"]["unmanaged"], 0);
    assert!(paths(&json(&repo, &["list-docs", "--json"])).contains("docs/misc/a.md"));
}

#[test]
fn broken_links_are_errors_and_existing_unmanaged_links_are_not_duplicated() {
    let repo = TestRepo::new(Some("flat"));
    write(&repo, "docs/misc/evidence.md", "# Evidence\n");
    write(
        &repo,
        "docs/principles.md",
        "# Principles\n\n[missing](missing.json) [existing](misc/evidence.md)\n",
    );
    let args = ["check", "--json", "--rules", "broken-link,unmanaged-docs"];
    let output = repo.command(&args).output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    let checks = report["checks"].as_array().unwrap();
    assert_eq!(checks.len(), 2);
    assert!(checks
        .iter()
        .any(|check| check["rule_id"] == "broken-link" && check["severity"] == "error"));
    write(&repo, "docs/missing.json", "{}\n");
    let report = json(&repo, &args);
    assert_eq!(report["checks"].as_array().unwrap().len(), 1);
    fs::remove_file(repo.root.join("docs/misc/evidence.md")).unwrap();
    let output = repo.command(&args).output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["checks"].as_array().unwrap().len(), 1);
    assert_eq!(report["checks"][0]["rule_id"], "broken-link");
}

#[test]
fn external_hash_changes_survive_queries_and_clear_when_referring_doc_is_updated() {
    let repo = TestRepo::new(Some("external"));
    let first = json(&repo, &["index", "--json"]);
    let hash = first["external_refs"][0]["content_hash"].clone();
    write(&repo, "research_runs/x/result.json", "{\"latency\":99}\n");
    repo.run(&["query", "quartz", "--json"]);
    for _ in 0..2 {
        let report = json(
            &repo,
            &["check", "--json", "--rules", "external-ref-changed"],
        );
        assert_eq!(report["checks"].as_array().unwrap().len(), 1);
        assert_eq!(
            report["checks"][0]["subject_path"],
            "docs/design-docs/probe.md"
        );
        assert_eq!(report["checks"][0]["evidence_json"]["recorded_hash"], hash);
    }
    let snapshot = json(&repo, &["history", "snapshot", "--json"]);
    assert!(!snapshot["files"]
        .as_array()
        .unwrap()
        .iter()
        .any(|file| file["path"] == "research_runs/x/result.json"));
    write(
        &repo,
        "docs/design-docs/probe.md",
        "# Reviewed probe\n\nReviewed `research_runs/x/result.json`.\n",
    );
    let report = json(
        &repo,
        &["check", "--json", "--rules", "external-ref-changed"],
    );
    assert!(report["checks"].as_array().unwrap().is_empty());
    let index = json(&repo, &["index", "--json"]);
    assert_eq!(index["external_refs"].as_array().unwrap().len(), 1);
    assert_ne!(index["external_refs"][0]["content_hash"], hash);
    write(&repo, "docs/design-docs/probe.md", "# No evidence\n");
    assert!(json(&repo, &["index", "--json"])["external_refs"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
fn external_inline_relative_paths_and_binary_hashes_are_supported() {
    let repo = TestRepo::new(Some("external"));
    fs::write(repo.root.join("research_runs/x/result.json"), [0, 255, 1]).unwrap();
    write(
        &repo,
        "docs/design-docs/probe.md",
        "# Probe\n\n`../../research_runs/x/result.json`\n",
    );
    let first = json(&repo, &["index", "--json"]);
    assert_eq!(first["external_refs"].as_array().unwrap().len(), 1);
    fs::write(repo.root.join("research_runs/x/result.json"), [0, 254, 1]).unwrap();
    let report = json(
        &repo,
        &["check", "--json", "--rules", "external-ref-changed"],
    );
    assert_eq!(report["checks"].as_array().unwrap().len(), 1);
}

#[test]
fn doctor_schema_preview_force_and_roundtrip_preserve_configuration() {
    let repo = TestRepo::new(Some("flat"));
    let before = json(&repo, &["doctor", "--layout", "--json"]);
    assert!(!repo.root.join(".harnesskit").exists());
    let written = json(&repo, &["doctor", "--layout", "--write-schema", "--json"]);
    assert_eq!(written["schema_written"], true);
    let schema_path = repo.root.join(".harnesskit/schema.yaml");
    let saved = fs::read(&schema_path).unwrap();
    write(&repo, "docs/architecture-map.md", "# Architecture\n");
    let preview = json(&repo, &["doctor", "--write-schema", "--json"]);
    assert_eq!(preview["schema_written"], false);
    assert_eq!(fs::read(&schema_path).unwrap(), saved);
    assert!(preview["schema_preview"]
        .as_str()
        .unwrap()
        .contains("architecture-map.md"));
    repo.run(&["doctor", "--write-schema", "--force", "--preview"]);
    assert_eq!(fs::read(&schema_path).unwrap(), saved);
    let written = json(&repo, &["doctor", "--write-schema", "--force", "--json"]);
    assert_eq!(written["schema_written"], true);
    let after = json(&repo, &["list-docs", "--json"]);
    assert_eq!(
        after["coverage"]["managed"].as_u64().unwrap(),
        before["coverage"]["managed"].as_u64().unwrap() + 1
    );
    assert_eq!(mapping(&after, "docs/principles.md")["role"], "notes");
    assert_eq!(
        mapping(&after, "docs/architecture-map.md")["role"],
        "design_docs"
    );
}

#[test]
fn explicit_schema_and_docs_root_disable_repository_fallback() {
    let repo = TestRepo::new(None);
    write(&repo, "README.md", "# Project\n");
    write(&repo, "guides/topic.md", "# Topic\n");
    let inferred = json(&repo, &["list-docs", "--json"]);
    assert_eq!(inferred["coverage"]["managed"], 2);
    assert_eq!(inferred["coverage"]["scan_root"], ".");
    assert!(inferred["hints"]
        .as_array()
        .unwrap()
        .iter()
        .any(|hint| hint.as_str().unwrap().contains("No default docs/")));
    assert_eq!(
        json(&repo, &["list-docs", "--docs-root", "knowledge", "--json"])["coverage"]["managed"],
        0
    );
    schema(&repo, "");
    assert_eq!(
        json(&repo, &["list-docs", "--json"])["coverage"]["managed"],
        0
    );
}

#[test]
fn nested_projects_and_fixtures_are_excluded_and_explained_across_commands() {
    for prefix in ["", "docs/"] {
        let repo = TestRepo::new(None);
        let mut expected = vec!["AGENTS.md".to_string(), "CLAUDE.md".to_string()];
        for path in ["index.md", "reference/fixtures/guide.md", "testdata-notes.md"] {
            expected.push(format!("{}{}", prefix, path));
        }
        if prefix.is_empty() {
            expected.extend([
                "examples/README.md".to_string(),
                "examples/tutorial/guide.md".to_string(),
                "testing/fixtures/guide.md".to_string(),
            ]);
        }
        for path in &expected {
            write(&repo, path, "# Repository documentation\n");
        }
        let excluded = [
            ("projects/agents/AGENTS.md", "nested-project"),
            ("projects/agents/nested/topic.md", "nested-project"),
            ("projects/claude/CLAUDE.md", "nested-project"),
            ("projects/claude/topic.md", "nested-project"),
            ("projects/state/nested/topic.md", "nested-project"),
            ("tests/fixtures/README.md", "fixture-dir"),
            ("tests/fixtures/plain/nested/data.MARKDOWN", "fixture-dir"),
            ("tests/fixtures/project/AGENTS.md", "fixture-dir"),
            ("tests/fixtures/project/topic.md", "fixture-dir"),
            ("testdata/input.md", "fixture-dir"),
            ("packages/worker/testdata/nested/input.md", "fixture-dir"),
            ("packages/worker/tests/fixtures/input.md", "fixture-dir"),
            ("examples/fixture/input.md", "fixture-dir"),
            ("examples/fixtures/input.md", "fixture-dir"),
            ("examples/demo/fixtures/input.md", "fixture-dir"),
        ];
        for (path, _) in excluded {
            write(
                &repo,
                &format!("{}{}", prefix, path),
                "# Excludedfixturetoken\n\n[Sample missing link](missing.md)\n",
            );
        }
        fs::create_dir_all(
            repo.root
                .join(format!("{}projects/state/.harnesskit", prefix)),
        )
        .unwrap();
        write(
            &repo,
            "AGENTS.md",
            &format!(
                "# Routes\n\n[Project]({}projects/agents/nested/topic.md) [Fixture]({}testdata/input.md)\n",
                prefix, prefix
            ),
        );

        let report = json(&repo, &["list-docs", "--json"]);
        let expected = expected.iter().map(String::as_str).collect::<BTreeSet<_>>();
        assert_eq!(paths(&report), expected);
        let coverage = &report["coverage"];
        assert_eq!(coverage["managed"], expected.len());
        assert_eq!(coverage["excluded"], excluded.len());
        assert_eq!(coverage["markdown_total"], expected.len() + excluded.len());
        assert_eq!(coverage["unmanaged"], 0);
        assert_eq!(coverage["ratio"], 1.0);
        assert_eq!(
            coverage["by_dir"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|dir| dir["status"] == "excluded")
                .map(|dir| dir["count"].as_u64().unwrap())
                .sum::<u64>(),
            excluded.len() as u64
        );
        for (path, reason) in excluded {
            let item = mapping(&report, &format!("{}{}", prefix, path));
            assert_eq!(item["status"], "excluded");
            assert_eq!(item["reason"], reason);
            assert!(item["role"].is_null());
        }
        for args in [
            vec!["index", "--json"],
            vec!["check", "--json", "--rules", "broken-link,unmanaged-docs"],
            vec!["history", "snapshot", "--json"],
            vec!["doctor", "--layout", "--json"],
        ] {
            let result = json(&repo, &args);
            assert_eq!(&result["coverage"], coverage, "{:?}", args);
            if args[0] == "history" {
                let files = result["files"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|file| file["path"].as_str().unwrap())
                    .collect::<BTreeSet<_>>();
                assert_eq!(files, expected);
            } else if args[0] == "check" {
                assert!(result["checks"].as_array().unwrap().is_empty());
            }
        }
        let query = json(&repo, &["query", "excludedfixturetoken", "--json"]);
        assert!(query["candidates"].as_array().unwrap().is_empty());

        let nested = json(
            &repo,
            &["list-docs", &format!("{}projects/agents", prefix), "--json"],
        );
        assert_eq!(
            paths(&nested),
            BTreeSet::from(["AGENTS.md", "nested/topic.md"])
        );
        assert_eq!(nested["coverage"]["excluded"], 0);
    }
}

#[test]
fn schema_include_overrides_default_exclusions_but_ignore_wins() {
    let repo = TestRepo::new(None);
    let included = [
        "projects/child/AGENTS.md",
        "projects/child/nested/guide.md",
        "tests/fixtures/sample/readme.md",
        "testdata/sample.md",
        "examples/demo/fixtures/sample.md",
    ];
    write(&repo, "README.md", "# Repository\n");
    for path in included
        .iter()
        .copied()
        .chain(["testdata/ignored.md", "tests/fixtures/unselected.md"])
    {
        write(&repo, path, "# Sample\n");
    }
    assert_eq!(
        paths(&json(&repo, &["list-docs", "--json"])),
        BTreeSet::from(["README.md"])
    );
    let config = include_str!("fixtures/compat/alpha6-schema.yaml")
        .replace("managed_root: docs", "managed_root: .");
    write(
        &repo,
        ".harnesskit/schema.yaml",
        &format!("{}\ninclude:\n  'projects/child/**': references\n  'tests/fixtures/sample/**': notes\n  'testdata/**': notes\n  'examples/**/fixtures/*.md': notes\nignore:\n  - 'testdata/ignored.md'\n", config),
    );
    let report = json(&repo, &["list-docs", "--json"]);
    assert_eq!(report["coverage"]["managed"], included.len() + 1);
    assert_eq!(report["coverage"]["excluded"], 2);
    for path in included {
        assert!(paths(&report).contains(path));
        assert!(mapping(&report, path)["reason"]
            .as_str()
            .unwrap()
            .starts_with("schema.include:"));
    }
    assert_eq!(
        mapping(&report, "projects/child/nested/guide.md")["role"],
        "references"
    );
    assert_eq!(mapping(&report, "testdata/ignored.md")["status"], "excluded");
    assert_eq!(
        mapping(&report, "testdata/ignored.md")["reason"],
        "schema.ignore: testdata/ignored.md"
    );
    assert_eq!(
        mapping(&report, "tests/fixtures/unselected.md")["reason"],
        "fixture-dir"
    );
    repo.run(&["doctor", "--write-schema", "--force"]);
    let persisted = json(&repo, &["list-docs", "--json"]);
    assert_eq!(paths(&persisted), paths(&report));
    assert_eq!(persisted["coverage"]["excluded"], 2);
    assert_eq!(
        mapping(&persisted, "tests/fixtures/unselected.md")["reason"],
        "fixture-dir"
    );
}

#[test]
fn nested_project_markers_refresh_existing_index_without_document_edits() {
    for marker in ["AGENTS.md", "CLAUDE.md", ".harnesskit"] {
        let repo = TestRepo::new(None);
        write(&repo, "README.md", "# Repository\n");
        write(&repo, "child/nested/topic.md", "# Nestedmarkertoken\n");
        let before = json(&repo, &["list-docs", "--json"]);
        assert!(paths(&before).contains("child/nested/topic.md"));
        let marker_path = repo.root.join("child").join(marker);
        if marker == ".harnesskit" {
            fs::create_dir(&marker_path).unwrap();
        } else {
            fs::write(&marker_path, "# Child routes\n").unwrap();
        }
        let report = json(&repo, &["list-docs", "--json"]);
        assert_eq!(paths(&report), BTreeSet::from(["README.md"]));
        assert_eq!(
            mapping(&report, "child/nested/topic.md")["reason"],
            "nested-project"
        );
        let query = json(&repo, &["query", "nestedmarkertoken", "--json"]);
        assert!(query["candidates"].as_array().unwrap().is_empty());
        if marker == ".harnesskit" {
            fs::remove_dir(&marker_path).unwrap();
        } else {
            fs::remove_file(&marker_path).unwrap();
        }
        let restored = json(&repo, &["list-docs", "--json"]);
        assert_eq!(paths(&restored), paths(&before));
        assert_eq!(restored["coverage"]["excluded"], 0);
    }
}

#[test]
fn repository_dogfood_needs_no_init_or_schema_and_explains_every_mapping() {
    let repo = TestRepo::new(None);
    let source = Path::new(env!("CARGO_MANIFEST_DIR"));
    for file in ["README.md", "ITERATION_PLAN.md"] {
        fs::copy(source.join(file), repo.root.join(file)).unwrap();
    }
    for dir in ["agent-surfaces", "examples", "tests/fixtures"] {
        copy_tree(&source.join(dir), &repo.root.join(dir));
    }
    let report = json(&repo, &["list-docs", "--json"]);
    assert_eq!(
        paths(&report),
        BTreeSet::from([
            "README.md",
            "ITERATION_PLAN.md",
            "agent-surfaces/README.md",
            "agent-surfaces/skills/harnesskit/SKILL.md",
        ])
    );
    assert_eq!(report["coverage"]["managed"], 4);
    assert!(report["coverage"]["excluded"].as_u64().unwrap() > 0);
    assert_eq!(report["coverage"]["ratio"], 1.0);
    assert!(report["coverage"]["mappings"]
        .as_array()
        .unwrap()
        .iter()
        .all(|item| !item["reason"].as_str().unwrap().is_empty()));
    for item in report["coverage"]["mappings"].as_array().unwrap() {
        let path = item["path"].as_str().unwrap();
        if path.starts_with("tests/fixtures/") {
            assert_eq!(item["status"], "excluded");
            assert_eq!(item["reason"], "fixture-dir");
        } else if path.starts_with("examples/") {
            assert_eq!(item["status"], "excluded");
            assert_eq!(item["reason"], "nested-project");
        }
    }
    assert!(!repo.root.join(".harnesskit/schema.yaml").exists());
    let before = paths(&report);
    repo.run(&["doctor", "--layout", "--write-schema"]);
    let after = json(&repo, &["list-docs", "--json"]);
    assert_eq!(paths(&after), before);
    assert_eq!(
        after["coverage"]["excluded"],
        report["coverage"]["excluded"]
    );
    assert_eq!(after["coverage"]["unmanaged"], 0);
}

#[test]
fn templates_hidden_build_trees_and_symlinks_are_not_scanned() {
    let repo = TestRepo::new(Some("flat"));
    for dir in [
        "docs/templates",
        "docs/reference/templates",
        "docs/.hidden",
        "docs/.harnesskit",
        "docs/target",
        "docs/node_modules",
    ] {
        write(&repo, &format!("{}/secret.md", dir), "# Skipped\n");
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&repo.root, repo.root.join("docs/loop")).unwrap();
        std::os::unix::fs::symlink(
            repo.root.join("docs/principles.md"),
            repo.root.join("docs/link.md"),
        )
        .unwrap();
    }
    schema(&repo, "include:\n  '**/*.md': notes\n");
    let report = json(&repo, &["index", "--json"]);
    assert_eq!(report["coverage"]["markdown_total"], 8);
    assert_eq!(report["coverage"]["managed"], 8);
    assert_eq!(report["coverage"]["excluded"], 0);
}

#[test]
fn invalid_globs_and_collections_are_usage_errors() {
    for extra in [
        "include:\n  'docs/*.md': unknown_collection\n",
        "ignore:\n  - '[broken'\n",
    ] {
        let repo = TestRepo::new(Some("flat"));
        schema(&repo, extra);
        let output = repo.command(&["index"]).output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(!repo.root.join(".harnesskit/state").exists());
    }
}

#[test]
fn ignored_core_files_do_not_produce_missing_layout_warnings() {
    let repo = TestRepo::new(Some("standard"));
    schema(&repo, "ignore:\n  - 'AGENTS.md'\n  - 'docs/index.md'\n  - 'docs/project.md'\n  - 'docs/design-docs/index.md'\n");
    let report = json(&repo, &["check", "--json", "--rules", "missing-entrypoint-doc,missing-required-doc,missing-collection-index,entrypoint-manifest-link"]);
    assert_eq!(report["coverage"]["excluded"], 4);
    assert!(report["checks"].as_array().unwrap().is_empty());
}

#[test]
fn schema_globs_with_hash_colon_and_brackets_roundtrip_through_doctor() {
    let repo = TestRepo::new(Some("flat"));
    write(&repo, "docs/misc/a#b:c[1].md", "# Special filename\n");
    schema(
        &repo,
        "include:\n  \"docs/misc/a#b:c[[]1[]].md\": notes # trailing comment\n",
    );
    let report = json(&repo, &["list-docs", "--json"]);
    assert!(paths(&report).contains("docs/misc/a#b:c[1].md"));
    repo.run(&["doctor", "--write-schema", "--force"]);
    let report = json(&repo, &["list-docs", "--json"]);
    assert!(paths(&report).contains("docs/misc/a#b:c[1].md"));
}

#[cfg(unix)]
#[test]
fn configured_entrypoint_symlinks_do_not_escape_the_scan() {
    let repo = TestRepo::new(Some("flat"));
    let external = TestRepo::new(None);
    write(&external, "secret.md", "# Outside the repository\n");
    fs::remove_file(repo.root.join("AGENTS.md")).unwrap();
    std::os::unix::fs::symlink(external.root.join("secret.md"), repo.root.join("AGENTS.md"))
        .unwrap();
    let report = json(&repo, &["index", "--json"]);
    assert_eq!(report["coverage"]["managed"], 7);
    assert!(!paths(&report).contains("AGENTS.md"));
}

#[test]
fn explicit_repository_root_uses_normalized_core_and_collection_paths() {
    let repo = TestRepo::new(None);
    write(&repo, ".harnesskit/schema.yaml", "schema_version: 0\nmanaged_root: .\nentrypoints:\n  manifest: index.md\n  project_map: project.md\ncore_files:\n  index:\n    path: index.md\n    template: docs-index\n    required: true\ndoc_collections:\n  decisions:\n    path: decisions\n    template: decision\nrules:\n  index_required: true\n");
    write(
        &repo,
        "index.md",
        "# Index\n\n[Decisions](decisions/index.md)\n",
    );
    write(&repo, "decisions/index.md", "# Decisions\n");
    let report = json(
        &repo,
        &[
            "check",
            "--json",
            "--rules",
            "missing-required-doc,missing-collection-index",
        ],
    );
    assert!(report["checks"].as_array().unwrap().is_empty());
    assert_eq!(report["coverage"]["managed"], 2);
}
