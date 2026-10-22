use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;

use flate2::{read::GzDecoder, write::GzEncoder, Compression};
use serde::Serialize;

use super::{
    ensure_history_dirs, history_content_hash, history_dir, history_object_path, history_refs_dir,
    history_snapshots_dir, latest_history_snapshot, latest_history_snapshot_id,
    list_history_snapshots, parse_history_snapshot_json, safe_relative_path, valid_object_hash,
    HistorySnapshot,
};
use crate::Result;

#[derive(Default, Serialize)]
pub(crate) struct TransferSummary {
    pub(crate) snapshots: usize,
    pub(crate) objects: usize,
    pub(crate) skipped_snapshots: usize,
    pub(crate) latest_snapshot: Option<String>,
}

pub(crate) fn export_history(target_dir: &Path, archive_path: &Path) -> Result<TransferSummary> {
    let snapshots = list_history_snapshots(target_dir)?;
    let mut hashes = BTreeSet::new();
    for snapshot in &snapshots {
        validate_snapshot(snapshot)?;
        for file in &snapshot.files {
            let bytes = fs::read(history_object_path(target_dir, &file.hash))?;
            validate_object(&file.hash, &bytes)?;
            if bytes.len() as u64 != file.size {
                return Err(format!("history object size mismatch for {}", file.path).into());
            }
            hashes.insert(file.hash.clone());
        }
    }
    let latest_snapshot = latest_history_snapshot_id(target_dir);
    if let Some(id) = &latest_snapshot {
        if !snapshots.iter().any(|snapshot| &snapshot.id == id) {
            return Err("history latest ref points to a missing snapshot".into());
        }
    }
    let parent = archive_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if let Ok(history) = history_dir(target_dir).canonicalize() {
        if parent.canonicalize()?.starts_with(history) {
            return Err("export destination must be outside .harnesskit/history".into());
        }
    }
    // Never truncate an existing archive or a file in the history store.
    let output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(archive_path)?;
    let result = (|| -> Result<()> {
        let mut archive = tar::Builder::new(GzEncoder::new(output, Compression::default()));
        for hash in &hashes {
            archive.append_path_with_name(
                history_object_path(target_dir, hash),
                format!("objects/{}/{}", &hash[..2], &hash[2..]),
            )?;
        }
        for snapshot in &snapshots {
            let name = format!("{}.json", snapshot.id);
            archive.append_path_with_name(
                history_snapshots_dir(target_dir).join(&name),
                format!("snapshots/{}", name),
            )?;
        }
        if latest_snapshot.is_some() {
            archive.append_path_with_name(
                history_refs_dir(target_dir).join("latest"),
                "refs/latest",
            )?;
        }
        archive.into_inner()?.finish()?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(archive_path);
    }
    result?;
    Ok(TransferSummary {
        snapshots: snapshots.len(),
        objects: hashes.len(),
        latest_snapshot,
        ..TransferSummary::default()
    })
}

pub(crate) fn import_history(target_dir: &Path, archive_path: &Path) -> Result<TransferSummary> {
    // Read and validate the complete archive before touching the destination.
    // Only recognized regular files are accepted; nothing is unpacked as a path.
    let mut archive = tar::Archive::new(GzDecoder::new(fs::File::open(archive_path)?));
    let mut objects = BTreeMap::new();
    let mut snapshots = BTreeMap::new();
    let mut latest = None;
    let mut seen = BTreeSet::new();
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry
            .path()?
            .to_str()
            .ok_or("non-UTF-8 history archive path")?
            .to_string();
        if !safe_relative_path(&path) || !seen.insert(path.clone()) {
            return Err(format!("invalid or duplicate history archive path: {}", path).into());
        }
        let parts = path.split('/').collect::<Vec<_>>();
        if entry.header().entry_type().is_dir()
            && (matches!(path.as_str(), "objects" | "snapshots" | "refs")
                || (parts.len() == 2 && parts[0] == "objects" && parts[1].len() == 2))
        {
            continue;
        }
        if !entry.header().entry_type().is_file() {
            return Err(format!("history archive entry must be a regular file: {}", path).into());
        }
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes)?;
        match parts.as_slice() {
            ["objects", prefix, suffix] if prefix.len() == 2 => {
                let hash = format!("{}{}", prefix, suffix);
                validate_object(&hash, &bytes)?;
                objects.insert(hash, bytes);
            }
            ["snapshots", name] if name.ends_with(".json") => {
                let snapshot = parse_history_snapshot_json(std::str::from_utf8(&bytes)?)?;
                validate_snapshot(&snapshot)?;
                if *name != format!("{}.json", snapshot.id) {
                    return Err("history snapshot filename does not match its id".into());
                }
                snapshots.insert(snapshot.id.clone(), (snapshot, bytes));
            }
            ["refs", "latest"] => latest = Some(std::str::from_utf8(&bytes)?.trim().to_string()),
            _ => return Err(format!("unrecognized history archive entry: {}", path).into()),
        }
    }
    // Consume the gzip trailer too, so truncated or corrupt archives cannot merge.
    io::copy(&mut archive.into_inner(), &mut io::sink())?;

    reject_symlinks(target_dir, Path::new(".harnesskit/history/refs/latest"))?;
    for (hash, bytes) in &objects {
        let path = history_object_path(target_dir, hash);
        reject_symlinks(target_dir, path.strip_prefix(target_dir)?)?;
        if path.exists() && fs::read(&path)? != *bytes {
            return Err(format!("existing history object conflicts with archive: {}", hash).into());
        }
    }
    for (snapshot, _) in snapshots.values() {
        let path = history_snapshots_dir(target_dir).join(format!("{}.json", snapshot.id));
        reject_symlinks(target_dir, path.strip_prefix(target_dir)?)?;
        for file in &snapshot.files {
            let bytes = match objects.get(&file.hash) {
                Some(bytes) => bytes.clone(),
                None => {
                    let path = history_object_path(target_dir, &file.hash);
                    reject_symlinks(target_dir, path.strip_prefix(target_dir)?)?;
                    fs::read(&path)
                        .map_err(|_| format!("missing history object for {}", file.path))?
                }
            };
            validate_object(&file.hash, &bytes)?;
            if bytes.len() as u64 != file.size {
                return Err(format!("history object size mismatch for {}", file.path).into());
            }
        }
    }
    let local_latest = latest_history_snapshot(target_dir)?;
    let incoming_latest = latest
        .as_ref()
        .map(|id| {
            snapshots
                .get(id)
                .map(|(snapshot, _)| snapshot)
                .ok_or("archive latest ref points to a missing snapshot")
        })
        .transpose()?
        .or_else(|| {
            snapshots
                .values()
                .map(|(snapshot, _)| snapshot)
                .max_by(|a, b| {
                    a.created_at_unix
                        .cmp(&b.created_at_unix)
                        .then_with(|| a.id.cmp(&b.id))
                })
        });
    // An existing ID is authoritative, including its original metadata.
    let incoming_latest = incoming_latest
        .map(|snapshot| {
            let path = history_snapshots_dir(target_dir).join(format!("{}.json", snapshot.id));
            if path.exists() {
                parse_history_snapshot_json(&fs::read_to_string(path)?)
            } else {
                Ok(snapshot.clone())
            }
        })
        .transpose()?;
    let latest_snapshot = [local_latest, incoming_latest]
        .into_iter()
        .flatten()
        .max_by(|a, b| {
            a.created_at_unix
                .cmp(&b.created_at_unix)
                .then_with(|| a.id.cmp(&b.id))
        })
        .map(|snapshot| snapshot.id);

    ensure_history_dirs(target_dir)?;
    let mut summary = TransferSummary {
        latest_snapshot,
        ..TransferSummary::default()
    };
    for (hash, bytes) in objects {
        let path = history_object_path(target_dir, &hash);
        fs::create_dir_all(path.parent().unwrap())?;
        if write_new_file(&path, &bytes)? {
            summary.objects += 1;
        }
    }
    for (id, (_, bytes)) in snapshots {
        let path = history_snapshots_dir(target_dir).join(format!("{}.json", id));
        if write_new_file(&path, &bytes)? {
            summary.snapshots += 1;
        } else {
            summary.skipped_snapshots += 1;
        }
    }
    if let Some(id) = &summary.latest_snapshot {
        fs::write(history_refs_dir(target_dir).join("latest"), id)?;
    }
    Ok(summary)
}

