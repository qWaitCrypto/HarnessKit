mod support;

use std::fs;
use std::process::Command;

use serde_json::{json, Value};
use support::{assert_success, TestRepo};

fn write(repo: &TestRepo, path: &str, text: &str) {
    let path = repo.root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn report(repo: &TestRepo, extra: &[&str], exit: i32) -> Value {
    let mut args = vec!["check", "--json"];
    args.extend_from_slice(extra);
    let output = repo.command(&args).output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(exit),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn findings<'a>(report: &'a Value, rule: &str) -> Vec<&'a Value> {
    report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|check| check["rule_id"] == rule)
        .collect()
}

fn git(repo: &TestRepo, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(&repo.root)
        .output()
        .unwrap();
    assert_success(&output, args);
}

#[test]
fn inferred_flat_and_repository_layouts_have_no_template_or_history_warnings() {
    for root in ["docs/", ""] {
        let repo = TestRepo::new(None);
        write(
            &repo,
            &format!("{}README.md", root),
            "# Project\n\n[Plan](plan.md)\n",
        );
        write(
            &repo,
            &format!("{}plan.md", root),
            "# Current plan\n\nDo the next task.\n",
        );
        for _ in 0..2 {
            let report = report(&repo, &["--strict", "--all"], 0);
            assert_eq!(report["summary"]["error"], 0);
            assert_eq!(report["summary"]["warning"], 0);
            assert!(report["checks"]
                .as_array()
                .unwrap()
                .iter()
                .all(|check| !check["subject_path"].as_str().unwrap().is_empty()));
            for rule in [
                "missing-entrypoint-doc",
                "missing-required-doc",
                "missing-collection-index",
                "history-missing",
                "managed-file-not-excluded",
            ] {
                assert!(findings(&report, rule).is_empty(), "{}", report);
            }
        }
    }
}

#[test]
fn explicit_layout_retains_required_docs_and_only_all_missing_entrypoints_are_errors() {
    let repo = TestRepo::new(None);
    repo.run(&["init", "--tracked"]);
    fs::remove_file(repo.root.join("AGENTS.md")).unwrap();
    fs::remove_file(repo.root.join("docs/commands.md")).unwrap();
    fs::remove_file(repo.root.join("docs/design-docs/index.md")).unwrap();
    let partial = report(&repo, &["--all"], 0);
    assert_eq!(
        findings(&partial, "missing-entrypoint-doc")[0]["severity"],
        "warning"
    );
    assert!(!findings(&partial, "missing-required-doc").is_empty());
    assert!(!findings(&partial, "missing-collection-index").is_empty());
    report(&repo, &["--strict"], 1);
    fs::remove_file(repo.root.join("CLAUDE.md")).unwrap();
    fs::remove_file(repo.root.join("ARCHITECTURE.md")).unwrap();
    let missing = report(
        &repo,
        &["--rules", "missing-entrypoint-doc", "--no-strict"],
        1,
    );
    assert_eq!(missing["summary"]["error"], 3);
    assert!(missing["checks"]
        .as_array()
        .unwrap()
        .iter()
        .all(|check| check["severity"] == "error"));
}

#[test]
fn baseline_reports_new_and_resolved_targets_without_implicit_updates() {
    let repo = TestRepo::new(None);
    write(&repo, "docs/note.md", "# Note\n\n[Old](old.md)\n");
    let args = ["--rules", "broken-link"];
    let first = report(&repo, &args, 1);
    assert_eq!(first["baseline"]["created"], true);
    assert_eq!(first["baseline"]["new_count"], 0);
    let path = repo.root.join(".harnesskit/state/check-baseline.json");
    let initial = fs::read(&path).unwrap();
    write(&repo, "docs/note.md", "# Note\n\n[New](new.md)\n");
    for _ in 0..2 {
        let changed = report(&repo, &args, 1);
        assert_eq!(changed["baseline"]["created"], false);
        assert_eq!(changed["baseline"]["new_count"], 1);
        assert_eq!(changed["baseline"]["resolved_count"], 1);
        assert!(changed["baseline"]["new"][0]["message"]
            .as_str()
            .unwrap()
            .contains("new.md"));
        assert!(changed["baseline"]["resolved"][0]["message"]
            .as_str()
            .unwrap()
            .contains("old.md"));
        assert_eq!(fs::read(&path).unwrap(), initial);
    }
    let output = repo
        .command(&["check", "--rules", "broken-link"])
        .output()
        .unwrap();
    let summary = String::from_utf8(output.stdout).unwrap();
    assert!(summary
        .lines()
        .any(|line| line.starts_with("[new]") && line.contains("new.md")));
    assert!(summary
        .lines()
        .any(|line| line.starts_with("[resolved]") && line.contains("old.md")));
    let updated = report(&repo, &["--rules", "broken-link", "--update-baseline"], 1);
    assert_eq!(updated["baseline"]["updated"], true);
    assert_eq!(updated["baseline"]["new_count"], 1);
    let same = report(&repo, &args, 1);
    assert_eq!(same["baseline"]["new_count"], 0);
    write(&repo, "docs/new.md", "# Target\n");
    let resolved = report(&repo, &args, 0);
    assert_eq!(resolved["baseline"]["resolved_count"], 1);
    assert_eq!(resolved["baseline"]["new_count"], 0);
}

