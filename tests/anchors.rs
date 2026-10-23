mod support;

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::Command;

use rusqlite::Connection;
use serde_json::{json, Value};
use support::{assert_success, TestRepo};

fn write(repo: &TestRepo, path: &str, text: &str) {
    let path = repo.root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn database(repo: &TestRepo) -> Connection {
    Connection::open(repo.root.join(".harnesskit/state/facts.sqlite")).unwrap()
}

fn edges(repo: &TestRepo) -> Vec<Value> {
    database(repo).prepare(
        "SELECT s.kind, s.name, t.kind, t.path, t.name, e.kind, e.confidence, e.weight, e.evidence_json
         FROM edges e JOIN nodes s ON s.id = e.src JOIN nodes t ON t.id = e.dst
         ORDER BY e.src, e.dst, e.evidence_json;",
    ).unwrap().query_map([], |row| {
        Ok(json!({"source_kind": row.get::<_, String>(0)?, "source": row.get::<_, String>(1)?,
            "target_kind": row.get::<_, String>(2)?, "path": row.get::<_, String>(3)?,
            "name": row.get::<_, String>(4)?, "kind": row.get::<_, String>(5)?,
            "confidence": row.get::<_, String>(6)?, "weight": row.get::<_, f64>(7)?,
            "evidence": serde_json::from_str::<Value>(&row.get::<_, String>(8)?).unwrap()}))
    }).unwrap().collect::<rusqlite::Result<_>>().unwrap()
}

fn findings(repo: &TestRepo) -> Vec<Value> {
    let report: Value = serde_json::from_str(
        &repo.run(&["check", "--rules", "anchors", "--strict", "--json", "--all"]),
    )
    .unwrap();
    assert!(report["coverage"].is_object());
    assert!(report["hints"].is_array());
    assert_eq!(report["summary"]["warning"], 0);
    assert_eq!(report["summary"]["error"], 0);
    report["checks"].as_array().unwrap().clone()
}

fn findings_allow_warnings(repo: &TestRepo) -> Vec<Value> {
    let report: Value = serde_json::from_str(
        &repo.run(&["check", "--rules", "anchors", "--json", "--all"]),
    )
    .unwrap();
    report["checks"].as_array().unwrap().clone()
}

fn git(repo: &TestRepo, args: &[&str]) {
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
}

#[test]
fn paths_directories_file_symbols_and_unique_qualified_symbols_have_source_evidence() {
    let repo = TestRepo::new(Some("standard"));
    write(&repo, "src/auth/refresh.rs", "pub fn refresh_token() {}\nmod inner {\n    pub struct Session;\n    impl Session { pub fn renew(&self) {} }\n}\n");
    write(&repo, "src/auth/legacy.rs", "fn legacy() {}\n");
    write(&repo, "src/auth-other.rs", "fn unrelated() {}\n");
    write(&repo, "docs/local.rs", "fn local() {}\n");
    write(
        &repo,
        "docs/anchors.md",
        concat!(
            "---\nstatus: active\n---\n",
            "`src/auth/refresh.rs`\n\n# Anchors\n\n## Refresh\n",
            "`src/auth/`\n",
            "`src/auth/refresh.rs#refresh_token`\n",
            "`src/auth/refresh.rs#Session::renew`\n",
            "`Session::renew`\n",
            "`../src/auth/refresh.rs#renew`\n",
            "`./local.rs`\n",
            "`` src/auth/refresh.rs#refresh_token ``\n",
        ),
    );
    repo.run(&["index"]);
    let edges = edges(&repo);
    assert_eq!(edges.len(), 9);
    assert!(edges.iter().all(|edge| edge["kind"] == "anchor"
        && edge["confidence"] == "EXTRACTED"
        && edge["weight"] == 1.0
        && edge["evidence"]["doc"] == "docs/anchors.md"));
    let preamble = edges
        .iter()
        .find(|edge| edge["source_kind"] == "doc")
        .unwrap();
    assert_eq!(preamble["evidence"]["line"], 4);
    assert_eq!(preamble["evidence"]["section"], "");
    for edge in edges.iter().filter(|edge| edge["source_kind"] == "section") {
        assert_eq!(edge["source"], "Refresh");
        assert_eq!(edge["evidence"]["section"], "Refresh");
        assert!(edge["evidence"]["line"].as_u64().unwrap() >= 9);
    }
    let directory = edges
        .iter()
        .filter(|edge| edge["evidence"]["anchor"] == "src/auth/")
        .map(|edge| edge["path"].as_str().unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        directory,
        BTreeSet::from(["src/auth/legacy.rs", "src/auth/refresh.rs"])
    );
    assert_eq!(
        edges
            .iter()
            .find(|edge| edge["evidence"]["anchor"] == "Session::renew")
            .unwrap()["name"],
        "inner::Session::renew"
    );
    assert!(findings(&repo).is_empty());
    assert!(repo.root.join(".harnesskit/knowledge.lock").is_file());
}

#[test]
fn qualified_methods_in_other_default_languages_and_unsupported_file_paths_resolve() {
    let repo = TestRepo::new(Some("standard"));
    write(&repo, "web/cache.ts", "export class Cache { renew() {} }\n");
    write(
        &repo,
        "worker/retry.py",
        "class Worker:\n    def retry(self):\n        pass\n",
    );
    write(
        &repo,
        "gateway/limit.go",
        "package gateway\ntype Limiter struct {}\nfunc (l *Limiter) Allow() {}\n",
    );
    write(&repo, "native/Main.java", "class Main {}\n");
    write(
        &repo,
        "docs/anchors.md",
        "# Anchors\n\n`Cache::renew`\n`Worker::retry`\n`Limiter::Allow`\n`native/Main.java`\n",
    );
    let output = repo.command(&["index"]).env("PATH", "").output().unwrap();
    assert_success(&output, &["index"]);
    let edges = edges(&repo);
    assert_eq!(edges.len(), 4);
    let symbols = edges
        .iter()
        .filter(|edge| edge["target_kind"] == "symbol")
        .map(|edge| edge["name"].as_str().unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        symbols,
        BTreeSet::from(["Cache::renew", "Worker::retry", "Limiter::Allow"])
    );
    assert!(findings(&repo).is_empty());
}

#[test]
fn ambiguity_and_unresolved_paths_are_info_with_candidates_and_lines_are_accepted() {
    let repo = TestRepo::new(Some("standard"));
    write(&repo, "src/a.rs", "struct Session;\nimpl Session { fn renew(&self) {} }\nstruct Other;\nimpl Other { fn renew(&self) {} }\n");
    write(
        &repo,
        "src/b.rs",
        "struct Session;\nimpl Session { fn renew(&self) {} }\n",
    );
    write(&repo, "docs/anchors.md", "# Anchors\n\n`Session::renew`\n`src/a.rs#renew`\n`src/a.rs#missing`\n`src/missing.rs`\n`src/a.rs#L1-L2`\n`src/a.rs#L2`\n");
    let checks = findings(&repo);
    assert_eq!(checks.len(), 5);
    assert!(checks
        .iter()
        .all(|check| check["severity"] == "info" && check["fixable"] == false));
    for ambiguous in checks
        .iter()
        .filter(|check| check["rule_id"] == "anchor-ambiguous")
    {
        let candidates = ambiguous["evidence_json"]["candidates"].as_array().unwrap();
        assert_eq!(candidates.len(), 2);
        assert!(ambiguous["message"].as_str().unwrap().contains("src/a.rs#"));
        assert!(candidates
            .iter()
            .all(|candidate| candidate["span_start"].as_u64().unwrap() > 0));
    }
    let missing = checks
        .iter()
        .find(|check| check["evidence_json"]["anchor"] == "src/a.rs#missing")
        .unwrap();
    assert_eq!(missing["rule_id"], "anchor-unresolved");
    assert_eq!(
        missing["evidence_json"]["candidates"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    for line in checks
        .iter()
        .filter(|check| check["rule_id"] == "anchor-line-number")
    {
        assert!(line["hint"].as_str().unwrap().contains("symbol"));
    }
    let edges = edges(&repo);
    assert_eq!(edges.len(), 2);
    assert!(edges
        .iter()
        .all(|edge| edge["path"] == "src/a.rs" && edge["target_kind"] == "file"));
    assert_eq!(
        edges
            .iter()
            .find(|edge| edge["evidence"]["anchor"] == "src/a.rs#L1-L2")
            .unwrap()["evidence"]["target_span"],
        json!({"start": 1, "end": 2})
    );
}

#[test]
fn commands_options_unknown_symbols_and_markdown_examples_are_not_anchors() {
    let repo = TestRepo::new(Some("standard"));
    write(&repo, "src/a.rs", "fn known() {}\n");
    write(
        &repo,
        "docs/anchors.md",
        concat!(
            "---\nsummary: '`src/missing.rs`'\n---\n# Examples\n\n",
            "`cargo test` `--json` `a::b` `known` `cargo test src/a.rs`\n",
            "`--output=src/a.rs` `a / b` `https://example.invalid/src/a.rs`\n",
            "`1.0` `v0.2.0` `foo.bar` `(src/a.rs)` `#L10-L20`\n",
            "`src/{a,b}.rs` `src/[ab].rs`\n",
            "\\`src/a.rs\\`\n\n",
            "`` `src/a.rs` ``\n\n",
            "```md\n`src/a.rs` `src/missing.rs`\n````\n\n",
            "~~~~md\n`src/a.rs`\n```\n~~~~\n\n",
            "    `src/a.rs`\n\n",
            "<!-- `src/a.rs` -->\n\n",
            "An unclosed `src/a.rs\n",
        ),
    );
    assert!(findings(&repo).is_empty());
    assert!(edges(&repo).is_empty());
}

#[test]
fn relative_paths_prefer_the_document_directory_and_missing_paths_are_prose() {
    let repo = TestRepo::new(Some("standard"));
    write(&repo, "local.rs", "fn root() {}\n");
    write(&repo, "docs/local.rs", "fn local() {}\n");
    write(
        &repo,
        "docs/anchors.md",
        concat!(
            "# Anchors\n\n",
            "`local.rs`\n",
            "`missing.rs`\n",
            "`local.rs#missing`\n",
            "`missing.rs#missing`\n",
            "`src/<name>.rs#missing`\n",
            "`src/{name}.rs#missing`\n",
            "`src/*.rs#missing`\n",
        ),
    );
    let checks = findings(&repo);
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0]["evidence_json"]["anchor"], "local.rs#missing");
    let edges = edges(&repo);
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0]["path"], "docs/local.rs");
}

