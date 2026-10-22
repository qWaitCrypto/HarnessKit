use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::{env, fs};

use crate::fs_utils::{home_dir, now_unix};
use crate::Result;

pub(crate) fn detect_release_target() -> Result<String> {
    let os = env::consts::OS;
    let arch = env::consts::ARCH;
    match (os, arch) {
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-gnu".to_string()),
        ("macos", "x86_64") => Ok("x86_64-apple-darwin".to_string()),
        ("macos", "aarch64") => Ok("aarch64-apple-darwin".to_string()),
        _ => Err(format!(
            "unsupported platform: {} {}. This alpha updater currently supports Linux x86_64 and macOS x86_64/arm64.",
            os, arch
        )
        .into()),
    }
}

pub(crate) fn update_from_release(
    asset_base_url: &str,
    archive: &str,
    package: &str,
    tmp_dir: &Path,
    current_exe: &Path,
    version: &str,
    target: &str,
) -> Result<()> {
    let archive_path = tmp_dir.join(archive);
    let sums_path = tmp_dir.join("SHA256SUMS");
    println!("Downloading HarnessKit {} for {}...", version, target);
    download_to_file(
        &format!("{}/{}", asset_base_url.trim_end_matches('/'), archive),
        &archive_path,
    )?;
    download_to_file(
        &format!("{}/SHA256SUMS", asset_base_url.trim_end_matches('/')),
        &sums_path,
    )?;
    let archive_sums = write_archive_sha256_file(tmp_dir, "SHA256SUMS", archive)?;
    verify_sha256(tmp_dir, &archive_sums)?;
    run_command(
        Command::new("tar")
            .arg("-xzf")
            .arg(&archive_path)
            .arg("-C")
            .arg(tmp_dir),
        "tar extract failed",
    )?;

    let package_dir = tmp_dir.join(package);
    let next_binary = package_dir.join("harnesskit");
    if !next_binary.is_file() {
        return Err(format!("release archive did not contain {}", next_binary.display()).into());
    }

    let replacement = current_exe.with_file_name(format!(
        ".harnesskit-update-{}-{}",
        std::process::id(),
        now_unix()
    ));
    fs::copy(&next_binary, &replacement)?;
    make_executable(&replacement)?;
    fs::rename(&replacement, current_exe).map_err(|err| {
        let _ = fs::remove_file(&replacement);
        format!(
            "failed to replace current binary at {}: {}",
            current_exe.display(),
            err
        )
    })?;
    println!("Updated CLI: {}", current_exe.display());

    sync_existing_installed_skills(&package_dir)?;
    Ok(())
}

pub(crate) fn write_archive_sha256_file(
    dir: &Path,
    sums_file: &str,
    archive: &str,
) -> Result<String> {
    let sums = fs::read_to_string(dir.join(sums_file))?;
    let line = sums
        .lines()
        .find(|line| {
            line.split_whitespace()
                .nth(1)
                .is_some_and(|name| name == archive)
        })
        .ok_or_else(|| format!("{} did not contain checksum for {}", sums_file, archive))?;
    let archive_sums = format!("{}.sha256", archive);
    fs::write(dir.join(&archive_sums), format!("{}\n", line))?;
    Ok(archive_sums)
}

fn download_to_file(url: &str, out: &Path) -> Result<()> {
    if command_exists("curl") {
        let status = Command::new("curl")
            .arg("-fsSL")
            .arg("--retry")
            .arg("3")
            .arg("--retry-delay")
            .arg("2")
            .arg("--connect-timeout")
            .arg("20")
            .arg("--max-time")
            .arg("120")
            .arg(url)
            .arg("-o")
            .arg(out)
            .status()?;
        if status.success() {
            return Ok(());
        }
    } else if command_exists("wget") {
        let status = Command::new("wget")
            .arg("-q")
            .arg("--tries=3")
            .arg("--timeout=120")
            .arg(url)
            .arg("-O")
            .arg(out)
            .status()?;
        if status.success() {
            return Ok(());
        }
    } else {
        return Err("missing required command: curl or wget".into());
    }

    Err(format!(
        "download failed: {}. If you are behind a proxy, set http_proxy, https_proxy, or all_proxy and retry.",
        url
    )
    .into())
}

fn verify_sha256(dir: &Path, sums_file: &str) -> Result<()> {
    if command_exists("sha256sum") {
        run_command(
            Command::new("sha256sum")
                .arg("-c")
                .arg(sums_file)
                .current_dir(dir),
            "sha256 verification failed",
        )
    } else if command_exists("shasum") {
        run_command(
            Command::new("shasum")
                .arg("-a")
                .arg("256")
                .arg("-c")
                .arg(sums_file)
                .current_dir(dir),
            "sha256 verification failed",
        )
    } else {
        Err("missing required command: sha256sum or shasum".into())
    }
}

fn run_command(command: &mut Command, message: &str) -> Result<()> {
    let output = command.output()?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else {
        output.status.to_string()
    };
    Err(format!("{}: {}", message, detail).into())
}

fn command_exists(name: &str) -> bool {
    Command::new(name)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

fn make_executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions)?;
    }
    Ok(())
}

fn sync_existing_installed_skills(package_dir: &Path) -> Result<()> {
    let source_skill = package_dir
        .join("agent-surfaces")
        .join("skills")
        .join("harnesskit")
        .join("SKILL.md");
    if !source_skill.is_file() {
        return Ok(());
    }

    if let Some(home) = home_dir() {
        let claude_skill = home
            .join(".claude")
            .join("skills")
            .join("harnesskit")
            .join("SKILL.md");
        if claude_skill.exists() {
            copy_skill(&source_skill, &claude_skill)?;
            println!("Updated Claude Code skill: {}", claude_skill.display());
        }

        let codex_home = env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".codex"));
        let codex_skill = codex_home
            .join("skills")
            .join("harnesskit")
            .join("SKILL.md");
        if codex_skill.exists() {
            copy_skill(&source_skill, &codex_skill)?;
            println!("Updated Codex skill: {}", codex_skill.display());
        }
    }

    Ok(())
}

fn copy_skill(source: &Path, target: &Path) -> Result<()> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(source, target)?;
    Ok(())
}
