mod support;

use std::fs::{self, File, FileTimes};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use support::TestRepo;

fn write(repo: &TestRepo, path: &str, text: &str) {
    let path = repo.root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn modified(repo: &TestRepo, path: &str, seconds: u64) {
    File::open(repo.root.join(path))
        .unwrap()
        .set_times(FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(seconds)))
        .unwrap();
}

fn report(repo: &TestRepo, args: &[&str]) -> Value {
    serde_json::from_str(&repo.run(args)).unwrap()
}

fn current(repo: &TestRepo) -> Value {
    report(repo, &["context", "--current", "--json"])
}

fn checks(repo: &TestRepo) -> Vec<Value> {
    let report = report(repo, &["check", "--rules", "lifecycle", "--json"]);
    let checks = report["checks"].as_array().unwrap().clone();
    for check in &checks {
        assert_eq!(check["severity"], "info");
        assert!(!check["hint"].as_str().unwrap().is_empty(), "{}", check);
        assert_eq!(check["hint"], check["evidence_json"]["hint"]);
    }
    checks
}

fn paths(docs: &Value) -> Vec<&str> {
    docs.as_array()
        .unwrap()
        .iter()
        .map(|doc| doc["path"].as_str().unwrap())
        .collect()
}

#[test]
fn lifecycle_fixture_defaults_to_short_current_routes_and_live_plans() {
    let repo = TestRepo::new(Some("lifecycle"));
    let packet = current(&repo);
    assert_eq!(report(&repo, &["context", "--json"]), packet);
    assert_eq!(packet["kind"], "current");
    assert_eq!(
        paths(&packet["entrypoints"]),
        ["AGENTS.md", "docs/index.md"]
    );
    assert_eq!(
        paths(&packet["active_plans"]),
        ["docs/exec-plans/active/relay-migration.md"]
    );
    assert_eq!(
        paths(&packet["recent_decisions"]),
        ["docs/decisions/0002-relay-routing.md"]
    );
    assert_eq!(packet["latest_snapshot"], Value::Null);
    assert_eq!(packet["changes_since_snapshot"], Value::Null);
    for field in ["entrypoints", "active_plans", "recent_decisions"] {
        assert!(packet[field]
            .as_array()
            .unwrap()
            .iter()
            .all(|doc| doc["lifecycle"] == "current"));
    }
    assert!(packet["coverage"].is_object());
    assert!(!packet["hints"].as_array().unwrap().is_empty());
    let text = repo.run(&["context"]);
    assert_eq!(text, repo.run(&["context", "--current"]));
    assert!(text.lines().count() < 40, "{}", text);
    assert!(!text.contains("completed/"));
    assert!(!text.contains("0001-relay-routing"));
    assert!(!text.contains("relay-transport-old"));
    assert!(!text.contains("Incoming refs"));
    let notices = checks(&repo);
    assert_eq!(notices.len(), 1, "{:?}", notices);
    assert_eq!(notices[0]["rule_id"], "active-but-historical");
    assert!(repo
        .run(&["check", "--rules", "lifecycle"])
        .contains("Hint:"));
}

