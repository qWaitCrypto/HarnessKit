use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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

// One log traversal resolves every document and scope. The derived cache is
// keyed by HEAD and the exact requested paths, so checkout mtimes do not matter.
pub(crate) fn last_commit_times(
    target_dir: &Path,
    paths: &[String],
) -> Option<BTreeMap<String, Option<u64>>> {
    find_host_git_dir(target_dir)?;
    let mut times = paths
        .iter()
        .map(|path| (path.clone(), None))
        .collect::<BTreeMap<_, _>>();
    let Ok(head) = run_git_capture(target_dir, &["rev-parse", "HEAD"]) else {
        return Some(times);
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    struct Cache {
        head: String,
        times: BTreeMap<String, Option<u64>>,
    }
    let cache_path = target_dir.join(".harnesskit/state/git-commit-times.json");
    if let Some(cache) = fs::read(&cache_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Cache>(&bytes).ok())
    {
        if cache.head == head && cache.times.keys().eq(times.keys()) {
            return Some(cache.times);
        }
    }
    if times.is_empty() {
        return Some(times);
    }
    let output = Command::new("git")
        .args([
            "--literal-pathspecs",
            "log",
            "--format=%x00%ct",
            "--name-only",
            "-z",
            "--no-renames",
            "--relative",
            "--",
        ])
        .args(times.keys())
        .current_dir(target_dir)
        .output()
        .ok()?;
    if !output.status.success() {
        return Some(times);
    }
    let mut timestamp = None;
    let mut header = true;
    for part in output.stdout.split(|byte| *byte == 0) {
        if part.is_empty() {
            header = true;
            continue;
        }
        let part = String::from_utf8_lossy(part);
        if header {
            timestamp = part.parse::<u64>().ok();
            header = false;
        } else {
            let file = part.strip_prefix('\n').unwrap_or(&part);
            for (path, time) in &mut times {
                let prefix = format!("{}/", path.trim_end_matches('/'));
                if time.is_none() && (file == path || file.starts_with(&prefix)) {
                    *time = timestamp;
                }
            }
        }
    }
    let cache = Cache {
        head,
        times: times.clone(),
    };
    // Caching is optional; callers can also inspect a read-only repository.
    if let Ok(bytes) = serde_json::to_vec(&cache) {
        let _ = fs::write(cache_path, bytes);
    }
    Some(times)
}

pub(crate) fn tracked_doc_paths(target_dir: &Path, paths: &[String]) -> Result<Vec<String>> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let output = Command::new("git")
        .args(["--literal-pathspecs", "ls-files", "-z", "--"])
        .args(paths)
        .current_dir(target_dir)
        .output()?;
    if !output.status.success() {
        return Err("cannot read tracked documentation paths from git".into());
    }
    Ok(output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8_lossy(path).into_owned())
        .collect())
}

// Git applies .gitignore (including nested files), info/exclude and global
// excludes with their normal negation rules. NUL input preserves spaces.
pub(crate) fn ignored_paths(target_dir: &Path, paths: &[String]) -> Result<Vec<String>> {
    let mut child = Command::new("git")
        .args(["check-ignore", "--no-index", "--stdin", "-z"])
        .current_dir(target_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut input = child
        .stdin
        .take()
        .ok_or("git check-ignore stdin is unavailable")?;
    let bytes = paths
        .iter()
        .flat_map(|path| path.as_bytes().iter().copied().chain(std::iter::once(0)))
        .collect::<Vec<_>>();
    // Drain stdout while feeding stdin; large repositories can fill both pipes.
    let writer = std::thread::spawn(move || input.write_all(&bytes));
    let output = child.wait_with_output()?;
    writer
        .join()
        .map_err(|_| "git check-ignore input writer failed")??;
    if !matches!(output.status.code(), Some(0 | 1)) {
        return Err(format!(
            "git check-ignore failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8_lossy(path).into_owned())
        .collect())
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
