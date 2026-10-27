//! Small deterministic cache for paged output result sets.

use std::fs;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::index::{cell, meta_value};
use crate::sqlite::SqliteConnection;
use crate::Result;

const CACHE_LIMIT: usize = 64;

#[derive(Serialize, serde::Deserialize)]
struct CacheEntry<T> {
    generation: String,
    payload: T,
}

pub(crate) fn generation(db: &SqliteConnection) -> Result<String> {
    let mut values = Vec::new();
    for key in [
        "fact_schema_version",
        "generated_at_unix",
        "discovery_fingerprint",
        "schema_fingerprint",
        "last_delta_added",
        "last_delta_changed",
        "last_delta_removed",
    ] {
        values.push(format!(
            "{key}={}",
            meta_value(db, key)?.unwrap_or_default()
        ));
    }
    // Include the current file-state hashes so two edits in one second still
    // advance the cache generation even when metadata timestamps collide.
    let states = db.query(
        "SELECT path, content_hash FROM file_states ORDER BY path;",
        [],
    )?;
    for row in states.rows {
        values.push(format!("{}={}", cell(&row, 0), cell(&row, 1)));
    }
    Ok(blake3::hash(values.join("\n").as_bytes())
        .to_hex()
        .to_string())
}

pub(crate) fn key(generation: &str, descriptor: &str) -> String {
    blake3::hash(format!("{generation}\n{descriptor}").as_bytes())
        .to_hex()
        .to_string()
}

fn cache_dir(root: &Path) -> PathBuf {
    root.join(".harnesskit").join("state").join("query-cache")
}

pub(crate) fn read<T: DeserializeOwned>(
    root: &Path,
    cache_key: &str,
    current_generation: &str,
) -> Result<Option<T>> {
    let path = cache_dir(root).join(format!("{cache_key}.json"));
    if !path.is_file() {
        return Ok(None);
    }
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(_) => return Ok(None),
    };
    let entry: CacheEntry<T> = match serde_json::from_slice(&bytes) {
        Ok(entry) => entry,
        Err(_) => return Ok(None),
    };
    if entry.generation != current_generation {
        let _ = fs::remove_file(path);
        return Ok(None);
    }
    // mtime is the LRU touch signal; cache errors never affect retrieval.
    let _ = filetime_touch(&path);
    Ok(Some(entry.payload))
}

pub(crate) fn write<T: Serialize>(
    root: &Path,
    cache_key: &str,
    current_generation: &str,
    payload: &T,
) -> Result<()> {
    let dir = cache_dir(root);
    fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{cache_key}.json"));
    let entry = CacheEntry {
        generation: current_generation.to_string(),
        payload,
    };
    let bytes = serde_json::to_vec(&entry)?;
    let temp = dir.join(format!(".{cache_key}.tmp-{}", std::process::id()));
    fs::write(&temp, bytes)?;
    fs::rename(&temp, &path)?;
    let _ = filetime_touch(&path);
    prune(&dir);
    Ok(())
}

fn prune(dir: &Path) {
    let mut files = fs::read_dir(dir)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let entry = entry.ok()?;
            (entry.path().extension().and_then(|ext| ext.to_str()) == Some("json")).then(|| {
                let modified = entry
                    .metadata()
                    .and_then(|metadata| metadata.modified())
                    .ok();
                (entry.path(), modified)
            })
        })
        .collect::<Vec<_>>();
    if files.len() <= CACHE_LIMIT {
        return;
    }
    files.sort_by_key(|(_, modified)| *modified);
    let remove_count = files.len() - CACHE_LIMIT;
    for (path, _) in files.into_iter().take(remove_count) {
        let _ = fs::remove_file(path);
    }
}

fn filetime_touch(path: &Path) -> std::io::Result<()> {
    // Touch the inode without changing its contents; this gives prune() an
    // access-order signal without adding a timestamp dependency.
    let metadata = fs::metadata(path)?;
    let permissions = metadata.permissions();
    if permissions.readonly() {
        return Ok(());
    }
    let file = fs::OpenOptions::new().write(true).open(path)?;
    file.set_len(metadata.len())
}
