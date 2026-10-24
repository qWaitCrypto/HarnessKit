mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;
use serde_json::Value;
use support::{assert_success, TestRepo};

fn file_contents(root: &Path, dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut files = BTreeMap::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files.extend(file_contents(root, &path));
        } else {
            files.insert(
                path.strip_prefix(root)
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .replace('\\', "/"),
                fs::read(path).unwrap(),
            );
        }
    }
    files
}

#[test]
fn preview_does_not_create_target_or_write_parent_files() {
    let repo = TestRepo::new(None);
    let target = repo.root.join("preview-target");
    let output = repo.run(&["init", target.to_str().unwrap(), "--preview"]);

    assert!(output.contains("Preview only: no files were written."));
    assert!(!target.exists());
    assert!(file_contents(&repo.root, &repo.root).is_empty());
}

#[test]
fn init_preserves_existing_fixture_and_is_idempotent_until_forced() {
    let repo = TestRepo::new(Some("standard"));
    let original = file_contents(&repo.root, &repo.root);

    repo.run(&["init", "--tracked"]);
    for (path, bytes) in &original {
        assert_eq!(&fs::read(repo.root.join(path)).unwrap(), bytes, "{}", path);
    }
    for path in [
        ".harnesskit/schema.yaml",
        "docs/templates/product-spec.md",
        "docs/templates/design-doc.md",
        "docs/templates/decision.md",
        "docs/templates/exec-plan.md",
        "docs/templates/worklog.md",
        "docs/templates/reference.md",
    ] {
        assert!(repo.root.join(path).is_file(), "missing {}", path);
    }
    for path in [
        ".harnesskit/state",
        ".harnesskit/history/objects",
        ".harnesskit/history/snapshots",
        ".harnesskit/history/refs",
    ] {
        assert!(repo.root.join(path).is_dir(), "missing {}", path);
    }

    let initialized = file_contents(&repo.root, &repo.root);
    repo.run(&["init", "--tracked"]);
    assert_eq!(file_contents(&repo.root, &repo.root), initialized);

    repo.run(&["init", "--tracked", "--force"]);
    assert_ne!(
        fs::read(repo.root.join("AGENTS.md")).unwrap(),
        original["AGENTS.md"]
    );
    assert_eq!(
        fs::read(repo.root.join("docs/design-docs/lantern.md")).unwrap(),
        original["docs/design-docs/lantern.md"]
    );
}

#[test]
fn local_init_exclude_is_idempotent_and_tracked_init_keeps_it_unchanged() {
    let repo = TestRepo::new(None);
    let git = Command::new("git")
        .args(["init", "."])
        .current_dir(&repo.root)
        .output()
        .unwrap();
    assert_success(&git, &["git", "init", "."]);
    let exclude_path = repo.root.join(".git/info/exclude");
    let original_exclude = fs::read(&exclude_path).unwrap();

    repo.run(&["init", "--tracked"]);
    assert_eq!(fs::read(&exclude_path).unwrap(), original_exclude);

    repo.run(&["init", "--local"]);
    let local_exclude = fs::read_to_string(&exclude_path).unwrap();
    for path in [
        "AGENTS.md",
        "CLAUDE.md",
        "ARCHITECTURE.md",
        "docs/",
        ".harnesskit/",
    ] {
        assert!(local_exclude.lines().any(|line| line == path), "{}", path);
    }
    repo.run(&["init", "--local"]);
    assert_eq!(fs::read_to_string(&exclude_path).unwrap(), local_exclude);
    repo.run(&["init", "--tracked"]);
    assert_eq!(fs::read_to_string(&exclude_path).unwrap(), local_exclude);
}

#[test]
fn docs_root_override_survives_project_schema_reload() {
    let repo = TestRepo::new(None);
    repo.run(&["init", "--tracked", "--docs-root", "knowledge"]);
    assert!(!repo.root.join("docs").exists());
    assert!(repo.root.join("knowledge/project.md").is_file());
    assert!(repo
        .root
        .join("knowledge/exec-plans/active/index.md")
        .is_file());

    repo.run(&["index"]);
    let listed = repo.run(&["list-docs"]);
    assert!(listed.contains("knowledge/project.md"));
    assert!(listed.contains("knowledge/design-docs/index.md"));
    let snapshot = repo.run(&["history", "snapshot", "-m", "custom docs root"]);
    assert!(snapshot.contains("Created HarnessKit history snapshot"));
    assert!(repo.run(&["history", "status"]).contains("dirty=no"));
}

#[test]
fn collection_indexes_are_short_manifests_without_repeated_workflow_boilerplate() {
    let repo = TestRepo::new(None);
    repo.run(&["init", "--tracked"]);
    let decision_index = fs::read_to_string(repo.root.join("docs/decisions/index.md")).unwrap();
    assert_eq!(decision_index, include_str!("golden/init-decisions-index.snap"));
    let schema: Value = serde_yaml::from_str(&fs::read_to_string(repo.root.join(".harnesskit/schema.yaml")).unwrap()).unwrap();
    let mut purposes = std::collections::BTreeSet::new();
    for collection in schema["doc_collections"].as_object().unwrap().values() {
        let index = fs::read_to_string(repo.root.join("docs").join(collection["path"].as_str().unwrap()).join("index.md")).unwrap();
        let nonempty = index.lines().filter(|line| !line.is_empty()).collect::<Vec<_>>();
        assert_eq!(nonempty.len(), 3, "{index}");
        assert!(nonempty[0].starts_with("# ") && nonempty[2].starts_with("- "));
        assert!(purposes.insert(nonempty[1].to_string()), "{index}");
    }
    let rules = fs::read_to_string(repo.root.join("docs/DOCUMENTATION_SYSTEM.md")).unwrap();
    assert!(rules.contains("keep it narrow and task-scoped"));
}
