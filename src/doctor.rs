use std::path::Path;
use std::process::Command;
use std::{env, fs};

use crate::fs_utils::{absolute_display_path, nearest_existing_parent, now_unix};
use crate::git::git_repo_check;
use crate::skills::{skill_locations, skill_version, BUNDLED_SKILL};

#[derive(serde::Serialize)]
pub(crate) struct SkillCheck {
    pub(crate) agent: String,
    pub(crate) path: String,
    pub(crate) version: Option<String>,
    pub(crate) matches_bundled: bool,
    pub(crate) error: Option<String>,
}

#[derive(serde::Serialize)]
pub(crate) struct CommandCheck {
    pub(crate) present: bool,
    pub(crate) version: Option<String>,
    pub(crate) error: Option<String>,
}

#[derive(serde::Serialize)]
pub(crate) struct DoctorReport {
    pub(crate) cli_version: String,
    pub(crate) os: String,
    pub(crate) arch: String,
    pub(crate) target_dir: String,
    pub(crate) current_dir: String,
    pub(crate) pwd: Option<String>,
    pub(crate) pwd_differs_from_current_dir: bool,
    #[serde(serialize_with = "crate::render::serialize_inline")]
    pub(crate) git: CommandCheck,
    pub(crate) inside_git_repo: Option<bool>,
    pub(crate) git_repo_error: Option<String>,
    pub(crate) state_exists: bool,
    pub(crate) state_writable: bool,
    pub(crate) state_error: Option<String>,
    pub(crate) history_exists: bool,
    pub(crate) schema_exists: bool,
    pub(crate) claude_skill_path: String,
    pub(crate) claude_skill_installed: bool,
    pub(crate) codex_skill_path: String,
    pub(crate) codex_skill_installed: bool,
    pub(crate) bundled_skill_version: String,
    pub(crate) skills: Vec<SkillCheck>,
    pub(crate) current_exe: Option<String>,
    pub(crate) path_contains_cli_dir: bool,
    pub(crate) hints: Vec<String>,
    pub(crate) coverage: crate::discover::Coverage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) schema_preview: Option<String>,
    pub(crate) schema_written: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) usage: Option<crate::usage::UsageReport>,
}

