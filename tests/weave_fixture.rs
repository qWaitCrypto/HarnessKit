mod support;

use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::process::Command;
use support::{assert_success, TestRepo};

fn corpus() -> Value {
    serde_json::from_str(include_str!("fixtures/weave/cases.json")).unwrap()
}

fn python(script: &str) -> Command {
    let mut command = Command::new("python3");
    command.arg(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("scripts")
            .join(script),
    );
    command.env("PYTHONDONTWRITEBYTECODE", "1");
    command
}

fn generate(repo: &TestRepo) -> Value {
    let output = python("generate-weave-fixture.py")
        .arg(&repo.root)
        .output()
        .unwrap();
    assert_success(&output, &["generate-weave-fixture.py"]);
    serde_json::from_slice(&output.stdout).unwrap()
}

fn git(repo: &TestRepo, args: &[&str]) -> String {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let null = if cfg!(windows) { "NUL" } else { "/dev/null" };
    let output = command
        .args(args)
        .current_dir(&repo.root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", null)
        .env("GIT_CONFIG_SYSTEM", null)
        .output()
        .unwrap();
    assert_success(&output, args);
    String::from_utf8(output.stdout).unwrap()
}

fn read_at(repo: &TestRepo, revision: &str, path: &str) -> String {
    git(repo, &["show", &format!("{}:{}", revision, path)])
}

fn changed_paths(repo: &TestRepo, revision: &str) -> BTreeSet<String> {
    git(
        repo,
        &[
            "diff-tree",
            "--root",
            "--no-commit-id",
            "--name-only",
            "--no-renames",
            "-r",
            revision,
            "--",
        ],
    )
    .lines()
    .map(str::to_string)
    .collect()
}

#[test]
fn history_is_deterministic_despite_caller_git_configuration_and_identity() {
    let first = TestRepo::new(None);
    let second = TestRepo::new(None);
    let hostile = TestRepo::new(None);
    let config = hostile.root.join("global-config");
    fs::write(
        &config,
        "[user]\nname = Caller\nemail = caller@example.invalid\n[init]\ndefaultBranch = caller\n[commit]\ngpgSign = true\n[tag]\ngpgSign = true\n[core]\nautocrlf = true\n",
    )
    .unwrap();
    let expected = generate(&first);
    let output = python("generate-weave-fixture.py")
        .arg(&second.root)
        .env("GIT_CONFIG_GLOBAL", &config)
        .env("GIT_CONFIG_SYSTEM", &config)
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "core.autocrlf")
        .env("GIT_CONFIG_VALUE_0", "true")
        .env("GIT_DEFAULT_HASH", "sha256")
        .env("GIT_DIR", hostile.root.join("redirected.git"))
        .env("GIT_WORK_TREE", &hostile.root)
        .env("GIT_INDEX_FILE", hostile.root.join("redirected-index"))
        .env("GIT_AUTHOR_NAME", "Caller author")
        .env("GIT_COMMITTER_NAME", "Caller committer")
        .env("GIT_AUTHOR_EMAIL", "author@example.invalid")
        .env("GIT_COMMITTER_EMAIL", "committer@example.invalid")
        .env("GIT_AUTHOR_DATE", "2099-01-01T00:00:00+0900")
        .env("GIT_COMMITTER_DATE", "2099-01-02T00:00:00-0500")
        .env("TZ", "Pacific/Honolulu")
        .output()
        .unwrap();
    assert_success(
        &output,
        &["generate-weave-fixture.py", "hostile environment"],
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        expected
    );
    assert_eq!(expected["revisions"].as_object().unwrap().len(), 10);
    for args in [
        vec!["rev-list", "--reverse", "HEAD"],
        vec!["ls-tree", "-r", "HEAD"],
        vec!["show-ref", "--tags"],
    ] {
        assert_eq!(git(&first, &args), git(&second, &args));
    }
    assert_eq!(git(&first, &["branch", "--show-current"]).trim(), "main");
    assert_eq!(git(&first, &["status", "--porcelain"]), "");
    assert_eq!(git(&second, &["status", "--porcelain"]), "");
    assert!(!hostile.root.join("redirected.git").exists());
    assert!(!hostile.root.join("redirected-index").exists());
    let metadata = git(
        &first,
        &["log", "--reverse", "--format=%an|%ae|%cn|%ce|%at|%ct"],
    );
    for (index, line) in metadata.lines().enumerate() {
        let timestamp = 1767225600 + index * 86400;
        assert_eq!(
            line,
            format!(
                "Weave Fixture|weave@example.invalid|Weave Fixture|weave@example.invalid|{}|{}",
                timestamp, timestamp
            )
        );
    }
}