fn validate_snapshot(snapshot: &HistorySnapshot) -> Result<()> {
    if !safe_relative_path(&snapshot.id) || Path::new(&snapshot.id).components().count() != 1 {
        return Err("invalid history snapshot id".into());
    }
    let mut paths = BTreeSet::new();
    for file in &snapshot.files {
        if !safe_relative_path(&file.path)
            || file.path.split('/').any(|part| part == ".git")
            || (file.path.starts_with(".harnesskit/") && file.path != ".harnesskit/schema.yaml")
            || !valid_object_hash(&file.hash)
            || !paths.insert(&file.path)
        {
            return Err(format!("invalid history file record: {}", file.path).into());
        }
    }
    Ok(())
}

fn validate_object(hash: &str, bytes: &[u8]) -> Result<()> {
    if !valid_object_hash(hash) || history_content_hash(bytes) != hash {
        return Err(format!("history object hash mismatch: {}", hash).into());
    }
    Ok(())
}

fn reject_symlinks(root: &Path, relative: &Path) -> Result<()> {
    let mut path = root.to_path_buf();
    for part in relative.components() {
        path.push(part);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_symlink() => {
                return Err(format!("history path is a symlink: {}", path.display()).into())
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn write_new_file(path: &Path, bytes: &[u8]) -> Result<bool> {
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => {
            if let Err(error) = file.write_all(bytes) {
                drop(file);
                let _ = fs::remove_file(path);
                return Err(error.into());
            }
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error.into()),
    }
}
