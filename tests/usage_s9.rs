mod support;

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use support::{assert_success, TestRepo};

fn command(repo: &TestRepo, home: &TestRepo, args: &[&str]) -> Command {
    let mut command = repo.command(args);
    command
        .env("HOME", &home.root)
        .env("HARNESSKIT_HOME", home.root.join("metrics"))
        .env_remove("HARNESSKIT_USAGE_LOG");
    command
}

fn log_path(home: &TestRepo) -> PathBuf {
    home.root.join("metrics/usage.jsonl")
}

fn records(home: &TestRepo) -> Vec<Value> {
    let text = fs::read_to_string(log_path(home)).unwrap();
    assert!(text.ends_with('\n'));
    text.lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn run(repo: &TestRepo, home: &TestRepo, args: &[&str]) -> Output {
    let output = command(repo, home, args).output().unwrap();
    assert_success(&output, args);
    output
}

fn report(repo: &TestRepo, home: &TestRepo, args: &[&str]) -> Value {
    let output = command(repo, home, args)
        .env("HARNESSKIT_USAGE_LOG", "off")
        .output()
        .unwrap();
    assert_success(&output, args);
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn minimal_logs_append_measured_queries_without_text_or_paths() {
    let repo = TestRepo::new(Some("standard"));
    let home = TestRepo::new(None);
    repo.run(&["index"]);
    for (terms, mode, zero) in [
        ("lantern", "strict", false),
        ("lantern secretmissingword", "relaxed", false),
        ("secretmissingword", "strict", true),
    ] {
        let output = run(&repo, &home, &["query", terms, "--json", "--limit", "2"]);
        let query: Value = serde_json::from_slice(&output.stdout).unwrap();
        let entries = records(&home);
        let entry = entries.last().unwrap();
        assert_eq!(entry["command"], "query");
        assert_eq!(
            entry["result_count"],
            query["candidates"].as_array().unwrap().len()
        );
        assert_eq!(entry["query_mode"], mode);
        assert_eq!(entry["zero_results"], zero);
        assert_eq!(entry["coverage_ratio"], query["coverage"]["ratio"]);
        assert_eq!(entry["exit_code"], 0);
        assert_eq!(entry["version"], env!("CARGO_PKG_VERSION"));
        assert!(entry["timestamp_unix"].as_u64().unwrap() > 0);
        assert!(entry["duration_ms"].as_u64().is_some());
        assert!(entry.get("query").is_none());
        let hash = entry["project_root_hash"].as_str().unwrap();
        assert_eq!(hash.len(), 16);
        assert!(hash.chars().all(|ch| ch.is_ascii_hexdigit()));
        assert_eq!(entry.as_object().unwrap().len(), 10);
    }
    assert_eq!(records(&home).len(), 3);
    let text = fs::read_to_string(log_path(&home)).unwrap();
    for private in [
        "lantern",
        "secretmissingword",
        repo.root.to_str().unwrap(),
        home.root.to_str().unwrap(),
    ] {
        assert!(!text.contains(private), "{}", text);
    }
    assert!(!home.root.join(".harnesskit").exists());
}

#[test]
fn full_mode_roundtrips_query_text_as_one_json_line() {
    let repo = TestRepo::new(Some("cjk"));
    let home = TestRepo::new(None);
    let text = "精灵蛋 \"private\"\nsecond line\\literal";
    let output = command(&repo, &home, &["query", text, "--json"])
        .env("HARNESSKIT_USAGE_LOG", "full")
        .output()
        .unwrap();
    assert_success(&output, &["query"]);
    let entries = records(&home);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["query"], text);
    assert!(!fs::read_to_string(log_path(&home))
        .unwrap()
        .contains(repo.root.to_str().unwrap()));
}

#[test]
fn default_home_and_harnesskit_home_override_are_isolated() {
    let repo = TestRepo::new(Some("standard"));
    let home = TestRepo::new(None);
    let output = command(&repo, &home, &["--version"])
        .env_remove("HARNESSKIT_HOME")
        .output()
        .unwrap();
    assert_success(&output, &["--version"]);
    let default = home.root.join(".harnesskit/usage.jsonl");
    let saved = fs::read(&default).unwrap();
    run(&repo, &home, &["--version"]);
    assert_eq!(fs::read(default).unwrap(), saved);
    assert_eq!(records(&home).len(), 1);
}

#[test]
fn off_never_creates_or_appends_usage_files() {
    let repo = TestRepo::new(Some("standard"));
    let home = TestRepo::new(None);
    for args in [
        vec!["--version"],
        vec!["query", "lantern"],
        vec!["check", "--invalid"],
    ] {
        command(&repo, &home, &args)
            .env("HARNESSKIT_USAGE_LOG", "off")
            .output()
            .unwrap();
    }
    assert_eq!(fs::read_dir(&home.root).unwrap().count(), 0);
    run(&repo, &home, &["--version"]);
    let saved = fs::read(log_path(&home)).unwrap();
    command(&repo, &home, &["--help"])
        .env("HARNESSKIT_USAGE_LOG", "off")
        .output()
        .unwrap();
    assert_eq!(fs::read(log_path(&home)).unwrap(), saved);
}

#[test]
fn missing_home_does_not_create_relative_usage_state() {
    let repo = TestRepo::new(None);
    let home = TestRepo::new(None);
    for empty in [false, true] {
        let mut invocation = command(&repo, &home, &["--version"]);
        invocation.env_remove("HARNESSKIT_HOME");
        if empty {
            invocation.env("HOME", "");
        } else {
            invocation.env_remove("HOME");
        }
        assert_success(&invocation.output().unwrap(), &["--version"]);
    }
    assert_eq!(fs::read_dir(&repo.root).unwrap().count(), 0);
}

#[test]
fn usage_write_failures_preserve_stdout_stderr_and_all_exit_codes() {
    let repo = TestRepo::new(Some("standard"));
    let home = TestRepo::new(None);
    // Compare telemetry with a stable presentation state, after the first
    // inferred-layout explanation has been shown.
    repo.run(&["index"]);
    fs::write(
        repo.root.join("docs/broken.md"),
        "# Broken\n\n[Missing](missing.md)\n",
    )
    .unwrap();
    // A regular file as HARNESSKIT_HOME fails before opening the log.
    let blocked = home.root.join("blocked");
    fs::write(&blocked, "preserve").unwrap();
    // A directory at the log filename fails when opening it for append.
    fs::create_dir_all(log_path(&home)).unwrap();
    for args in [
        vec!["--version"],
        vec!["query", "lantern", "--json"],
        vec!["check", "--rules", "broken-link", "--json"],
        vec!["query", "lantern", "missing-target", "--json"],
        vec!["check", "--invalid"],
    ] {
        // Establish the check baseline before comparing its deterministic output.
        let expected = command(&repo, &home, &args)
            .env("HARNESSKIT_USAGE_LOG", "off")
            .output()
            .unwrap();
        let expected = if args[0] == "check" && args.contains(&"--json") {
            command(&repo, &home, &args)
                .env("HARNESSKIT_USAGE_LOG", "off")
                .output()
                .unwrap()
        } else {
            expected
        };
        for location in [home.root.join("metrics"), blocked.clone()] {
            let actual = command(&repo, &home, &args)
                .env("HARNESSKIT_HOME", location)
                .output()
                .unwrap();
            assert_eq!(actual.status.code(), expected.status.code(), "{:?}", args);
            assert_eq!(actual.stdout, expected.stdout, "{:?}", args);
            assert_eq!(actual.stderr, expected.stderr, "{:?}", args);
        }
    }
    assert_eq!(fs::read_to_string(blocked).unwrap(), "preserve");
}

#[test]
fn every_invocation_logs_once_including_parser_and_check_exits() {
    let repo = TestRepo::new(Some("standard"));
    let home = TestRepo::new(None);
    fs::write(
        repo.root.join("docs/broken.md"),
        "# Broken\n\n[Missing](missing.md)\n",
    )
    .unwrap();
    let cases = [
        (vec!["--version"], "--version", 0),
        (vec!["--help"], "--help", 0),
        (vec!["query", "--help"], "query", 0),
        (vec!["query"], "query", 2),
        (vec!["check", "--invalid"], "check", 2),
        (vec!["private-unknown-command"], "unknown", 2),
        (vec!["history", "private-subcommand"], "history", 2),
        (vec!["history", "list"], "history list", 0),
        (
            vec!["check", "--rules", "broken-link", "--json"],
            "check",
            1,
        ),
        (vec!["check", "--rules", "broken-link"], "check", 1),
    ];
    for (index, (args, name, code)) in cases.iter().enumerate() {
        let output = command(&repo, &home, args).output().unwrap();
        assert_eq!(output.status.code(), Some(*code), "{:?}", args);
        let entries = records(&home);
        assert_eq!(entries.len(), index + 1);
        assert_eq!(entries[index]["command"], *name);
        assert_eq!(entries[index]["exit_code"], *code);
    }
    assert!(!fs::read_to_string(log_path(&home))
        .unwrap()
        .contains("private-"));
}

#[test]
fn check_records_coverage_and_new_findings_without_changing_exit_one() {
    let repo = TestRepo::new(Some("standard"));
    let home = TestRepo::new(None);
    run(&repo, &home, &["check", "--rules", "broken-link", "--json"]);
    fs::write(
        repo.root.join("docs/new-broken.md"),
        "# Broken\n\n[Missing](missing.md)\n",
    )
    .unwrap();
    let output = command(&repo, &home, &["check", "--rules", "broken-link", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let check: Value = serde_json::from_slice(&output.stdout).unwrap();
    let entries = records(&home);
    assert_eq!(entries[0]["check_new_findings"], 0);
    assert_eq!(
        entries[1]["check_new_findings"],
        check["baseline"]["new_count"]
    );
    assert_eq!(entries[1]["check_new_findings"], 1);
    assert_eq!(entries[1]["coverage_ratio"], check["coverage"]["ratio"]);
    assert_eq!(entries[1]["result_count"], 1);
    assert!(entries[1].get("zero_results").is_none());
}

#[test]
fn invalid_schema_check_still_logs_exit_one() {
    let repo = TestRepo::new(Some("standard"));
    let home = TestRepo::new(None);
    fs::write(repo.root.join("bad.yaml"), "managed_root: [broken\n").unwrap();
    let output = command(&repo, &home, &["check", "--schema", "bad.yaml", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let entry = &records(&home)[0];
    assert_eq!(entry["exit_code"], 1);
    assert_eq!(entry["result_count"], 1);
    assert!(entry.get("coverage_ratio").is_none());
    assert!(entry.get("check_new_findings").is_none());
}

#[test]
fn invalid_targets_have_no_project_hash_and_are_counted_separately() {
    let repo = TestRepo::new(Some("flat"));
    let home = TestRepo::new(None);
    let cases = [
        vec!["index", "missing-target"],
        vec!["check", "missing-target", "--json"],
        vec!["query", "secret-query", "missing-target", "--json"],
        vec!["context", "--current", "missing-target", "--json"],
        vec!["doctor", "missing-target", "--json"],
        vec!["history", "snapshot", "missing-target", "--json"],
        vec!["check", "--invalid"],
    ];
    for args in &cases {
        let output = command(&repo, &home, args).output().unwrap();
        assert_eq!(output.status.code(), Some(2), "{:?}", args);
    }
    for entry in records(&home) {
        assert_eq!(entry["invalid"], true);
        assert!(entry.get("project_root_hash").is_none());
    }
    assert!(!repo.root.join("missing-target").exists());
    let invalid = report(&repo, &home, &["doctor", "--usage", "--json"]);
    assert_eq!(invalid["usage"]["calls"], cases.len());
    assert_eq!(invalid["usage"]["invalid"], cases.len());
    assert!(invalid["usage"]["projects"].as_array().unwrap().is_empty());
    run(&repo, &home, &["index"]);
    let mixed = report(&repo, &home, &["doctor", "--usage", "--json"]);
    assert_eq!(mixed["usage"]["projects"].as_array().unwrap().len(), 1);
    assert_eq!(mixed["usage"]["projects"][0]["calls"], 1);
    assert_eq!(mixed["usage"]["invalid"], cases.len());
    assert!(!fs::read_to_string(log_path(&home))
        .unwrap()
        .contains("secret-query"));
}

#[test]
fn legacy_exit_two_records_are_excluded_from_project_aggregates() {
    let repo = TestRepo::new(Some("flat"));
    let home = TestRepo::new(None);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let mut invalid = synthetic(now, "query", "phantom", 1, 2, None, None);
    invalid.as_object_mut().unwrap().remove("project_root_hash");
    invalid["invalid"] = true.into();
    write_records(
        &home,
        &[
            synthetic(now, "index", "real", 1, 0, None, None),
            synthetic(now, "query", "phantom", 1, 2, None, None),
            invalid,
        ],
    );
    let usage = report(&repo, &home, &["doctor", "--usage", "--json"]);
    assert_eq!(usage["usage"]["calls"], 3);
    assert_eq!(usage["usage"]["invalid"], 2);
    assert_eq!(usage["usage"]["ignored_lines"], 0);
    assert_eq!(usage["usage"]["projects"].as_array().unwrap().len(), 1);
    assert_eq!(usage["usage"]["projects"][0]["project_root_hash"], "real");
}

#[test]
fn root_hash_uses_actual_target_for_relative_absolute_and_context_modes() {
    let repo = TestRepo::new(Some("standard"));
    let home = TestRepo::new(None);
    run(&repo, &home, &["index"]);
    run(&home, &home, &["index", repo.root.to_str().unwrap()]);
    run(
        &home,
        &home,
        &[
            "context",
            "--current",
            repo.root.to_str().unwrap(),
            "--json",
        ],
    );
    run(&home, &home, &["init", "new/project", "--preview"]);
    run(&home, &home, &["init", "new/project", "--tracked"]);
    run(&home, &home, &["index", "new/project"]);
    let entries = records(&home);
    assert_eq!(
        entries[0]["project_root_hash"],
        entries[1]["project_root_hash"]
    );
    assert_eq!(
        entries[0]["project_root_hash"],
        entries[2]["project_root_hash"]
    );
    assert_eq!(
        entries[3]["project_root_hash"],
        entries[4]["project_root_hash"]
    );
    assert_eq!(
        entries[4]["project_root_hash"],
        entries[5]["project_root_hash"]
    );
    assert_ne!(
        entries[0]["project_root_hash"],
        entries[3]["project_root_hash"]
    );
}

fn synthetic(
    timestamp: u64,
    command: &str,
    project: &str,
    duration: u64,
    exit: i32,
    count: Option<usize>,
    mode: Option<&str>,
) -> Value {
    json!({
        "timestamp_unix": timestamp, "version": "synthetic", "command": command,
        "project_root_hash": project, "duration_ms": duration, "exit_code": exit,
        "result_count": count, "query_mode": mode
    })
}

fn write_records(home: &TestRepo, entries: &[Value]) {
    fs::create_dir_all(log_path(home).parent().unwrap()).unwrap();
    let text = entries
        .iter()
        .map(|entry| format!("{}\n", entry))
        .collect::<String>();
    fs::write(log_path(home), text).unwrap();
}

#[test]
fn doctor_usage_reports_cross_project_rates_commands_duration_and_patterns() {
    let repo = TestRepo::new(Some("standard"));
    let home = TestRepo::new(None);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let mut entries = vec![
        synthetic(now - 10, "index", "aaaa", 100, 0, None, None),
        synthetic(now - 10, "index", "bbbb", 200, 0, None, None),
        synthetic(now - 9, "check", "aaaa", 300, 0, None, None),
        synthetic(now - 9, "check", "bbbb", 400, 0, None, None),
        synthetic(now - 8, "query", "aaaa", 5, 0, Some(2), Some("strict")),
        synthetic(now - 7, "query", "aaaa", 15, 0, Some(3), Some("relaxed")),
        synthetic(now - 6, "query", "aaaa", 20, 0, Some(0), Some("strict")),
        synthetic(now - 5, "query", "bbbb", 30, 2, None, None),
    ];
    for _ in 0..3 {
        entries.push(synthetic(now - 4, "--version", "aaaa", 10, 0, None, None));
    }
    entries.push(synthetic(
        now - 3 * 86400,
        "query",
        "old",
        1,
        0,
        Some(0),
        Some("strict"),
    ));
    entries.push(synthetic(
        now + 86400,
        "query",
        "future",
        1,
        0,
        Some(0),
        Some("strict"),
    ));
    write_records(&home, &entries);
    let text = fs::read_to_string(log_path(&home)).unwrap();
    fs::write(
        log_path(&home),
        format!("not-json\n{}\n{{\"partial\":", text),
    )
    .unwrap();
    let doctor = report(
        &repo,
        &home,
        &["doctor", "--usage", "--days", "2", "--json"],
    );
    assert!(doctor["coverage"].is_object());
    assert!(doctor["hints"].is_array());
    let usage = &doctor["usage"];
    assert_eq!(usage["calls"], 11);
    assert_eq!(usage["query_calls"], 3);
    assert_eq!(usage["zero_result_queries"], 1);
    assert_eq!(usage["relaxed_queries"], 1);
    assert!((usage["zero_hit_rate"].as_f64().unwrap() - 1.0 / 3.0).abs() < 1e-6);
    assert!((usage["relaxed_hit_rate"].as_f64().unwrap() - 1.0 / 3.0).abs() < 1e-6);
    assert_eq!(usage["average_duration_ms"], 100.0);
    assert_eq!(
        usage["most_used_commands"][0],
        json!({"command": "query", "calls": 4})
    );
    assert_eq!(usage["patterns"]["index_then_check"], 2);
    assert_eq!(usage["patterns"]["frequent_version"], true);
    assert_eq!(usage["projects"].as_array().unwrap().len(), 2);
    assert_eq!(usage["projects"][0]["calls"], 8);
    assert_eq!(usage["projects"][1]["query_calls"], 0);
    assert_eq!(usage["ignored_lines"], 2);
    assert!(doctor["hints"]
        .as_array()
        .unwrap()
        .iter()
        .any(|hint| hint.as_str().unwrap().contains("refreshes the index")));
    let all = report(&repo, &home, &["doctor", "--usage", "--json"]);
    assert_eq!(all["usage"]["calls"], 12);
    assert_eq!(all["usage"]["query_calls"], 4);
    let output = command(&repo, &home, &["doctor", "--usage", "--days", "2"])
        .env("HARNESSKIT_USAGE_LOG", "off")
        .output()
        .unwrap();
    assert_success(&output, &["doctor"]);
    let text = String::from_utf8(output.stdout).unwrap();
    for expected in [
        "zero-hit rate: 33.3%",
        "Most used subcommands: query (4)",
        "Average duration: 100.00 ms",
        "Project aaaa: 8 calls",
    ] {
        assert!(text.contains(expected), "{}", text);
    }
}

#[test]
fn patterns_require_success_and_close_consecutive_calls_in_the_same_project() {
    let repo = TestRepo::new(Some("standard"));
    let home = TestRepo::new(None);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    write_records(
        &home,
        &[
            synthetic(now - 200, "index", "a", 1, 0, None, None),
            synthetic(now - 100, "check", "a", 1, 0, None, None),
            synthetic(now - 90, "index", "a", 1, 2, None, None),
            synthetic(now - 89, "check", "a", 1, 0, None, None),
            synthetic(now - 80, "index", "a", 1, 0, None, None),
            synthetic(now - 79, "query", "a", 1, 0, Some(1), Some("strict")),
            synthetic(now - 78, "check", "a", 1, 0, None, None),
            synthetic(now - 70, "index", "a", 1, 0, None, None),
            synthetic(now - 69, "check", "a", 1, 1, None, None),
            synthetic(now - 68, "check", "b", 1, 0, None, None),
            synthetic(now - 60, "--version", "a", 1, 0, None, None),
            synthetic(now - 59, "--version", "a", 1, 0, None, None),
        ],
    );
    let doctor = report(&repo, &home, &["doctor", "--usage", "--json"]);
    assert_eq!(doctor["usage"]["patterns"]["index_then_check"], 0);
    assert_eq!(doctor["usage"]["patterns"]["frequent_version"], false);
}

#[test]
fn empty_and_unreadable_logs_are_actionable_without_failing_doctor() {
    let repo = TestRepo::new(Some("standard"));
    let home = TestRepo::new(None);
    let doctor = report(&repo, &home, &["doctor", "--usage", "--json"]);
    assert_eq!(doctor["usage"]["calls"], 0);
    assert_eq!(doctor["usage"]["zero_hit_rate"], 0.0);
    assert_eq!(doctor["usage"]["average_duration_ms"], 0.0);
    assert!(!home.root.join("metrics").exists());
    fs::create_dir_all(log_path(&home)).unwrap();
    let doctor = report(&repo, &home, &["doctor", "--usage", "--json"]);
    assert!(doctor["usage"]["read_error"].is_string());
    assert!(doctor["hints"]
        .as_array()
        .unwrap()
        .iter()
        .any(|hint| hint.as_str().unwrap().contains("permissions")));
}

#[test]
fn doctor_logs_itself_after_reporting_and_days_requires_usage_and_positive_integer() {
    let repo = TestRepo::new(Some("standard"));
    let home = TestRepo::new(None);
    let first = run(&repo, &home, &["doctor", "--usage", "--json"]);
    let doctor: Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(doctor["usage"]["calls"], 0);
    assert_eq!(records(&home).len(), 1);
    let second = run(&repo, &home, &["doctor", "--usage", "--json"]);
    let doctor: Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(doctor["usage"]["calls"], 1);
    for args in [
        vec!["doctor", "--days", "2"],
        vec!["doctor", "--usage", "--days", "0"],
        vec!["doctor", "--usage", "--days", "-1"],
        vec!["doctor", "--usage", "--days", "many"],
    ] {
        let output = command(&repo, &home, &args).output().unwrap();
        assert_eq!(output.status.code(), Some(2));
    }
    assert_eq!(records(&home).len(), 6);
}

#[test]
fn concurrent_commands_append_independent_json_lines() {
    let repo = TestRepo::new(None);
    let home = TestRepo::new(None);
    let children = (0..12)
        .map(|_| {
            command(&repo, &home, &["--version"])
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap()
        })
        .collect::<Vec<_>>();
    for mut child in children {
        assert!(child.wait().unwrap().success());
    }
    let entries = records(&home);
    assert_eq!(entries.len(), 12);
    assert!(entries
        .iter()
        .all(|entry| entry["command"] == "--version" && entry["exit_code"] == 0));
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::os::fd::OwnedFd;
    use std::os::unix::{fs::symlink, net::UnixStream};
    use std::process::Stdio;

    #[test]
    fn closed_pipes_and_output_write_errors_record_the_actual_exit_code() {
        let repo = TestRepo::new(Some("standard"));
        let home = TestRepo::new(None);
        for args in [
            vec!["--version"],
            vec!["query", "lantern", "--json"],
            vec!["check", "--strict"],
        ] {
            let (reader, writer) = UnixStream::pair().unwrap();
            drop(reader);
            let output = command(&repo, &home, &args)
                .stdout(Stdio::from(OwnedFd::from(writer)))
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(0));
            assert!(output.stderr.is_empty());
            assert_eq!(records(&home).last().unwrap()["exit_code"], 0);
        }
        if std::path::Path::new("/dev/full").exists() {
            let output = command(&repo, &home, &["--version"])
                .stdout(
                    fs::OpenOptions::new()
                        .write(true)
                        .open("/dev/full")
                        .unwrap(),
                )
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(2));
            assert_eq!(records(&home).last().unwrap()["exit_code"], 2);
        }
    }

    #[test]
    fn failing_append_after_open_is_silent_and_symlinked_targets_share_hash() {
        let repo = TestRepo::new(Some("standard"));
        let home = TestRepo::new(None);
        let link = home.root.join("alias");
        symlink(&repo.root, &link).unwrap();
        run(&repo, &home, &["index"]);
        run(&home, &home, &["index", link.to_str().unwrap()]);
        let entries = records(&home);
        assert_eq!(
            entries[0]["project_root_hash"],
            entries[1]["project_root_hash"]
        );
        if std::path::Path::new("/dev/full").exists() {
            fs::remove_file(log_path(&home)).unwrap();
            symlink("/dev/full", log_path(&home)).unwrap();
            let output = run(&repo, &home, &["query", "lantern", "--json"]);
            assert!(output.stderr.is_empty());
        }
    }
}