pub(crate) fn build_doctor_report(target_dir: &Path) -> DoctorReport {
    let current_dir = env::current_dir().ok();
    let current_dir_display = current_dir
        .as_ref()
        .map(|path| absolute_display_path(path))
        .unwrap_or_else(|| "(unavailable)".to_string());
    let pwd = env::var("PWD").ok();
    let pwd_differs_from_current_dir = pwd
        .as_ref()
        .map(|pwd| pwd != &current_dir_display)
        .unwrap_or(false);
    let git = command_check("git", &["--version"]);
    let (inside_git_repo, git_repo_error) = git_repo_check(target_dir);
    let state_dir = target_dir.join(".harnesskit").join("state");
    let history_dir = target_dir.join(".harnesskit").join("history");
    let schema_path = target_dir.join(".harnesskit").join("schema.yaml");
    let (state_writable, state_error) = state_writable_check(target_dir, &state_dir);
    let locations = skill_locations();
    let skill_path = |agent| {
        locations
            .iter()
            .find(|location| location.agent == agent)
            .map(|location| absolute_display_path(&location.path))
            .unwrap_or_default()
    };
    let skill_installed = |agent| {
        locations
            .iter()
            .any(|location| location.agent == agent && location.path.exists())
    };
    let bundled_skill_version = skill_version(BUNDLED_SKILL).unwrap_or_default();
    let mut skills = Vec::new();
    let mut hints = Vec::new();
    for location in locations.iter().filter(|location| location.path.exists()) {
        let (version, error) = match fs::read_to_string(&location.path) {
            Ok(text) => (skill_version(&text), None),
            Err(error) => (None, Some(error.to_string())),
        };
        let matches_bundled = version.as_deref() == Some(bundled_skill_version.as_str());
        let path = absolute_display_path(&location.path);
        if !matches_bundled {
            hints.push(format!(
                "Installed {} skill at {} has version {} (bundled {}). Run `harnesskit update` or sync from this checkout with {} (include references/).{}",
                location.agent, path, version.as_deref().unwrap_or("unknown"),
                bundled_skill_version, location.sync,
                error.as_ref().map(|error| format!(" Read error: {}", error)).unwrap_or_default()
            ));
        }
        skills.push(SkillCheck {
            agent: location.agent.to_string(),
            path,
            version,
            matches_bundled,
            error,
        });
    }
    let current_exe = env::current_exe().ok();
    let path_contains_cli_dir = current_exe
        .as_ref()
        .and_then(|path| path.parent())
        .map(path_contains_dir)
        .unwrap_or(false);

    DoctorReport {
        cli_version: env!("CARGO_PKG_VERSION").to_string(),
        os: env::consts::OS.to_string(),
        arch: env::consts::ARCH.to_string(),
        target_dir: absolute_display_path(target_dir),
        current_dir: current_dir_display,
        pwd,
        pwd_differs_from_current_dir,
        git,
        inside_git_repo,
        git_repo_error,
        state_exists: state_dir.exists(),
        state_writable,
        state_error,
        history_exists: history_dir.exists(),
        schema_exists: schema_path.exists(),
        claude_skill_path: skill_path("Claude Code"),
        claude_skill_installed: skill_installed("Claude Code"),
        codex_skill_path: skill_path("Codex"),
        codex_skill_installed: skill_installed("Codex"),
        bundled_skill_version,
        skills,
        path_contains_cli_dir,
        current_exe: current_exe.as_ref().map(|path| absolute_display_path(path)),
        hints,
        coverage: Default::default(),
        schema_preview: None,
        schema_written: false,
        usage: None,
    }
}

fn command_check(name: &str, args: &[&str]) -> CommandCheck {
    match Command::new(name).args(args).output() {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            CommandCheck {
                present: true,
                version: Some(if stdout.is_empty() { stderr } else { stdout }),
                error: None,
            }
        }
        Ok(output) => CommandCheck {
            present: true,
            version: None,
            error: Some(render_command_output_error(name, &output)),
        },
        Err(err) => CommandCheck {
            present: false,
            version: None,
            error: Some(err.to_string()),
        },
    }
}

pub(crate) fn render_command_output_error(name: &str, output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !stderr.is_empty() {
        format!("{} exited with {}: {}", name, output.status, stderr)
    } else if !stdout.is_empty() {
        format!("{} exited with {}: {}", name, output.status, stdout)
    } else {
        format!("{} exited with {}", name, output.status)
    }
}

fn state_writable_check(target_dir: &Path, state_dir: &Path) -> (bool, Option<String>) {
    let harnesskit_dir = target_dir.join(".harnesskit");
    let probe_dir = if state_dir.exists() {
        state_dir.to_path_buf()
    } else if harnesskit_dir.exists() {
        harnesskit_dir
    } else if target_dir.exists() {
        target_dir.to_path_buf()
    } else {
        match nearest_existing_parent(target_dir) {
            Some(path) => path,
            None => {
                return (
                    false,
                    Some("target dir and parents do not exist".to_string()),
                )
            }
        }
    };
    let probe = probe_dir.join(format!(
        ".harnesskit-doctor-write-test-{}-{}",
        std::process::id(),
        now_unix()
    ));
    match fs::write(&probe, b"ok") {
        Ok(()) => {
            let _ = fs::remove_file(&probe);
            (true, None)
        }
        Err(err) => (
            false,
            Some(format!(
                "cannot write probe at {}: {}",
                probe.display(),
                err
            )),
        ),
    }
}

fn path_contains_dir(dir: &Path) -> bool {
    let Some(path_value) = env::var_os("PATH") else {
        return false;
    };
    env::split_paths(&path_value).any(|entry| entry == dir)
}
