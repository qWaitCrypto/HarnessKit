mod support;

use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use support::{copy_tree, TestRepo};

fn latest_snapshot(repo: &TestRepo) -> Value {
    let id = fs::read_to_string(repo.root.join(".harnesskit/history/refs/latest")).unwrap();
    serde_json::from_slice(
        &fs::read(
            repo.root
                .join(format!(".harnesskit/history/snapshots/{}.json", id)),
        )
        .unwrap(),
    )
    .unwrap()
}

#[test]
fn snapshot_list_diff_and_restore_roundtrip_through_cli() {
    let repo = TestRepo::new(Some("standard"));
    repo.run(&["init", "--tracked"]);
    assert_eq!(
        repo.run(&["history", "list"]),
        "No HarnessKit history snapshots found.\n"
    );
    let message = "baseline \"quoted\" \\ route\n合成记录";
    repo.run(&["history", "snapshot", "-m", message]);
    let first = latest_snapshot(&repo);
    let first_id = first["id"].as_str().unwrap();
    assert_eq!(first["message"], message);
    assert_eq!(first["docs_root"], "docs");
    let original = first["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| {
            let path = file["path"].as_str().unwrap();
            (path.to_string(), fs::read(repo.root.join(path)).unwrap())
        })
        .collect::<BTreeMap<_, _>>();
    assert!(original.contains_key(".harnesskit/schema.yaml"));
    assert!(!original.contains_key("docs/templates/design-doc.md"));
    assert!(repo.run(&["history", "status"]).contains("dirty=no"));

    let changed_path = "docs/design-docs/lantern.md";
    let added_path = "docs/references/added.md";
    let removed_path = "docs/decisions/0001-queue.md";
    let changed = b"# Lantern Revision\n\nA synthetic changed relay.\n";
    fs::write(repo.root.join(changed_path), changed).unwrap();
    fs::write(
        repo.root.join(added_path),
        "# Added Reference\n\nSynthetic evidence.\n",
    )
    .unwrap();
    fs::remove_file(repo.root.join(removed_path)).unwrap();
    let schema_path = repo.root.join(".harnesskit/schema.yaml");
    let mut changed_schema = fs::read(&schema_path).unwrap();
    changed_schema.extend_from_slice(b"\n# Synthetic schema revision.\n");
    fs::write(&schema_path, &changed_schema).unwrap();

    let expected_diff = format!(
        "Added:\n- {}\nChanged:\n- .harnesskit/schema.yaml\n- {}\nRemoved:\n- {}\n",
        added_path, changed_path, removed_path
    );
    assert_eq!(repo.run(&["history", "diff", first_id]), expected_diff);
    assert_eq!(repo.run(&["history", "diff", "latest"]), expected_diff);

    let preview = repo.run(&["history", "restore", first_id]);
    assert!(preview.contains("Preview only."));
    assert_eq!(fs::read(repo.root.join(changed_path)).unwrap(), changed);
    assert_eq!(fs::read(&schema_path).unwrap(), changed_schema);
    assert!(repo.root.join(added_path).is_file());
    assert!(!repo.root.join(removed_path).exists());
    let rejected = repo
        .command(&["history", "restore", first_id, "--apply"])
        .output()
        .unwrap();
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("use --force"));
    assert_eq!(fs::read(repo.root.join(changed_path)).unwrap(), changed);
    assert_eq!(fs::read(&schema_path).unwrap(), changed_schema);
    assert!(repo.root.join(added_path).is_file());
    assert!(!repo.root.join(removed_path).exists());

    repo.run(&["history", "snapshot", "--message", "revised route"]);
    let second = latest_snapshot(&repo);
    let second_id = second["id"].as_str().unwrap();
    assert_ne!(first_id, second_id);
    let listed = repo.run(&["history", "list"]);
    assert!(listed.contains(first_id));
    assert!(listed.contains(second_id));
    assert!(listed.contains(message));
    assert!(listed.contains("revised route"));
    assert_eq!(
        repo.run(&["history", "diff", first_id, second_id]),
        expected_diff
    );

    repo.run(&["history", "restore", first_id, "--apply"]);
    for (path, bytes) in original {
        assert_eq!(fs::read(repo.root.join(&path)).unwrap(), bytes, "{}", path);
    }
    assert!(!repo.root.join(added_path).exists());
    repo.run(&["history", "restore", "latest", "--apply", "--force"]);
    assert_eq!(fs::read(repo.root.join(changed_path)).unwrap(), changed);
    assert_eq!(fs::read(schema_path).unwrap(), changed_schema);
    assert!(repo.root.join(added_path).is_file());
    assert!(!repo.root.join(removed_path).exists());
    assert!(repo.run(&["history", "status"]).contains("dirty=no"));
    assert_eq!(
        repo.run(&["history", "diff", "latest"]),
        "Added:\n- none\nChanged:\n- none\nRemoved:\n- none\n"
    );
}

