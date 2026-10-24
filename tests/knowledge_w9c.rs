mod support;

use std::fs;

use rusqlite::Connection;
use serde_json::Value;
use support::TestRepo;

fn write(repo: &TestRepo, path: &str, text: &str) {
    let path = repo.root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn report(repo: &TestRepo) -> Value {
    serde_json::from_str(
        &repo.run(&["check", "--rules", "code-drift", "--json", "--all"]),
    )
    .unwrap()
}

#[test]
fn changelog_mentions_do_not_seal_while_decision_anchors_still_drift() {
    let repo = TestRepo::new(None);
    write(
        &repo,
        ".harnesskit/schema.yaml",
        &format!(
            "{}\ninclude:\n  'CHANGELOG.md': notes\n",
            include_str!("../schemas/file-first-v0.yaml")
        ),
    );
    write(
        &repo,
        "src/policy.rs",
        "pub fn policy(value: u64) -> u64 { value + 1 }\n",
    );
    write(
        &repo,
        "CHANGELOG.md",
        "# Changelog\n\nThe release mentions src/policy.rs#policy.\n",
    );
    write(
        &repo,
        "docs/decisions/policy.md",
        "---\nstatus: accepted\n---\n\n# Policy\n\nThe current rule is src/policy.rs#policy.\n",
    );

    repo.run(&["index"]);
    let db = Connection::open(repo.root.join(".harnesskit/state/facts.sqlite")).unwrap();
    let changelog_edge: String = db
        .query_row(
            "SELECT e.kind FROM edges e
             WHERE e.confidence = 'EXTRACTED'
               AND json_extract(e.evidence_json, '$.doc') = 'CHANGELOG.md'
             LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(changelog_edge, "mention");
    let lock = fs::read_to_string(repo.root.join(".harnesskit/knowledge.lock")).unwrap();
    assert!(!lock.contains("CHANGELOG.md"), "{lock}");
    assert!(lock.contains("docs/decisions/policy.md\tsrc/policy.rs#policy"));

    write(
        &repo,
        "src/policy.rs",
        "pub fn policy(value: u64) -> u64 { value + 2 }\n",
    );
    let checks = report(&repo)["checks"].as_array().unwrap().clone();
    assert!(checks.iter().any(|check| {
        check["rule_id"] == "code-drift"
            && check["subject_path"] == "docs/decisions/policy.md"
    }));
    assert!(!checks.iter().any(|check| {
        check["rule_id"] == "code-drift" && check["subject_path"] == "CHANGELOG.md"
    }));
}
