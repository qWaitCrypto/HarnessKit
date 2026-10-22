mod support;

use std::fs;
use support::{assert_success, TestRepo};

#[test]
fn core_commands_work_with_no_external_programs_on_path() {
    let repo = TestRepo::new(Some("standard"));
    for args in [
        vec!["init", "--tracked"],
        vec!["index"],
        vec!["query", "backoff", "--json"],
        vec!["check", "--json"],
        vec!["inspect", "docs/design-docs/lantern.md"],
        vec!["refs", "docs/design-docs/lantern.md"],
        vec!["context", "backoff", "--json"],
        vec!["graph", "docs/design-docs/lantern.md", "--json"],
        vec!["rank"],
        vec!["list-docs"],
        vec!["history", "snapshot", "-m", "no external programs"],
        vec!["history", "export", "portable.tar.gz"],
        vec!["history", "import", "portable.tar.gz"],
        vec!["history", "show", "latest", "AGENTS.md"],
        vec!["history", "list"],
        vec!["history", "diff", "latest"],
        vec!["history", "restore", "latest", "--apply"],
        vec!["history", "status"],
    ] {
        let output = repo.command(&args).env("PATH", "").output().unwrap();
        assert_success(&output, &args);
        if args[0] == "query" {
            let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert!(json["candidates"]
                .as_array()
                .unwrap()
                .iter()
                .any(|doc| doc["path"] == "docs/design-docs/lantern.md"));
        }
    }
}

#[test]
fn quoted_paths_and_content_survive_index_queries_and_incremental_updates() {
    let repo = TestRepo::new(Some("standard"));
    let path = "docs/design-docs/owner's-relay.md";
    let title = "Owner's \"relay\" 合成";
    fs::write(
        repo.root.join(path),
        format!("# {title}\n\nNeedle's route \\ survives.\n"),
    )
    .unwrap();
    repo.run(&["index"]);
    let output: serde_json::Value =
        serde_json::from_str(&repo.run(&["query", "Needle's", "--json"])).unwrap();
    assert_eq!(output["candidates"][0]["path"], path);
    assert_eq!(output["candidates"][0]["title"], title);
    assert!(repo.run(&["inspect", path]).contains(title));
    assert!(repo
        .run(&["graph", path, "--json"])
        .contains("owner's-relay.md"));
    assert!(repo.run(&["refs", path]).contains(path));
    fs::write(
        repo.root.join(path),
        "# Changed\n\nChangedneedle's route.\n",
    )
    .unwrap();
    assert!(repo
        .run(&["query", "Changedneedle's", "--json"])
        .contains("owner's-relay.md"));
    fs::remove_file(repo.root.join(path)).unwrap();
    assert!(!repo.run(&["list-docs"]).contains(path));
}

#[test]
fn clap_help_and_existing_argument_forms_remain_available() {
    let repo = TestRepo::new(None);
    for command in [
        "init",
        "index",
        "check",
        "rank",
        "list-docs",
        "inspect",
        "refs",
        "query",
        "graph",
        "context",
        "doctor",
        "update",
    ] {
        assert!(repo.run(&[command, "--help"]).contains("Usage: harnesskit"));
    }
    for command in [
        "snapshot", "list", "status", "diff", "restore", "export", "import", "show",
    ] {
        assert!(repo
            .run(&["history", command, "--help"])
            .contains("Usage: harnesskit history"));
    }
    repo.run(&["init", "--tracked", "--docs-root", "knowledge"]);
    repo.run(&["history", "snapshot", "--message", "long form"]);
    let listed = repo.run(&["history", "list", repo.root.to_str().unwrap()]);
    assert!(listed.contains("long form"));
    let diff = repo.run(&["history", "diff", "latest", repo.root.to_str().unwrap()]);
    assert!(diff.contains("Changed:\n- none"));
    repo.run(&[
        "history",
        "diff",
        "latest",
        "latest",
        repo.root.to_str().unwrap(),
    ]);
    let duplicate = repo
        .command(&[
            "index",
            "--schema",
            "missing",
            "--schema",
            ".harnesskit/schema.yaml",
        ])
        .output()
        .unwrap();
    assert_success(&duplicate, &["index"]);
    assert_eq!(
        String::from_utf8(duplicate.stderr).unwrap(),
        "warning: duplicate --schema; using last value\n"
    );
}