#[test]
fn generation_refuses_to_overwrite_an_existing_directory() {
    let repo = TestRepo::new(None);
    fs::write(repo.root.join("sentinel"), "preserve me\n").unwrap();
    let output = python("generate-weave-fixture.py")
        .arg(&repo.root)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("destination must be empty"));
    assert_eq!(
        fs::read_to_string(repo.root.join("sentinel")).unwrap(),
        "preserve me\n"
    );
    assert!(!repo.root.join(".git").exists());
}

#[test]
fn history_contains_the_expected_logic_format_moves_deletions_and_reviews() {
    let repo = TestRepo::new(None);
    generate(&repo);
    let cases = corpus();
    for case in cases["drift"].as_array().unwrap() {
        let before = case["before"].as_str().unwrap();
        let after = case["after"].as_str().unwrap();
        let (path, symbol) = case["anchor"].as_str().unwrap().split_once('#').unwrap();
        let old = read_at(&repo, before, path);
        let new = read_at(&repo, after, path);
        assert!(old.contains(symbol), "{}", case["id"]);
        match case["expected_rule"].as_str() {
            Some("code-drift") => assert_eq!(old.replace("+ 60", "+ 90"), new),
            Some("anchor-moved") => {
                let (target, name) = case["expected_target"]
                    .as_str()
                    .unwrap()
                    .split_once('#')
                    .unwrap();
                assert_eq!(name, symbol);
                let moved = read_at(&repo, after, target);
                assert!(old.contains(moved.trim_start_matches("package gateway\n\n")));
                assert!(!new.contains(symbol));
            }
            Some("anchor-broken") => {
                let tracked = git(&repo, &["ls-tree", "-r", "--name-only", after]);
                for source in tracked.lines().filter(|path| {
                    [".rs", ".ts", ".py", ".go"]
                        .iter()
                        .any(|suffix| path.ends_with(suffix))
                }) {
                    assert!(!read_at(&repo, after, source).contains(symbol));
                }
            }
            None if after.starts_with("reviewed-") => {
                let paths = changed_paths(&repo, after);
                assert!(paths.contains(path));
                assert!(paths.contains(case["doc"].as_str().unwrap()));
                assert_eq!(old.replace("+ 90", "+ 120"), new);
            }
            None => {
                assert_eq!((before, after), ("logic", "format"));
                assert_ne!(old, new);
                let diff = git(
                    &repo,
                    &[
                        "diff",
                        "--ignore-all-space",
                        "--unified=0",
                        before,
                        after,
                        "--",
                        path,
                    ],
                );
                let added = diff
                    .lines()
                    .filter(|line| line.starts_with('+') && !line.starts_with("+++"))
                    .map(|line| line[1..].trim())
                    .collect::<Vec<_>>();
                assert_eq!(added.len(), 1);
                assert!(matches!(
                    added[0],
                    "// Formatting-only checkpoint." | "# Formatting-only checkpoint."
                ));
                assert!(!diff
                    .lines()
                    .any(|line| line.starts_with('-') && !line.starts_with("---")));
            }
            other => panic!("unexpected drift expectation {:?}", other),
        }
    }
}

