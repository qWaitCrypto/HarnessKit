use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_harnesskit"))
}

fn temp_dir(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "harnesskit-cli-{}-{}-{}",
        name,
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&path).unwrap();
    path
}

fn run(args: &[&str]) -> String {
    let output = Command::new(bin()).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "command failed: harnesskit {}\nstdout:\n{}\nstderr:\n{}",
        args.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn run_in(dir: &Path, program: &str, args: &[&str]) {
    let output = Command::new(program)
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "command failed: {} {}\nstdout:\n{}\nstderr:\n{}",
        program,
        args.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn cli_init_preview_local_tracked_and_context_json_smoke() {
    let preview_parent = temp_dir("preview-parent");
    let preview_target = preview_parent.join("preview-target");
    run_in(&preview_parent, "git", &["init"]);

    let preview = run(&["init", preview_target.to_str().unwrap(), "--preview"]);
    assert!(preview.contains("Preview only: no files were written."));
    assert!(preview.contains("Git exclude: would add"));
    assert!(!preview_target.exists());

    let local = temp_dir("local");
    run_in(&local, "git", &["init"]);
    run(&["init", local.to_str().unwrap(), "--local"]);
    let exclude = fs::read_to_string(local.join(".git/info/exclude")).unwrap();
    assert!(exclude.contains("docs/"));
    assert!(exclude.contains("AGENTS.md"));
    let status = Command::new("git")
        .args(["status", "--short"])
        .current_dir(&local)
        .output()
        .unwrap();
    assert!(status.status.success());
    assert_eq!(String::from_utf8_lossy(&status.stdout), "");

    let tracked = temp_dir("tracked");
    run_in(&tracked, "git", &["init"]);
    run(&["init", tracked.to_str().unwrap(), "--tracked"]);
    let tracked_exclude = fs::read_to_string(tracked.join(".git/info/exclude")).unwrap();
    assert!(!tracked_exclude.contains("AGENTS.md"));
    let tracked_status = Command::new("git")
        .args(["status", "--short"])
        .current_dir(&tracked)
        .output()
        .unwrap();
    assert!(tracked_status.status.success());
    let tracked_status_text = String::from_utf8_lossy(&tracked_status.stdout);
    assert!(tracked_status_text.contains("?? AGENTS.md"));
    assert!(tracked_status_text.contains("?? docs/"));

    run(&["index", tracked.to_str().unwrap()]);
    let check = run(&["check", tracked.to_str().unwrap(), "--json"]);
    assert!(check.contains("\"docs_count\""));
    assert!(check.contains("\"checks\""));
    let doctor = run(&["doctor", tracked.to_str().unwrap(), "--json"]);
    assert!(doctor.contains("\"sqlite3\""));
    assert!(doctor.contains("\"state_writable\""));
    let context = run(&[
        "context",
        "architecture",
        tracked.to_str().unwrap(),
        "--json",
    ]);
    assert!(context.contains("\"kind\": \"context\""));
    assert!(context.contains("\"recommended_reading_order\""));
}
