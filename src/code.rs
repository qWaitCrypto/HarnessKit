mod extract;

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use ignore::WalkBuilder;
use rusqlite::params;
use serde::Serialize;

use crate::discover::{is_ignored, is_markdown};
use crate::fs_utils::{file_modified_parts, normalize_path};
use crate::git::{git_repo_check, ignored_paths, tracked_doc_paths};
use crate::index::{cell, meta_value, parse_file_state_rows, FileStateRecord};
use crate::schema::Schema;
use crate::sqlite::SqliteConnection;
use crate::Result;

#[derive(Debug)]
struct CodeNode {
    id: String,
    kind: &'static str,
    path: String,
    name: String,
    // Inclusive, one-based source line numbers.
    span_start: usize,
    span_end: usize,
    fingerprint: String,
    lang: String,
    // Keep declarations available to explicit anchors, but not inferred mentions.
    is_definition: bool,
}

#[derive(Serialize)]
pub(crate) struct CodeReport {
    pub(crate) scan_mode: String,
    pub(crate) files: usize,
    pub(crate) symbols: usize,
    pub(crate) languages: Vec<LanguageStats>,
}

#[derive(Serialize)]
pub(crate) struct LanguageStats {
    pub(crate) lang: String,
    pub(crate) files: usize,
    pub(crate) symbols: usize,
}

pub(crate) fn code_report(db: &SqliteConnection) -> Result<CodeReport> {
    let rows = db.query(
        "SELECT lang, SUM(kind = 'file'), SUM(kind = 'symbol') FROM nodes
         WHERE kind IN ('file', 'symbol') GROUP BY lang ORDER BY lang;",
        [],
    )?;
    let languages = rows
        .rows
        .iter()
        .map(|row| LanguageStats {
            lang: cell(row, 0).to_string(),
            files: cell(row, 1).parse().unwrap_or(0),
            symbols: cell(row, 2).parse().unwrap_or(0),
        })
        .collect::<Vec<_>>();
    Ok(CodeReport {
        scan_mode: meta_value(db, "code_scan_mode")?.unwrap_or_default(),
        files: languages.iter().map(|language| language.files).sum(),
        symbols: languages.iter().map(|language| language.symbols).sum(),
        languages,
    })
}

fn excluded_path(path: &Path) -> bool {
    path.components().any(|part| {
        !matches!(part, std::path::Component::Normal(_))
            || matches!(part.as_os_str().to_str(), Some(".git" | ".harnesskit"))
    })
}