#[test]
fn cochange_expectations_have_small_commit_evidence_and_bulk_only_negative_controls() {
    let repo = TestRepo::new(None);
    let manifest = generate(&repo);
    let changes = manifest["revisions"]
        .as_object()
        .unwrap()
        .keys()
        .map(|revision| (revision.as_str(), changed_paths(&repo, revision)))
        .collect::<BTreeMap<_, _>>();
    assert!(changes["baseline"].len() > 30);
    assert_eq!(changes["bulk"].len(), 33);
    for case in corpus()["cochange"].as_array().unwrap() {
        let doc = case["doc"].as_str().unwrap();
        let small = changes
            .values()
            .filter(|paths| paths.len() <= 30 && paths.contains(doc))
            .collect::<Vec<_>>();
        for expected in case["expected_paths"].as_array().unwrap() {
            let path = expected.as_str().unwrap();
            assert!(
                small.iter().any(|paths| paths.contains(path)),
                "{}",
                case["id"]
            );
        }
        for forbidden in case["forbidden_paths"].as_array().unwrap() {
            let path = forbidden.as_str().unwrap();
            assert!(
                small.iter().all(|paths| !paths.contains(path)),
                "{}",
                case["id"]
            );
        }
    }
}

#[test]
fn runner_executes_all_w0_cases_including_chinese_code_retrieval() {
    let output = python("evaluate-weave.py").output().unwrap();
    assert_success(&output, &["evaluate-weave.py"]);
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["fixture_validated"], true);
    assert_eq!(report["fixture_commits"], 10);
    assert_eq!(
        report["summary"],
        json!({"total": 37, "executed": 37, "pending": 0,
            "by_kind": {"retrieval": 8, "drift": 20, "cochange": 9}})
    );
    assert!(report["cochange_coverage"].as_f64().unwrap() >= 0.9);
    assert!(
        report["retrieval_metrics"]["symbol_top3_rate"]
            .as_f64()
            .unwrap()
            >= 0.8
    );
    assert_eq!(report["retrieval_metrics"]["chinese_queries"], 4);
    assert_eq!(report["retrieval_metrics"]["section_top3_hits"], 8);
    let cases = report["cases"].as_array().unwrap();
    let ids = cases
        .iter()
        .map(|case| case["id"].as_str().unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(ids.len(), 37);
    for case in cases {
        assert_eq!(case["status"], "passed", "{}", case);
        assert!(!case["required_steps"].as_array().unwrap().is_empty());
        if case["kind"] == "cochange" || case["kind"] == "retrieval" {
            assert!(case["score"].as_f64().unwrap() >= 0.9);
        } else {
            assert!(case.get("score").is_none());
        }
    }
    assert!(!Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/weave/.git")
        .exists());
}

#[test]
fn runner_rejects_missing_expectation_targets() {
    let repo = TestRepo::new(None);
    let mut invalid = corpus();
    invalid["retrieval"][0]["expected_symbols"][0] =
        json!("src/auth/refresh.rs#missing_fixture_symbol");
    let path = repo.root.join("invalid-cases.json");
    fs::write(&path, serde_json::to_vec(&invalid).unwrap()).unwrap();
    let output = python("evaluate-weave.py")
        .arg("--corpus")
        .arg(path)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("missing fixture target"));
}

#[test]
fn generated_documents_are_managed_and_searchable_by_the_existing_cli() {
    let repo = TestRepo::new(None);
    generate(&repo);
    let report: Value = serde_json::from_str(&repo.run(&["index", "--json"])).unwrap();
    assert_eq!(report["coverage"]["unmanaged"], 0);
    let listed: Value = serde_json::from_str(&repo.run(&["list-docs", "--json"])).unwrap();
    let listed = serde_json::to_string(&listed).unwrap();
    for case in corpus()["cochange"].as_array().unwrap() {
        assert!(listed.contains(case["doc"].as_str().unwrap()));
    }
    let query: Value = serde_json::from_str(&repo.run(&["query", "令牌刷新", "--json"])).unwrap();
    assert!(query["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .any(|candidate| candidate["path"] == "docs/decisions/rust.md"));
    assert_eq!(git(&repo, &["status", "--porcelain"]), "");
}
