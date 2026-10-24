mod support;

use serde_json::Value;
use std::fs;
use std::io::Write;
use std::process::Stdio;
use support::TestRepo;

fn project() -> TestRepo {
    let repo = TestRepo::new(None);
    let init = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&repo.root)
        .output()
        .unwrap();
    assert!(init.status.success());
    repo.run(&["init", ".", "--tracked"]);
    repo
}

fn write(repo: &TestRepo, path: &str, text: &str) {
    let path = repo.root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn commit_all(repo: &TestRepo, message: &str) {
    let add = std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(&repo.root)
        .output()
        .unwrap();
    assert!(add.status.success());
    let commit = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-qm",
            message,
        ])
        .current_dir(&repo.root)
        .output()
        .unwrap();
    assert!(commit.status.success());
}

fn hook(
    repo: &TestRepo,
    subcommand: &str,
    session: &str,
    path: &str,
    hook_event_name: Option<&str>,
) -> String {
    let mut command = repo.command(&["hook", subcommand]);
    command.stdin(Stdio::piped()).stdout(Stdio::piped());
    let mut child = command.spawn().unwrap();
    let input = match hook_event_name {
        Some(event) => format!(
            "{{\"hook_event_name\":\"{}\",\"session_id\":\"{}\",\"tool_input\":{{\"file_path\":\"{}\"}}}}",
            event, session, path
        ),
        None => format!(
            "{{\"session_id\":\"{}\",\"tool_input\":{{\"file_path\":\"{}\"}}}}",
            session, path
        ),
    };
    child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap()
}

