mod support;

use serde_json::Value;
use std::fs;
use std::process::Output;
use support::{assert_success, TestRepo};

fn target_commands(target: &str) -> Vec<Vec<&str>> {
    vec![
        vec!["index", target],
        vec!["check", target, "--json"],
        vec!["doctor", target, "--json"],
        vec!["rank", target],
        vec!["list-docs", target],
        vec!["inspect", "AGENTS.md", target],
        vec!["refs", "AGENTS.md", target],
        vec!["query", "needle", target, "--json"],
        vec!["graph", "AGENTS.md", target, "--json"],
        vec!["context", "needle", target, "--json"],
        vec!["history", "snapshot", target],
        vec!["history", "list", target],
        vec!["history", "status", target],
        vec!["history", "diff", "latest", target],
        vec!["history", "diff", "latest", "latest", target],
        vec!["history", "restore", "latest", target, "--apply", "--force"],
    ]
}

fn assert_exit(output: &Output, code: i32, args: &[&str]) {
    assert_eq!(
        output.status.code(),
        Some(code),
        "args: {:?}\nstdout: {}\nstderr: {}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn orphan_hints(report: &Value) -> Vec<&str> {
    report["hints"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .filter(|hint| hint.contains("Possible orphan"))
        .collect()
}

#[test]
fn missing_targets_fail_before_any_command_creates_state() {
    let repo = TestRepo::new(None);
    for args in target_commands("missing/nested") {
        let output = repo.command(&args).output().unwrap();
        assert_exit(&output, 2, &args);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("target_dir does not exist"), "{}", stderr);
        assert!(stderr.contains("argument mistaken for target_dir"));
        assert!(!repo.root.join("missing").exists());
        assert!(!repo.root.join(".harnesskit").exists());
    }
}

#[test]
fn mistaken_option_and_limit_positionals_do_not_create_directories() {
    let repo = TestRepo::new(None);
    for args in [vec!["check", "json"], vec!["query", "x", "10"]] {
        let output = repo.command(&args).output().unwrap();
        assert_exit(&output, 2, &args);
        assert!(String::from_utf8_lossy(&output.stderr).contains("target_dir"));
    }
    assert_eq!(fs::read_dir(&repo.root).unwrap().count(), 0);
}

#[test]
fn file_targets_are_rejected_including_init_preview() {
    let repo = TestRepo::new(None);
    fs::write(repo.root.join("file"), b"keep this file").unwrap();
    let mut commands = target_commands("file");
    commands.push(vec!["init", "file"]);
    commands.push(vec!["init", "file", "--preview"]);
    for args in commands {
        let output = repo.command(&args).output().unwrap();
        assert_exit(&output, 2, &args);
        assert!(String::from_utf8_lossy(&output.stderr).contains("not a directory"));
    }
    assert_eq!(fs::read(repo.root.join("file")).unwrap(), b"keep this file");
    assert_eq!(fs::read_dir(&repo.root).unwrap().count(), 1);
}

#[test]
fn init_can_create_missing_targets_but_preview_is_read_only() {
    let repo = TestRepo::new(None);
    repo.run(&["init", "new/project", "--preview"]);
    assert!(!repo.root.join("new").exists());
    repo.run(&["init", "new/project", "--tracked"]);
    assert!(repo.root.join("new/project/AGENTS.md").is_file());
    assert!(repo
        .root
        .join("new/project/.harnesskit/schema.yaml")
        .is_file());
    repo.run(&["index", "new/project"]);
}

#[test]
fn usage_and_schema_errors_exit_two_without_creating_state() {
    let repo = TestRepo::new(None);
    fs::write(repo.root.join("invalid.yaml"), "unknown_field: yes\n").unwrap();
    for args in [
        vec![],
        vec!["unknown-command"],
        vec!["check", "--unknown-option"],
        vec!["query"],
        vec!["history"],
        vec!["history", "unknown-command"],
        vec!["history", "diff"],
        vec!["history", "diff", "a", "b", ".", "extra"],
        vec!["init", "--local", "--tracked"],
        vec!["check", "--schema", "missing.yaml"],
        vec!["index", "--schema", "invalid.yaml"],
        vec!["update", "--repo", "invalid"],
    ] {
        let output = repo.command(&args).output().unwrap();
        assert_exit(&output, 2, &args);
        assert!(!output.stderr.is_empty());
        assert!(!repo.root.join(".harnesskit").exists());
    }
    for args in [vec!["--help"], vec!["--version"]] {
        let output = repo.command(&args).output().unwrap();
        assert_exit(&output, 0, &args);
        assert!(output.stderr.is_empty());
    }
}

#[test]
fn warnings_exit_one_only_in_strict_mode() {
    let repo = TestRepo::new(None);
    repo.run(&["init", "--tracked"]);
    fs::remove_file(repo.root.join("AGENTS.md")).unwrap();
    for (flags, code) in [
        (vec![], 0),
        (vec!["--strict"], 1),
        (vec!["--strict", "--no-strict"], 0),
    ] {
        for json in [false, true] {
            let mut args = vec!["check", "--rules", "missing-entrypoint-doc"];
            args.extend(flags.iter().copied());
            if json {
                args.push("--json");
            }
            let output = repo.command(&args).output().unwrap();
            assert_exit(&output, code, &args);
            if json {
                let report: Value = serde_json::from_slice(&output.stdout).unwrap();
                assert!(report["checks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|check| check["severity"] == "warning"));
            }
        }
    }
}

#[test]
fn schema_strict_mode_honors_no_strict_and_suppressions() {
    let repo = TestRepo::new(Some("standard"));
    repo.run(&["init", "--tracked"]);
    fs::remove_file(repo.root.join("AGENTS.md")).unwrap();
    let path = repo.root.join(".harnesskit/schema.yaml");
    let schema = fs::read_to_string(&path)
        .unwrap()
        .replace("strict_checks: false", "strict_checks: true");
    assert!(schema.contains("strict_checks: true"));
    fs::write(&path, &schema).unwrap();
    let args = ["check", "--rules", "missing-entrypoint-doc", "--json"];
    assert_exit(&repo.command(&args).output().unwrap(), 1, &args);
    repo.run(&["check", "--rules", "missing-entrypoint-doc", "--no-strict"]);
    fs::write(
        &path,
        format!("{}\nsuppressions:\n  missing_agent:\n    rule_id: missing-entrypoint-doc\n    path: AGENTS.md\n    reason: Synthetic suppression\n", schema),
    )
    .unwrap();
    let report: Value = serde_json::from_str(&repo.run(&args)).unwrap();
    assert_eq!(report["checks"][0]["suppressed"], true);
}

#[test]
fn error_findings_exit_one_without_strict_mode() {
    let repo = TestRepo::new(Some("standard"));
    fs::write(
        repo.root.join("docs/broken.md"),
        "# Broken\n\n[Missing](missing.md)\n",
    )
    .unwrap();
    for args in [
        vec!["check", "--rules", "broken-link"],
        vec!["check", "--rules", "broken-link", "--json"],
    ] {
        let output = repo.command(&args).output().unwrap();
        assert_exit(&output, 1, &args);
        assert!(String::from_utf8_lossy(&output.stdout).contains("broken-link"));
    }
}

#[test]
fn every_target_command_hints_when_project_docs_are_absent() {
    let mut commands = target_commands(".");
    commands.push(vec!["init", ".", "--preview"]);
    for args in commands {
        let repo = TestRepo::new(None);
        let output = repo.command(&args).output().unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        let hints: Vec<_> = stderr
            .lines()
            .filter(|line| line.starts_with("Hint:"))
            .collect();
        assert_eq!(hints.len(), 1, "{:?}: {}", args, stderr);
        assert!(hints[0].contains("harnesskit init --preview"));
        if args.contains(&"--json") && output.status.success() {
            let report: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert!(report["hints"]
                .as_array()
                .unwrap()
                .iter()
                .any(|hint| hint.as_str().unwrap().contains("harnesskit init --preview")));
        }
    }
}

#[test]
fn project_detection_respects_docs_roots_and_entrypoints() {
    for entry in ["AGENTS.md", "CLAUDE.md", "ARCHITECTURE.md", "docs"] {
        let repo = TestRepo::new(None);
        if entry == "docs" {
            fs::create_dir(repo.root.join(entry)).unwrap();
        } else {
            fs::write(repo.root.join(entry), "# Synthetic entry\n").unwrap();
        }
        let output = repo.command(&["index"]).output().unwrap();
        assert_success(&output, &["index"]);
        assert!(output.stderr.is_empty());
    }
    let repo = TestRepo::new(None);
    fs::create_dir(repo.root.join("knowledge")).unwrap();
    let output = repo
        .command(&["index", "--docs-root", "knowledge"])
        .output()
        .unwrap();
    assert_success(&output, &["index"]);
    assert!(output.stderr.is_empty());
    fs::write(
        repo.root.join("custom.yaml"),
        include_str!("../schemas/file-first-v0.yaml")
            .replace("managed_root: docs", "managed_root: knowledge")
            .replace("agents: AGENTS.md", "agents: ROUTES.md"),
    )
    .unwrap();
    fs::remove_dir(repo.root.join("knowledge")).unwrap();
    fs::write(repo.root.join("ROUTES.md"), "# Routes\n").unwrap();
    let output = repo
        .command(&["doctor", "--schema", "custom.yaml", "--json"])
        .output()
        .unwrap();
    assert_success(&output, &["doctor"]);
    assert!(output.stderr.is_empty());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(orphan_hints(&report).is_empty());
}

#[test]
fn doctor_and_check_report_nested_orphans_without_touching_their_history() {
    let repo = TestRepo::new(Some("standard"));
    repo.run(&["index"]);
    let history = repo.root.join("accidental/json/.harnesskit/history");
    fs::create_dir_all(&history).unwrap();
    fs::write(history.join("keep.txt"), b"preserve history").unwrap();
    for command in ["doctor", "check"] {
        let report: Value = serde_json::from_str(&repo.run(&[command, "--json"])).unwrap();
        let hints = orphan_hints(&report);
        assert_eq!(hints.len(), 1);
        assert!(hints[0].contains("accidental/json/.harnesskit"));
        assert!(hints[0].contains("preserve .harnesskit/history"));
        assert!(repo.run(&[command]).contains("accidental/json/.harnesskit"));
        assert_eq!(
            fs::read(history.join("keep.txt")).unwrap(),
            b"preserve history"
        );
    }
    // Discovery is live even when no indexed docs changed.
    fs::create_dir(repo.root.join("accidental/json/docs")).unwrap();
    let report: Value = serde_json::from_str(&repo.run(&["check", "--json"])).unwrap();
    assert!(orphan_hints(&report).is_empty());
}

#[test]
fn orphan_state_at_the_target_itself_is_reported() {
    let repo = TestRepo::new(None);
    fs::create_dir_all(repo.root.join(".harnesskit/state")).unwrap();
    for command in ["doctor", "check"] {
        let report: Value = serde_json::from_str(&repo.run(&[command, "--json"])).unwrap();
        assert_eq!(orphan_hints(&report).len(), 1);
        assert!(orphan_hints(&report)[0].contains("\".harnesskit\""));
    }
}

#[test]
fn orphan_scan_respects_nested_schemas_and_skips_hidden_build_and_dependency_trees() {
    let repo = TestRepo::new(Some("standard"));
    repo.run(&["init", "nested", "--tracked", "--docs-root", "knowledge"]);
    for entry in ["AGENTS.md", "CLAUDE.md", "ARCHITECTURE.md"] {
        fs::remove_file(repo.root.join("nested").join(entry)).unwrap();
    }
    for path in [".hidden", ".git", "node_modules/dependency", "target/debug"] {
        fs::create_dir_all(repo.root.join(path).join(".harnesskit/state")).unwrap();
    }
    for command in ["doctor", "check"] {
        let report: Value = serde_json::from_str(&repo.run(&[command, "--json"])).unwrap();
        assert!(orphan_hints(&report).is_empty());
    }
    // A parent's docs-root override must not leak into an unconfigured nested project.
    fs::create_dir_all(repo.root.join("ordinary/docs")).unwrap();
    fs::create_dir(repo.root.join("ordinary/.harnesskit")).unwrap();
    for command in ["doctor", "check"] {
        let report: Value =
            serde_json::from_str(&repo.run(&[command, "--docs-root", "knowledge", "--json"]))
                .unwrap();
        assert!(orphan_hints(&report).is_empty());
    }
}

#[test]
fn json_reports_describe_coverage_and_include_hints() {
    let repo = TestRepo::new(Some("standard"));
    for args in [
        vec!["check", "--json"],
        vec!["doctor", "--json"],
        vec!["query", "lantern", "--json"],
        vec!["graph", "AGENTS.md", "--json"],
        vec!["context", "AGENTS.md", "--json"],
    ] {
        let report: Value = serde_json::from_str(&repo.run(&args)).unwrap();
        assert!(report["coverage"]["markdown_total"].as_u64().unwrap() > 0);
        assert!(report["hints"].is_array());
    }
}

#[test]
fn doctor_no_longer_requires_or_advertises_external_sqlite() {
    let repo = TestRepo::new(Some("standard"));
    for args in [vec!["doctor"], vec!["doctor", "--json"]] {
        let output = repo.command(&args).env("PATH", "").output().unwrap();
        assert_success(&output, &args);
        assert!(!String::from_utf8_lossy(&output.stdout).contains("sqlite3"));
        assert!(!String::from_utf8_lossy(&output.stderr).contains("sqlite3"));
        if args.contains(&"--json") {
            let report: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(report["state_writable"], true);
            assert!(report.get("sqlite3").is_none());
        }
    }
}

#[test]
fn state_io_failures_exit_two_with_diagnostics() {
    let repo = TestRepo::new(Some("standard"));
    fs::create_dir(repo.root.join(".harnesskit")).unwrap();
    fs::write(repo.root.join(".harnesskit/state"), b"not a directory").unwrap();
    for command in ["index", "doctor"] {
        let output = repo.command(&[command, "--json"]).output().unwrap();
        assert_exit(&output, 2, &[command]);
        assert!(!output.stderr.is_empty());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("sqlite3"));
        if command == "doctor" {
            let report: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(report["state_writable"], false);
        }
    }
}

#[test]
fn history_diff_preserves_legacy_ids_and_explicit_targets() {
    let repo = TestRepo::new(Some("standard"));
    repo.run(&["history", "snapshot", "-m", "first"]);
    let latest = fs::read_to_string(repo.root.join(".harnesskit/history/refs/latest")).unwrap();
    fs::copy(
        repo.root
            .join(format!(".harnesskit/history/snapshots/{}.json", latest)),
        repo.root
            .join(".harnesskit/history/snapshots/legacy-id.json"),
    )
    .unwrap();
    for args in [
        vec!["history", "diff", "latest", "legacy-id"],
        vec!["history", "diff", "latest", "legacy-id", "."],
        vec!["history", "diff", "latest", "."],
    ] {
        assert_eq!(
            repo.run(&args),
            "Added:\n- none\nChanged:\n- none\nRemoved:\n- none\n"
        );
    }
    for target in ["missing", "./missing", "missing/nested"] {
        let args = ["history", "diff", "latest", target];
        let output = repo.command(&args).output().unwrap();
        assert_exit(&output, 2, &args);
        assert!(String::from_utf8_lossy(&output.stderr).contains("target_dir does not exist"));
        assert!(!repo.root.join("missing").exists());
    }
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::os::fd::OwnedFd;
    use std::os::unix::{fs::symlink, net::UnixStream};
    use std::process::Stdio;

    fn closed_pipe() -> Stdio {
        let (reader, writer) = UnixStream::pair().unwrap();
        drop(reader);
        Stdio::from(OwnedFd::from(writer))
    }

    #[test]
    fn closed_stdout_exits_zero_without_panic_including_help_version_and_strict() {
        let repo = TestRepo::new(Some("standard"));
        repo.run(&["history", "snapshot"]);
        let mut commands = target_commands(".");
        // These inputs must produce output rather than fail with a missing document/query.
        commands.retain(|args| args[0] != "context");
        commands.extend([
            vec!["context", "AGENTS.md", "--json"],
            vec!["--help"],
            vec!["--version"],
            vec!["check", "--help"],
            vec!["init", "--preview"],
            vec!["check", "--strict", "--json"],
        ]);
        for args in commands {
            let output = repo.command(&args).stdout(closed_pipe()).output().unwrap();
            assert_exit(&output, 0, &args);
            assert!(output.stderr.is_empty(), "{:?}: {:?}", args, output);
        }
    }

    #[test]
    fn closing_stdout_after_one_line_exits_zero() {
        let repo = TestRepo::new(Some("standard"));
        for i in 0..100 {
            fs::write(
                repo.root.join(format!("docs/design-docs/large-{}.md", i)),
                format!("# {} {}\n\nSynthetic body.\n", i, "wide".repeat(1024)),
            )
            .unwrap();
        }
        repo.run(&["index"]);
        let mut child = repo
            .command(&["list-docs"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut reader = BufReader::new(child.stdout.take().unwrap());
        let mut first_line = String::new();
        reader.read_line(&mut first_line).unwrap();
        assert_eq!(first_line, "Managed docs:\n");
        drop(reader);
        let output = child.wait_with_output().unwrap();
        assert_exit(&output, 0, &["list-docs"]);
        assert!(output.stderr.is_empty());
    }

    #[test]
    fn closed_stderr_does_not_panic_on_hints_or_parser_errors() {
        let repo = TestRepo::new(None);
        for args in [vec!["index"], vec!["check", "--invalid"], vec![]] {
            let output = repo.command(&args).stderr(closed_pipe()).output().unwrap();
            assert_exit(&output, 0, &args);
        }
    }

    #[test]
    fn orphan_scan_does_not_follow_symlink_cycles_or_external_projects() {
        let repo = TestRepo::new(Some("standard"));
        let external = TestRepo::new(None);
        fs::create_dir(external.root.join(".harnesskit")).unwrap();
        symlink(&external.root, repo.root.join("external")).unwrap();
        symlink(&repo.root, repo.root.join("cycle")).unwrap();
        for command in ["doctor", "check"] {
            let report: Value = serde_json::from_str(&repo.run(&[command, "--json"])).unwrap();
            assert!(orphan_hints(&report).is_empty());
        }
    }

    #[test]
    fn output_errors_other_than_broken_pipe_exit_two() {
        if !std::path::Path::new("/dev/full").exists() {
            return;
        }
        let repo = TestRepo::new(None);
        let output = repo
            .command(&["--version"])
            .stdout(
                fs::OpenOptions::new()
                    .write(true)
                    .open("/dev/full")
                    .unwrap(),
            )
            .output()
            .unwrap();
        assert_exit(&output, 2, &["--version"]);
        assert!(!output.stderr.is_empty());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("panicked"));
    }
}