#[test]
fn inferred_status_obeys_precedence_while_historical_conflicts_stay_out_of_current() {
    let repo = TestRepo::new(Some("lifecycle"));
    for (path, text) in [
        (
            "docs/exec-plans/active/ended.md",
            "---\nstatus: completed\n---\n# Ended\n\nFinished.\n",
        ),
        (
            "docs/exec-plans/active/banner.md",
            "# Banner\n\n> History only\n",
        ),
        ("docs/worklog/archive/trial.md", "# Trial\n\nEvidence.\n"),
        ("docs/design-docs/banner.md", "# Banner\n\n> Superseded\n"),
        (
            "docs/plans/loose.md",
            "# Loose plan\n\nCurrent execution.\n",
        ),
        (
            "docs/exec-plans/active/draft.md",
            "---\nstatus: draft\n---\n# Draft\n\nPending approval.\n",
        ),
    ] {
        write(&repo, path, text);
    }
    let indexed = report(&repo, &["index", "--json"]);
    for (path, status) in [
        ("docs/exec-plans/active/ended.md", "completed"),
        ("docs/exec-plans/active/banner.md", "active"),
        ("docs/exec-plans/completed/relay-migration.md", "completed"),
        ("docs/worklog/archive/trial.md", "archived"),
        ("docs/design-docs/banner.md", "superseded"),
        ("docs/plans/loose.md", "active"),
        ("docs/exec-plans/active/draft.md", "draft"),
        ("docs/design-docs/relay-transport-old.md", "active"),
    ] {
        let doc = indexed["docs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|doc| doc["path"] == path)
            .unwrap();
        assert_eq!(doc["status"], status, "{}", path);
    }
    let packet = current(&repo);
    let plans = paths(&packet["active_plans"]);
    assert_eq!(plans.len(), 2);
    assert!(plans.contains(&"docs/plans/loose.md"));
    assert!(plans.contains(&"docs/exec-plans/active/relay-migration.md"));
    // An inferred active status is not an explicit contradictory declaration.
    assert!(!checks(&repo)
        .iter()
        .any(|check| check["subject_path"] == "docs/exec-plans/active/banner.md"));
}

#[test]
fn reverse_and_partial_supersession_survive_incremental_edits() {
    let repo = TestRepo::new(Some("lifecycle"));
    let old = "docs/decisions/partial-base.md";
    let new = "docs/decisions/partial-new.md";
    write(
        &repo,
        old,
        "---\nstatus: active\n---\n# Partial beacon\n\nBeacon remains in force.\n",
    );
    write(&repo, new, &format!("---\nsupersedes_parts_of: [{old}]\n---\n# Replacement beacon\n\nChanges one section.\n"));
    let candidates = report(&repo, &["query", "beacon", "--json"]);
    let base = candidates["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|doc| doc["path"] == old)
        .unwrap();
    assert_eq!(base["lifecycle"], "current");
    assert!(base.get("superseded_by").is_none());
    let graph = report(&repo, &["graph", new, "--json"]);
    assert!(graph["outgoing"]
        .as_array()
        .unwrap()
        .iter()
        .any(
            |relation| relation["relation_type"] == "supersedes_parts_of"
                && relation["dst_path"] == old
        ));
    write(&repo, old, &format!("---\nstatus: active\nsuperseded_by:\n  - {new}\n---\n# Partial beacon\n\nReplaced in full.\n"));
    let candidates = report(&repo, &["query", "beacon", "--json"]);
    let base = candidates["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|doc| doc["path"] == old)
        .unwrap();
    assert_eq!(base["lifecycle"], "historical");
    assert_eq!(base["superseded_by"], json!([new]));
    let graph = report(&repo, &["context", old, "--json"]);
    assert_eq!(graph["anchor"]["superseded_by"], json!([new]));
    write(
        &repo,
        old,
        "---\nstatus: active\n---\n# Partial beacon\n\nCurrent again.\n",
    );
    let graph = report(&repo, &["context", old, "--json"]);
    assert_eq!(graph["anchor"]["lifecycle"], "current");
    assert!(graph["anchor"].get("superseded_by").is_none());
    fs::remove_file(repo.root.join(new)).unwrap();
    let graph = report(&repo, &["context", old, "--json"]);
    assert!(!graph["incoming"]
        .as_array()
        .unwrap()
        .iter()
        .any(|relation| relation["src_path"] == new));
}

#[test]
fn current_acceptance_counts_ignore_history_checked_items_and_fenced_examples() {
    let repo = TestRepo::new(Some("lifecycle"));
    write(
        &repo,
        "docs/project.md",
        "# Project map\n\nCurrent focus.\n\n- [ ] Review handoff\n",
    );
    write(&repo, "docs/exec-plans/active/relay-migration.md", "# Current migration\n\nLive plan.\n\n- [ ] Receipt recovery\n  * [ ] Restart\n1. [ ] Trace\n- [x] Compiles\n```md\n- [ ] Example\n```\n~~~md\n- [ ] Example\n~~~\n");
    write(
        &repo,
        "docs/worklog/archive/trial.md",
        "# Old trial\n\n- [ ] Obsolete acceptance\n",
    );
    write(
        &repo,
        "docs/design-docs/relay-transport-old.md",
        "---\nstatus: active\n---\n# Old design\n\n> 历史证据，已被取代\n\n- [ ] Obsolete trial\n",
    );
    let packet = current(&repo);
    assert_eq!(packet["project_map"]["path"], "docs/project.md");
    assert_eq!(packet["unverified_items"], 4);
    assert_eq!(packet["unverified_documents"].as_array().unwrap().len(), 2);
}

#[test]
fn current_packet_orders_by_mtime_and_keeps_text_below_forty_lines() {
    let repo = TestRepo::new(Some("lifecycle"));
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    for number in 0..20 {
        let path = format!("docs/exec-plans/active/plan-{:02}.md", number);
        write(&repo, &path, "# Additional plan\n\nCurrent execution.\n");
        modified(&repo, &path, now + number);
    }
    for number in 0..8 {
        let path = format!("docs/decisions/decision-{:02}.md", number);
        write(&repo, &path, "# Additional decision\n\nCurrent policy.\n");
        modified(&repo, &path, now + number);
    }
    let packet = current(&repo);
    assert_eq!(packet["active_plans"].as_array().unwrap().len(), 21);
    assert_eq!(
        packet["active_plans"][0]["path"],
        "docs/exec-plans/active/plan-19.md"
    );
    assert_eq!(
        packet["recent_decisions"][0]["path"],
        "docs/decisions/decision-07.md"
    );
    assert_eq!(packet["recent_decisions"].as_array().unwrap().len(), 5);
    let text = repo.run(&["context", "--current"]);
    assert!(text.lines().count() < 40, "{}", text);
    assert!(text.contains("13 more active plans; use --json"));
}

#[test]
fn snapshot_handoff_uses_content_and_reports_added_modified_removed_titles_and_summaries() {
    let repo = TestRepo::new(Some("lifecycle"));
    let snapshot = report(
        &repo,
        &["history", "snapshot", "-m", "handoff baseline", "--json"],
    );
    let id = snapshot["id"].as_str().unwrap();
    let path = "docs/exec-plans/active/relay-migration.md";
    let prior_mtime = fs::metadata(repo.root.join(path))
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    write(
        &repo,
        path,
        "# Receipt recovery\n\nVerify restart receipts.\n\n- [ ] Runtime acceptance\n",
    );
    modified(&repo, path, prior_mtime);
    write(
        &repo,
        "docs/new-note.MARKDOWN",
        "# Handoff note\n\nA new operational observation.\n",
    );
    fs::remove_file(repo.root.join("docs/references/evidence.md")).unwrap();
    let packet = report(&repo, &["context", "--since", id, "--json"]);
    assert_eq!(paths(&packet["added"]), ["docs/new-note.MARKDOWN"]);
    assert_eq!(paths(&packet["modified"]), [path]);
    assert_eq!(packet["removed"], json!(["docs/references/evidence.md"]));
    assert_eq!(packet["modified"][0]["title"], "Receipt recovery");
    assert_eq!(packet["modified"][0]["summary"], "Verify restart receipts.");
    assert_eq!(packet["snapshot"]["id"], id);
    let latest = report(&repo, &["context", "--since", "latest", "--json"]);
    for key in ["added", "modified", "removed", "snapshot"] {
        assert_eq!(packet[key], latest[key]);
    }
    let text = repo.run(&["context", "--since", "latest"]);
    assert!(text.contains("Receipt recovery | Verify restart receipts."));
    let current = current(&repo);
    assert_eq!(current["latest_snapshot"]["id"], id);
    assert_eq!(
        current["changes_since_snapshot"]["added"],
        json!(["docs/new-note.MARKDOWN"])
    );
    assert_eq!(current["changes_since_snapshot"]["modified"], json!([path]));
    assert_eq!(
        current["changes_since_snapshot"]["removed"],
        packet["removed"]
    );
    assert!(current["coverage"].is_object());
    assert!(latest["coverage"].is_object());
    assert!(!latest["hints"].as_array().unwrap().is_empty());
}

#[test]
fn date_handoff_includes_utc_boundary_and_explains_mtime_limits() {
    let repo = TestRepo::new(None);
    for (name, time) in [("before", 86399), ("boundary", 86400), ("after", 86401)] {
        let path = format!("docs/{}.md", name);
        write(&repo, &path, &format!("# {}\n\nObservation.\n", name));
        modified(&repo, &path, time);
    }
    let packet = report(&repo, &["context", "--since", "1970-01-02", "--json"]);
    assert_eq!(packet["cutoff_unix"], 86400);
    assert_eq!(
        paths(&packet["modified"]),
        ["docs/after.md", "docs/boundary.md"]
    );
    assert_eq!(packet["added"], json!([]));
    assert!(packet["hints"].as_array().unwrap().iter().any(|hint| hint
        .as_str()
        .unwrap()
        .contains("mtime cannot identify creation")));
    let future = report(&repo, &["context", "--since", "2099-01-01", "--json"]);
    assert_eq!(future["modified"], json!([]));
    for args in [
        vec!["context", "--current", "--since", "latest"],
        vec!["context", "--since", "2026-02-29"],
        vec!["context", "--since", "latest"],
        vec!["context", "--since", "missing-snapshot"],
    ] {
        assert_eq!(
            repo.command(&args).output().unwrap().status.code(),
            Some(2),
            "{:?}",
            args
        );
    }
}

#[test]
fn current_and_since_accept_target_directories_without_breaking_legacy_context() {
    let repo = TestRepo::new(Some("lifecycle"));
    let parent = TestRepo::new(None);
    let path = repo.root.to_str().unwrap();
    let output = parent.run(&["context", "--current", path, "--json"]);
    let packet: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(packet, current(&repo));
    let legacy = report(&parent, &["context", "relay migration", path, "--json"]);
    assert_eq!(legacy["kind"], "context");
    assert_eq!(
        legacy["anchor"]["path"],
        "docs/exec-plans/active/relay-migration.md"
    );
    repo.run(&["history", "snapshot", "-m", "target baseline"]);
    let since = report(&parent, &["context", "--since", "latest", path, "--json"]);
    assert_eq!(since["modified"], json!([]));
    let missing = parent.root.join("missing");
    let output = parent
        .command(&["context", "--current", missing.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(!missing.exists());
}

#[test]
fn old_schemas_allow_lifecycle_fields_and_state_rebuild_preserves_history() {
    let repo = TestRepo::new(Some("lifecycle"));
    let schema = include_str!("fixtures/compat/alpha6-schema.yaml");
    write(&repo, ".harnesskit/schema.yaml", schema);
    write(&repo, "docs/exec-plans/active/new.md", "---\nstatus: active\nsuperseded_by: []\nsupersedes_parts_of: [docs/exec-plans/completed/relay-migration.md]\n---\n# New plan\n\nCurrent rollout.\n");
    write(
        &repo,
        "docs/worklog/active/new.md",
        "---\nstatus: active\n---\n# Log\n\nRuntime evidence.\n",
    );
    let initial = report(
        &repo,
        &["history", "snapshot", "-m", "old schema", "--json"],
    );
    let snapshot_path = repo.root.join(format!(
        ".harnesskit/history/snapshots/{}.json",
        initial["id"].as_str().unwrap()
    ));
    let snapshot_bytes = fs::read(&snapshot_path).unwrap();
    repo.run(&["index"]);
    let before = current(&repo);
    let db = rusqlite::Connection::open(repo.root.join(".harnesskit/state/facts.sqlite")).unwrap();
    db.execute(
        "UPDATE meta SET value = '5' WHERE key = 'fact_schema_version'",
        [],
    )
    .unwrap();
    drop(db);
    assert_eq!(current(&repo), before);
    let fields = report(&repo, &["check", "--rules", "frontmatter", "--json"]);
    assert_eq!(fields["checks"], json!([]));
    assert_eq!(
        fs::read_to_string(repo.root.join(".harnesskit/schema.yaml")).unwrap(),
        schema
    );
    assert_eq!(fs::read(&snapshot_path).unwrap(), snapshot_bytes);
    assert!(repo.run(&["history", "list"]).contains("old schema"));
    assert!(repo
        .run(&["history", "diff", "latest"])
        .contains("Changed:\n- none"));
    repo.run(&["history", "restore", "latest", "--apply"]);
    let restored = current(&repo);
    assert_eq!(
        restored["changes_since_snapshot"],
        before["changes_since_snapshot"]
    );
    assert_eq!(restored["active_plans"].as_array().unwrap().len(), 2);
    assert!(restored["active_plans"]
        .as_array()
        .unwrap()
        .iter()
        .all(|doc| doc["status"] == "active"));
}

#[test]
fn entrypoint_length_uses_raw_lines_and_schema_threshold_roundtrips() {
    let repo = TestRepo::new(Some("lifecycle"));
    write(&repo, "AGENTS.md", &"Route\n".repeat(120));
    assert!(!checks(&repo)
        .iter()
        .any(|check| check["rule_id"] == "entrypoint-too-long"));
    write(&repo, "AGENTS.md", &"Route\n".repeat(121));
    assert_eq!(
        checks(&repo)
            .iter()
            .filter(|check| check["rule_id"] == "entrypoint-too-long")
            .count(),
        1
    );
    let schema = include_str!("fixtures/compat/alpha6-schema.yaml").replace(
        "  strict_checks: false",
        "  strict_checks: false\n  entrypoint_max_lines: 200\n  stale_active_plan_days: 60",
    );
    write(&repo, ".harnesskit/schema.yaml", &schema);
    assert!(!checks(&repo)
        .iter()
        .any(|check| check["rule_id"] == "entrypoint-too-long"));
    repo.run(&["doctor", "--layout", "--write-schema", "--force"]);
    let saved = fs::read_to_string(repo.root.join(".harnesskit/schema.yaml")).unwrap();
    assert!(saved.contains("entrypoint_max_lines: 200"));
    assert!(saved.contains("stale_active_plan_days: 60"));
    for invalid in ["0", "nope", "-1"] {
        write(
            &repo,
            ".harnesskit/schema.yaml",
            &schema.replace(
                "entrypoint_max_lines: 200",
                &format!("entrypoint_max_lines: {}", invalid),
            ),
        );
        assert_eq!(
            repo.command(&["check"]).output().unwrap().status.code(),
            Some(2)
        );
    }
}

#[test]
fn duplicate_current_pointers_require_live_labeled_links_in_distinct_documents() {
    let repo = TestRepo::new(Some("lifecycle"));
    let plan = "docs/exec-plans/active/relay-migration.md";
    write(
        &repo,
        "AGENTS.md",
        &format!("# Routes\n\nCurrent plan: [migration]({plan})\n"),
    );
    write(&repo, "docs/design-docs/examples.md", &format!("# Examples\n\nRead [plan](../exec-plans/active/relay-migration.md) for context.\n\n```md\nCurrent plan: `{plan}`\n```\n"));
    assert!(!checks(&repo)
        .iter()
        .any(|check| check["rule_id"] == "duplicated-current-pointer"));
    write(
        &repo,
        "docs/project.md",
        "# Project\n\n## 当前计划\n\n[迁移](exec-plans/active/relay-migration.md)\n",
    );
    let duplicated = checks(&repo);
    let repeated = duplicated
        .iter()
        .filter(|check| check["rule_id"] == "duplicated-current-pointer")
        .collect::<Vec<_>>();
    assert_eq!(repeated.len(), 1, "{:?}", repeated);
    assert!(repeated[0]["message"].as_str().unwrap().contains("3 files"));
    assert!(repeated[0]["message"]
        .as_str()
        .unwrap()
        .contains("AGENTS.md"));
    write(&repo, "docs/index.md", "# Docs\n");
    write(
        &repo,
        "docs/exec-plans/active/other.md",
        "# Other plan\n\nA concurrent operation.\n",
    );
    write(
        &repo,
        "docs/project.md",
        "# Project\n\nCurrent plan: [other](exec-plans/active/other.md)\n",
    );
    let conflicts = checks(&repo);
    let conflicts = conflicts
        .iter()
        .filter(|check| check["rule_id"] == "duplicated-current-pointer")
        .collect::<Vec<_>>();
    assert_eq!(conflicts.len(), 1);
    assert!(conflicts[0]["message"]
        .as_str()
        .unwrap()
        .contains("disagree"));
    write(
        &repo,
        "docs/exec-plans/active/other.md",
        "---\nstatus: completed\n---\n# Other plan\n\nFinished.\n",
    );
    assert!(!checks(&repo)
        .iter()
        .any(|check| check["rule_id"] == "duplicated-current-pointer"));
}

#[test]
fn superseded_not_marked_clears_after_explicit_status_update() {
    let repo = TestRepo::new(Some("lifecycle"));
    write(
        &repo,
        "docs/decisions/0001-relay-routing.md",
        "# Old routing\n\nOriginal route.\n",
    );
    let missing = checks(&repo);
    assert_eq!(
        missing
            .iter()
            .filter(|check| check["rule_id"] == "superseded-not-marked")
            .count(),
        1
    );
    write(&repo, "docs/decisions/0001-relay-routing.md", "---\nstatus: superseded\nsuperseded_by: docs/decisions/0002-relay-routing.md\n---\n# Old routing\n\nOriginal route.\n");
    assert!(!checks(&repo)
        .iter()
        .any(|check| check["rule_id"] == "superseded-not-marked"));
}

#[test]
fn stale_active_plan_uses_configurable_age_and_ignores_historical_plans() {
    let repo = TestRepo::new(Some("lifecycle"));
    let path = "docs/exec-plans/active/relay-migration.md";
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let old = now - 31 * 86400;
    modified(&repo, path, old);
    write(
        &repo,
        "docs/exec-plans/active/history.md",
        "---\nstatus: active\n---\n# Old plan\n\n> History only\n",
    );
    modified(&repo, "docs/exec-plans/active/history.md", old);
    modified(&repo, "docs/exec-plans/completed/relay-migration.md", old);
    let notices = checks(&repo);
    let stale = notices
        .iter()
        .filter(|check| check["rule_id"] == "stale-active-plan")
        .collect::<Vec<_>>();
    assert_eq!(stale.len(), 1);
    assert_eq!(stale[0]["subject_path"], path);
    let schema = include_str!("fixtures/compat/alpha6-schema.yaml").replace(
        "  strict_checks: false",
        "  strict_checks: false\n  stale_active_plan_days: 60",
    );
    write(&repo, ".harnesskit/schema.yaml", &schema);
    assert!(!checks(&repo)
        .iter()
        .any(|check| check["rule_id"] == "stale-active-plan"));
    write(
        &repo,
        ".harnesskit/schema.yaml",
        include_str!("fixtures/compat/alpha6-schema.yaml"),
    );
    modified(&repo, path, now);
    assert!(!checks(&repo)
        .iter()
        .any(|check| check["rule_id"] == "stale-active-plan"));
}

#[test]
fn repository_dogfood_has_no_new_lifecycle_noise() {
    let repo = TestRepo::new(None);
    for path in [
        "README.md",
        "ITERATION_PLAN.md",
        "agent-surfaces/README.md",
        "agent-surfaces/skills/harnesskit/SKILL.md",
    ] {
        write(
            &repo,
            path,
            &fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(path)).unwrap(),
        );
    }
    assert!(checks(&repo).is_empty());
    let packet = current(&repo);
    assert_eq!(paths(&packet["active_plans"]), ["ITERATION_PLAN.md"]);
    assert_eq!(packet["coverage"]["managed"], 4);
}
