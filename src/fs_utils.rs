use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{env, fs};

use crate::Result;

pub(crate) struct PathDiagnostics {
    pub(crate) target_dir: String,
    pub(crate) current_dir: String,
    pub(crate) pwd: Option<String>,
    pub(crate) state_dir: String,
    pub(crate) facts_path: String,
}

pub(crate) fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME").map(PathBuf::from)
}

pub(crate) fn file_modified_parts(metadata: &fs::Metadata) -> Result<(u64, u64)> {
    let modified = metadata.modified()?.duration_since(UNIX_EPOCH)?;
    let nanos = modified.as_nanos().min(u128::from(u64::MAX)) as u64;
    Ok((modified.as_secs(), nanos))
}

pub(crate) fn ensure_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    Ok(())
}

pub(crate) fn validate_target_dir(path: &Path, allow_missing: bool) -> Result<()> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(format!("target_dir is not a directory: {}", path.display()).into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if allow_missing {
                return Ok(());
            }
            Err(format!(
                "target_dir does not exist: {}. Was an argument mistaken for target_dir? Only `harnesskit init` can create a target directory.",
                path.display()
            )
            .into())
        }
        Err(error) => Err(format!(
            "cannot access target_dir {}: {}",
            path.display(),
            error
        )
        .into()),
    }
}

fn ensure_writable_dir(path: &Path, purpose: &str) -> Result<()> {
    fs::create_dir_all(path).map_err(|err| {
        format!(
            "cannot create {} directory at {}: {}.",
            purpose,
            path.display(),
            err
        )
    })?;

    let probe = path.join(format!(
        ".harnesskit-write-test-{}-{}",
        std::process::id(),
        now_unix()
    ));
    match fs::write(&probe, b"ok") {
        Ok(()) => {
            let _ = fs::remove_file(&probe);
            Ok(())
        }
        Err(err) => Err(format!(
            "cannot write {} directory at {}: {}. Check directory permissions or run HarnessKit from a writable project path.",
            purpose,
            path.display(),
            err
        )
        .into()),
    }
}

pub(crate) fn ensure_writable_fact_store_dir(
    target_dir: &Path,
    state_dir: &Path,
    purpose: &str,
) -> Result<()> {
    ensure_writable_dir(state_dir, purpose).map_err(|err| {
        format!(
            "{}\n\n{}",
            err,
            fact_store_path_hint(target_dir, Some(state_dir), None)
        )
        .into()
    })
}

pub(crate) fn fact_store_path_hint(
    target_dir: &Path,
    state_dir: Option<&Path>,
    facts_path: Option<&Path>,
) -> String {
    let diagnostics = path_diagnostics(target_dir, state_dir, facts_path);
    let mut out = String::new();
    out.push_str("HarnessKit path diagnostics:\n");
    out.push_str(&format!("- target_dir: {}\n", diagnostics.target_dir));
    out.push_str(&format!("- current_dir: {}\n", diagnostics.current_dir));
    out.push_str(&format!(
        "- PWD: {}\n",
        diagnostics.pwd.as_deref().unwrap_or("(not set)")
    ));
    out.push_str(&format!("- state_dir: {}\n", diagnostics.state_dir));
    out.push_str(&format!("- facts_path: {}\n", diagnostics.facts_path));
    out.push_str("Hint: run `harnesskit doctor --json` to verify git, PATH, and HarnessKit state directories. If diagnostics do not point at the intended writable project root, pass the project root explicitly, for example `harnesskit check /path/to/repo --json` or `harnesskit index /path/to/repo`.\n");
    out.push_str("Do not delete `.harnesskit/history` unless you intentionally want to discard local doc checkpoints.");
    out
}

fn path_diagnostics(
    target_dir: &Path,
    state_dir: Option<&Path>,
    facts_path: Option<&Path>,
) -> PathDiagnostics {
    let state = state_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(|| target_dir.join(".harnesskit").join("state"));
    let facts = facts_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| state.join("facts.sqlite"));
    PathDiagnostics {
        target_dir: absolute_display_path(target_dir),
        current_dir: env::current_dir()
            .map(|path| absolute_display_path(&path))
            .unwrap_or_else(|err| format!("(unavailable: {})", err)),
        pwd: env::var("PWD").ok(),
        state_dir: absolute_display_path(&state),
        facts_path: absolute_display_path(&facts),
    }
}

pub(crate) fn nearest_existing_parent(path: &Path) -> Option<PathBuf> {
    let mut current = path;
    loop {
        if current.exists() {
            return Some(current.to_path_buf());
        }
        current = current.parent()?;
    }
}

pub(crate) fn read_text_lossy(path: &Path) -> Result<String> {
    let bytes = fs::read(path)?;
    Ok(String::from_utf8_lossy(&bytes).to_string())
}

pub(crate) fn normalize_path(path: &Path) -> String {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop();
            }
            Component::Normal(text) => parts.push(text.to_string_lossy().to_string()),
            Component::RootDir | Component::Prefix(_) => {}
        }
    }
    parts.join("/")
}

pub(crate) fn absolute_display_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

pub(crate) fn simple_content_hash(text: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in text.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{:016x}", hash)
}

pub(crate) fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}