#[test]
fn example_documents_suppress_diagnostics_but_keep_real_edges() {
    let repo = TestRepo::new(Some("standard"));
    write(&repo, "src/known.rs", "fn known() {}\n");
    write(
        &repo,
        "docs/design-docs/examples/guide.md",
        "# Example\n\n`src/missing.rs#unknown`\n`src/known.rs#known`\n",
    );
    write(
        &repo,
        "docs/placeholders.md",
        "# Template\n\n`src/<name>.rs#unknown` `src/{name}.rs#unknown` `src/*.rs#unknown`\n",
    );
    let checks = findings(&repo);
    assert!(checks.is_empty());
    let edges = edges(&repo);
    assert!(edges
        .iter()
        .any(|edge| edge["evidence"]["doc"] == "docs/design-docs/examples/guide.md"
            && edge["path"] == "src/known.rs"));
}

#[test]
fn commonmark_code_spans_preserve_crlf_lines_heading_names_and_duplicate_occurrences() {
    let repo = TestRepo::new(Some("standard"));
    write(&repo, "src/a.rs", "fn first() {}\n");
    write(&repo, "src/owner's file.rs", "fn quoted() {}\n");
    write(
        &repo,
        "docs/owner's anchors.md",
        concat!(
            "---\r\nstatus: active\r\n---\r\n",
            "# Root\r\n\r\n## Use `src/a.rs`\r\n",
            "> `src/a.rs#first` and `src/a.rs#first`\r\n\r\n",
            "Repeated\r\n--------\r\n\r\n",
            "- ``src/owner's file.rs#quoted``\r\n",
        ),
    );
    assert!(findings(&repo).is_empty());
    let edges = edges(&repo);
    assert_eq!(edges.len(), 4);
    let repeated = edges
        .iter()
        .filter(|edge| edge["evidence"]["line"] == 7)
        .collect::<Vec<_>>();
    assert_eq!(repeated.len(), 2);
    assert_ne!(
        repeated[0]["evidence"]["column"],
        repeated[1]["evidence"]["column"]
    );
    assert!(repeated.iter().all(|edge| edge["source"] == "Use src/a.rs"));
    let quoted = edges
        .iter()
        .find(|edge| edge["path"] == "src/owner's file.rs")
        .unwrap();
    assert_eq!(quoted["evidence"]["line"], 12);
    assert_eq!(quoted["source"], "Repeated");
    let sections: Vec<(String, usize, usize)> = database(&repo).prepare(
        "SELECT name, span_start, span_end FROM nodes WHERE kind = 'section' AND path = ?1 ORDER BY span_start;"
    ).unwrap().query_map(["docs/owner's anchors.md"], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap().collect::<rusqlite::Result<_>>().unwrap();
    assert_eq!(
        sections,
        vec![
            ("Root".into(), 4, 12),
            ("Use src/a.rs".into(), 6, 8),
            ("Repeated".into(), 9, 12)
        ]
    );
}

#[test]
fn incremental_refresh_re_resolves_code_changes_and_removes_deleted_doc_edges() {
    let repo = TestRepo::new(Some("standard"));
    write(
        &repo,
        "src/a.rs",
        "struct Session;\nimpl Session { fn renew(&self) {} }\n",
    );
    write(
        &repo,
        "docs/anchors.md",
        "# Anchors\n\n`Session::renew`\n`src/a.rs#Session::renew`\n",
    );
    repo.run(&["index"]);
    let initial = edges(&repo);
    assert_eq!(initial.len(), 2);
    let db = database(&repo);
    db.execute_batch("CREATE TABLE anchor_audit(action TEXT);
        CREATE TRIGGER anchor_edge_insert AFTER INSERT ON edges BEGIN INSERT INTO anchor_audit VALUES ('edge'); END;
        CREATE TRIGGER anchor_edge_delete AFTER DELETE ON edges BEGIN INSERT INTO anchor_audit VALUES ('edge'); END;
        CREATE TRIGGER anchor_node_insert AFTER INSERT ON nodes BEGIN INSERT INTO anchor_audit VALUES ('node'); END;
        CREATE TRIGGER anchor_node_delete AFTER DELETE ON nodes BEGIN INSERT INTO anchor_audit VALUES ('node'); END;")
        .unwrap();
    repo.run(&["index"]);
    assert_eq!(edges(&repo), initial);
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM anchor_audit;", [], |row| row
            .get::<_, usize>(0))
            .unwrap(),
        0
    );
    write(
        &repo,
        "src/b.rs",
        "struct Session;\nimpl Session { fn renew(&self) {} }\n",
    );
    assert_eq!(
        findings(&repo)
            .iter()
            .filter(|check| check["rule_id"] == "anchor-ambiguous")
            .count(),
        1
    );
    assert_eq!(edges(&repo).len(), 1);
    fs::remove_file(repo.root.join("src/a.rs")).unwrap();
    let checks = findings_allow_warnings(&repo);
    assert!(checks.iter().any(|check| check["rule_id"] == "anchor-moved"));
    assert_eq!(edges(&repo).len(), 1);
    assert_eq!(edges(&repo)[0]["path"], "src/b.rs");
    write(
        &repo,
        "docs/anchors.md",
        "# Revised\n\n## New section\n\n`src/b.rs#Session::renew`\n",
    );
    repo.run(&["index"]);
    assert!(findings(&repo).is_empty());
    assert_eq!(edges(&repo)[0]["source"], "New section");
    assert_eq!(edges(&repo)[0]["evidence"]["line"], 5);
    fs::remove_file(repo.root.join("docs/anchors.md")).unwrap();
    repo.run(&["index"]);
    assert!(edges(&repo).is_empty());
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM nodes WHERE path = 'docs/anchors.md';",
            [],
            |row| row.get::<_, usize>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM inline_code_spans WHERE doc_path = 'docs/anchors.md';",
            [],
            |row| row.get::<_, usize>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(db.query_row("SELECT COUNT(*) FROM edges e LEFT JOIN nodes s ON e.src = s.id LEFT JOIN nodes t ON e.dst = t.id WHERE s.id IS NULL OR t.id IS NULL;", [], |row| row.get::<_, usize>(0)).unwrap(), 0);
}

