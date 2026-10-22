mod support;

use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use support::TestRepo;

fn index_json(repo: &TestRepo) -> Value {
    serde_json::from_slice(&fs::read(repo.root.join(".harnesskit/state/doc-index.json")).unwrap())
        .unwrap()
}

fn query_paths(repo: &TestRepo, query: &str) -> Vec<String> {
    let result: Value = serde_json::from_str(&repo.run(&["query", query, "--json"])).unwrap();
    assert_eq!(result["query"], query);
    result["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|candidate| candidate["path"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn standard_index_preserves_documents_metadata_and_relations() {
    let repo = TestRepo::new(Some("standard"));
    repo.run(&["index"]);
    assert!(repo.root.join(".harnesskit/state/facts.sqlite").is_file());
    let index = index_json(&repo);
    assert_eq!(index["managed_root"], "docs");
    let docs = index["docs"].as_array().unwrap();
    let paths = docs
        .iter()
        .map(|doc| doc["path"].as_str().unwrap())
        .collect::<BTreeSet<_>>();
    for path in [
        "AGENTS.md",
        "CLAUDE.md",
        "ARCHITECTURE.md",
        "docs/index.md",
        "docs/project.md",
        "docs/product-specs/delivery.md",
        "docs/design-docs/lantern.md",
        "docs/decisions/0001-queue.md",
        "docs/exec-plans/active/receipts.md",
        "docs/exec-plans/completed/prototype.md",
        "docs/worklog/active/receipt-trial.md",
        "docs/worklog/archive/prototype-trial.md",
        "docs/references/wire-protocol.md",
    ] {
        assert!(paths.contains(path), "missing {}", path);
    }
    assert!(!paths.contains("docs/templates/design-doc.md"));
    let decision = docs
        .iter()
        .find(|doc| doc["path"] == "docs/decisions/0001-queue.md")
        .unwrap();
    assert_eq!(decision["title"], "Decision: Durable Queue");
    assert_eq!(decision["status"], "active");
    assert_eq!(decision["authority"], "canonical");
    assert!(index["relations"]
        .as_array()
        .unwrap()
        .iter()
        .any(|relation| {
            relation["src_path"] == "docs/design-docs/lantern.md"
                && relation["dst_path"] == "docs/decisions/0001-queue.md"
                && relation["relation_type"] == "doc_links_doc"
        }));

    repo.run(&["index"]);
    assert_eq!(index_json(&repo), index);
    assert!(query_paths(&repo, "lantern").contains(&"docs/design-docs/lantern.md".to_string()));
    let context: Value =
        serde_json::from_str(&repo.run(&["context", "docs/design-docs/lantern.md", "--json"]))
            .unwrap();
    assert_eq!(context["anchor"]["path"], "docs/design-docs/lantern.md");
}

#[test]
fn index_refresh_handles_added_changed_and_removed_docs() {
    let repo = TestRepo::new(Some("standard"));
    repo.run(&["index"]);
    let unchanged = fs::read(repo.root.join("docs/project.md")).unwrap();
    fs::write(
        repo.root.join("docs/design-docs/lantern.md"),
        "# Lantern Relay Design\n\nZephyrcache now retains each courier message.\n",
    )
    .unwrap();
    fs::write(
        repo.root.join("docs/design-docs/backup.md"),
        "# Backup Relay\n\nNimbusvault retains backup parcels.\n",
    )
    .unwrap();
    fs::remove_file(repo.root.join("docs/references/wire-protocol.md")).unwrap();
    repo.run(&["index"]);

    assert_eq!(
        query_paths(&repo, "zephyrcache"),
        ["docs/design-docs/lantern.md"]
    );
    assert_eq!(
        query_paths(&repo, "nimbusvault"),
        ["docs/design-docs/backup.md"]
    );
    let listed = repo.run(&["list-docs"]);
    assert!(!listed.contains("- docs/references/wire-protocol.md ["));
    assert!(listed.contains("- docs/project.md ["));
    assert_eq!(
        fs::read(repo.root.join("docs/project.md")).unwrap(),
        unchanged
    );
}

#[test]
fn derived_state_can_be_rebuilt_without_touching_history_or_schema() {
    let repo = TestRepo::new(Some("standard"));
    let old_schema =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/compat/alpha6-schema.yaml");
    fs::create_dir_all(repo.root.join(".harnesskit")).unwrap();
    fs::copy(old_schema, repo.root.join(".harnesskit/schema.yaml")).unwrap();
    let schema = fs::read(repo.root.join(".harnesskit/schema.yaml")).unwrap();
    repo.run(&["history", "snapshot", "-m", "before state rebuild"]);
    let snapshot = fs::read_to_string(repo.root.join(".harnesskit/history/refs/latest")).unwrap();
    let snapshot_path = repo
        .root
        .join(format!(".harnesskit/history/snapshots/{}.json", snapshot));
    let snapshot_bytes = fs::read(&snapshot_path).unwrap();
    repo.run(&["index"]);
    let before = query_paths(&repo, "lantern");

    fs::remove_dir_all(repo.root.join(".harnesskit/state")).unwrap();
    repo.run(&["index"]);
    assert_eq!(query_paths(&repo, "lantern"), before);
    assert_eq!(fs::read(snapshot_path).unwrap(), snapshot_bytes);
    assert_eq!(
        fs::read(repo.root.join(".harnesskit/schema.yaml")).unwrap(),
        schema
    );
    assert!(repo.run(&["history", "status"]).contains("dirty=no"));
}

#[test]
fn incompatible_fact_store_is_automatically_rebuilt() {
    let repo = TestRepo::new(Some("standard"));
    repo.run(&["index"]);
    let before = query_paths(&repo, "lantern");
    let database = repo.root.join(".harnesskit/state/facts.sqlite");
    let db = rusqlite::Connection::open(&database).unwrap();
    db.execute_batch(
        "UPDATE meta SET value = 'legacy' WHERE key = 'fact_schema_version'; DROP TABLE raw_links;",
    )
    .unwrap();
    drop(db);

    repo.run(&["index"]);
    assert_eq!(query_paths(&repo, "lantern"), before);
    assert!(repo.run(&["list-docs"]).contains("- docs/project.md ["));
    let db = rusqlite::Connection::open(database).unwrap();
    let restored: usize = db
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'raw_links';",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(restored, 1);
}
