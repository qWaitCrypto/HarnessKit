mod support;

use std::fs;
use std::path::Path;
use std::process::Command;

use serde_json::Value;
use support::{assert_success, copy_tree, TestRepo};

fn doctor(repo: &TestRepo, home: &TestRepo, flags: &[&str]) -> Value {
    let mut args = vec!["doctor", "--json"];
    args.extend_from_slice(flags);
    let output = repo
        .command(&args)
        .env("HOME", &home.root)
        .env_remove("CODEX_HOME")
        .env_remove("CLAUDE_CONFIG_DIR")
        .output()
        .unwrap();
    assert_success(&output, &args);
    serde_json::from_slice(&output.stdout).unwrap()
}

fn write(repo: &TestRepo, path: &str, text: &str) {
    let path = repo.root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn skill(report: &Value, agent: &str) -> Value {
    report["skills"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["agent"] == agent)
        .unwrap()
        .clone()
}

fn has_explanation(text: &str) -> bool {
    text.contains("Coverage scans Markdown") || text.contains("inferred managed_root")
}

#[test]
fn bundled_skill_is_short_and_versioned_with_the_cli() {
    let text = include_str!("../agent-surfaces/skills/harnesskit/SKILL.md");
    assert!(text.lines().count() <= 35);
    let header: Value = serde_yaml::from_str(text.split("---").nth(1).unwrap()).unwrap();
    assert_eq!(header["metadata"]["version"], env!("CARGO_PKG_VERSION"));
    assert!(Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("agent-surfaces/skills/harnesskit/references/cli.md")
        .is_file());
}

#[test]
fn direct_installs_carry_references_and_doctor_detects_old_skills() {
    let repo = TestRepo::new(Some("flat"));
    let home = TestRepo::new(None);
    let source = Path::new(env!("CARGO_MANIFEST_DIR"));
    for agent in ["codex", "claude"] {
        let output = Command::new("bash")
            .arg(source.join(format!("scripts/install-{}-skill.sh", agent)))
            .args(["--home", home.root.to_str().unwrap()])
            .args(["--repo-root", source.to_str().unwrap()])
            .env_remove("CODEX_HOME")
            .env_remove("CLAUDE_CONFIG_DIR")
            .current_dir(&home.root)
            .output()
            .unwrap();
        assert_success(&output, &["install skill"]);
        for file in ["SKILL.md", "references/cli.md"] {
            assert_eq!(
                fs::read(
                    home.root
                        .join(format!(".{}/skills/harnesskit/{}", agent, file))
                )
                .unwrap(),
                fs::read(source.join("agent-surfaces/skills/harnesskit").join(file)).unwrap()
            );
        }
    }
    let current = doctor(&repo, &home, &[]);
    assert_eq!(current["bundled_skill_version"], env!("CARGO_PKG_VERSION"));
    for agent in ["Codex", "Claude Code"] {
        assert_eq!(skill(&current, agent)["matches_bundled"], true);
    }
    write(&home, ".claude/skills/harnesskit/SKILL.md", "# Old skill\n");
    write(
        &home,
        ".codex/skills/harnesskit/SKILL.md",
        "---\nmetadata:\n  version: old\n---\n# Old\n",
    );
    let old = doctor(&repo, &home, &[]);
    assert!(skill(&old, "Claude Code")["version"].is_null());
    assert_eq!(skill(&old, "Codex")["version"], "old");
    for agent in ["Codex", "Claude Code"] {
        assert_eq!(skill(&old, agent)["matches_bundled"], false);
    }
    let hints = old["hints"].to_string();
    for action in [
        "harnesskit update",
        "install-codex-skill.sh",
        "install-claude-skill.sh",
    ] {
        assert!(hints.contains(action), "{}", hints);
    }
}

#[test]
fn doctor_handles_absent_skills_overrides_and_invalid_headers() {
    let repo = TestRepo::new(Some("flat"));
    let home = TestRepo::new(None);
    let absent = doctor(&repo, &home, &[]);
    assert_eq!(absent["codex_skill_installed"], false);
    assert_eq!(absent["claude_skill_installed"], false);
    assert!(absent["skills"].as_array().unwrap().is_empty());
    write(
        &home,
        ".codex/skills/harnesskit/SKILL.md",
        "# Ignored default\n",
    );
    write(
        &home,
        "codex-custom/skills/harnesskit/SKILL.md",
        &format!(
            "---\r\nmetadata:\r\n  version: '{}'\r\n---\r\n",
            env!("CARGO_PKG_VERSION")
        ),
    );
    write(
        &home,
        "claude-custom/skills/harnesskit/SKILL.md",
        "---\nmetadata: [bad\n---\n",
    );
    write(
        &home,
        ".agents/skills/harnesskit/SKILL.md",
        "# Body\nversion: ignored\n",
    );
    let output = repo
        .command(&["doctor", "--json"])
        .env("HOME", &home.root)
        .env("CODEX_HOME", home.root.join("codex-custom"))
        .env("CLAUDE_CONFIG_DIR", home.root.join("claude-custom"))
        .output()
        .unwrap();
    assert_success(&output, &["doctor"]);
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(skill(&report, "Codex")["matches_bundled"], true);
    assert_eq!(skill(&report, "Claude Code")["matches_bundled"], false);
    assert!(skill(&report, "shared agents")["version"].is_null());
    assert!(report["codex_skill_path"]
        .as_str()
        .unwrap()
        .contains("codex-custom"));
}

#[test]
fn inferred_layout_explanation_is_shared_once_across_commands_and_formats() {
    let repo = TestRepo::new(None);
    write(
        &repo,
        "overview.md",
        "# Overview\n\nA searchable handoff.\n",
    );
    let first = repo.run(&["index"]);
    assert!(has_explanation(&first));
    for args in [
        vec!["index"],
        vec!["list-docs"],
        vec!["check", "--all"],
        vec!["check", "--format", "full"],
        vec!["history", "snapshot"],
    ] {
        let text = repo.run(&args);
        assert_eq!(
            text.lines()
                .filter(|line| line.starts_with("Coverage:"))
                .count(),
            1
        );
        assert!(!has_explanation(&text), "{:?}: {}", args, text);
    }
    for args in [
        vec!["index", "--json"],
        vec!["check", "--json"],
        vec!["check", "--format", "json"],
        vec!["list-docs", "--json"],
        vec!["history", "snapshot", "--json"],
        vec!["query", "handoff", "--json"],
        vec!["context", "--current", "--json"],
        vec!["context", "overview.md", "--json"],
        vec!["graph", "overview.md", "--json"],
    ] {
        let text = repo.run(&args);
        let report: Value = serde_json::from_str(&text).unwrap();
        assert!(report["coverage"].is_object());
        assert!(report["hints"].is_array());
        assert!(!has_explanation(&report["hints"].to_string()), "{:?}", args);
    }
    for args in [
        vec!["index", "--verbose"],
        vec!["check", "--verbose"],
        vec!["history", "snapshot", "--verbose"],
    ] {
        assert!(has_explanation(&repo.run(&args)), "{:?}", args);
    }
    let home = TestRepo::new(None);
    assert!(!has_explanation(
        &doctor(&repo, &home, &[])["hints"].to_string()
    ));
    assert!(has_explanation(
        &doctor(&repo, &home, &["--layout"])["hints"].to_string()
    ));
    // Rebuilding the fact store must not reset the presentation record.
    fs::remove_file(repo.root.join(".harnesskit/state/facts.sqlite")).unwrap();
    assert!(!has_explanation(&repo.run(&["index"])));
}

#[test]
fn explicit_schema_is_quiet_and_layout_preview_stays_read_only() {
    let repo = TestRepo::new(Some("flat"));
    let home = TestRepo::new(None);
    let layout = doctor(&repo, &home, &["--layout"]);
    assert!(has_explanation(&layout["hints"].to_string()));
    assert!(!repo.root.join(".harnesskit").exists());
    repo.run(&["init", "--tracked"]);
    for args in [
        vec!["index"],
        vec!["check", "--all"],
        vec!["history", "snapshot"],
    ] {
        assert!(!has_explanation(&repo.run(&args)), "{:?}", args);
    }
    assert!(has_explanation(&repo.run(&["index", "--verbose"])));
    assert!(!repo
        .root
        .join(".harnesskit/state/layout-hints-shown")
        .exists());
}

#[test]
fn plugin_packaging_and_installation_include_the_reference_tree() {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"));
    let checkout = TestRepo::new(None);
    copy_tree(
        &source.join("agent-surfaces"),
        &checkout.root.join("agent-surfaces"),
    );
    fs::create_dir(checkout.root.join("scripts")).unwrap();
    let synced = Command::new("bash")
        .arg(source.join("scripts/sync-agent-surfaces.sh"))
        .current_dir(&checkout.root)
        .output()
        .unwrap();
    assert_success(&synced, &["sync agent surfaces"]);
    assert!(checkout
        .root
        .join("agent-surfaces/codex/skills/harnesskit/references/cli.md")
        .is_file());
    let home = TestRepo::new(None);
    let installed = Command::new("bash")
        .arg(source.join("scripts/install-codex-plugin-local.sh"))
        .args([
            "--repo-root",
            source.to_str().unwrap(),
            "--home",
            home.root.to_str().unwrap(),
        ])
        .current_dir(&home.root)
        .output()
        .unwrap();
    assert_success(&installed, &["install plugin"]);
    assert!(home
        .root
        .join("plugins/harnesskit/skills/harnesskit/references/cli.md")
        .is_file());
}

#[test]
fn release_install_and_update_sync_references_for_existing_skills() {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"));
    let release = TestRepo::new(None);
    let target = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        _ => return,
    };
    let package = format!("harnesskit-{}", target);
    let package_dir = release.root.join(&package);
    copy_tree(
        &source.join("agent-surfaces"),
        &package_dir.join("agent-surfaces"),
    );
    fs::write(package_dir.join("harnesskit"), "synthetic updated CLI\n").unwrap();
    let archive = format!("{}.tar.gz", package);
    let tar = Command::new("tar")
        .args(["-czf", &archive, &package])
        .current_dir(&release.root)
        .output()
        .unwrap();
    assert_success(&tar, &["tar"]);
    let mut checksum = if cfg!(target_os = "macos") {
        let mut command = Command::new("shasum");
        command.args(["-a", "256"]);
        command
    } else {
        Command::new("sha256sum")
    };
    let sums = checksum
        .arg(&archive)
        .current_dir(&release.root)
        .output()
        .unwrap();
    assert_success(&sums, &["checksum"]);
    fs::write(release.root.join("SHA256SUMS"), sums.stdout).unwrap();
    let base_url = format!("file://{}", release.root.display());
    let home = TestRepo::new(None);
    let install = Command::new("bash")
        .arg(source.join("install.sh"))
        .args([
            "--asset-base-url",
            &base_url,
            "--prefix",
            home.root.to_str().unwrap(),
            "--with-codex-plugin",
        ])
        .env("HOME", &home.root)
        .env_remove("CODEX_HOME")
        .env_remove("CLAUDE_CONFIG_DIR")
        .output()
        .unwrap();
    assert_success(&install, &["install release"]);
    let installed_dirs = [
        ".codex/skills/harnesskit",
        ".claude/skills/harnesskit",
        "plugins/harnesskit/skills/harnesskit",
    ];
    for path in installed_dirs {
        assert!(home.root.join(path).join("references/cli.md").is_file());
        fs::remove_dir_all(home.root.join(path).join("references")).unwrap();
        fs::write(home.root.join(path).join("SKILL.md"), "old").unwrap();
    }
    let installed_cli = home.root.join("bin/harnesskit");
    fs::copy(env!("CARGO_BIN_EXE_harnesskit"), &installed_cli).unwrap();
    let update = Command::new(&installed_cli)
        .args(["update", "--asset-base-url", &base_url])
        .env("HOME", &home.root)
        .env_remove("CODEX_HOME")
        .env_remove("CLAUDE_CONFIG_DIR")
        .env("HARNESSKIT_USAGE_LOG", "off")
        .output()
        .unwrap();
    assert_success(&update, &["update release"]);
    for path in installed_dirs {
        for file in ["SKILL.md", "references/cli.md"] {
            assert_eq!(
                fs::read(home.root.join(path).join(file)).unwrap(),
                fs::read(source.join("agent-surfaces/skills/harnesskit").join(file)).unwrap()
            );
        }
    }
    assert!(!home.root.join(".agents/skills/harnesskit").exists());
    assert_eq!(fs::read(installed_cli).unwrap(), b"synthetic updated CLI\n");
}