#[test]
fn filtered_baselines_cover_all_rules_and_ignore_volatile_counts() {
    let repo = TestRepo::new(None);
    write(&repo, "docs/a.md", "# A\n\n[Missing](missing.md)\n");
    report(&repo, &["--rules", "orphan-managed-doc"], 0);
    let broken = report(&repo, &["--rules", "broken-link"], 1);
    assert_eq!(broken["baseline"]["new_count"], 0);
    write(&repo, "docs/b.md", "# B\n");
    let orphan = report(&repo, &["--rules", "orphan-managed-doc"], 0);
    assert_eq!(orphan["baseline"]["new_count"], 0);
    assert_eq!(orphan["baseline"]["resolved_count"], 0);
    let evidence = &orphan["checks"][0]["evidence_json"]["paths"];
    assert_eq!(evidence.as_array().unwrap().len(), 2);
    report(
        &repo,
        &["--rules", "orphan-managed-doc", "--update-baseline"],
        0,
    );
    assert_eq!(
        report(&repo, &["--rules", "broken-link"], 1)["baseline"]["new_count"],
        0
    );
}

#[test]
fn derived_baseline_recovers_after_incompatible_or_invalid_data() {
    let repo = TestRepo::new(None);
    write(&repo, "docs/a.md", "# A\n");
    report(&repo, &[], 0);
    let path = repo.root.join(".harnesskit/state/check-baseline.json");
    for bytes in ["{broken", "{\"version\":99,\"findings\":[]}"] {
        fs::write(&path, bytes).unwrap();
        assert_eq!(report(&repo, &[], 0)["baseline"]["created"], true);
    }
}

#[test]
fn summaries_are_brief_and_full_json_preserves_every_finding() {
    let repo = TestRepo::new(None);
    let links = (0..40)
        .map(|number| format!("[Missing](missing-{}.md)\n", number))
        .collect::<String>();
    write(&repo, "docs/a.md", &format!("# A\n\n{}", links));
    // Both JSON aliases should be compared after the one-time layout notice.
    repo.run(&["index"]);
    let compact = report(&repo, &["--rules", "broken-link"], 1);
    assert_eq!(compact["checks"].as_array().unwrap().len(), 20);
    assert_eq!(compact["truncated"], 20);
    assert_eq!(compact["summary"]["error"], 40);
    assert_eq!(compact["summary"]["by_rule"][0]["error"], 40);
    let full = report(&repo, &["--rules", "broken-link", "--all"], 1);
    assert_eq!(full["checks"].as_array().unwrap().len(), 40);
    assert_eq!(full["truncated"], 0);
    for flags in [
        vec![],
        vec!["--format", "summary"],
        vec!["--format", "full"],
        vec!["--all"],
    ] {
        let mut args = vec!["check", "--rules", "broken-link"];
        args.extend_from_slice(&flags);
        let output = repo.command(&args).output().unwrap();
        assert_eq!(output.status.code(), Some(1));
        let text = String::from_utf8(output.stdout).unwrap();
        let count = text
            .lines()
            .filter(|line| line.starts_with("[error]"))
            .count();
        if flags.contains(&"full") || flags.contains(&"--all") {
            assert_eq!(count, 40);
        } else {
            assert_eq!(count, 3);
            assert!(text.lines().count() < 20, "{}", text);
        }
    }
    let output = repo
        .command(&["check", "--format", "json", "--rules", "broken-link"])
        .output()
        .unwrap();
    let alias: Value = serde_json::from_slice(&output.stdout).unwrap();
    for field in ["checks", "summary", "truncated", "coverage", "hints"] {
        assert_eq!(alias[field], compact[field]);
    }
}