#[test]
fn directory_and_file_anchors_respect_the_existing_git_and_schema_scan_boundaries() {
    let repo = TestRepo::new(Some("standard"));
    write(&repo, "src/tracked.rs", "fn tracked() {}\n");
    write(&repo, "src/ignored.rs", "fn ignored() {}\n");
    write(&repo, "src/schema.rs", "fn schema() {}\n");
    write(&repo, ".gitignore", ".harnesskit/state/\nsrc/ignored.rs\n");
    write(
        &repo,
        ".harnesskit/schema.yaml",
        &format!(
            "{}\nignore:\n  - 'src/schema.rs'\n",
            include_str!("../schemas/file-first-v0.yaml")
        ),
    );
    write(
        &repo,
        "docs/anchors.md",
        "# Anchors\n\n`src/`\n`src/untracked.rs`\n`src/ignored.rs`\n`src/schema.rs`\n",
    );
    git(&repo, &["init", "--quiet"]);
    git(&repo, &["add", "."]);
    git(&repo, &["add", "--force", "src/ignored.rs"]);
    write(&repo, "src/untracked.rs", "fn untracked() {}\n");
    let checks = findings(&repo);
    assert!(checks.is_empty());
    let edges = edges(&repo);
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0]["path"], "src/tracked.rs");
}