fn regular_file(root: &Path, relative: &str) -> Result<bool> {
    let mut current = root.to_path_buf();
    for part in Path::new(relative).components() {
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => return Ok(false),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(current.is_file())
}

fn has_git_marker(root: &Path) -> bool {
    root.ancestors().any(|ancestor| {
        let marker = ancestor.join(".git");
        // An empty directory named .git is not a repository. Worktrees and
        // submodules instead use a file pointing at their actual Git directory.
        marker.join("HEAD").is_file()
            || fs::read_to_string(marker)
                .map(|text| text.starts_with("gitdir: "))
                .unwrap_or(false)
    })
}

fn collect_code_paths(root: &Path, schema: &Schema) -> Result<(Vec<String>, &'static str)> {
    if git_repo_check(root).0 == Some(true) {
        let candidates = tracked_doc_paths(root, &[".".to_string()])?;
        let mut paths = Vec::new();
        for path in candidates {
            if !excluded_path(Path::new(&path))
                && !is_markdown(&path)
                && !is_ignored(schema, &path)
                && regular_file(root, &path)?
            {
                paths.push(path);
            }
        }
        let ignored = ignored_paths(root, &paths)?
            .into_iter()
            .collect::<BTreeSet<_>>();
        paths.retain(|path| !ignored.contains(path));
        paths.sort();
        paths.dedup();
        return Ok((paths, "git-tracked"));
    }

    // A Git failure is not permission to scan untracked files. Keep the last
    // code index in that case, without disrupting the existing docs commands.
    if has_git_marker(&root.canonicalize()?) {
        return Ok((Vec::new(), "git-unavailable"));
    }

    // .gitignore also applies outside Git, including nested files and negations.
    let mut walker = WalkBuilder::new(root);
    walker.require_git(false).hidden(false).follow_links(false);
    let walk_root = root.to_path_buf();
    let walk_schema = schema.clone();
    walker.filter_entry(move |entry| {
        let Ok(relative) = entry.path().strip_prefix(&walk_root) else {
            return false;
        };
        relative.as_os_str().is_empty()
            || (!excluded_path(relative) && !is_ignored(&walk_schema, &normalize_path(relative)))
    });
    let mut paths = Vec::new();
    for entry in walker.build() {
        let entry = entry?;
        if entry
            .file_type()
            .map(|kind| kind.is_file())
            .unwrap_or(false)
        {
            let path = normalize_path(entry.path().strip_prefix(root)?);
            if !is_markdown(&path) {
                paths.push(path);
            }
        }
    }
    paths.sort();
    Ok((paths, "walk"))
}

pub(crate) fn refresh_code_facts(
    db: &SqliteConnection,
    root: &Path,
    schema: &Schema,
) -> Result<()> {
    let rows = db.query(
        "SELECT path, content_hash, file_size_bytes, mtime_unix, mtime_ns
         FROM file_states WHERE kind = 'code' ORDER BY path;",
        [],
    )?;
    let previous = parse_file_state_rows(&rows);
    let (paths, scan_mode) = collect_code_paths(root, schema)?;
    if scan_mode == "git-unavailable" {
        return db.execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES ('code_scan_mode', ?1);",
            [scan_mode],
        );
    }
    let mut extractor = extract::Extractor::default();
    let mut current = BTreeSet::new();
    let mut updates = Vec::new();
    for path in paths {
        let metadata = fs::metadata(root.join(&path))?;
        let (mtime_unix, mtime_ns) = file_modified_parts(&metadata)?;
        if let Some(state) = previous.get(&path) {
            if state.file_size_bytes == metadata.len() && state.mtime_ns == mtime_ns {
                current.insert(path);
                continue;
            }
        }
        let bytes = fs::read(root.join(&path))?;
        // Unsupported text languages keep a file node; binary assets do not.
        let Ok(text) = std::str::from_utf8(&bytes) else {
            continue;
        };
        if bytes.contains(&0) {
            continue;
        }
        let content_hash = blake3::hash(&bytes).to_hex().to_string();
        let nodes = if previous.get(&path).map(|state| &state.content_hash) == Some(&content_hash) {
            None
        } else {
            Some(extractor.extract(&path, text)?)
        };
        current.insert(path.clone());
        updates.push((
            path,
            FileStateRecord {
                content_hash,
                file_size_bytes: metadata.len(),
                mtime_unix,
                mtime_ns,
            },
            nodes,
        ));
    }
    db.transaction(|db| {
        for path in previous.keys().filter(|path| !current.contains(*path)) {
            db.execute("DELETE FROM nodes WHERE path = ?1;", [path])?;
            db.execute("DELETE FROM file_states WHERE path = ?1 AND kind = 'code';", [path])?;
        }
        for (path, state, nodes) in &updates {
            if let Some(nodes) = nodes {
                db.execute("DELETE FROM nodes WHERE path = ?1;", [path])?;
                for node in nodes {
                    db.execute(
                        "INSERT INTO nodes(id, kind, path, name, span_start, span_end, fingerprint, lang, is_definition)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9);",
                        params![node.id, node.kind, node.path, node.name, node.span_start,
                            node.span_end, node.fingerprint, node.lang, node.is_definition],
                    )?;
                }
            }
            db.execute(
                "INSERT OR REPLACE INTO file_states(path, kind, content_hash, file_size_bytes, mtime_unix, mtime_ns)
                 VALUES (?1, 'code', ?2, ?3, ?4, ?5);",
                params![path, state.content_hash, state.file_size_bytes, state.mtime_unix, state.mtime_ns],
            )?;
        }
        db.execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES ('code_scan_mode', ?1);",
            [scan_mode],
        )
    })
}

fn file_language(path: &str) -> String {
    let extension = Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("unknown")
        .to_ascii_lowercase();
    match extension.as_str() {
        "rs" => "rust",
        "ts" | "tsx" | "mts" | "cts" => "typescript",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "py" | "pyi" => "python",
        "go" => "go",
        "rb" => "ruby",
        "h" => "c",
        "cc" | "cxx" | "hpp" | "hxx" => "cpp",
        "sh" | "bash" => "shell",
        _ => return extension,
    }
    .to_string()
}