#[test]
fn json_limits_nested_lists_and_keeps_totals_and_ratio_precise() {
    let repo = TestRepo::new(None);
    for number in 0..35 {
        write(
            &repo,
            &format!("docs/note-{}.md", number),
            &format!("# Note {}\n", number),
        );
    }
    let compact = report(&repo, &[], 0);
    assert_eq!(compact["coverage"]["managed"], 35);
    assert_eq!(
        compact["coverage"]["mappings"].as_array().unwrap().len(),
        20
    );
    assert_eq!(compact["coverage"]["mappings_truncated"], 15);
    assert_eq!(compact["coverage"]["truncated"], 15);
    assert_eq!(compact["checks"][0]["evidence_json"]["paths_truncated"], 15);
    assert_eq!(
        report(&repo, &["--all"], 0)["coverage"]["mappings"]
            .as_array()
            .unwrap()
            .len(),
        35
    );
}

#[test]
fn every_finding_has_an_actionable_hint_and_an_automatic_fix_marker() {
    let repo = TestRepo::new(Some("standard"));
    repo.run(&["init", "--tracked"]);
    write(
        &repo,
        "docs/design-docs/a.md",
        "---\nstrange_field: true\n---\n# A\n\n[Missing](absent.md)\n",
    );
    for check in report(&repo, &["--all"], 1)["checks"].as_array().unwrap() {
        assert!(!check["hint"].as_str().unwrap().is_empty(), "{}", check);
        assert!(check["fixable"].is_boolean(), "{}", check);
        assert_eq!(check["hint"], check["evidence_json"]["hint"]);
    }
}

#[test]
fn invalid_frontmatter_is_an_error_but_valid_yaml_is_accepted() {
    let repo = TestRepo::new(None);
    for text in [
        "---\nstatus: [broken\n---\n# A\n",
        "---\nstatus: active\n# Unclosed\n",
        "---\n- not a mapping\n---\n# A\n",
    ] {
        write(&repo, "docs/a.md", text);
        let invalid = report(&repo, &["--rules", "frontmatter"], 1);
        assert_eq!(findings(&invalid, "frontmatter-parse-error").len(), 1);
        assert_eq!(invalid["checks"][0]["severity"], "error");
    }
    for text in ["---\n---\n# A\n", "---\nstatus: active\nnotes: |\n  multiple\n  lines\nscope_paths: [src/a, src/b]\n---\n# A\n", "---\r\nstatus: active\r\n---\r\n# A\r\n"] {
        write(&repo, "docs/a.md", text);
        assert!(report(&repo, &["--rules", "frontmatter"], 0)["checks"].as_array().unwrap().is_empty());
    }
}

#[test]
fn schema_syntax_errors_are_findings_and_environment_errors_still_exit_two() {
    let repo = TestRepo::new(None);
    write(&repo, "invalid.yaml", "unknown: invalid\n");
    let invalid = report(&repo, &["--schema", "invalid.yaml"], 1);
    assert_eq!(invalid["checks"][0]["rule_id"], "schema-parse-error");
    assert_eq!(invalid["summary"]["error"], 1);
    assert!(invalid["coverage"].is_object());
    assert!(invalid["hints"].is_array());
    assert!(!repo.root.join(".harnesskit").exists());
    for args in [
        vec!["check", "--schema", "absent.yaml"],
        vec!["index", "--schema", "invalid.yaml"],
        vec!["check", "absent"],
        vec!["check", "--format", "invalid"],
    ] {
        assert_eq!(
            repo.command(&args).output().unwrap().status.code(),
            Some(2),
            "{:?}",
            args
        );
    }
}

#[test]
fn reference_mentions_are_allowed_and_markdown_links_are_informational() {
    let repo = TestRepo::new(None);
    write(&repo, "docs/references/evidence.md", "# Evidence\n");
    write(
        &repo,
        "docs/design-docs/a.md",
        "# Contract\n\nSee `docs/references/evidence.md`.\n",
    );
    assert!(report(
        &repo,
        &["--rules", "canonical-links-reference", "--strict"],
        0
    )["checks"]
        .as_array()
        .unwrap()
        .is_empty());
    write(
        &repo,
        "docs/design-docs/a.md",
        "# Contract\n\nSee [evidence](../references/evidence.md).\n",
    );
    let linked = report(
        &repo,
        &["--rules", "canonical-links-reference", "--strict"],
        0,
    );
    assert_eq!(linked["checks"].as_array().unwrap().len(), 1);
    assert_eq!(linked["checks"][0]["severity"], "info");
}

