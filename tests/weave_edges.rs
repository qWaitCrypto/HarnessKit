mod support;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;
use serde_json::{json, Value};
use support::{assert_success, TestRepo};

fn write(repo: &TestRepo, path: &str, text: impl AsRef<[u8]>) {
    let path = repo.root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn git_command(repo: &TestRepo, args: &[&str]) -> Command {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let null = if cfg!(windows) { "NUL" } else { "/dev/null" };
    command
        .args(args)
        .current_dir(&repo.root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", null)
        .env("GIT_CONFIG_SYSTEM", null);
    command
}

fn git(repo: &TestRepo, args: &[&str]) -> String {
    let output = git_command(repo, args).output().unwrap();
    assert_success(&output, args);
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn commit_at(repo: &TestRepo, timestamp: u64) {
    git(repo, &["add", "."]);
    let date = format!("@{timestamp} +0000");
    let args = ["commit", "--quiet", "-m", "synthetic change"];
    let output = git_command(repo, &args)
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &date)
        .output()
        .unwrap();
    assert_success(&output, &args);
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn git_repo() -> TestRepo {
    let repo = TestRepo::new(None);
    write(
        &repo,
        ".gitignore",
        ".harnesskit/state/\n.harnesskit/knowledge.lock\n",
    );
    write(&repo, "docs/index.md", "# Documentation\n");
    write(&repo, "docs/project.md", "# Project\n");
    write(
        &repo,
        ".harnesskit/schema.yaml",
        concat!(
            "schema_version: 0\nmanaged_root: docs\nentrypoints:\n",
            "  manifest: docs/index.md\n  project_map: docs/project.md\n",
            "include:\n  'CHANGELOG.md': notes\n",
        ),
    );
    git(&repo, &["init", "--quiet"]);
    git(&repo, &["config", "user.name", "Weave tests"]);
    git(&repo, &["config", "user.email", "weave@example.invalid"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    git(&repo, &["config", "core.autocrlf", "false"]);
    commit_at(&repo, now());
    repo
}

fn database(repo: &TestRepo) -> Connection {
    Connection::open(repo.root.join(".harnesskit/state/facts.sqlite")).unwrap()
}

fn meta(repo: &TestRepo, key: &str) -> String {
    database(repo)
        .query_row("SELECT value FROM meta WHERE key = ?1;", [key], |row| {
            row.get(0)
        })
        .unwrap()
}

fn edges(repo: &TestRepo, kind: &str) -> Vec<Value> {
    database(repo)
        .prepare(
            "SELECT s.path, t.path, t.name, e.weight, e.confidence, e.evidence_json
         FROM edges e JOIN nodes s ON s.id = e.src JOIN nodes t ON t.id = e.dst
         WHERE e.kind = ?1 ORDER BY s.path, t.path, t.name, e.evidence_json;",
        )
        .unwrap()
        .query_map([kind], |row| {
            Ok(
                json!({"doc": row.get::<_, String>(0)?, "path": row.get::<_, String>(1)?,
            "name": row.get::<_, String>(2)?, "weight": row.get::<_, f64>(3)?,
            "confidence": row.get::<_, String>(4)?,
            "evidence": serde_json::from_str::<Value>(&row.get::<_, String>(5)?).unwrap()}),
            )
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn stats(repo: &TestRepo) -> BTreeMap<(String, String), (f64, u64)> {
    database(repo).prepare(
        "SELECT doc_path, code_path, weight, cochange_count FROM cochange_stats ORDER BY doc_path, code_path;",
    ).unwrap().query_map([], |row| Ok(((row.get(0)?, row.get(1)?), (row.get(2)?, row.get(3)?))))
        .unwrap().collect::<rusqlite::Result<_>>().unwrap()
}

fn pair(repo: &TestRepo, doc: &str, code: &str) -> (f64, u64) {
    stats(repo)[&(doc.to_string(), code.to_string())]
}

fn assert_same_stats(
    left: &BTreeMap<(String, String), (f64, u64)>,
    right: &BTreeMap<(String, String), (f64, u64)>,
) {
    assert_eq!(
        left.keys().collect::<Vec<_>>(),
        right.keys().collect::<Vec<_>>()
    );
    for (key, (weight, count)) in left {
        assert_eq!(*count, right[key].1, "{key:?}");
        assert!(
            (weight - right[key].0).abs() < 0.00001,
            "{key:?}: {weight} vs {}",
            right[key].0
        );
    }
}

#[test]
fn cochange_weights_are_bounded_decay_and_penalize_frequently_changed_code() {
    let repo = git_repo();
    let timestamp = now();
    for revision in 0..4 {
        write(
            &repo,
            "docs/feature.md",
            format!("# Feature\n\nRevision {revision}.\n"),
        );
        for file in ["src/specific.rs", "src/common.rs"] {
            write(
                &repo,
                file,
                format!("fn implement_feature() {{ let revision = {revision}; }}\n"),
            );
        }
        commit_at(&repo, timestamp);
    }
    for revision in 4..12 {
        write(
            &repo,
            "src/common.rs",
            format!("fn implement_feature() {{ let revision = {revision}; }}\n"),
        );
        commit_at(&repo, timestamp);
    }
    write(
        &repo,
        "docs/feature.md",
        "# Feature\n\nDocumentation-only refinement.\n",
    );
    commit_at(&repo, timestamp);
    for (doc, code, date) in [
        ("docs/old.md", "src/old.rs", timestamp - 90 * 86400),
        ("docs/recent.md", "src/recent.rs", timestamp),
    ] {
        write(&repo, doc, "# Isolated feature\n");
        write(&repo, code, "fn isolated_feature() {}\n");
        commit_at(&repo, date);
    }
    repo.run(&["index"]);
    let all = stats(&repo);
    assert!(all.values().all(|(weight, _)| (0.0..=1.0).contains(weight)));
    let specific = pair(&repo, "docs/feature.md", "src/specific.rs");
    let common = pair(&repo, "docs/feature.md", "src/common.rs");
    assert_eq!((specific.1, common.1), (4, 4));
    assert!(specific.0 > 2.0 * common.0, "{specific:?} vs {common:?}");
    let old = pair(&repo, "docs/old.md", "src/old.rs").0;
    let recent = pair(&repo, "docs/recent.md", "src/recent.rs").0;
    assert!((old / recent - 0.5).abs() < 0.000001);
    let edge = edges(&repo, "cochange")
        .into_iter()
        .find(|edge| edge["path"] == "src/common.rs")
        .unwrap();
    assert_eq!(edge["confidence"], "INFERRED");
    assert_eq!(edge["evidence"]["doc_commits"], 5);
    assert_eq!(edge["evidence"]["code_commits"], 12);
    assert_eq!(edge["evidence"]["count"], 4);
    assert_eq!(edge["evidence"]["half_life_days"], 90);
    assert_eq!(edge["evidence"]["last_commit"].as_str().unwrap().len(), 40);
}

#[test]
fn default_cochange_filter_excludes_noise_on_both_sides_and_schema_can_replace_it() {
    let repo = git_repo();
    let noise = [
        "Cargo.lock",
        "nested/deps.lock",
        "package-lock.json",
        "web/pnpm-lock.yaml",
        "web/yarn.lock",
        "go.sum",
        "Cargo.toml",
        "web/package.json",
        "agent/.codex-plugin/plugin.json",
        "web/manifest.json",
        "pyproject.toml",
        "go.mod",
        "CHANGELOG.md",
        "docs/CHANGELOG.md",
        "generated/api.rs",
        "dist/app.js",
        "target/debug/output.rs",
        "src/widget.generated.ts",
        "web/app.min.js",
        "web/app.js.map",
    ];
    write(&repo, "docs/feature.md", "# Feature\n");
    write(&repo, "src/service.rs", "fn serve_request() {}\n");
    for path in noise {
        write(&repo, path, "# Synthetic artifact\n");
    }
    commit_at(&repo, now());
    repo.run(&["index"]);
    let feature_edges = edges(&repo, "cochange")
        .into_iter()
        .filter(|edge| edge["doc"] == "docs/feature.md")
        .collect::<Vec<_>>();
    assert_eq!(feature_edges.len(), 1);
    assert_eq!(feature_edges[0]["path"], "src/service.rs");
    assert!(stats(&repo)
        .keys()
        .all(|(doc, _)| !doc.contains("CHANGELOG")));
    let schema = fs::read_to_string(repo.root.join(".harnesskit/schema.yaml")).unwrap();
    write(
        &repo,
        ".harnesskit/schema.yaml",
        format!("{schema}\ncochange_ignore: []\n"),
    );
    repo.run(&["index"]);
    for path in noise.iter().filter(|path| !path.ends_with(".md")) {
        assert!(pair(&repo, "docs/feature.md", path).0 > 0.0, "{path}");
    }
    assert!(pair(&repo, "CHANGELOG.md", "src/service.rs").0 > 0.0);
    // The override replaces defaults, and basename globs apply at any depth.
    write(
        &repo,
        ".harnesskit/schema.yaml",
        format!("{schema}\ncochange_ignore:\n  - '*.rs'\n"),
    );
    repo.run(&["index"]);
    assert!(stats(&repo).keys().all(|(_, code)| !code.ends_with(".rs")));
    assert!(pair(&repo, "docs/feature.md", "Cargo.toml").0 > 0.0);
}

#[test]
fn large_commits_are_filtered_before_noise_exclusions_and_limit_is_configurable() {
    let repo = git_repo();
    write(&repo, "docs/feature.md", "# Feature\n");
    write(&repo, "src/service.rs", "fn serve_request() {}\n");
    for number in 0..29 {
        write(&repo, &format!("generated/{number}.rs"), "// generated\n");
    }
    commit_at(&repo, now());
    repo.run(&["index"]);
    assert!(!stats(&repo).keys().any(|(doc, _)| doc == "docs/feature.md"));
    let schema = fs::read_to_string(repo.root.join(".harnesskit/schema.yaml")).unwrap();
    write(
        &repo,
        ".harnesskit/schema.yaml",
        format!("{schema}\nrules:\n  cochange_max_files: 31\n"),
    );
    repo.run(&["index"]);
    assert_eq!(pair(&repo, "docs/feature.md", "src/service.rs").1, 1);
    let edge = edges(&repo, "cochange")
        .into_iter()
        .find(|edge| edge["doc"] == "docs/feature.md")
        .unwrap();
    assert_eq!(edge["evidence"]["max_commit_files"], 31);
}

#[test]
fn incremental_history_preserves_counts_through_doc_refresh_and_matches_a_cold_rebuild() {
    let repo = git_repo();
    for revision in 0..2 {
        write(
            &repo,
            "docs/feature.md",
            format!("# Feature\n\nRevision {revision}.\n"),
        );
        write(
            &repo,
            "src/service.rs",
            format!("fn serve_request() {{ let revision = {revision}; }}\n"),
        );
        commit_at(&repo, now());
        repo.run(&["index"]);
        assert_eq!(
            pair(&repo, "docs/feature.md", "src/service.rs").1,
            revision + 1
        );
    }
    assert_eq!(meta(&repo, "cochange_scan_mode"), "delta");
    let cursor = meta(&repo, "cochange_last_commit");
    let before = stats(&repo);
    write(&repo, "docs/project.md", "# Project\n\nWork in progress.\n");
    repo.run(&["index"]);
    repo.run(&["index"]);
    assert_eq!(cursor, meta(&repo, "cochange_last_commit"));
    assert_same_stats(&before, &stats(&repo));
    let incremental = stats(&repo);
    fs::remove_dir_all(repo.root.join(".harnesskit/state")).unwrap();
    repo.run(&["index"]);
    assert_eq!(meta(&repo, "cochange_scan_mode"), "full");
    assert_same_stats(&incremental, &stats(&repo));
}

#[test]
fn rewritten_history_discards_old_counts_and_matches_a_cold_rebuild() {
    let repo = git_repo();
    for (path, text) in [
        ("docs/one.md", "# One\n"),
        ("docs/two.md", "# Two\n"),
        ("src/one.rs", "fn feature_one() {}\n"),
        ("src/two.rs", "fn feature_two() {}\n"),
    ] {
        write(&repo, path, text);
    }
    commit_at(&repo, now());
    let base = git(&repo, &["rev-parse", "HEAD"]);
    write(&repo, "docs/one.md", "# One\n\nOld branch.\n");
    write(&repo, "src/one.rs", "fn feature_one() { old_branch(); }\n");
    commit_at(&repo, now());
    repo.run(&["index"]);
    assert_eq!(pair(&repo, "docs/one.md", "src/one.rs").1, 2);
    git(&repo, &["reset", "--hard", "--quiet", &base]);
    write(&repo, "docs/two.md", "# Two\n\nReplacement branch.\n");
    write(&repo, "src/two.rs", "fn feature_two() { new_branch(); }\n");
    commit_at(&repo, now());
    repo.run(&["index"]);
    assert_eq!(meta(&repo, "cochange_scan_mode"), "full");
    assert_eq!(pair(&repo, "docs/one.md", "src/one.rs").1, 1);
    assert_eq!(pair(&repo, "docs/two.md", "src/two.rs").1, 2);
    let rebuilt = stats(&repo);
    fs::remove_dir_all(repo.root.join(".harnesskit/state")).unwrap();
    repo.run(&["index"]);
    assert_same_stats(&rebuilt, &stats(&repo));
}

#[test]
fn cochange_top_eight_retains_candidates_for_later_incremental_ranking() {
    let repo = git_repo();
    write(&repo, "docs/feature.md", "# Feature\n");
    for number in 0..10 {
        write(
            &repo,
            &format!("src/candidate{number:02}.rs"),
            "fn feature_impl() {}\n",
        );
    }
    commit_at(&repo, now());
    repo.run(&["index"]);
    let targets = || {
        edges(&repo, "cochange")
            .into_iter()
            .filter(|edge| edge["doc"] == "docs/feature.md")
            .map(|edge| edge["path"].as_str().unwrap().to_string())
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(targets().len(), 8);
    assert!(!targets().contains("src/candidate09.rs"));
    assert_eq!(
        stats(&repo)
            .keys()
            .filter(|(doc, _)| doc == "docs/feature.md")
            .count(),
        10
    );
    write(
        &repo,
        "docs/feature.md",
        "# Feature\n\nSpecific implementation.\n",
    );
    write(
        &repo,
        "src/candidate09.rs",
        "fn feature_impl() { improved(); }\n",
    );
    commit_at(&repo, now());
    repo.run(&["index"]);
    assert_eq!(targets().len(), 8);
    assert!(targets().contains("src/candidate09.rs"));
}

#[test]
fn newly_tracked_files_replay_history_and_legacy_w4_state_is_rebuilt() {
    let repo = git_repo();
    write(&repo, "docs/feature.md", "# Feature\n");
    write(&repo, "src/service.rs", "fn serve_request() {}\n");
    commit_at(&repo, now());
    git(&repo, &["rm", "--cached", "src/service.rs"]);
    repo.run(&["index"]);
    assert!(!stats(&repo)
        .keys()
        .any(|(_, code)| code == "src/service.rs"));
    git(&repo, &["add", "src/service.rs"]);
    repo.run(&["index"]);
    assert_eq!(meta(&repo, "cochange_scan_mode"), "full");
    assert_eq!(pair(&repo, "docs/feature.md", "src/service.rs").1, 1);
    let schema = fs::read(repo.root.join(".harnesskit/schema.yaml")).unwrap();
    database(&repo)
        .execute_batch(
            "UPDATE meta SET value = '9' WHERE key = 'fact_schema_version';
        UPDATE cochange_stats SET weight = 99; DROP TABLE cochange_files;",
        )
        .unwrap();
    repo.run(&["index"]);
    assert!(pair(&repo, "docs/feature.md", "src/service.rs").0 <= 1.0);
    assert_eq!(
        schema,
        fs::read(repo.root.join(".harnesskit/schema.yaml")).unwrap()
    );
}

#[test]
fn mentions_filter_plain_words_short_names_and_declarations_but_keep_definitions() {
    let repo = TestRepo::new(None);
    write(
        &repo,
        "src/main.rs",
        concat!(
            "mod context; mod index; mod cli; mod api_client; mod DeclaredModule;\n",
            "use elsewhere::ImportedType;\nfn context() {}\nfn index() {}\nfn Abc() {}\n",
            "fn api_client() {}\nfn snake_case() {}\nfn camelCase() {}\nstruct Session;\n",
            "const MAX_RETRIES: usize = 3;\nmod inline_module { pub fn module_action() {} }\n",
        ),
    );
    write(
        &repo,
        "src/types.ts",
        "import { ImportedClass } from './other';\nexport function processRequest() {}\n",
    );
    write(
        &repo,
        "src/worker.py",
        "from elsewhere import ImportedWorker\ndef retry_request(): pass\n",
    );
    write(
        &repo,
        "src/server.go",
        "package api_client\nimport ImportedServer \"other\"\nfunc HandleRequest() {}\n",
    );
    write(
        &repo,
        "docs/mentions.md",
        concat!(
            "# Mentions\n\n`context` `index` `cli` `Abc` `DeclaredModule` `ImportedType` ",
            "`ImportedClass` `ImportedWorker` `ImportedServer` `unknown_name` `cargo test` ",
            "`api_client` `snake_case` `camelCase` `Session` `MAX_RETRIES` `inline_module` ",
            "`module_action` `processRequest` `retry_request` `HandleRequest`\n",
        ),
    );
    repo.run(&["index"]);
    let mentions = edges(&repo, "mention");
    let identifiers = mentions
        .iter()
        .map(|edge| edge["evidence"]["identifier"].as_str().unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        identifiers,
        BTreeSet::from([
            "api_client",
            "snake_case",
            "camelCase",
            "Session",
            "MAX_RETRIES",
            "inline_module",
            "module_action",
            "processRequest",
            "retry_request",
            "HandleRequest"
        ])
    );
    let api_client = mentions
        .iter()
        .filter(|edge| edge["evidence"]["identifier"] == "api_client")
        .collect::<Vec<_>>();
    assert_eq!(api_client.len(), 1);
    assert_eq!(api_client[0]["path"], "src/main.rs");
    assert_eq!(api_client[0]["evidence"]["ambiguous"], false);
    // The out-of-line module is still indexed for explicit anchors.
    let declared: usize = database(&repo)
        .query_row(
            "SELECT COUNT(*) FROM nodes WHERE name = 'DeclaredModule' AND is_definition = 0;",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(declared, 1);
}

#[test]
fn mentions_prefer_nearby_paths_and_keep_only_top_ties_ambiguous() {
    let repo = TestRepo::new(None);
    write(&repo, ".harnesskit/schema.yaml", concat!(
        "schema_version: 0\nentrypoints:\n  manifest: docs/index.md\n  project_map: docs/project.md\n",
        "include:\n  'docs/auth/*.md': notes\n",
    ));
    for path in ["src/auth/one.rs", "src/auth/two.rs", "src/cache/other.rs"] {
        write(&repo, path, "fn refresh_token() {}\n");
    }
    write(&repo, "docs/auth/guide.md", "# Guide\n\n`refresh_token`\n");
    repo.run(&["index"]);
    let mentions = edges(&repo, "mention");
    assert_eq!(mentions.len(), 2);
    assert!(mentions
        .iter()
        .all(|edge| edge["path"].as_str().unwrap().starts_with("src/auth/")));
    for edge in &mentions {
        assert_eq!(edge["evidence"]["ambiguous"], true);
        assert_eq!(edge["weight"], 0.1);
        assert_eq!(edge["evidence"]["candidates"].as_array().unwrap().len(), 3);
    }
    fs::remove_file(repo.root.join("src/auth/two.rs")).unwrap();
    repo.run(&["index"]);
    let mentions = edges(&repo, "mention");
    assert_eq!(mentions.len(), 1);
    assert_eq!(mentions[0]["path"], "src/auth/one.rs");
    assert_eq!(mentions[0]["evidence"]["ambiguous"], false);
    assert_eq!(mentions[0]["weight"], 1.0);
}

#[test]
fn mentions_use_cochange_to_resolve_equal_paths_and_never_duplicate_explicit_anchors() {
    let repo = git_repo();
    write(&repo, "src/first.rs", "fn refresh_token() {}\n");
    write(&repo, "src/second.rs", "fn refresh_token() {}\n");
    commit_at(&repo, now());
    write(
        &repo,
        "docs/feature.md",
        "# Feature\n\n`refresh_token` `src/first.rs#refresh_token`\n",
    );
    write(
        &repo,
        "src/second.rs",
        "fn refresh_token() { refreshed(); }\n",
    );
    commit_at(&repo, now());
    repo.run(&["index"]);
    let mentions = edges(&repo, "mention");
    assert_eq!(mentions.len(), 1);
    assert_eq!(mentions[0]["path"], "src/second.rs");
    assert_eq!(mentions[0]["evidence"]["ambiguous"], false);
    assert!(mentions[0]["evidence"]["cochange_weight"].as_f64().unwrap() > 0.0);
    assert_eq!(edges(&repo, "anchor").len(), 1);
    assert_eq!(edges(&repo, "anchor")[0]["path"], "src/first.rs");
    // Non-Git mode drops stale history and turns the unresolved mention into a tie.
    fs::remove_dir_all(repo.root.join(".git")).unwrap();
    repo.run(&["index"]);
    assert!(stats(&repo).is_empty());
    assert!(edges(&repo, "cochange").is_empty());
    let mentions = edges(&repo, "mention");
    assert_eq!(mentions.len(), 2);
    assert!(mentions
        .iter()
        .all(|edge| edge["evidence"]["ambiguous"] == true));
}