#[test]
fn w1_derived_state_rebuilds_without_changing_history_schema_or_a_lock_file() {
    let repo = TestRepo::new(Some("standard"));
    write(&repo, "src/a.rs", "fn first() {}\n");
    write(&repo, "docs/anchors.md", "# Anchors\n\n`src/a.rs#first`\n");
    repo.run(&["init", "--tracked"]);
    repo.run(&["history", "snapshot", "-m", "before W2 rebuild"]);
    repo.run(&["index"]);
    let initial = edges(&repo);
    let schema = fs::read(repo.root.join(".harnesskit/schema.yaml")).unwrap();
    let latest = fs::read(repo.root.join(".harnesskit/history/refs/latest")).unwrap();
    write(&repo, ".harnesskit/knowledge.lock", "untouched W3 state\n");
    database(&repo)
        .execute_batch(
            "DROP TABLE edges; DROP TABLE inline_code_spans;
        DELETE FROM nodes WHERE kind IN ('doc', 'section');
        UPDATE meta SET value = '7' WHERE key = 'fact_schema_version';",
        )
        .unwrap();
    repo.run(&["index"]);
    assert_eq!(edges(&repo), initial);
    assert_eq!(
        fs::read(repo.root.join(".harnesskit/schema.yaml")).unwrap(),
        schema
    );
    assert_eq!(
        fs::read(repo.root.join(".harnesskit/history/refs/latest")).unwrap(),
        latest
    );
    assert_eq!(
        fs::read_to_string(repo.root.join(".harnesskit/knowledge.lock")).unwrap(),
        "untouched W3 state\n"
    );
}