#[test]
fn suppressions_accept_globs_and_rule_alias_without_changing_legacy_syntax() {
    let repo = TestRepo::new(None);
    repo.run(&["init", "--tracked"]);
    let path = repo.root.join(".harnesskit/schema.yaml");
    let schema = fs::read_to_string(&path).unwrap();
    write(
        &repo,
        "docs/design-docs/a.md",
        "# A\n\n[Missing](absent.md)\n",
    );
    write(
        &repo,
        "docs/design-docs/nested/b.md",
        "# B\n\n[Missing](absent.md)\n",
    );
    report(&repo, &["--rules", "broken-link"], 1);
    for suppression in [
        "  - rule: broken-link\n    path: 'docs/design-docs/**/*.md'\n    reason: 'Accepted # migration'\n",
        "  accepted:\n    rule: broken-link\n    path: 'docs/design-docs/**/*.md'\n    reason: 'Accepted # migration'\n",
    ] {
        fs::write(&path, format!("{}\nsuppressions:\n{}", schema, suppression)).unwrap();
        let suppressed = report(&repo, &["--rules", "broken-link", "--strict"], 0);
        assert_eq!(suppressed["summary"]["suppressed"], 2);
        assert_eq!(suppressed["baseline"]["resolved_count"], 2);
        assert!(suppressed["checks"].as_array().unwrap().iter().all(|check| check["suppression_reason"] == "Accepted # migration"));
        repo.run(&["doctor", "--write-schema", "--force"]);
        assert_eq!(report(&repo, &["--rules", "broken-link"], 0)["summary"]["suppressed"], 2);
    }
    write(
        &repo,
        "docs/design-docs/[draft].md",
        "# Draft\n\n[Missing](absent.md)\n",
    );
    fs::write(&path, format!("{}\nsuppressions:\n  legacy:\n    rule_id: broken-link\n    path: docs/design-docs/[draft].md\n    reason: literal legacy path\n", schema)).unwrap();
    let literal = report(&repo, &["--rules", "broken-link"], 1);
    assert_eq!(literal["summary"]["suppressed"], 1);
    assert!(literal["checks"]
        .as_array()
        .unwrap()
        .iter()
        .any(
            |check| check["subject_path"] == "docs/design-docs/[draft].md"
                && check["suppressed"] == true
        ));
    write(
        &repo,
        "docs/design-docs/[draft.md",
        "# Draft\n\n[Missing](absent.md)\n",
    );
    fs::write(&path, format!("{}\nsuppressions:\n  legacy:\n    rule_id: broken-link\n    path: docs/design-docs/[draft.md\n    reason: literal legacy path\n", schema)).unwrap();
    assert_eq!(
        report(&repo, &["--rules", "broken-link"], 1)["summary"]["suppressed"],
        1
    );
    fs::write(&path, format!("{}\nsuppressions:\n  - rule: broken-link\n    path: '[invalid'\n    reason: bad glob\n", schema)).unwrap();
    assert_eq!(
        report(&repo, &[], 1)["checks"][0]["rule_id"],
        "schema-parse-error"
    );
}

#[test]
fn local_isolation_uses_both_ignore_sources_and_tracked_docs_skip_it() {
    let repo = TestRepo::new(None);
    git(&repo, &["init", "-q"]);
    repo.run(&["init", "--local"]);
    let rules = [
        "--rules",
        "managed-file-not-excluded,host-git-exclude-missing",
        "--strict",
    ];
    assert_eq!(report(&repo, &rules, 0)["checks"], json!([]));
    fs::write(
        repo.root.join(".git/info/exclude"),
        "AGENTS.md\nCLAUDE.md\nARCHITECTURE.md\n.harnesskit/\n",
    )
    .unwrap();
    write(&repo, ".gitignore", "docs/*\n");
    assert_eq!(report(&repo, &rules, 0)["checks"], json!([]));
    write(&repo, ".gitignore", "docs/*\n!docs/commands.md\n");
    let missing = report(&repo, &rules, 1);
    assert_eq!(missing["checks"].as_array().unwrap().len(), 1);
    assert_eq!(missing["checks"][0]["subject_path"], "docs/commands.md");
    git(&repo, &["add", "-f", "docs/project.md"]);
    assert_eq!(report(&repo, &rules, 0)["checks"], json!([]));
}

