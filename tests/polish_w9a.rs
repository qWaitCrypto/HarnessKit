mod support;

use std::fs;
use std::path::Path;
use std::process::Command;

use rusqlite::Connection;
use serde_json::Value;
use support::{assert_success, TestRepo};

fn write(repo: &TestRepo, path: &str, text: &str) {
    let path = repo.root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn fixture(revision: &str) -> TestRepo {
    let repo = TestRepo::new(None);
    let output = Command::new("python3")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/generate-weave-fixture.py"))
        .arg(&repo.root)
        .output()
        .unwrap();
    assert_success(&output, &["generate-weave-fixture.py"]);
    git(&repo, &["checkout", "-q", revision]);
    repo
}

fn git(repo: &TestRepo, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(&repo.root)
        .output()
        .unwrap();
    assert_success(&output, args);
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn committed_drift_uses_twelve_characters_in_text_and_full_hash_in_evidence() {
    let repo = fixture("baseline");
    repo.run(&["index"]);
    git(&repo, &["checkout", "-q", "logic"]);
    let commit = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();
    let output = repo.run(&["check", "--rules", "code-drift", "--json", "--all"]);
    let report: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(report["checks"].as_array().unwrap().len(), 8);
    for check in report["checks"].as_array().unwrap() {
        let message = check["message"].as_str().unwrap();
        assert!(message.contains(&commit[..12]), "{message}");
        assert!(!message.contains(&commit), "{message}");
        assert_eq!(check["evidence_json"]["commit"], commit);
    }
}

#[test]
fn new_warnings_are_printed_once_with_their_actionable_hint() {
    let repo = fixture("baseline");
    repo.run(&["check", "--rules", "code-drift"]);
    write(
        &repo,
        "src/auth/refresh.rs",
        "pub fn refresh_token(expires_in: u64) -> u64 { expires_in + 999 }\n",
    );
    let output = repo.run(&["check", "--rules", "code-drift"]);
    let findings = output
        .lines()
        .filter(|line| line.starts_with("[new]") || line.starts_with("[warning]"))
        .collect::<Vec<_>>();
    assert!(!findings.is_empty(), "{output}");
    for line in findings.iter().filter(|line| line.starts_with("[new]")) {
        assert!(
            !findings.iter().any(|other| other.starts_with("[warning]")
                && other.split_once("] ").unwrap().1 == line.split_once("] ").unwrap().1),
            "{output}"
        );
    }
    assert!(output.contains("check --ack"), "{output}");
}

#[test]
fn inline_rust_test_symbols_rank_below_production_in_mixed_and_code_search() {
    let repo = TestRepo::new(None);
    write(
        &repo,
        "src/usage.rs",
        r#"
/// Usage log records activity for the operator.
pub fn usage_log() {}
mod tests {
    /// Usage log records activity for the operator.
    fn module_probe() {}
}
#[cfg(test)]
mod fixtures {
    /// Usage log records activity for the operator.
    fn cfg_module_probe() {}
}
#[cfg(test)]
/// Usage log records activity for the operator.
fn cfg_function_probe() {}
#[test]
/// Usage log records activity for the operator.
fn attribute_probe() {}
#[cfg(not(test))]
fn production_only() {}
"#,
    );
    for flags in [vec![], vec!["--code"]] {
        let mut args = vec!["query", "usage log", "--json", "--limit", "20"];
        args.extend(&flags);
        let report: Value = serde_json::from_str(&repo.run(&args)).unwrap();
        let bundles = report["bundles"].as_array().unwrap();
        assert_eq!(bundles[0]["primary"]["name"], "usage_log", "{report}");
        assert!(
            bundles
                .iter()
                .any(|bundle| bundle["primary"]["name"] == "tests::module_probe"),
            "{report}"
        );
        let mut exact = vec!["query", "cfg_function_probe", "--json"];
        exact.extend(&flags);
        let found: Value = serde_json::from_str(&repo.run(&exact)).unwrap();
        assert_eq!(found["bundles"][0]["primary"]["name"], "cfg_function_probe");
    }
    let db = Connection::open(repo.root.join(".harnesskit/state/facts.sqlite")).unwrap();
    for (name, expected) in [
        ("usage_log", 0),
        ("tests", 1),
        ("tests::module_probe", 1),
        ("fixtures::cfg_module_probe", 1),
        ("cfg_function_probe", 1),
        ("attribute_probe", 1),
        ("production_only", 0),
    ] {
        let actual: i32 = db
            .query_row("SELECT is_test FROM nodes WHERE name = ?1", [name], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(actual, expected, "{name}");
    }
}

#[test]
fn short_latin_words_do_not_match_longer_word_prefixes() {
    let repo = TestRepo::new(None);
    write(
        &repo,
        "src/logic.rs",
        "/// Logic handles decisions.\npub fn logic() {}\n",
    );
    write(
        &repo,
        "src/usage.rs",
        "/// Usage log records activity.\npub fn usage_log() {}\n",
    );
    write(
        &repo,
        "docs/logic.md",
        "# Logic\n\nLogic handles decisions.\n",
    );
    for flags in [vec![], vec!["--code"], vec!["--docs"]] {
        let mut args = vec!["query", "log", "--json"];
        args.extend(&flags);
        let output = repo.run(&args);
        let report: Value = serde_json::from_str(&output).unwrap();
        assert!(!report["bundles"].to_string().contains("logic"), "{output}");
        assert!(
            !report["candidates"].to_string().contains("logic"),
            "{output}"
        );
    }
}

#[test]
fn expanded_file_links_have_one_label_and_kwic_preserves_inline_paths() {
    let repo = TestRepo::new(None);
    write(&repo, "scripts/x.sh", "echo ready\n");
    write(&repo, "docs/usage.md", "# Usage\n\n## Log rules\n\nUsage log writes records through `scripts/x.sh` before returning to the caller. Another sentence is unrelated.\n");
    let output = repo.run(&["query", "usage log", "--expand", "1"]);
    assert!(
        output.contains("`scripts/x.sh` before returning to the caller."),
        "{output}"
    );
    assert!(
        !output.contains("file scripts/x.sh: file scripts/x.sh"),
        "{output}"
    );
    assert_eq!(
        output
            .lines()
            .filter(|line| line.contains('↳') && line.contains("scripts/x.sh"))
            .count(),
        1,
        "{output}"
    );
}

#[test]
fn expanded_merged_file_signatures_are_neither_prefixed_nor_repeated() {
    let repo = TestRepo::new(None);
    write(&repo, "src/migration.rs", "struct LayoutMigration;\n");
    write(
        &repo,
        "docs/guide.md",
        "# Guide\n\nThis migration plan explains the release.\n",
    );
    let output = repo.run(&["query", "migration plan", "--expand", "2"]);
    assert!(!output.contains("— ; struct Layout"), "{output}");
    let plain = output.replace('«', "").replace('»', "");
    assert_eq!(plain.matches("struct LayoutMigration;").count(), 1, "{output}");
}

#[test]
fn fenced_kwic_uses_the_hit_line_for_docs_and_readme() {
    let repo = TestRepo::new(None);
    write(
        &repo,
        "docs/commands.md",
        "# Commands\n\n## Local usage\n\n```bash\nexport HARNESSKIT_USAGE_LOG=off # Disable future writes\nharnesskit doctor --usage --days 30\n```\n",
    );
    write(
        &repo,
        "docs/readme.md",
        "# HarnessKit\n\n## Local Usage Metrics\n\n```bash\nexport HARNESSKIT_USAGE_LOG=off # Disable logging for this shell\nharnesskit doctor --usage --days 30\n```\n",
    );
    let output = repo.run(&["query", "usage log", "--limit", "20"]);
    assert!(!output.contains("```"), "{output}");
    assert!(output.contains("docs/commands.md"), "{output}");
    assert!(output.contains("docs/readme.md"), "{output}");
    assert!(output.contains("export HARNESSKIT_«USAGE»_«LOG»=off"), "{output}");
    assert!(!output.contains("harnesskit doctor --«usage»"), "{output}");
}

#[test]
fn mixed_relevance_threshold_drops_weak_test_results_after_strong_documents() {
    let repo = TestRepo::new(None);
    write(
        &repo,
        "docs/usage.md",
        "# Usage\n\n## Log policy\n\nThe usage log is the canonical local record.\n",
    );
    write(
        &repo,
        "tests/noisy.rs",
        "/// Usage log appears in a test helper.\npub fn noisy_usage_log() {}\n",
    );
    let report: Value = serde_json::from_str(
        &repo.run(&["query", "usage log", "--json", "--limit", "20"]),
    )
    .unwrap();
    let bundles = report["bundles"].as_array().unwrap();
    assert_eq!(bundles[0]["primary"]["path"], "docs/usage.md", "{report}");
    assert!(
        bundles
            .iter()
            .all(|bundle| !bundle["primary"]["path"].as_str().unwrap().starts_with("tests/")),
        "{report}"
    );
}

#[test]
fn diff_text_keeps_each_file_documents_adjacent_and_summarizes_truncation() {
    let repo = fixture("baseline");
    for path in [
        "src/auth/refresh.rs",
        "web/cache.ts",
        "worker/retry.py",
        "gateway/limit.go",
    ] {
        let source = fs::read_to_string(repo.root.join(path)).unwrap();
        let updated = source.replace("+ 60", "+ 61");
        assert_ne!(source, updated, "{path}");
        write(&repo, path, &updated);
    }
    let new_files = (1..=10)
        .map(|number| format!("src/new_{number}.rs"))
        .collect::<Vec<_>>();
    for (number, path) in new_files.iter().enumerate() {
        write(&repo, path, &format!("pub fn new_{number}() -> u64 {{ {number} }}\n"));
    }
    let mut add_args = vec!["add"];
    add_args.extend(new_files.iter().map(String::as_str));
    git(&repo, &add_args);

    let output = repo.run(&["context", "--diff"]);
    let lines = output.lines().collect::<Vec<_>>();
    let parents = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.starts_with("- ") && line.contains(" ["))
        .collect::<Vec<_>>();
    assert!(parents.len() >= 5, "{output}");
    for (index, parent) in parents {
        let child = lines.get(index + 1).copied().unwrap_or_default();
        assert!(child.starts_with("  → "), "{parent}: {output}");
    }
    assert!(output.contains("more files"), "{output}");
    assert!(output.contains("uncovered regions"), "{output}");
    assert!(!output.contains("Truncated: {"), "{output}");
    assert!(!output.contains("§ ::"), "{output}");
}

#[test]
fn diff_lists_uncovered_file_regions_once_and_hints_use_product_language() {
    let repo = fixture("baseline");
    write(
        &repo,
        "bulk/unrelated.rs",
        "// Changed file header\npub fn unrelated() -> u64 { 2 }\n",
    );
    let output = repo.run(&["context", "--diff"]);
    assert_eq!(output.matches("bulk/unrelated.rs").count(), 1, "{output}");
    assert!(!output.contains("Uncovered code regions:"), "{output}");
    assert!(!output.contains("W3"), "{output}");
}

#[test]
fn migration_instructs_review_then_snapshot_in_text_and_json() {
    for json in [false, true] {
        let repo = fixture("baseline");
        let args = if json {
            vec!["migrate", "--apply", "--json"]
        } else {
            vec!["migrate", "--apply"]
        };
        let output = repo.run(&args);
        assert!(output.contains("Review the migration diff"), "{output}");
        assert!(
            output.contains("harnesskit history snapshot -m"),
            "{output}"
        );
        let before: Value = serde_json::from_str(&repo.run(&[
            "check",
            "--rules",
            "history-schema-not-snapshotted",
            "--all",
            "--json",
        ]))
        .unwrap();
        assert!(!before["checks"].as_array().unwrap().is_empty());
        repo.run(&["history", "snapshot", "-m", "reviewed migration"]);
        let after: Value = serde_json::from_str(&repo.run(&[
            "check",
            "--rules",
            "history-schema-not-snapshotted",
            "--all",
            "--json",
        ]))
        .unwrap();
        assert!(after["checks"].as_array().unwrap().is_empty(), "{after}");
    }
}

#[test]
fn derived_index_without_test_classification_is_rebuilt_without_changing_history() {
    let repo = fixture("baseline");
    repo.run(&["history", "snapshot", "-m", "baseline"]);
    repo.run(&["index"]);
    let history = repo.run(&["history", "list", "--json"]);
    let db = Connection::open(repo.root.join(".harnesskit/state/facts.sqlite")).unwrap();
    db.execute_batch("ALTER TABLE nodes DROP COLUMN is_test;")
        .unwrap();
    drop(db);
    repo.run(&["query", "令牌刷新"]);
    assert_eq!(repo.run(&["history", "list", "--json"]), history);
    let db = Connection::open(repo.root.join(".harnesskit/state/facts.sqlite")).unwrap();
    assert!(db.prepare("SELECT is_test FROM nodes").is_ok());
}

#[test]
fn warm_refresh_detects_repeated_edits_even_when_git_status_paths_stay_the_same() {
    let repo = fixture("baseline");
    repo.run(&["index"]);
    let path = "src/auth/refresh.rs";
    let original = fs::read_to_string(repo.root.join(path)).unwrap();
    let mut fingerprints = Vec::new();
    let mut statuses = Vec::new();
    for increment in [91, 92] {
        write(
            &repo,
            path,
            &original.replace("+ 60", &format!("+ {increment}")),
        );
        statuses.push(git(&repo, &["status", "--porcelain"]));
        repo.run(&["query", "refresh_token", "--code", "--json"]);
        let db = Connection::open(repo.root.join(".harnesskit/state/facts.sqlite")).unwrap();
        fingerprints.push(
            db.query_row(
                "SELECT fingerprint FROM nodes WHERE name = 'refresh_token'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        );
    }
    assert_eq!(statuses[0], statuses[1]);
    assert_ne!(fingerprints[0], fingerprints[1]);
}

#[test]
fn incremental_native_cards_preserve_file_bridges_and_remove_renamed_symbols() {
    let repo = fixture("baseline");
    let path = "src/auth/refresh.rs";
    write(
        &repo,
        "docs/notes/delivery.md",
        "# 包裹投递\n\n包裹投递遵循固定顺序。\n\n`src/auth/refresh.rs` 负责包裹投递。\n",
    );
    repo.run(&["index"]);
    let original = fs::read_to_string(repo.root.join(path)).unwrap();
    for name in ["deliver_parcel", "deliver_package"] {
        write(&repo, path, &format!("{original}\npub fn {name}() {{}}\n"));
        repo.run(&["index"]);
        let db = Connection::open(repo.root.join(".harnesskit/state/facts.sqlite")).unwrap();
        let bridge: String = db
            .query_row(
                "SELECT bridged FROM code_cards WHERE name = ?1",
                [name],
                |row| row.get(0),
            )
            .unwrap();
        assert!(bridge.contains("包裹投递"), "{bridge}");
        let count: usize = db
            .query_row(
                "SELECT COUNT(*) FROM code_fts WHERE code_fts MATCH ?1",
                [format!("name : \"{name}\"")],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }
    let db = Connection::open(repo.root.join(".harnesskit/state/facts.sqlite")).unwrap();
    let stale: usize = db
        .query_row(
            "SELECT COUNT(*) FROM code_fts WHERE code_fts MATCH 'name : \"deliver_parcel\"'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stale, 0);
    drop(db);
    write(
        &repo,
        "docs/notes/delivery.md",
        "# 快递签收\n\n快递签收遵循固定顺序。\n\n`src/auth/refresh.rs` 负责快递签收。\n",
    );
    repo.run(&["index"]);
    let db = Connection::open(repo.root.join(".harnesskit/state/facts.sqlite")).unwrap();
    let bridge: String = db
        .query_row(
            "SELECT bridged FROM code_cards WHERE name = 'deliver_package'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(bridge.contains("快递签收"));
    assert!(!bridge.contains("包裹投递"));
}

#[test]
fn a_cached_worktree_signature_cannot_hide_facts_from_before_an_edit() {
    let repo = fixture("baseline");
    repo.run(&["index"]);
    let path = "src/auth/refresh.rs";
    let db = Connection::open(repo.root.join(".harnesskit/state/facts.sqlite")).unwrap();
    let old_state: (String, u64, u64, u64) = db.query_row(
        "SELECT content_hash, file_size_bytes, mtime_unix, mtime_ns FROM file_states WHERE path = ?1", [path],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))).unwrap();
    let old_fingerprint: String = db
        .query_row(
            "SELECT fingerprint FROM nodes WHERE name = 'refresh_token'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let original = fs::read_to_string(repo.root.join(path)).unwrap();
    write(&repo, path, &original.replace("+ 60", "+ 91"));
    repo.run(&["query", "refresh_token", "--code"]);
    let expected: String = db
        .query_row(
            "SELECT fingerprint FROM nodes WHERE name = 'refresh_token'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_ne!(expected, old_fingerprint);
    // Model an edit arriving after parsing but before the final input stamp:
    // the stamp describes the new worktree while the facts still describe it
    // before that edit. The next command must finish processing the source.
    db.execute("UPDATE file_states SET content_hash = ?1, file_size_bytes = ?2, mtime_unix = ?3, mtime_ns = ?4 WHERE path = ?5",
        rusqlite::params![old_state.0, old_state.1, old_state.2, old_state.3, path]).unwrap();
    db.execute(
        "UPDATE nodes SET fingerprint = ?1 WHERE name = 'refresh_token'",
        [old_fingerprint],
    )
    .unwrap();
    repo.run(&["query", "refresh_token", "--code"]);
    let actual: String = db
        .query_row(
            "SELECT fingerprint FROM nodes WHERE name = 'refresh_token'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(actual, expected);
}