#[test]
fn w0_retrieval_and_drift_cases_have_real_anchor_edges_at_the_applicable_revisions() {
    let repo = TestRepo::new(None);
    let output = Command::new("python3")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/generate-weave-fixture.py"))
        .arg(&repo.root)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .output()
        .unwrap();
    assert_success(&output, &["generate-weave-fixture.py"]);
    let cases: Value = serde_json::from_str(include_str!("fixtures/weave/cases.json")).unwrap();
    for case in cases["retrieval"].as_array().unwrap() {
        git(
            &repo,
            &[
                "checkout",
                "--quiet",
                "--detach",
                case["revision"].as_str().unwrap(),
            ],
        );
        repo.run(&["index"]);
        let edges = edges(&repo);
        for section in case["expected_sections"].as_array().unwrap() {
            for anchor in case["expected_symbols"].as_array().unwrap() {
                let (path, name) = anchor.as_str().unwrap().split_once('#').unwrap();
                assert!(
                    edges
                        .iter()
                        .any(|edge| edge["evidence"]["doc"] == section["path"]
                            && edge["source"] == section["heading"]
                            && edge["path"] == path
                            && edge["name"] == name),
                    "{}",
                    case["id"]
                );
            }
        }
    }
    for case in cases["drift"].as_array().unwrap() {
        for revision_key in ["before", "after"] {
            git(
                &repo,
                &[
                    "checkout",
                    "--quiet",
                    "--detach",
                    case[revision_key].as_str().unwrap(),
                ],
            );
            repo.run(&["index"]);
            let found = edges(&repo).iter().any(|edge| {
                edge["evidence"]["doc"] == case["doc"]
                    && edge["evidence"]["anchor"] == case["anchor"]
            });
            let unresolved = revision_key == "after"
                && matches!(
                    case["expected_rule"].as_str(),
                    Some("anchor-moved" | "anchor-broken")
                );
            assert_eq!(found, !unresolved, "{} {revision_key}", case["id"]);
            if unresolved {
                assert!(findings_allow_warnings(&repo).iter().any(|check| {
                    check["rule_id"] == case["expected_rule"]
                        && check["evidence_json"]["doc"] == case["doc"]
                        && check["evidence_json"]["anchor"] == case["anchor"]
                }));
            }
        }
    }
    assert!(repo.root.join(".harnesskit/knowledge.lock").is_file());
}