fn commit(repo: &TestRepo, timestamp: &str) {
    git(repo, &["add", "."]);
    let args = [
        "-c",
        "user.name=Fixture",
        "-c",
        "user.email=fixture@example.invalid",
        "commit",
        "-qm",
        "Synthetic fixture",
    ];
    let output = Command::new("git")
        .args(args)
        .env("GIT_AUTHOR_DATE", timestamp)
        .env("GIT_COMMITTER_DATE", timestamp)
        .current_dir(&repo.root)
        .output()
        .unwrap();
    assert_success(&output, &args);
}

#[test]
fn stale_checks_use_cached_commit_times_merge_scopes_and_skip_history() {
    let repo = TestRepo::new(None);
    git(&repo, &["init", "-q"]);
    write(&repo, ".gitignore", ".harnesskit/\n");
    write(&repo, "src/a.rs", "pub fn a() {}\n");
    write(&repo, "src/b.rs", "pub fn b() {}\n");
    let text = "---\nstatus: active\nscope_paths: [src/a.rs, src/b.rs]\n---\n# Scoped\n\nCurrent contract.\n";
    for path in [
        "docs/design-docs/current.md",
        "docs/decisions/choice.md",
        "docs/exec-plans/completed/old.md",
        "docs/exec-plans/archive/old.md",
    ] {
        write(&repo, path, text);
    }
    write(
        &repo,
        "docs/design-docs/history.md",
        &text.replace("Current contract.", "History only: obsolete contract."),
    );
    write(&repo, "docs/design-docs/replaced.md", text);
    write(
        &repo,
        "docs/design-docs/successor.md",
        "---\nsupersedes: [docs/design-docs/replaced.md]\n---\n# Successor\n",
    );
    commit(&repo, "2020-01-01T00:00:00Z");
    let args = ["--rules", "stale-explicit-anchor", "--all"];
    assert_eq!(report(&repo, &args, 0)["checks"], json!([]));
    let cache = repo.root.join(".harnesskit/state/git-commit-times.json");
    let before = fs::read(&cache).unwrap();
    write(&repo, "src/a.rs", "pub fn a() { /* uncommitted */ }\n");
    assert_eq!(report(&repo, &args, 0)["checks"], json!([]));
    assert_eq!(fs::read(&cache).unwrap(), before);
    write(&repo, "src/b.rs", "pub fn b() { /* updated */ }\n");
    commit(&repo, "2020-01-02T00:00:00Z");
    let stale = report(&repo, &args, 0);
    assert_eq!(stale["checks"].as_array().unwrap().len(), 1, "{}", stale);
    assert_eq!(
        stale["checks"][0]["subject_path"],
        "docs/design-docs/current.md"
    );
    assert_eq!(stale["checks"][0]["severity"], "warning");
    assert_eq!(
        stale["checks"][0]["evidence_json"]["scope_paths"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_ne!(fs::read(&cache).unwrap(), before);
    // A checkout/touch changes mtime but cannot change the committed ordering.
    write(&repo, "docs/design-docs/current.md", text);
    assert_eq!(report(&repo, &args, 0)["checks"], stale["checks"]);
    report(&repo, &["--rules", "stale-explicit-anchor", "--strict"], 1);
}

#[cfg(unix)]
#[test]
fn without_git_stale_mtime_checks_are_informational() {
    let repo = TestRepo::new(None);
    write(&repo, "src/a.rs", "pub fn a() {}\n");
    write(
        &repo,
        "docs/design-docs/current.md",
        "---\nscope_paths: [src/a.rs]\n---\n# Current\n",
    );
    let output = Command::new("touch")
        .args(["-m", "-d", "@1000000000", "docs/design-docs/current.md"])
        .current_dir(&repo.root)
        .output()
        .unwrap();
    assert_success(&output, &["touch"]);
    let stale = report(&repo, &["--rules", "stale-explicit-anchor", "--strict"], 0);
    assert_eq!(stale["checks"].as_array().unwrap().len(), 1);
    assert_eq!(stale["checks"][0]["severity"], "info");
    assert_eq!(stale["checks"][0]["evidence_json"]["time_source"], "mtime");
}
