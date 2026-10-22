use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::doctor::render_command_output_error;
use crate::fs_utils::nearest_existing_parent;
use crate::schema::{managed_memory_paths, Schema};
use crate::Result;

#[derive(Default)]
pub(crate) struct HostGitInfo {
    pub(crate) head: Option<String>,
    pub(crate) branch: Option<String>,
    pub(crate) dirty: bool,
}

#[derive(Copy, Clone)]
pub(crate) enum HostGitMode {
    Full,
}

pub(crate) fn ensure_host_git_exclude(
    target_dir: &Path,
    schema: &Schema,
) -> Result<Option<(usize, usize)>> {
    let Some(git_dir) = find_host_git_dir(target_dir) else {
        return Ok(None);
    };
    let info_dir = git_dir.join("info");
    let exclude_path = info_dir.join("exclude");
    fs::create_dir_all(&info_dir)?;
    let existing = fs::read_to_string(&exclude_path).unwrap_or_default();
    let mut lines = existing
        .lines()
        .map(|line| line.to_string())
        .collect::<Vec<_>>();
    let mut added = 0usize;
    let mut already = 0usize;
    for path in managed_memory_paths(schema) {
        if lines.iter().any(|line| line.trim() == path) {
            already += 1;
        } else {
            lines.push(path);
            added += 1;
        }
    }
    if added > 0 {
        fs::write(&exclude_path, lines.join("\n") + "\n")?;
    }
    Ok(Some((added, already)))
}

pub(crate) fn find_host_git_dir(target_dir: &Path) -> Option<PathBuf> {
    let probe_dir = nearest_existing_parent(target_dir)?;
    let output = Command::new("git")
        .arg("rev-parse")
        .arg("--git-dir")
        .current_dir(&probe_dir)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if raw.is_empty() {
        return None;
    }

    let git_dir = PathBuf::from(raw);
    if git_dir.is_absolute() {
        Some(git_dir)
    } else {
        Some(probe_dir.join(git_dir))
    }
}

pub(crate) fn host_git_info(target_dir: &Path, mode: HostGitMode) -> HostGitInfo {
    let present = find_host_git_dir(target_dir).is_some();
    if !present {
        return HostGitInfo::default();
    }
    let head = run_git_capture(target_dir, &["rev-parse", "HEAD"]).ok();
    let branch = run_git_capture(target_dir, &["branch", "--show-current"])
        .ok()
        .filter(|value| !value.is_empty());
    let dirty = match mode {
        HostGitMode::Full => run_git_capture(target_dir, &["status", "--porcelain"])
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false),
    };
    HostGitInfo {
        head,
        branch,
        dirty,
    }
}

pub(crate) fn run_git_capture(target_dir: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(target_dir)
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            "git command failed".to_string()
        } else {
            stderr
        }
        .into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub(crate) fn git_repo_check(target_dir: &Path) -> (Option<bool>, Option<String>) {
    match Command::new("git")
        .arg("rev-parse")
        .arg("--is-inside-work-tree")
        .current_dir(target_dir)
        .output()
    {
        Ok(output) if output.status.success() => {
            let value = String::from_utf8_lossy(&output.stdout).trim() == "true";
            (Some(value), None)
        }
        Ok(output) => (
            Some(false),
            Some(render_command_output_error("git", &output)),
        ),
        Err(err) => (None, Some(err.to_string())),
    }
}
