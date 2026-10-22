mod support;

use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use flate2::{read::GzDecoder, write::GzEncoder, Compression};
use serde_json::{json, Value};
use support::{copy_tree, TestRepo};

fn report(repo: &TestRepo, args: &[&str]) -> Value {
    let report: Value = serde_json::from_str(&repo.run(args)).unwrap();
    assert!(report["coverage"].is_object(), "{}", report);
    assert!(report["hints"].is_array(), "{}", report);
    report
}

fn status(repo: &TestRepo) -> Value {
    report(repo, &["history", "status", "--json"])
}

fn snapshot(repo: &TestRepo) -> Value {
    report(repo, &["history", "snapshot", "--json"])
}

fn stored(repo: &TestRepo, id: &str) -> Value {
    serde_json::from_slice(
        &fs::read(
            repo.root
                .join(format!(".harnesskit/history/snapshots/{}.json", id)),
        )
        .unwrap(),
    )
    .unwrap()
}

fn write(repo: &TestRepo, path: &str, bytes: impl AsRef<[u8]>) {
    let path = repo.root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

fn set_snapshot_age(repo: &TestRepo, id: &str, age: u64) {
    let mut value = stored(repo, id);
    value["created_at_unix"] = (SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        - age)
        .into();
    write(
        repo,
        &format!(".harnesskit/history/snapshots/{}.json", id),
        serde_json::to_vec(&value).unwrap(),
    );
}

#[test]
fn snapshots_persist_all_inferred_managed_documents_and_coverage() {
    for fixture in ["flat", "standard"] {
        let repo = TestRepo::new(Some(fixture));
        write(&repo, "docs/misc/unmanaged.md", "# Outside scope\n");
        let snapshot = snapshot(&repo);
        let stored = stored(&repo, snapshot["id"].as_str().unwrap());
        assert_eq!(stored["coverage"], snapshot["coverage"]);
        assert_eq!(stored["host_git_dirty"], Value::Null);
        let files = stored["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|file| file["path"].as_str().unwrap())
            .collect::<BTreeSet<_>>();
        let managed = stored["coverage"]["mappings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["status"] == "managed")
            .map(|item| item["path"].as_str().unwrap())
            .collect::<BTreeSet<_>>();
        assert_eq!(files, managed);
        assert!(!files.contains("docs/misc/unmanaged.md"));
        if fixture == "flat" {
            assert!(files.contains("docs/principles.md"));
            assert!(files.contains("docs/reference/relay.md"));
        }
    }
}

#[test]
fn unchanged_contents_skip_snapshots_even_after_mtime_or_unmanaged_changes() {
    let repo = TestRepo::new(Some("flat"));
    let first = snapshot(&repo);
    assert_eq!(first["skipped"], false);
    let id = first["id"].as_str().unwrap();
    let stored_bytes = fs::read(
        repo.root
            .join(format!(".harnesskit/history/snapshots/{}.json", id)),
    )
    .unwrap();
    let bytes = fs::read(repo.root.join("docs/principles.md")).unwrap();
    write(&repo, "docs/principles.md", &bytes);
    write(&repo, "docs/misc/unmanaged.md", "# Not managed\n");
    let skipped = snapshot(&repo);
    assert_eq!(skipped["skipped"], true);
    assert_eq!(skipped["id"], first["id"]);
    assert_eq!(skipped["coverage"]["unmanaged"], 1);
    assert!(skipped["hints"]
        .as_array()
        .unwrap()
        .iter()
        .any(|hint| hint.as_str().unwrap().contains("--allow-empty")));
    assert!(repo
        .run(&["history", "snapshot"])
        .contains("Snapshot skipped"));
    assert_eq!(
        fs::read(
            repo.root
                .join(format!(".harnesskit/history/snapshots/{}.json", id))
        )
        .unwrap(),
        stored_bytes
    );
    assert_eq!(
        report(&repo, &["history", "list", "--json"])["snapshots"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let forced = report(&repo, &["history", "snapshot", "--allow-empty", "--json"]);
    assert_eq!(forced["skipped"], false);
    assert_ne!(forced["id"], first["id"]);
    assert_eq!(
        stored(&repo, forced["id"].as_str().unwrap())["coverage"]["unmanaged"],
        1
    );
}

#[test]
fn additions_removals_document_and_schema_content_changes_create_snapshots() {
    let repo = TestRepo::new(Some("flat"));
    repo.run(&["init", "--tracked"]);
    let mut previous = snapshot(&repo);
    for (path, content) in [
        ("docs/principles.md", Some("# Revised principles\n")),
        ("docs/new-note.md", Some("# Added note\n")),
        ("docs/new-note.md", None),
    ] {
        if let Some(content) = content {
            write(&repo, path, content);
        } else {
            fs::remove_file(repo.root.join(path)).unwrap();
        }
        let next = snapshot(&repo);
        assert_eq!(next["skipped"], false);
        assert_ne!(next["id"], previous["id"]);
        previous = next;
    }
    let mut schema = fs::read(repo.root.join(".harnesskit/schema.yaml")).unwrap();
    schema.extend_from_slice(b"\n# Revised schema\n");
    write(&repo, ".harnesskit/schema.yaml", schema);
    let next = snapshot(&repo);
    assert_eq!(next["skipped"], false);
    assert_ne!(next["id"], previous["id"]);
    assert_eq!(status(&repo)["dirty"], false);
}

#[test]
fn status_reports_age_changes_unmanaged_docs_and_nullable_git_state() {
    let repo = TestRepo::new(Some("flat"));
    let initial = status(&repo);
    assert_eq!(initial["days_since_snapshot"], Value::Null);
    assert_eq!(initial["host_git_dirty"], Value::Null);
    assert_eq!(initial["docs_git_tracked"], false);
    assert_eq!(initial["changed_files_count"], 8);
    let first = snapshot(&repo);
    let id = first["id"].as_str().unwrap();
    set_snapshot_age(&repo, id, 9 * 86_400);
    write(&repo, "docs/principles.md", "# New content\n");
    write(&repo, "docs/new-note.md", "# New note\n");
    fs::remove_file(repo.root.join("docs/execution-plan.md")).unwrap();
    write(&repo, "docs/misc/unmanaged.md", "# Unmanaged\n");
    let current = status(&repo);
    assert_eq!(current["days_since_snapshot"], 9);
    assert_eq!(current["changed_files_count"], 3);
    assert_eq!(current["unmanaged_files_count"], 1);
    assert_eq!(current["git_untracked_docs_count"], 8);
    assert!(current["hints"].as_array().unwrap().iter().any(|hint| hint
        .as_str()
        .unwrap()
        .contains("history is the only version record")));
    assert!(repo
        .run(&["history", "status"])
        .contains("Days since latest snapshot: 9"));
    let indexed = report(&repo, &["index", "--json"]);
    assert_eq!(indexed["host_git_dirty"], Value::Null);
    assert_eq!(
        report(&repo, &["index", "--json"])["host_git_dirty"],
        Value::Null
    );
}

#[test]
fn git_tracking_is_measured_per_managed_document_including_partial_tracking() {
    let repo = TestRepo::new(Some("flat"));
    for args in [vec!["init", "-q"], vec!["add", "AGENTS.md"]] {
        assert!(Command::new("git")
            .args(args)
            .current_dir(&repo.root)
            .output()
            .unwrap()
            .status
            .success());
    }
    let partial = status(&repo);
    assert_eq!(partial["host_git_dirty"], true);
    assert_eq!(partial["docs_git_tracked"], false);
    assert_eq!(partial["git_tracked_docs_count"], 1);
    assert_eq!(partial["git_untracked_docs_count"], 7);
    assert!(Command::new("git")
        .args(["add", "docs"])
        .current_dir(&repo.root)
        .output()
        .unwrap()
        .status
        .success());
    let tracked = status(&repo);
    assert_eq!(tracked["docs_git_tracked"], true);
    assert_eq!(tracked["git_untracked_docs_count"], 0);
    assert!(!tracked["hints"]
        .as_array()
        .unwrap()
        .iter()
        .any(|hint| hint.as_str().unwrap().contains("only version record")));
    assert_eq!(snapshot(&repo)["host_git_dirty"], true);
}

fn history_warnings(repo: &TestRepo) -> Value {
    report(
        repo,
        &[
            "check",
            "--rules",
            "history-dirty-since-snapshot",
            "--all",
            "--json",
        ],
    )["checks"]
        .clone()
}

#[test]
fn dirty_history_warns_only_after_seven_days_with_changes_or_over_twenty_files() {
    let repo = TestRepo::new(Some("flat"));
    let first = snapshot(&repo);
    let id = first["id"].as_str().unwrap();
    let original = fs::read(repo.root.join("docs/principles.md")).unwrap();
    write(&repo, "docs/principles.md", "# Changed\n");
    assert_eq!(history_warnings(&repo), json!([]));
    set_snapshot_age(&repo, id, 7 * 86_400 - 60);
    assert_eq!(history_warnings(&repo), json!([]));
    set_snapshot_age(&repo, id, 7 * 86_400 + 60);
    let warnings = history_warnings(&repo);
    assert_eq!(warnings.as_array().unwrap().len(), 1);
    assert!(warnings[0]["hint"]
        .as_str()
        .unwrap()
        .contains("history snapshot"));
    write(&repo, "docs/principles.md", original);
    assert_eq!(history_warnings(&repo), json!([]));
    set_snapshot_age(&repo, id, 0);
    for n in 0..20 {
        write(
            &repo,
            &format!("docs/added-{}.md", n),
            format!("# Note {}\n", n),
        );
    }
    assert_eq!(status(&repo)["changed_files_count"], 20);
    assert_eq!(history_warnings(&repo), json!([]));
    write(&repo, "docs/added-20.md", "# Twenty first note\n");
    assert_eq!(history_warnings(&repo).as_array().unwrap().len(), 1);
}

#[test]
fn export_import_roundtrip_preserves_status_show_metadata_and_all_snapshots() {
    let source = TestRepo::new(Some("flat"));
    let first = snapshot(&source);
    write(
        &source,
        "docs/principles.md",
        "# Revised 合成\n\nNo final newline",
    );
    let second = snapshot(&source);
    write(&source, "docs/new-note.md", "# Unsnapshotted\n");
    let target = TestRepo::new(None);
    copy_tree(&source.root.join("docs"), &target.root.join("docs"));
    fs::copy(source.root.join("AGENTS.md"), target.root.join("AGENTS.md")).unwrap();
    let archive = source.root.join("portable history.tar.gz");
    let archive = archive.to_str().unwrap();
    let exported = report(&source, &["history", "export", archive, "--json"]);
    assert_eq!(exported["snapshots"], 2);
    assert!(!target.root.join(".harnesskit").exists());
    let imported = report(
        &source,
        &[
            "history",
            "import",
            archive,
            target.root.to_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(imported["snapshots"], 2);
    assert_eq!(imported["objects"], exported["objects"]);
    assert_eq!(status(&target), status(&source));
    assert_eq!(
        report(&target, &["history", "list", "--json"]),
        report(&source, &["history", "list", "--json"])
    );
    for snapshot in [&first, &second] {
        let id = snapshot["id"].as_str().unwrap();
        for file in snapshot["files"].as_array().unwrap() {
            let path = file["path"].as_str().unwrap();
            assert_eq!(
                source.run(&["history", "show", id, path]),
                target.run(&["history", "show", id, path])
            );
        }
        assert_eq!(stored(&target, id), stored(&source, id));
    }
    assert_eq!(
        report(
            &target,
            &["history", "show", "latest", "docs/principles.md", "--json"]
        ),
        report(
            &source,
            &["history", "show", "latest", "docs/principles.md", "--json"]
        )
    );
    assert_eq!(
        target.run(&["history", "diff", "latest"]),
        source.run(&["history", "diff", "latest"])
    );
    let repeated = report(&target, &["history", "import", archive, "--json"]);
    assert_eq!(repeated["snapshots"], 0);
    assert_eq!(repeated["objects"], 0);
    assert_eq!(repeated["skipped_snapshots"], 2);
    target.run(&[
        "history",
        "restore",
        first["id"].as_str().unwrap(),
        "--apply",
        "--force",
    ]);
    assert!(!target.root.join("docs/new-note.md").exists());
    assert_eq!(
        fs::read(target.root.join("docs/principles.md")).unwrap(),
        source
            .run(&[
                "history",
                "show",
                first["id"].as_str().unwrap(),
                "docs/principles.md"
            ])
            .as_bytes()
    );
}

#[test]
fn importing_conflicting_ids_keeps_existing_snapshot_bytes_and_latest() {
    let target = TestRepo::new(Some("flat"));
    let existing = snapshot(&target);
    let id = existing["id"].as_str().unwrap();
    let path = format!(".harnesskit/history/snapshots/{}.json", id);
    let original = fs::read(target.root.join(&path)).unwrap();
    let source = TestRepo::new(Some("flat"));
    write(
        &source,
        "docs/principles.md",
        "# Incoming conflicting document\n",
    );
    let incoming = snapshot(&source);
    let mut conflict = stored(&source, incoming["id"].as_str().unwrap());
    conflict["id"] = id.into();
    conflict["message"] = "Conflicting imported metadata".into();
    write(&source, &path, serde_json::to_vec(&conflict).unwrap());
    fs::remove_file(source.root.join(format!(
        ".harnesskit/history/snapshots/{}.json",
        incoming["id"].as_str().unwrap()
    )))
    .unwrap();
    write(&source, ".harnesskit/history/refs/latest", id);
    let archive = source.root.join("conflict.tar.gz");
    source.run(&["history", "export", archive.to_str().unwrap()]);
    let result = report(
        &target,
        &["history", "import", archive.to_str().unwrap(), "--json"],
    );
    assert_eq!(result["skipped_snapshots"], 1);
    assert_eq!(fs::read(target.root.join(path)).unwrap(), original);
    assert_eq!(status(&target)["latest_snapshot"], id);
    assert_eq!(status(&target)["dirty"], false);
}

#[test]
fn legacy_snapshots_export_import_and_show_without_adding_coverage() {
    let source = TestRepo::new(None);
    copy_tree(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/compat/alpha6-history"),
        &source.root.join(".harnesskit/history"),
    );
    let archive = source.root.join("legacy.tar.gz");
    source.run(&["history", "export", archive.to_str().unwrap()]);
    let target = TestRepo::new(None);
    target.run(&["history", "import", archive.to_str().unwrap()]);
    let id = status(&target)["latest_snapshot"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(stored(&target, &id).get("coverage").is_none());
    assert_eq!(
        target.run(&[
            "history",
            "show",
            "latest",
            "docs/design-docs/legacy-relay.md"
        ]),
        "# Legacy Relay\n\n旧版合成记录：保留 acknowledgement 与恢复边界。\n"
    );
    target.run(&["history", "restore", "latest", "--apply", "--force"]);
    assert_eq!(status(&target)["dirty"], false);
    assert_eq!(snapshot(&target)["skipped"], true);
}

fn archive_entries(path: &Path) -> Vec<(String, Vec<u8>)> {
    tar::Archive::new(GzDecoder::new(fs::File::open(path).unwrap()))
        .entries()
        .unwrap()
        .map(|entry| {
            let mut entry = entry.unwrap();
            let name = entry.path().unwrap().to_str().unwrap().to_string();
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).unwrap();
            (name, bytes)
        })
        .collect()
}

fn make_archive(path: &Path, entries: &[(String, Vec<u8>)]) {
    let mut archive = tar::Builder::new(GzEncoder::new(
        fs::File::create(path).unwrap(),
        Compression::default(),
    ));
    for (name, bytes) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        archive
            .append_data(&mut header, name, bytes.as_slice())
            .unwrap();
    }
    archive.into_inner().unwrap().finish().unwrap();
}

#[test]
fn invalid_archives_fail_preflight_without_creating_history_or_writing_documents() {
    let source = TestRepo::new(Some("flat"));
    snapshot(&source);
    let archive = source.root.join("valid.tar.gz");
    source.run(&["history", "export", archive.to_str().unwrap()]);
    let original = archive_entries(&archive);
    for case in [
        "hash",
        "missing-object",
        "unsafe-document",
        "unknown-entry",
        "missing-latest",
        "duplicate",
    ] {
        let mut entries = original.clone();
        match case {
            "hash" => entries
                .iter_mut()
                .find(|(name, _)| name.starts_with("objects/"))
                .unwrap()
                .1
                .push(0),
            "missing-object" => {
                let n = entries
                    .iter()
                    .position(|(name, _)| name.starts_with("objects/"))
                    .unwrap();
                entries.remove(n);
            }
            "unsafe-document" => {
                let (_, bytes) = entries
                    .iter_mut()
                    .find(|(name, _)| name.starts_with("snapshots/"))
                    .unwrap();
                let mut value: Value = serde_json::from_slice(bytes).unwrap();
                value["files"][0]["path"] = "../escaped.md".into();
                *bytes = serde_json::to_vec(&value).unwrap();
            }
            "unknown-entry" => entries.push(("other/file".to_string(), vec![])),
            "missing-latest" => {
                entries
                    .iter_mut()
                    .find(|(name, _)| name == "refs/latest")
                    .unwrap()
                    .1 = b"missing-id".to_vec()
            }
            "duplicate" => entries.push(entries[0].clone()),
            _ => unreachable!(),
        }
        let path = source.root.join(format!("{}.tar.gz", case));
        make_archive(&path, &entries);
        let target = TestRepo::new(Some("flat"));
        let before = fs::read(target.root.join("docs/principles.md")).unwrap();
        let output = target
            .command(&["history", "import", path.to_str().unwrap()])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{}", case);
        assert!(!target.root.join(".harnesskit").exists(), "{}", case);
        assert_eq!(
            fs::read(target.root.join("docs/principles.md")).unwrap(),
            before
        );
    }
    let mut corrupt = fs::read(&archive).unwrap();
    *corrupt.last_mut().unwrap() ^= 1;
    let path = source.root.join("bad-trailer.tar.gz");
    fs::write(&path, corrupt).unwrap();
    let target = TestRepo::new(None);
    assert_eq!(
        target
            .command(&["history", "import", path.to_str().unwrap()])
            .output()
            .unwrap()
            .status
            .code(),
        Some(2)
    );
    assert!(!target.root.join(".harnesskit").exists());
}

#[test]
fn show_prints_exact_bytes_and_missing_paths_are_errors_without_restoring() {
    let repo = TestRepo::new(Some("flat"));
    write(&repo, "docs/raw.md", b"# Raw\n\xff\x00no final newline");
    snapshot(&repo);
    let original = fs::read(repo.root.join("docs/raw.md")).unwrap();
    write(&repo, "docs/raw.md", "# Current content\n");
    let output = repo
        .command(&["history", "show", "latest", "docs/raw.md"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, original);
    assert_eq!(
        fs::read(repo.root.join("docs/raw.md")).unwrap(),
        b"# Current content\n"
    );
    let missing = repo
        .command(&["history", "show", "latest", "missing.md"])
        .output()
        .unwrap();
    assert_eq!(missing.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&missing.stderr).contains("not in history snapshot"));
    let invalid = repo
        .command(&["history", "show", "../escape", "docs/raw.md"])
        .output()
        .unwrap();
    assert_eq!(invalid.status.code(), Some(2));
}
