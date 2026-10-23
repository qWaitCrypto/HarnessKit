mod support;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::process::Command;

use rusqlite::Connection;
use serde_json::Value;
use support::{assert_success, TestRepo};

fn write(repo: &TestRepo, path: &str, text: impl AsRef<[u8]>) {
    let path = repo.root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
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

fn database(repo: &TestRepo) -> Connection {
    Connection::open(repo.root.join(".harnesskit/state/facts.sqlite")).unwrap()
}

fn fingerprints(repo: &TestRepo) -> BTreeMap<(String, String), String> {
    database(repo)
        .prepare(
            "SELECT path, name, fingerprint FROM nodes WHERE kind = 'symbol' ORDER BY path, name;",
        )
        .unwrap()
        .query_map([], |row| Ok(((row.get(0)?, row.get(1)?), row.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn file_paths(repo: &TestRepo) -> BTreeSet<String> {
    database(repo)
        .prepare("SELECT path FROM nodes WHERE kind = 'file' ORDER BY path;")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

#[test]
fn w0_history_preserves_format_fingerprints_and_changes_logic_fingerprints() {
    let repo = TestRepo::new(None);
    let output = Command::new("python3")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/generate-weave-fixture.py"))
        .arg(&repo.root)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .output()
        .unwrap();
    assert_success(&output, &["generate-weave-fixture.py"]);
    let targets = [
        ("src/auth/refresh.rs", "refresh_token"),
        ("web/cache.ts", "cacheTtl"),
        ("worker/retry.py", "retry_delay"),
        ("gateway/limit.go", "limitRequests"),
    ];
    git(&repo, &["checkout", "--quiet", "--detach", "baseline"]);
    repo.run(&["index"]);
    let baseline = fingerprints(&repo);
    git(&repo, &["checkout", "--quiet", "--detach", "logic"]);
    repo.run(&["index"]);
    let logic = fingerprints(&repo);
    for (path, name) in targets {
        let key = (path.to_string(), name.to_string());
        assert_ne!(baseline[&key], logic[&key], "{path}#{name}");
    }
    git(&repo, &["checkout", "--quiet", "--detach", "format"]);
    repo.run(&["index"]);
    let formatted = fingerprints(&repo);
    assert_eq!(logic, formatted);
    let files_before = file_paths(&repo);
    let file_hashes = |repo: &TestRepo| -> BTreeMap<String, String> {
        database(repo)
            .prepare("SELECT path, fingerprint FROM nodes WHERE kind = 'file';")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    let formatted_files = file_hashes(&repo);
    git(&repo, &["checkout", "--quiet", "--detach", "logic"]);
    repo.run(&["index"]);
    assert_eq!(formatted_files, file_hashes(&repo));
    git(&repo, &["checkout", "--quiet", "--detach", "moved"]);
    repo.run(&["index"]);
    let moved = fingerprints(&repo);
    for (old_path, new_path, name) in [
        (
            "src/auth/refresh.rs",
            "src/auth/normalize.rs",
            "normalize_token",
        ),
        ("web/cache.ts", "web/keys.ts", "normalizeKey"),
        ("worker/retry.py", "worker/queues.py", "normalize_queue"),
        ("gateway/limit.go", "gateway/routes.go", "normalizeRoute"),
    ] {
        let before = (old_path.to_string(), name.to_string());
        let after = (new_path.to_string(), name.to_string());
        assert!(!moved.contains_key(&before));
        assert_eq!(formatted[&before], moved[&after]);
        assert!(!files_before.contains(new_path));
        assert!(file_paths(&repo).contains(new_path));
    }
    git(&repo, &["checkout", "--quiet", "--detach", "deleted"]);
    repo.run(&["index"]);
    let deleted = fingerprints(&repo);
    for name in ["legacy_token", "legacyCache", "legacy_retry", "legacyLimit"] {
        assert!(!deleted.keys().any(|(_, found)| found == name));
    }
}

#[test]
fn git_scans_only_tracked_files_and_applies_gitignore_and_schema_ignore() {
    let repo = TestRepo::new(Some("standard"));
    write(&repo, "src/active.rs", "fn active() {}\n");
    write(&repo, "src/schema-ignored.rs", "fn schema_ignored() {}\n");
    write(&repo, "src/ignored.rs", "fn ignored() {}\n");
    write(&repo, "src/nested/.gitignore", "*.rs\n!kept.rs\n");
    write(&repo, "src/nested/ignored.rs", "fn nested_ignored() {}\n");
    write(&repo, "src/nested/kept.rs", "fn kept() {}\n");
    write(&repo, ".gitignore", ".harnesskit/state/\nsrc/ignored.rs\n");
    write(
        &repo,
        ".harnesskit/schema.yaml",
        format!(
            "{}\nignore:\n  - 'src/schema-ignored.rs'\n",
            include_str!("../schemas/file-first-v0.yaml")
        ),
    );
    git(&repo, &["init", "--quiet"]);
    git(&repo, &["add", "."]);
    git(
        &repo,
        &["add", "--force", "src/ignored.rs", "src/nested/ignored.rs"],
    );
    write(&repo, "src/untracked.rs", "fn untracked() {}\n");
    repo.run(&["index"]);
    let names = fingerprints(&repo)
        .keys()
        .map(|(_, name)| name.clone())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        names,
        BTreeSet::from(["active".to_string(), "kept".to_string()])
    );
    let report: Value = serde_json::from_str(&repo.run(&["doctor", "--code", "--json"])).unwrap();
    assert_eq!(report["code"]["scan_mode"], "git-tracked");

    // A file left on disk after being removed from the index is no longer code input.
    git(
        &repo,
        &["rm", "--cached", "--quiet", "--force", "src/active.rs"],
    );
    repo.run(&["index"]);
    assert!(repo.root.join("src/active.rs").is_file());
    assert!(!file_paths(&repo).contains("src/active.rs"));
    git(&repo, &["add", "src/untracked.rs"]);
    repo.run(&["index"]);
    assert!(file_paths(&repo).contains("src/untracked.rs"));
}

#[test]
fn non_git_walk_respects_nested_ignores_schema_and_binary_boundaries() {
    let repo = TestRepo::new(Some("standard"));
    write(&repo, ".gitignore", "src/ignored.rs\n");
    write(&repo, "src/.ignore", "*.py\n!kept.py\n");
    write(&repo, "src/ignored.rs", "fn ignored() {}\n");
    write(&repo, "src/ignored.py", "def ignored(): pass\n");
    write(&repo, "src/kept.py", "def kept(): pass\n");
    write(&repo, "src/Main.java", "class Main {}\n");
    write(&repo, "src/binary.rs", b"fn binary() {}\0\n");
    write(&repo, "src/invalid.rs", [0xff, 0xfe]);
    write(&repo, "src/schema.rs", "fn schema() {}\n");
    write(
        &repo,
        ".harnesskit/schema.yaml",
        format!(
            "{}\nignore:\n  - 'src/schema.rs'\n",
            include_str!("../schemas/file-first-v0.yaml")
        ),
    );
    repo.run(&["index"]);
    let paths = file_paths(&repo);
    assert!(paths.contains("src/kept.py"));
    assert!(paths.contains("src/Main.java"));
    for path in [
        "src/ignored.rs",
        "src/ignored.py",
        "src/binary.rs",
        "src/invalid.rs",
        "src/schema.rs",
    ] {
        assert!(!paths.contains(path), "{path}");
    }
    let names = fingerprints(&repo);
    assert_eq!(names.len(), 1);
    assert!(names.contains_key(&("src/kept.py".to_string(), "kept".to_string())));
}

#[test]
fn incremental_code_updates_reuse_states_and_leave_unchanged_nodes_untouched() {
    let repo = TestRepo::new(Some("standard"));
    write(&repo, "src/stable.rs", "fn stable() { one(); }\n");
    write(&repo, "src/change.rs", "fn change() { one(); }\n");
    write(&repo, "src/delete.rs", "fn delete() {}\n");
    repo.run(&["index"]);
    let initial = fingerprints(&repo);
    let db = database(&repo);
    db.execute_batch("CREATE TABLE node_audit(path TEXT);
        CREATE TRIGGER audit_insert AFTER INSERT ON nodes BEGIN INSERT INTO node_audit VALUES (NEW.path); END;
        CREATE TRIGGER audit_delete AFTER DELETE ON nodes BEGIN INSERT INTO node_audit VALUES (OLD.path); END;
        CREATE TRIGGER audit_update AFTER UPDATE ON nodes BEGIN INSERT INTO node_audit VALUES (NEW.path); END;")
        .unwrap();
    repo.run(&["index"]);
    let audit_count = || {
        db.query_row("SELECT COUNT(*) FROM node_audit;", [], |row| {
            row.get::<_, usize>(0)
        })
        .unwrap()
    };
    assert_eq!(audit_count(), 0);
    let state: (String, u64, u64) = db.query_row(
        "SELECT content_hash, file_size_bytes, mtime_ns FROM file_states WHERE path = 'src/stable.rs' AND kind = 'code';",
        [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).unwrap();
    assert_eq!(state.0.len(), 64);
    assert!(state.1 > 0 && state.2 > 0);
    // Rewriting identical content updates metadata without rebuilding its nodes.
    write(&repo, "src/stable.rs", "fn stable() { one(); }\n");
    repo.run(&["index"]);
    assert_eq!(audit_count(), 0);
    write(&repo, "src/change.rs", "fn change() { two(); }\n");
    write(&repo, "src/added.js", "function added() {}\n");
    fs::remove_file(repo.root.join("src/delete.rs")).unwrap();
    repo.run(&["index"]);
    let changed = fingerprints(&repo);
    let stable = ("src/stable.rs".to_string(), "stable".to_string());
    let change = ("src/change.rs".to_string(), "change".to_string());
    assert_eq!(initial[&stable], changed[&stable]);
    assert_ne!(initial[&change], changed[&change]);
    assert!(changed.contains_key(&("src/added.js".to_string(), "added".to_string())));
    assert!(!file_paths(&repo).contains("src/delete.rs"));
    let stable_writes: usize = db
        .query_row(
            "SELECT COUNT(*) FROM node_audit WHERE path = 'src/stable.rs';",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stable_writes, 0);
    // Document refresh must not treat code states as deleted documents.
    write(
        &repo,
        "docs/project.md",
        "# Updated project\n\nOnly the docs changed.\n",
    );
    repo.run(&["index"]);
    assert_eq!(fingerprints(&repo), changed);
}

#[test]
fn deleted_ignored_and_binary_files_remove_stale_nodes_and_states() {
    let repo = TestRepo::new(Some("standard"));
    write(&repo, "a.rs", "fn first() {}\n");
    repo.run(&["index"]);
    write(&repo, "a.rs", b"\0binary data");
    repo.run(&["index"]);
    assert!(!file_paths(&repo).contains("a.rs"));
    let states: usize = database(&repo)
        .query_row(
            "SELECT COUNT(*) FROM file_states WHERE path = 'a.rs';",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(states, 0);
    write(&repo, "a.rs", "fn restored() {}\n");
    repo.run(&["index"]);
    assert!(file_paths(&repo).contains("a.rs"));
    write(&repo, ".gitignore", "a.rs\n");
    repo.run(&["index"]);
    assert!(!file_paths(&repo).contains("a.rs"));
}

#[test]
fn doctor_code_reports_languages_files_symbols_and_refreshes_without_prior_index() {
    let repo = TestRepo::new(Some("standard"));
    for (path, text) in [
        ("a.rs", "fn rust_symbol() {}"),
        ("a.ts", "function tsSymbol() {}"),
        ("a.js", "function jsSymbol() {}"),
        ("a.py", "def python_symbol(): pass"),
        ("a.go", "package a\nfunc goSymbol() {}"),
        ("Main.java", "class Main {}"),
    ] {
        write(&repo, path, text);
    }
    let ordinary: Value = serde_json::from_str(&repo.run(&["doctor", "--json"])).unwrap();
    assert!(ordinary.get("code").is_none());
    assert!(!repo.root.join(".harnesskit/state/facts.sqlite").exists());
    let report: Value = serde_json::from_str(&repo.run(&["doctor", "--code", "--json"])).unwrap();
    assert!(report["coverage"].is_object());
    assert!(report["hints"].is_array());
    assert_eq!(report["code"]["scan_mode"], "walk");
    assert_eq!(report["state_exists"], true);
    assert_eq!(report["code"]["files"], 6);
    assert_eq!(report["code"]["symbols"], 6); // Go also supplies a package symbol.
    let languages = report["code"]["languages"].as_array().unwrap();
    assert_eq!(languages.len(), 6);
    let java = languages
        .iter()
        .find(|language| language["lang"] == "java")
        .unwrap();
    assert_eq!(java["files"], 1);
    assert_eq!(java["symbols"], 0);
    let text = repo.run(&["doctor", "--code"]);
    assert!(text.contains("Code: 6 files, 6 symbols (walk)"));
    assert!(text.contains("- rust: 1 files, 1 symbols"));
    assert!(repo.run(&["doctor", "--help"]).contains("--code"));
}

#[test]
fn old_derived_schema_rebuilds_code_nodes_without_changing_history_or_schema() {
    let repo = TestRepo::new(Some("standard"));
    write(&repo, "a.rs", "fn first() {}\n");
    repo.run(&["init", "--tracked"]);
    let schema = fs::read(repo.root.join(".harnesskit/schema.yaml")).unwrap();
    repo.run(&["history", "snapshot", "-m", "before W1 rebuild"]);
    let latest = fs::read(repo.root.join(".harnesskit/history/refs/latest")).unwrap();
    repo.run(&["index"]);
    let before = fingerprints(&repo);
    database(&repo)
        .execute_batch(
            "DROP TABLE nodes; ALTER TABLE file_states DROP COLUMN kind;
         UPDATE meta SET value = '6' WHERE key = 'fact_schema_version';",
        )
        .unwrap();
    repo.run(&["index"]);
    assert_eq!(before, fingerprints(&repo));
    assert_eq!(
        schema,
        fs::read(repo.root.join(".harnesskit/schema.yaml")).unwrap()
    );
    assert_eq!(
        latest,
        fs::read(repo.root.join(".harnesskit/history/refs/latest")).unwrap()
    );
}

#[test]
fn supported_code_indexing_requires_no_external_program_on_path() {
    let repo = TestRepo::new(Some("standard"));
    write(&repo, "a.js", "function standalone() {}\n");
    let output = repo.command(&["index"]).env("PATH", "").output().unwrap();
    assert_success(&output, &["index"]);
    assert!(fingerprints(&repo).contains_key(&("a.js".to_string(), "standalone".to_string())));
}

#[test]
fn empty_git_directory_in_an_ancestor_does_not_disable_non_git_scanning() {
    let repo = TestRepo::new(None);
    fs::create_dir(repo.root.join(".git")).unwrap();
    write(&repo, "module/a.rs", "fn standalone() {}\n");
    let target = repo.root.join("module");
    let output = repo
        .command(&["doctor", "--code", "--json", target.to_str().unwrap()])
        .env("PATH", "")
        .output()
        .unwrap();
    assert_success(&output, &["doctor", "--code"]);
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["code"]["scan_mode"], "walk");
    assert_eq!(report["code"]["files"], 1);
    assert_eq!(report["code"]["symbols"], 1);
}

#[test]
fn missing_git_recognizes_worktree_markers_and_does_not_scan_untracked_files() {
    let repo = TestRepo::new(None);
    write(&repo, ".git", "gitdir: /unavailable/worktree\n");
    write(&repo, "untracked.rs", "fn untracked() {}\n");
    let output = repo
        .command(&["doctor", "--code", "--json"])
        .env("PATH", "")
        .output()
        .unwrap();
    assert_success(&output, &["doctor", "--code"]);
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["code"]["scan_mode"], "git-unavailable");
    assert_eq!(report["code"]["files"], 0);
    assert!(file_paths(&repo).is_empty());
}

#[test]
fn tracked_paths_with_quotes_spaces_and_newlines_and_subdirectory_targets_are_safe() {
    let repo = TestRepo::new(Some("standard"));
    write(&repo, "module/owner's weird\nname.rs", "fn strange() {}\n");
    write(&repo, "outside.rs", "fn outside() {}\n");
    git(&repo, &["init", "--quiet"]);
    git(&repo, &["add", "."]);
    let target = repo.root.join("module");
    repo.run(&["index", target.to_str().unwrap()]);
    let db = Connection::open(target.join(".harnesskit/state/facts.sqlite")).unwrap();
    let paths = db
        .prepare("SELECT path FROM nodes WHERE kind = 'file';")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(paths, ["owner's weird\nname.rs"]);
}

#[test]
fn missing_git_preserves_cached_code_and_never_walks_untracked_git_files() {
    let repo = TestRepo::new(Some("standard"));
    write(&repo, "tracked.rs", "fn tracked() {}\n");
    git(&repo, &["init", "--quiet"]);
    git(&repo, &["add", "tracked.rs"]);
    repo.run(&["index"]);
    let before = fingerprints(&repo);
    write(&repo, "untracked.rs", "fn untracked() {}\n");
    let output = repo.command(&["index"]).env("PATH", "").output().unwrap();
    assert_success(&output, &["index"]);
    assert_eq!(fingerprints(&repo), before);
    assert!(!file_paths(&repo).contains("untracked.rs"));
    let output = repo
        .command(&["doctor", "--code", "--json"])
        .env("PATH", "")
        .output()
        .unwrap();
    assert_success(&output, &["doctor", "--code"]);
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["code"]["scan_mode"], "git-unavailable");
    assert!(report["hints"].as_array().unwrap().iter().any(|hint| hint
        .as_str()
        .unwrap()
        .contains("Code index was not refreshed")));
}

#[cfg(unix)]
#[test]
fn symlink_files_and_directories_are_not_followed_in_either_scan_mode() {
    use std::os::unix::fs::symlink;
    let repo = TestRepo::new(Some("standard"));
    let outside = TestRepo::new(None);
    write(&outside, "outside.rs", "fn outside() {}\n");
    symlink(outside.root.join("outside.rs"), repo.root.join("link.rs")).unwrap();
    symlink(&outside.root, repo.root.join("linked-dir")).unwrap();
    write(&repo, "real.rs", "fn real() {}\n");
    for tracked in [false, true] {
        if tracked {
            git(&repo, &["init", "--quiet"]);
            git(&repo, &["add", "real.rs", "link.rs", "linked-dir"]);
        }
        repo.run(&["index"]);
        assert_eq!(file_paths(&repo), BTreeSet::from(["real.rs".to_string()]));
    }
}
