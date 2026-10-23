use std::path::PathBuf;
use std::{env, fs};

use crate::fs_utils::home_dir;

pub(crate) const BUNDLED_SKILL: &str = include_str!("../agent-surfaces/skills/harnesskit/SKILL.md");

pub(crate) struct SkillLocation {
    pub(crate) agent: &'static str,
    pub(crate) path: PathBuf,
    pub(crate) sync: &'static str,
}

pub(crate) fn skill_locations() -> Vec<SkillLocation> {
    let home = home_dir().filter(|path| !path.as_os_str().is_empty());
    let codex = env::var_os("CODEX_HOME")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|path| path.join(".codex")));
    let claude = env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|path| path.join(".claude")));
    let mut locations = Vec::new();
    for (agent, root, sync) in [
        ("Claude Code", claude, "scripts/install-claude-skill.sh"),
        ("Codex", codex, "scripts/install-codex-skill.sh"),
        (
            "shared agents",
            home.as_ref().map(|path| path.join(".agents")),
            "copy agent-surfaces/skills/harnesskit/ to the installed skill directory",
        ),
        (
            "Codex plugin",
            home.as_ref().map(|path| path.join("plugins/harnesskit")),
            "scripts/install-codex-plugin-local.sh",
        ),
    ] {
        if let Some(root) = root {
            locations.push(SkillLocation {
                agent,
                path: root.join("skills/harnesskit/SKILL.md"),
                sync,
            });
        }
    }
    locations
}

pub(crate) fn skill_version(text: &str) -> Option<String> {
    let text = text.replace("\r\n", "\n");
    let raw = text.strip_prefix("---\n")?.split_once("\n---")?.0;
    let frontmatter: serde_yaml::Value = serde_yaml::from_str(raw).ok()?;
    frontmatter["metadata"]["version"]
        .as_str()
        .or_else(|| frontmatter["version"].as_str())
        .filter(|version| !version.is_empty())
        .map(str::to_string)
}

// References are part of the installed skill, including during CLI updates.
pub(crate) fn copy_skill(source: &std::path::Path, target: &std::path::Path) -> crate::Result<()> {
    fs::create_dir_all(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let destination = target.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_skill(&entry.path(), &destination)?;
        } else {
            fs::copy(entry.path(), destination)?;
        }
    }
    Ok(())
}