#[test]
fn missing_history_object_rejects_restore_before_writing_any_files() {
    let repo = TestRepo::new(Some("standard"));
    repo.run(&["history", "snapshot", "-m", "preflight"]);
    let snapshot = latest_snapshot(&repo);
    let last = snapshot["files"].as_array().unwrap().last().unwrap();
    let hash = last["hash"].as_str().unwrap();
    fs::remove_file(
        repo.root
            .join(".harnesskit/history/objects")
            .join(&hash[..2])
            .join(&hash[2..]),
    )
    .unwrap();
    let changed = b"# Changed Agent Routes\n\nDo not overwrite before preflight completes.\n";
    fs::write(repo.root.join("AGENTS.md"), changed).unwrap();

    let rejected = repo
        .command(&["history", "restore", "latest", "--apply", "--force"])
        .output()
        .unwrap();
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("missing history object"));
    assert_eq!(fs::read(repo.root.join("AGENTS.md")).unwrap(), changed);
}

#[test]
fn frozen_alpha6_history_can_be_listed_diffed_and_restored() {
    let repo = TestRepo::new(None);
    copy_tree(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/compat/alpha6-history"),
        &repo.root.join(".harnesskit/history"),
    );
    let snapshot = latest_snapshot(&repo);
    let id = snapshot["id"].as_str().unwrap();
    let listed = repo.run(&["history", "list"]);
    assert!(listed.contains(id));
    assert!(listed.contains("alpha.6 \"legacy\" snapshot: 合成记录"));
    let removed = "Added:\n- none\nChanged:\n- none\nRemoved:\n- docs/design-docs/legacy-relay.md\n- docs/index.md\n";
    assert_eq!(repo.run(&["history", "diff", id]), removed);
    let preview = repo.run(&["history", "restore", "latest"]);
    assert!(preview.contains("Preview only."));
    assert!(!repo.root.join("docs").exists());

    repo.run(&["history", "restore", id, "--apply", "--force"]);
    for file in snapshot["files"].as_array().unwrap() {
        let hash = file["hash"].as_str().unwrap();
        let object = repo
            .root
            .join(".harnesskit/history/objects")
            .join(&hash[..2])
            .join(&hash[2..]);
        assert_eq!(
            fs::read(repo.root.join(file["path"].as_str().unwrap())).unwrap(),
            fs::read(object).unwrap()
        );
    }
    assert_eq!(
        fs::read_to_string(repo.root.join("docs/design-docs/legacy-relay.md")).unwrap(),
        "# Legacy Relay\n\n旧版合成记录：保留 acknowledgement 与恢复边界。\n"
    );
    assert!(repo.run(&["history", "status"]).contains("dirty=no"));
}

#[test]
fn external_evidence_is_not_snapshotted_or_overwritten_by_restore() {
    let repo = TestRepo::new(Some("external"));
    repo.run(&["history", "snapshot", "-m", "external pointer"]);
    let snapshot = latest_snapshot(&repo);
    assert!(snapshot["files"]
        .as_array()
        .unwrap()
        .iter()
        .all(|file| { !file["path"].as_str().unwrap().starts_with("research_runs/") }));
    let external_path = repo.root.join("research_runs/x/result.json");
    let revised_evidence = b"{\"probe\":\"quartz\",\"samples\":[99]}\n";
    fs::write(&external_path, revised_evidence).unwrap();
    assert!(repo.run(&["history", "status"]).contains("dirty=no"));
    repo.run(&["history", "restore", "latest", "--apply"]);
    assert_eq!(fs::read(external_path).unwrap(), revised_evidence);
}
