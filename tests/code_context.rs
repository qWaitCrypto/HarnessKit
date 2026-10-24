mod support;

use std::fs;
use std::path::Path;
use std::process::Command;

use serde_json::Value;
use support::{assert_success, TestRepo};

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
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert_success(&output, args);
    String::from_utf8(output.stdout).unwrap()
}

fn write(repo: &TestRepo, path: &str, text: &str) {
    let path = repo.root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn replace(repo: &TestRepo, path: &str, from: &str, to: &str) {
    let text = fs::read_to_string(repo.root.join(path)).unwrap();
    assert!(text.contains(from));
    write(repo, path, &text.replace(from, to));
}

fn report(repo: &TestRepo, args: &[&str]) -> Value {
    let packet: Value = serde_json::from_str(&repo.run(args)).unwrap();
    assert!(packet["coverage"].is_object());
    assert!(packet["hints"].is_array());
    packet
}

fn for_code(repo: &TestRepo, input: &str) -> Value {
    report(repo, &["context", "--for", input, "--json"])
}

fn diff(repo: &TestRepo) -> Value {
    report(repo, &["context", "--diff", "--json"])
}

fn current(repo: &TestRepo) -> Value {
    report(repo, &["context", "--current", "--json"])
}

fn seal(repo: &TestRepo) {
    repo.run(&["index", "--json"]);
}

fn has_doc(packet: &Value, path: &str, heading: &str) -> bool {
    packet["documents"]
        .as_array()
        .unwrap()
        .iter()
        .any(|doc| doc["path"] == path && doc["heading"] == heading)
}

#[test]
fn w0_paths_symbols_and_directories_return_chapters_decisions_and_lifecycle() {
    let repo = fixture("baseline");
    for (path, symbol, language, heading) in [
        ("src/auth/refresh.rs", "refresh_token", "rust", "刷新策略"),
        ("web/cache.ts", "cacheTtl", "typescript", "缓存有效期"),
        ("worker/retry.py", "retry_delay", "python", "重试退避"),
        ("gateway/limit.go", "limitRequests", "go", "请求限流"),
    ] {
        let doc_path = format!("docs/decisions/{language}.md");
        for input in [
            path.to_string(),
            symbol.to_string(),
            format!("{path}#{symbol}"),
        ] {
            let packet = for_code(&repo, &input);
            assert!(has_doc(&packet, &doc_path, heading), "{input}: {packet}");
            assert!(packet["decisions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|path| path == &doc_path));
            assert_eq!(packet["documents"][0]["lifecycle"], "current");
            assert_eq!(packet["documents"][0]["status"], "accepted");
            assert_eq!(
                packet["documents"][0]["links"][0]["confidence"],
                "EXTRACTED"
            );
            assert!(!packet["documents"][0]["summary"]
                .as_str()
                .unwrap()
                .is_empty());
        }
    }
    for directory in ["src/auth", "src/auth/"] {
        let packet = for_code(&repo, directory);
        assert!(has_doc(&packet, "docs/notes/rust-pitfalls.md", "规范化"));
        assert!(has_doc(&packet, "docs/notes/rust-pitfalls.md", "旧接口"));
    }
    assert!(has_doc(
        &for_code(
            &repo,
            repo.root.join("src/auth/refresh.rs").to_str().unwrap()
        ),
        "docs/decisions/rust.md",
        "刷新策略"
    ));
}

#[test]
fn mentions_and_active_plans_sort_ahead_of_historical_decisions() {
    let repo = fixture("baseline");
    write(
        &repo,
        "docs/exec-plans/active/review.md",
        "# Review\n\n## Current task\n\nReview `refresh_token`.\n",
    );
    write(&repo, "docs/decisions/old.md", "---\nstatus: superseded\n---\n# Old\n\n## Historical rule\n\n`src/auth/refresh.rs#refresh_token`\n");
    let packet = for_code(&repo, "refresh_token");
    let docs = packet["documents"].as_array().unwrap();
    assert_eq!(docs[0]["path"], "docs/decisions/rust.md");
    assert_eq!(docs[1]["path"], "docs/exec-plans/active/review.md");
    assert_eq!(docs[1]["links"][0]["kind"], "mention");
    assert_eq!(docs.last().unwrap()["lifecycle"], "historical");
    assert_eq!(
        packet["active_plans"][0],
        "docs/exec-plans/active/review.md"
    );
    assert!(!packet["decisions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|path| path == "docs/decisions/old.md"));
}

#[test]
fn no_association_is_explicit_and_suggests_inline_anchors_without_w7() {
    let repo = fixture("baseline");
    let packet = for_code(&repo, "bulk/unrelated.rs");
    assert_eq!(packet["message"], "No related documents");
    assert!(packet["hints"]
        .as_array()
        .unwrap()
        .iter()
        .any(|hint| hint.as_str().unwrap().contains("Suggestion:")));
    let text = repo.run(&["context", "--for", "bulk/unrelated.rs"]);
    assert!(text.contains("No related documents"));
    assert!(text.contains("`bulk/unrelated.rs#symbol`"));
    assert!(for_code(&repo, "unknown_symbol")["documents"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(for_code(&repo, "unrelated")["hints"]
        .as_array()
        .unwrap()
        .iter()
        .any(|hint| hint
            .as_str()
            .unwrap()
            .contains("bulk/unrelated.rs#unrelated")));
}

#[test]
fn reverse_cochange_uses_w0_small_commits_and_excludes_bulk_noise() {
    let repo = fixture("bulk");
    let packet = for_code(&repo, "src/auth/refresh.rs");
    let files = packet["cochanged_files"].as_array().unwrap();
    assert!(files.len() <= 5);
    assert!(files
        .iter()
        .any(|file| file["path"] == "docs/decisions/rust.md"));
    assert!(
        files.iter().any(|file| file["path"] == "web/cache.ts"),
        "{packet}"
    );
    assert!(files.iter().all(
        |file| file["path"] != "bulk/unrelated.rs" && file["path"] != "docs/notes/bulk-only.md"
    ));
    assert!(files
        .iter()
        .all(|file| file["confidence"] == "INFERRED" && file["count"].as_u64().unwrap() > 0));
}

#[test]
fn for_reports_logic_drift_but_not_format_changes_and_retains_moved_deleted_anchors() {
    let repo = fixture("baseline");
    seal(&repo);
    git(&repo, &["checkout", "-q", "logic"]);
    let packet = for_code(&repo, "refresh_token");
    assert!(packet["anchor_status"]
        .as_array()
        .unwrap()
        .iter()
        .any(|state| state["status"] == "drifted"));
    git(&repo, &["checkout", "-q", "moved"]);
    let moved = for_code(&repo, "normalize_token");
    assert!(has_doc(&moved, "docs/notes/rust-pitfalls.md", "规范化"));
    assert!(moved["anchor_status"]
        .as_array()
        .unwrap()
        .iter()
        .any(|state| state["status"] == "moved"));
    assert!(has_doc(
        &for_code(&repo, "src/auth/refresh.rs#normalize_token"),
        "docs/notes/rust-pitfalls.md",
        "规范化"
    ));
    git(&repo, &["checkout", "-q", "deleted"]);
    let deleted = for_code(&repo, "legacy_token");
    assert!(has_doc(&deleted, "docs/notes/rust-pitfalls.md", "旧接口"));
    assert!(deleted["anchor_status"]
        .as_array()
        .unwrap()
        .iter()
        .any(|state| state["status"] == "broken"));

    let formatted = fixture("logic");
    seal(&formatted);
    git(&formatted, &["checkout", "-q", "format"]);
    assert!(for_code(&formatted, "refresh_token")["anchor_status"]
        .as_array()
        .unwrap()
        .iter()
        .all(|state| state["status"] == "current"));
}

#[test]
fn staged_removals_directory_anchors_and_single_line_symbols_keep_their_coverage() {
    let repo = fixture("baseline");
    write(
        &repo,
        "docs/notes/directory.md",
        "# Directory\n\n## Scope\n\n`src/auth/` keeps authentication isolated.\n",
    );
    seal(&repo);
    git(&repo, &["rm", "--cached", "src/auth/refresh.rs"]);
    assert!(repo.root.join("src/auth/refresh.rs").is_file());
    let packet = diff(&repo);
    assert_eq!(packet["files"][0]["change"], "removed");
    assert!(packet["files"][0]["documents"]
        .as_array()
        .unwrap()
        .iter()
        .any(|doc| doc["path"] == "docs/notes/directory.md"));
    assert!(for_code(&repo, "src/auth/refresh.rs")["documents"]
        .as_array()
        .unwrap()
        .iter()
        .any(|doc| doc["path"] == "docs/notes/directory.md"));

    let packed = fixture("baseline");
    write(
        &packed,
        "bulk/unrelated.rs",
        "fn unchanged() {} fn changed_symbol() -> u64 { 1 }\n",
    );
    git(&packed, &["add", "bulk/unrelated.rs"]);
    // Compare the staged replacement against the existing W0 revision.
    let packet = diff(&packed);
    let names = packet["files"][0]["regions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|impact| impact["region"]["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(names.contains(&"changed_symbol"), "{packet}");
    assert!(names.contains(&"unchanged"), "{packet}");
}

#[test]
fn empty_git_history_and_ambiguous_symbols_have_explicit_results() {
    let repo = fixture("baseline");
    fs::remove_dir_all(repo.root.join(".git")).unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["add", "src/auth/refresh.rs"]);
    assert_eq!(diff(&repo)["files"][0]["change"], "added");
    write(&repo, "src/other.rs", "pub fn refresh_token() {}\n");
    git(&repo, &["add", "src/other.rs"]);
    let packet = for_code(&repo, "refresh_token");
    assert_eq!(packet["ambiguous"], true);
    assert_eq!(packet["targets"].as_array().unwrap().len(), 2);
    assert!(packet["hints"]
        .as_array()
        .unwrap()
        .iter()
        .any(|hint| hint.as_str().unwrap().contains("path#symbol")));
}

#[test]
fn diff_includes_staged_and_unstaged_symbols_and_uncovered_additions() {
    let repo = fixture("baseline");
    seal(&repo);
    replace(&repo, "src/auth/refresh.rs", "+ 60", "+ 91");
    git(&repo, &["add", "src/auth/refresh.rs"]);
    replace(&repo, "worker/retry.py", "+ 60", "+ 92");
    write(&repo, "src/new.rs", "pub fn no_contract() -> u64 { 7 }\n");
    git(&repo, &["add", "src/new.rs"]);
    let packet = diff(&repo);
    assert_eq!(packet["totals"]["files"], 3);
    assert!(packet["files"]
        .as_array()
        .unwrap()
        .iter()
        .any(|file| file["path"] == "worker/retry.py"));
    assert!(packet["files"]
        .as_array()
        .unwrap()
        .iter()
        .any(|file| file["path"] == "src/auth/refresh.rs"
            && file["decisions"][0] == "docs/decisions/rust.md"));
    assert!(packet["uncovered_regions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|region| region["name"] == "no_contract"));
    assert!(packet["predicted_drift"]
        .as_array()
        .unwrap()
        .iter()
        .any(|state| state["anchor"] == "src/auth/refresh.rs#refresh_token"));
    assert!(packet["totals"]["covered"].as_u64().unwrap() >= 2);
    assert!(packet["files"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|file| file["regions"].as_array().unwrap())
        .any(|impact| impact["region"]["name"] == "refresh_token" && impact["covered"] == true));
}

#[test]
fn diff_revision_includes_committed_changes_plus_worktree_and_rejects_bad_revisions() {
    let repo = fixture("baseline");
    seal(&repo);
    git(&repo, &["checkout", "-q", "logic"]);
    replace(&repo, "src/auth/refresh.rs", "+ 90", "+ 101");
    assert_eq!(diff(&repo)["totals"]["files"], 1);
    let packet = report(&repo, &["context", "--diff", "baseline", "--json"]);
    assert_eq!(packet["totals"]["files"], 4);
    assert_eq!(packet["revision"], "baseline");
    assert_eq!(packet["includes_worktree"], true);
    let output = repo
        .command(&["context", "--diff", "missing-revision"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn diff_formatting_has_no_false_drift_and_top_level_changes_do_not_borrow_symbol_docs() {
    let repo = fixture("logic");
    seal(&repo);
    replace(
        &repo,
        "src/auth/refresh.rs",
        "    expires_in",
        "      expires_in",
    );
    let format = diff(&repo);
    assert_eq!(format["totals"]["drifting_anchors"], 0);
    assert_eq!(format["files"][0]["structural_change"], false);
    git(&repo, &["checkout", "--", "src/auth/refresh.rs"]);
    let old = fs::read_to_string(repo.root.join("src/auth/refresh.rs")).unwrap();
    write(
        &repo,
        "src/auth/refresh.rs",
        &format!("use std::time::Duration;\n{old}"),
    );
    let imports = diff(&repo);
    assert!(imports["uncovered_regions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|region| region["kind"] == "file"));
    assert_eq!(imports["files"][0]["coverage"], "uncovered");
    assert_eq!(imports["totals"]["drifting_anchors"], 0);
}

#[test]
fn diff_predicts_deleted_and_moved_symbols_and_compares_the_preindex_lock() {
    let repo = fixture("baseline");
    seal(&repo);
    git(&repo, &["checkout", "-q", "deleted"]);
    let packet = report(&repo, &["context", "--diff", "baseline", "--json"]);
    assert!(packet["predicted_drift"]
        .as_array()
        .unwrap()
        .iter()
        .any(|state| state["status"] == "broken"));
    assert!(packet["predicted_drift"]
        .as_array()
        .unwrap()
        .iter()
        .any(|state| state["status"] == "moved"));

    let reviewed = fixture("baseline");
    seal(&reviewed);
    replace(&reviewed, "src/auth/refresh.rs", "+ 60", "+ 90");
    replace(&reviewed, "docs/decisions/rust.md", "增加 60", "增加 90");
    let packet = diff(&reviewed);
    assert!(packet["predicted_drift"]
        .as_array()
        .unwrap()
        .iter()
        .any(|state| state["doc"] == "docs/decisions/rust.md" && state["status"] == "drifted"));
    assert!(for_code(&reviewed, "refresh_token")["anchor_status"]
        .as_array()
        .unwrap()
        .iter()
        .any(|state| state["doc"] == "docs/decisions/rust.md" && state["status"] == "current"));
}

#[test]
fn diff_flags_accepted_decision_body_changes_even_without_code_changes() {
    let repo = fixture("baseline");
    let path = "docs/decisions/body.md";
    let original = "---\nstatus: accepted\nauthority: canonical\n---\n\n# Ownership\n\nThe operation must return failures to its caller.\n";
    write(&repo, path, original);
    git(&repo, &["add", path]);
    git(&repo, &["-c", "user.name=Test", "-c", "user.email=test@example.invalid", "commit", "-qm", "Accept ownership decision"]);
    seal(&repo);
    write(&repo, path, &original.replace("must return", "must retry then return"));
    let packet = diff(&repo);
    assert_eq!(packet["totals"]["files"], 0);
    assert_eq!(packet["totals"]["accepted_decisions_changed"], 1);
    assert_eq!(packet["accepted_decision_changes"][0]["path"], path);
    let text = repo.run(&["context", "--diff"]);
    assert!(text.contains("accepted decision changed — confirm with the user / supersede instead"), "{text}");
    assert!(!text.contains("automatically confirms"), "{text}");

    // Changing status at the same time cannot hide a rewrite of an accepted body.
    write(&repo, path, &original.replace("status: accepted", "status: superseded")
        .replace("must return", "must retry then return"));
    git(&repo, &["add", path]);
    assert_eq!(diff(&repo)["accepted_decision_changes"][0]["path"], path);
}

#[test]
fn diff_does_not_flag_frontmatter_only_or_draft_decision_edits() {
    let repo = fixture("baseline");
    let path = "docs/decisions/metadata.md";
    let original = "---\nstatus: accepted\n---\n# Ownership\n\nThe caller owns the retry policy.\n";
    write(&repo, path, original);
    git(&repo, &["add", path]);
    git(&repo, &["-c", "user.name=Test", "-c", "user.email=test@example.invalid", "commit", "-qm", "Accept retry policy"]);
    write(&repo, path, &original.replace("status: accepted", "status: superseded\nsuperseded_by: replacement.md"));
    assert!(diff(&repo)["accepted_decision_changes"].as_array().unwrap().is_empty());
    write(&repo, "docs/decisions/draft.md", "---\nstatus: draft\n---\n# Draft\n\nA proposed policy.\n");
    git(&repo, &["add", "docs/decisions/draft.md"]);
    assert!(diff(&repo)["accepted_decision_changes"].as_array().unwrap().is_empty());
}

#[test]
fn current_snapshot_tracks_dirty_code_baselines_and_code_only_checkpoints() {
    let repo = fixture("baseline");
    seal(&repo);
    replace(&repo, "src/auth/refresh.rs", "+ 60", "+ 80");
    report(&repo, &["history", "snapshot", "--json"]);
    assert_eq!(current(&repo)["code_changes_since_snapshot"]["drifted"], 0);
    replace(&repo, "src/auth/refresh.rs", "+ 80", "+ 100");
    replace(&repo, "bulk/unrelated.rs", "{ 1 }", "{ 2 }");
    let packet = current(&repo);
    let summary = &packet["code_changes_since_snapshot"];
    assert_eq!(summary["available"], true);
    assert_eq!(summary["drifted"], 1);
    assert_eq!(summary["uncovered"], 1);
    let snapshot = report(&repo, &["history", "snapshot", "--json"]);
    assert_eq!(snapshot["skipped"], false);
    assert_eq!(current(&repo)["code_changes_since_snapshot"]["drifted"], 0);
    assert_eq!(
        report(&repo, &["history", "snapshot", "--json"])["skipped"],
        true
    );
}

#[test]
fn current_reports_covered_and_deleted_regions_and_limits_its_code_section_to_eight_lines() {
    let repo = fixture("baseline");
    replace(&repo, "bulk/unrelated.rs", "fn unrelated", "fn free_return");
    write(
        &repo,
        "docs/notes/mention.md",
        "# Mention\n\n`free_return` has a simple return value.\n",
    );
    seal(&repo);
    report(&repo, &["history", "snapshot", "--json"]);
    replace(&repo, "bulk/unrelated.rs", "{ 1 }", "{ 2 }");
    replace(&repo, "src/auth/refresh.rs", "+ 60", "+ 90");
    let packet = current(&repo);
    assert_eq!(packet["code_changes_since_snapshot"]["covered"], 1);
    assert_eq!(packet["code_changes_since_snapshot"]["drifted"], 1);
    git(
        &repo,
        &["checkout", "-q", "deleted", "--", "src/auth/refresh.rs"],
    );
    let summary = current(&repo);
    assert!(
        summary["code_changes_since_snapshot"]["drifted"]
            .as_u64()
            .unwrap()
            >= 2
    );
    for number in 0..10 {
        write(
            &repo,
            &format!("src/add{number}.rs"),
            &format!("fn added_{number}() {{}}\n"),
        );
    }
    git(&repo, &["add", "src"]);
    let text = repo.run(&["context", "--current"]);
    let section = text
        .lines()
        .skip_while(|line| !line.starts_with("Code changes since snapshot"))
        .take_while(|line| !line.starts_with("Unverified:"))
        .collect::<Vec<_>>();
    assert!(section.len() <= 8, "{text}");
    assert!(section.iter().any(|line| line.contains("more regions")), "{text}");
}

#[test]
fn non_git_for_and_current_work_while_diff_gives_an_explicit_exit_two() {
    let repo = fixture("baseline");
    fs::remove_dir_all(repo.root.join(".git")).unwrap();
    assert!(has_doc(
        &for_code(&repo, "refresh_token"),
        "docs/decisions/rust.md",
        "刷新策略"
    ));
    let output = repo
        .command(&["context", "--diff", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("requires a Git working tree"));
    report(&repo, &["history", "snapshot", "--json"]);
    replace(&repo, "bulk/unrelated.rs", "{ 1 }", "{ 2 }");
    assert_eq!(
        current(&repo)["code_changes_since_snapshot"]["uncovered"],
        1
    );
}

#[test]
fn legacy_snapshots_remain_readable_with_explicit_code_baseline_fallbacks() {
    let repo = fixture("baseline");
    let snapshot = report(&repo, &["history", "snapshot", "--json"]);
    let path = repo.root.join(format!(
        ".harnesskit/history/snapshots/{}.json",
        snapshot["id"].as_str().unwrap()
    ));
    let mut stored: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    stored.as_object_mut().unwrap().remove("code_regions");
    fs::write(&path, serde_json::to_vec(&stored).unwrap()).unwrap();
    replace(&repo, "src/auth/refresh.rs", "+ 60", "+ 90");
    let packet = current(&repo);
    assert_eq!(
        packet["code_changes_since_snapshot"]["baseline"],
        "legacy-git-head"
    );
    assert!(packet["code_changes_since_snapshot"]["hint"]
        .as_str()
        .unwrap()
        .contains("uncommitted"));
    stored["host_git_head"] = Value::Null;
    fs::write(&path, serde_json::to_vec(&stored).unwrap()).unwrap();
    assert_eq!(
        current(&repo)["code_changes_since_snapshot"]["available"],
        false
    );
    assert!(current(&repo)["code_changes_since_snapshot"]["hint"]
        .as_str()
        .unwrap()
        .contains("--allow-empty"));
    report(&repo, &["history", "list", "--json"]);
    report(&repo, &["history", "diff", "latest", "--json"]);
    report(&repo, &["history", "restore", "latest", "--json"]);
}

#[test]
fn new_modes_preserve_target_positionals_conflicts_and_legacy_doc_context() {
    let repo = fixture("baseline");
    let target = repo.root.to_str().unwrap();
    assert_eq!(
        report(
            &repo,
            &["context", "--for", "refresh_token", target, "--json"]
        )["kind"],
        "code"
    );
    assert_eq!(
        report(&repo, &["context", target, "--diff", "--json"])["kind"],
        "diff"
    );
    for flags in [
        vec!["--for", "refresh_token", "--current"],
        vec!["--diff", "--since", "latest"],
        vec!["--for", "refresh_token", "--diff"],
    ] {
        let mut args = vec!["context"];
        args.extend(flags);
        assert_eq!(repo.command(&args).output().unwrap().status.code(), Some(2));
    }
    assert_eq!(
        report(&repo, &["context", "docs/decisions/rust.md", "--json"])["kind"],
        "context"
    );
}

#[test]
fn outputs_budget_documents_files_regions_and_report_omitted_counts() {
    let repo = fixture("baseline");
    for number in 0..14 {
        write(&repo, &format!("docs/notes/budget{number}.md"), &format!("# Budget {number}\n\n## Rule\n\n`src/auth/refresh.rs#refresh_token` requires coverage.\n"));
    }
    seal(&repo);
    let packet = for_code(&repo, "src/auth/refresh.rs");
    assert_eq!(packet["documents"].as_array().unwrap().len(), 8);
    assert!(packet["truncated"]["documents"].as_u64().unwrap() > 0);
    for number in 0..14 {
        write(
            &repo,
            &format!("src/added{number}.rs"),
            &format!("fn missing_{number}() {{}}\n"),
        );
    }
    git(&repo, &["add", "src"]);
    let packet = diff(&repo);
    assert_eq!(packet["files"].as_array().unwrap().len(), 5);
    assert_eq!(packet["totals"]["files"], 14);
    assert_eq!(packet["truncated"]["files"], 9);
    assert!(packet["uncovered_regions"].as_array().unwrap().len() <= 8);
    for args in [
        vec!["context", "--for", "src/auth/refresh.rs", "--verbose"],
        vec!["context", "--diff", "--verbose"],
        vec!["context", "--for", "missing\nname"],
    ] {
        let text = repo.run(&args);
        assert!(text.lines().count() <= 40, "{text}");
    }
}