fn direct_hook(repo: &TestRepo, subcommand: &str, path: &str) -> String {
    let output = repo
        .command(&["hook", subcommand, path])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn suggest_anchors_renders_evidence_and_applies_only_code_lines() {
    let repo = project();
    write(
        &repo,
        "src/auth.rs",
        "pub fn refresh_token(value: u64) -> u64 { value + 1 }\n",
    );
    write(
        &repo,
        "docs/decisions/refresh.md",
        "# Refresh\n\n## Policy\n\nThe policy mentions `refresh_token`.\n",
    );
    write(
        &repo,
        "docs/exec-plans/active/refresh.md",
        &format!(
            "# Refresh plan\n\nThe active plan tracks `refresh_token`. {}\n",
            "This active plan tracks the refresh implementation and its operational constraints. ".repeat(12)
        ),
    );
    commit_all(&repo, "refresh docs");
    repo.run(&["index", "."]);

    let json = repo.run(&["suggest-anchors", "docs/decisions/refresh.md", "--json"]);
    let report: Value = serde_json::from_str(&json).unwrap();
    assert_eq!(report["suggestions"][0]["anchor"], "src/auth.rs#refresh_token");
    assert!(report["suggestions"][0]["reason"]
        .as_str()
        .unwrap()
        .contains("Document mentions"));

    repo.run(&["suggest-anchors", "docs/decisions/refresh.md", "--apply", "1"]);
    let updated = fs::read_to_string(repo.root.join("docs/decisions/refresh.md")).unwrap();
    assert!(updated.contains("The policy mentions `refresh_token`."));
    assert!(updated.contains("Code: `src/auth.rs#refresh_token`"));
}

#[test]
fn hooks_read_claude_json_deduplicate_and_report_drift() {
    let repo = project();
    write(
        &repo,
        "src/auth.rs",
        "pub fn refresh_token(value: u64) -> u64 { value + 1 }\n",
    );
    write(
        &repo,
        "docs/decisions/refresh.md",
        "# Refresh\n\n## Policy\n\nThe policy mentions `refresh_token`.\n",
    );
    write(
        &repo,
        "docs/exec-plans/active/refresh.md",
        &format!(
            "# Refresh plan\n\nThe active plan tracks `refresh_token`. {}\n",
            "This active plan tracks the refresh implementation and its operational constraints. ".repeat(12)
        ),
    );
    commit_all(&repo, "refresh docs");
    repo.run(&["index", "."]);
    let first = hook(&repo, "pre-edit", "session-1", "src/auth.rs", None);
    assert!(first.contains("Constraint: docs/decisions/refresh.md"));
    assert!(first.contains("Related ~: docs/exec-plans/active/refresh.md"));
    assert!(first.lines().all(|line| line.chars().count() <= 160));
    assert!(first.contains("The policy mentions"));
    assert!(hook(&repo, "pre-edit", "session-1", "src/auth.rs", None).is_empty());

    let claude = hook(
        &repo,
        "pre-edit",
        "session-2",
        "src/auth.rs",
        Some("PreToolUse"),
    );
    let claude: Value = serde_json::from_str(&claude).unwrap();
    assert_eq!(claude["hookSpecificOutput"]["hookEventName"], "PreToolUse");
    let additional = claude["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(additional.contains("Constraint:"));
    assert!(additional.lines().all(|line| line.chars().count() <= 160));
    assert!(direct_hook(&repo, "pre-edit", "src/auth.rs").starts_with("Constraint:"));

    write(&repo, "src/unrelated.rs", "pub fn unrelated() {}\n");
    repo.run(&["index", "."]);
    assert!(hook(
        &repo,
        "pre-edit",
        "session-unrelated",
        "src/unrelated.rs",
        Some("PreToolUse"),
    )
    .is_empty());

    repo.run(&["suggest-anchors", "docs/decisions/refresh.md", "--apply", "1"]);
    repo.run(&["index", "."]);
    write(
        &repo,
        "src/auth.rs",
        "pub fn refresh_token(value: u64) -> u64 { value + 99 }\n",
    );
    let post = hook(&repo, "post-edit", "session-3", "src/auth.rs", None);
    assert!(post.contains("Anchor drift"));
    assert!(post.contains("check --ack docs/decisions/refresh.md"));

    let post_json = hook(
        &repo,
        "post-edit",
        "session-4",
        "src/auth.rs",
        Some("PostToolUse"),
    );
    let post_json: Value = serde_json::from_str(&post_json).unwrap();
    assert_eq!(post_json["hookSpecificOutput"]["hookEventName"], "PostToolUse");
    assert!(post_json["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .contains("Anchor drift"));
}

#[test]
fn accepted_decision_hooks_warn_on_body_edits_and_ignore_metadata_only_changes() {
    let repo = project();
    let path = "docs/decisions/ownership.md";
    let original = "---\nstatus: accepted\nauthority: canonical\n---\n\n# Ownership\n\nThe caller must handle failures.\n";
    write(&repo, path, original);
    commit_all(&repo, "Accept ownership");
    repo.run(&["index"]);
    let reminder = "accepted decision changed — confirm with the user / supersede instead";
    let pre = direct_hook(&repo, "pre-edit", path);
    assert!(pre.contains(reminder), "{pre}");
    assert_eq!(pre.lines().count(), 1);
    write(&repo, path, &original.replace("authority: canonical", "authority: supporting"));
    assert!(direct_hook(&repo, "post-edit", path).is_empty());
    write(&repo, path, &original.replace("must handle", "must ignore"));
    let post = hook(&repo, "post-edit", "decision-body", path, Some("PostToolUse"));
    let post: Value = serde_json::from_str(&post).unwrap();
    assert_eq!(post["hookSpecificOutput"]["additionalContext"], reminder);

    write(&repo, path, original);
    repo.run(&["index"]);
    fs::remove_dir_all(repo.root.join(".git")).unwrap();
    write(&repo, path, &original.replace("must handle", "must ignore"));
    assert_eq!(direct_hook(&repo, "post-edit", path).trim(), reminder);
}

#[test]
fn install_hooks_previews_then_backs_up_existing_settings() {
    let repo = project();
    let preview = repo.run(&["install", "--hooks", "."]);
    assert!(preview.contains("PreToolUse"));
    assert!(!repo.root.join(".claude/settings.json").exists());

    write(&repo, ".claude/settings.json", "{\"permissions\":{}}\n");
    repo.run(&["install", "--hooks", "--write", "."]);
    assert!(repo.root.join(".claude/settings.json").exists());
    assert!(repo.root.join(".claude/settings.json.bak").exists());
    let settings: Value = serde_json::from_str(
        &fs::read_to_string(repo.root.join(".claude/settings.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(settings["hooks"]["PreToolUse"][0]["matcher"], "Edit|Write|MultiEdit");
}

#[test]
fn undocumented_hotspot_is_info_limited_and_excludes_tests() {
    let repo = project();
    write(&repo, "src/hot.rs", "pub fn hot(value: u64) -> u64 { value + 1 }\n");
    write(&repo, "tests/hot.rs", "pub fn test_hot(value: u64) -> u64 { value + 1 }\n");
    repo.run(&["index", "."]);
    for value in ["2", "3", "4"] {
        write(&repo, "src/hot.rs", &format!("pub fn hot(value: u64) -> u64 {{ value + {} }}\n", value));
        write(&repo, "tests/hot.rs", &format!("pub fn test_hot(value: u64) -> u64 {{ value + {} }}\n", value));
        let add = std::process::Command::new("git")
            .args(["add", "src/hot.rs", "tests/hot.rs"])
            .current_dir(&repo.root)
            .output()
            .unwrap();
        assert!(add.status.success());
        let commit = std::process::Command::new("git")
            .args(["-c", "user.name=Test", "-c", "user.email=test@example.invalid", "commit", "-qm", value])
            .current_dir(&repo.root)
            .output()
            .unwrap();
        assert!(commit.status.success());
    }
    let report: Value = serde_json::from_str(&repo.run(&["check", ".", "--json"])).unwrap();
    let hotspots = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|check| check["rule_id"] == "undocumented-hotspot")
        .collect::<Vec<_>>();
    assert!(hotspots.iter().any(|check| check["subject_path"] == "src/hot.rs"));
    assert!(hotspots.iter().all(|check| check["subject_path"] != "tests/hot.rs"));
    let text = repo.run(&["check", ".", "--all", "--rules", "weave"]);
    assert_eq!(text.matches("Hint: ").count(), 1, "{}", text);
}
