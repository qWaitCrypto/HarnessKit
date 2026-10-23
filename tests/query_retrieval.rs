mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde_json::{json, Value};
use support::TestRepo;

fn write(repo: &TestRepo, path: &str, text: &str) {
    let path = repo.root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn query(repo: &TestRepo, terms: &str) -> Value {
    serde_json::from_str(&repo.run(&["query", terms, "--json"])).unwrap()
}

fn paths(report: &Value) -> Vec<&str> {
    report["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|doc| doc["path"].as_str().unwrap())
        .collect()
}

#[test]
fn frozen_query_metrics_improve_on_s4_without_per_query_regressions() {
    let corpus: Vec<Value> = serde_json::from_str(include_str!("fixtures/queries.json")).unwrap();
    let baseline: Value =
        serde_json::from_str(include_str!("../scripts/query-baseline-s4.json")).unwrap();
    assert_eq!(corpus.len(), baseline["cases"].as_array().unwrap().len());
    let fixtures = ["standard", "flat", "cjk", "lifecycle", "external"];
    let repos = fixtures
        .iter()
        .map(|name| (*name, TestRepo::new(Some(name))))
        .collect::<BTreeMap<_, _>>();
    let (mut zero, mut top1, mut top3) = (0, 0, 0);
    let (mut cjk_count, mut cjk_zero, mut cjk_top1, mut cjk_top3) = (0, 0, 0, 0);
    for (case, before) in corpus.iter().zip(baseline["cases"].as_array().unwrap()) {
        assert_eq!(case["fixture"], before["fixture"]);
        assert_eq!(case["query"], before["query"]);
        let report = query(
            &repos[case["fixture"].as_str().unwrap()],
            case["query"].as_str().unwrap(),
        );
        let paths = paths(&report);
        let rank = paths
            .iter()
            .position(|path| {
                case["expected_paths"]
                    .as_array()
                    .unwrap()
                    .contains(&json!(path))
            })
            .map(|index| index + 1);
        let hit1 = usize::from(rank == Some(1));
        let hit3 = usize::from(rank.map(|rank| rank <= 3).unwrap_or(false));
        zero += usize::from(paths.is_empty());
        top1 += hit1;
        top3 += hit3;
        if let Some(old_rank) = before["rank"].as_u64() {
            assert!(
                rank.map(|rank| rank <= old_rank as usize).unwrap_or(false),
                "regression: {} -> {:?} {:?}",
                case["query"],
                rank,
                paths
            );
        }
        if let Some(forbidden) = case["not_first_paths"].as_array() {
            assert!(
                !paths
                    .first()
                    .map(|path| forbidden.contains(&json!(path)))
                    .unwrap_or(false),
                "historical result ranked first: {}",
                case["query"]
            );
        }
        if matches!(case["category"].as_str(), Some("cjk" | "mixed")) {
            cjk_count += 1;
            cjk_zero += usize::from(paths.is_empty());
            cjk_top1 += hit1;
            cjk_top3 += hit3;
        }
    }
    eprintln!("S4: zero={}/{} (40%), top1={}/{} (54.29%), top3={}/{} (60%); CJK: zero=7/7, top1=0/7, top3=0/7",
        baseline["metrics"]["zero_hits"], corpus.len(), baseline["metrics"]["top1_hits"], corpus.len(), baseline["metrics"]["top3_hits"], corpus.len());
    eprintln!("S5: zero={}/{} ({:.2}%), top1={}/{} ({:.2}%), top3={}/{} ({:.2}%); CJK: zero={}/{}, top1={}/{}, top3={}/{}",
        zero, corpus.len(), zero as f64 * 100.0 / corpus.len() as f64,
        top1, corpus.len(), top1 as f64 * 100.0 / corpus.len() as f64,
        top3, corpus.len(), top3 as f64 * 100.0 / corpus.len() as f64,
        cjk_zero, cjk_count, cjk_top1, cjk_count, cjk_top3, cjk_count);
    // Actual S5 measurements are the floor, not just an aggregate improvement.
    assert_eq!(zero, 0);
    assert!(top1 as f64 / corpus.len() as f64 >= 34.0 / 35.0);
    assert_eq!(top3, corpus.len());
    assert_eq!((cjk_zero, cjk_top1, cjk_top3), (0, cjk_count, cjk_count));
    assert!(top1 >= baseline["metrics"]["top1_hits"].as_u64().unwrap() as usize + 7);
    assert!(top3 >= baseline["metrics"]["top3_hits"].as_u64().unwrap() as usize + 7);

    let mut total_bytes = 0;
    let mut total_doc_index_bytes = 0;
    for (fixture, repo) in repos {
        let database = repo.root.join(".harnesskit/state/facts.sqlite");
        let db = rusqlite::Connection::open(&database).unwrap();
        db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .unwrap();
        let bytes = fs::metadata(database).unwrap().len();
        // The S5 budget bounds document/trigram storage. Weave adds a separate
        // graph; count its actual SQLite pages without relaxing the old gate.
        let weave_bytes: u64 = db
            .query_row(
                "SELECT COALESCE(SUM(pgsize), 0) FROM dbstat
             WHERE name IN ('nodes', 'idx_nodes_path', 'edges', 'idx_edges_dst',
                            'cochange_stats', 'idx_cochange_stats_doc', 'cochange_files',
                            'inline_code_spans', 'doc_sections',
                            'sqlite_autoindex_doc_sections_1', 'sqlite_autoindex_code_cards_1')
                OR name LIKE 'code_cards' OR name LIKE 'code_fts_%' OR name LIKE 'code_trigram_%'
                OR name LIKE 'sections_fts_%' OR name LIKE 'sections_trigram_%';",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let doc_index_bytes = bytes.checked_sub(weave_bytes).unwrap();
        total_bytes += bytes;
        total_doc_index_bytes += doc_index_bytes;
        eprintln!(
            "index {}: S4={} bytes -> current={} bytes (document index={}, Weave={})",
            fixture, baseline["index_sizes"][fixture]["bytes"], bytes, doc_index_bytes, weave_bytes
        );
        let duplicated_content: usize = db
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = 'docs_trigram_content';",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            duplicated_content, 0,
            "trigram must share the existing content store"
        );
    }
    let baseline_bytes = baseline["total_index_bytes"].as_u64().unwrap();
    eprintln!(
        "document index total: {} -> {} bytes ({:+.2}%); including Weave: {} bytes",
        baseline_bytes,
        total_doc_index_bytes,
        (total_doc_index_bytes as f64 / baseline_bytes as f64 - 1.0) * 100.0,
        total_bytes,
    );
    assert!(
        total_doc_index_bytes * 100 <= baseline_bytes * 135,
        "trigram index grew beyond the measured fixture budget"
    );
}

#[test]
fn strict_threshold_precedes_limit_and_relaxed_matches_explain_missing_terms() {
    let repo = TestRepo::new(None);
    for name in ["a", "b"] {
        write(
            &repo,
            &format!("docs/{}.md", name),
            "# Matching\n\nAlpha beta share a document.\n",
        );
    }
    write(&repo, "docs/partial.md", "# Partial\n\nAlpha alone.\n");
    let report = query(&repo, "ALPHA alpha beta");
    assert_eq!(paths(&report).len(), 3);
    for doc in report["candidates"].as_array().unwrap().iter().take(2) {
        assert_eq!(
            doc["match"],
            json!({"matched_terms": ["alpha", "beta"], "missing_terms": [], "mode": "strict"})
        );
    }
    assert_eq!(
        report["candidates"][2]["match"],
        json!({"matched_terms": ["alpha"], "missing_terms": ["beta"], "mode": "relaxed"})
    );
    write(
        &repo,
        "docs/c.md",
        "# Matching\n\nAlpha beta share a document.\n",
    );
    assert_eq!(
        paths(&query(&repo, "alpha beta")),
        ["docs/a.md", "docs/b.md", "docs/c.md"]
    );
    let limited: Value =
        serde_json::from_str(&repo.run(&["query", "alpha beta", "--limit", "1", "--json"]))
            .unwrap();
    assert_eq!(paths(&limited), ["docs/a.md"]);
    assert_eq!(limited["candidates"][0]["match"]["mode"], "strict");
}

#[test]
fn limit_defaults_to_eight_and_rejects_invalid_values() {
    let repo = TestRepo::new(None);
    for index in 0..12 {
        write(
            &repo,
            &format!("docs/sample-{}.md", index),
            "# Sample\n\nShared sample.\n",
        );
    }
    assert_eq!(paths(&query(&repo, "sample")).len(), 8);
    for (limit, expected) in [("1", 1), ("10", 10), ("99", 12)] {
        let result: Value = serde_json::from_str(&repo.run(&[
            "query",
            "sample",
            repo.root.to_str().unwrap(),
            "--limit",
            limit,
            "--json",
        ]))
        .unwrap();
        assert_eq!(paths(&result).len(), expected);
    }
    for limit in ["0", "-1", "not-a-number"] {
        assert_eq!(
            repo.command(&["query", "sample", "--limit", limit])
                .output()
                .unwrap()
                .status
                .code(),
            Some(2)
        );
    }
}

#[test]
fn field_weights_prefer_title_then_path_heading_summary_body() {
    let repo = TestRepo::new(None);
    for (path, title, heading, summary, body) in [
        (
            "docs/title.md",
            "zebraquartz",
            "ordinary",
            "ordinary",
            "ordinary",
        ),
        (
            "docs/zebraquartz.md",
            "ordinary",
            "ordinary",
            "ordinary",
            "ordinary",
        ),
        (
            "docs/heading.md",
            "ordinary",
            "zebraquartz",
            "ordinary",
            "ordinary",
        ),
        (
            "docs/summary.md",
            "ordinary",
            "ordinary",
            "zebraquartz",
            "ordinary",
        ),
        (
            "docs/body.md",
            "ordinary",
            "ordinary",
            "ordinary",
            "zebraquartz",
        ),
    ] {
        write(
            &repo,
            path,
            &format!(
                "# {}\n\n{} introduction.\n\n## {} section\n\n{} conclusion.\n",
                title, summary, heading, body
            ),
        );
    }
    assert_eq!(
        paths(&query(&repo, "zebraquartz")),
        [
            "docs/title.md",
            "docs/zebraquartz.md",
            "docs/heading.md",
            "docs/summary.md",
            "docs/body.md"
        ]
    );
}

#[test]
fn cjk_short_terms_substrings_and_identifiers_share_matching_rules() {
    let repo = TestRepo::new(Some("cjk"));
    for terms in [
        "精灵蛋",
        "玩家获得精灵蛋",
        "三天",
        "孵",
        "EV-01",
        "Egg 精灵蛋",
        "incub",
    ] {
        assert!(
            paths(&query(&repo, terms)).contains(&"docs/product-specs/egg-incubation.md"),
            "{}",
            terms
        );
    }
    write(
        &repo,
        "docs/multilingual.md",
        "# Other scripts\n\nカタカナの説明と한글규칙테스트。\n",
    );
    for term in ["カタ", "한글", "규칙테스트"] {
        assert_eq!(paths(&query(&repo, term)), ["docs/multilingual.md"]);
    }
    let context: Value = serde_json::from_str(&repo.run(&["context", "精灵蛋", "--json"])).unwrap();
    assert_eq!(
        context["anchor"]["path"],
        "docs/product-specs/egg-incubation.md"
    );
    for terms in ["OR NOT", "\" AND (", "' ; DROP TABLE docs --"] {
        query(&repo, terms);
    }
    assert!(!paths(&query(&repo, "精灵蛋")).is_empty());
}

#[test]
fn historical_results_keep_status_and_explain_supersession() {
    let repo = TestRepo::new(Some("lifecycle"));
    for (term, current, old) in [
        (
            "relay routing",
            "docs/decisions/0002-relay-routing.md",
            "docs/decisions/0001-relay-routing.md",
        ),
        (
            "relay migration",
            "docs/exec-plans/active/relay-migration.md",
            "docs/exec-plans/completed/relay-migration.md",
        ),
        (
            "relay transport",
            "docs/design-docs/relay-transport.md",
            "docs/design-docs/relay-transport-old.md",
        ),
    ] {
        let report = query(&repo, term);
        assert_eq!(report["candidates"][0]["path"], current);
        assert_eq!(report["candidates"][0]["lifecycle"], "current");
        let historical = report["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .find(|doc| doc["path"] == old)
            .unwrap();
        assert_eq!(historical["lifecycle"], "historical");
        if term == "relay routing" {
            assert_eq!(historical["superseded_by"], json!([current]));
        }
        if term == "relay transport" {
            assert_eq!(historical["status"], "active");
        }
    }
}

#[test]
fn first_paragraph_and_reverse_supersedes_mark_history_without_changing_frontmatter() {
    let repo = TestRepo::new(None);
    for (index, banner) in [
        "History only",
        "不再适用",
        "历史证据",
        "已被取代",
        "Superseded",
    ]
    .iter()
    .enumerate()
    {
        write(
            &repo,
            &format!("docs/banner{}.md", index),
            &format!(
                "---\nstatus: active\n---\n\n# Banner\n\n> {}\n\nBeacon evidence.\n",
                banner
            ),
        );
    }
    write(&repo, "docs/retired.md", "# Beacon\n\nOriginal evidence.\n");
    write(&repo, "docs/current.md", "---\nsupersedes: [docs/retired.md]\n---\n\n# Beacon\n\nCurrent evidence.\n\nHistorical notes in a later paragraph must not retire this page.\n");
    let report = query(&repo, "beacon");
    assert_eq!(report["candidates"][0]["path"], "docs/current.md");
    for doc in report["candidates"].as_array().unwrap().iter().skip(1) {
        assert_eq!(doc["lifecycle"], "historical");
        if doc["path"] == "docs/retired.md" {
            assert_eq!(doc["superseded_by"], json!(["docs/current.md"]));
        } else {
            assert_eq!(doc["status"], "active");
        }
    }
}

#[test]
fn zero_result_diagnostics_scan_only_unmanaged_files_and_cap_paths() {
    let repo = TestRepo::new(Some("standard"));
    for index in 0..8 {
        write(
            &repo,
            &format!("docs/unclassified/evidence{}.md", index),
            "# Evidence\n\nOutsidescope token.\n",
        );
    }
    write(&repo, "docs/templates/excluded.md", "# Outsidescope\n");
    let report = query(&repo, "outsidescope missingword");
    assert!(paths(&report).is_empty());
    assert!(report["coverage"].is_object());
    let diagnostics = &report["diagnostics"];
    assert_eq!(diagnostics["index_empty"], false);
    assert_eq!(diagnostics["target_dir_empty"], false);
    assert_eq!(diagnostics["target_dir"], repo.root.to_str().unwrap());
    assert_eq!(diagnostics["terms"][0]["managed_hits"], 0);
    assert_eq!(diagnostics["terms"][0]["unmanaged_hits"], 8);
    let outside = diagnostics["terms"][0]["unmanaged_paths"]
        .as_array()
        .unwrap();
    assert_eq!(outside.len(), 5);
    assert!(outside
        .iter()
        .all(|path| path.as_str().unwrap().starts_with("docs/unclassified/")));
    assert_eq!(diagnostics["terms"][1]["unmanaged_hits"], 0);
    let text = repo.run(&["query", "outsidescope missingword"]);
    assert!(text.contains("schema include"));
}

#[test]
fn empty_targets_punctuation_and_title_typos_produce_actionable_output() {
    let empty = TestRepo::new(None);
    let report = query(&empty, "absent");
    assert_eq!(report["diagnostics"]["target_dir_empty"], true);
    assert_eq!(report["diagnostics"]["index_empty"], true);
    assert_eq!(report["diagnostics"]["indexed_docs"], 0);
    assert!(report["hints"]
        .as_array()
        .unwrap()
        .iter()
        .any(|hint| hint.as_str().unwrap().contains("target_dir")));
    let repo = TestRepo::new(Some("standard"));
    let punctuation = query(&repo, "?! --- ::");
    assert!(paths(&punctuation).is_empty());
    assert_eq!(punctuation["diagnostics"]["query_empty"], true);
    assert_eq!(punctuation["diagnostics"]["terms"], json!([]));
    let typo = query(&repo, "lantarn");
    assert!(paths(&typo).is_empty());
    assert!(typo["did_you_mean"]
        .as_array()
        .unwrap()
        .iter()
        .any(|suggestion| suggestion["path"] == "docs/design-docs/lantern.md"));
    assert!(repo.run(&["query", "lantarn"]).contains("Did you mean"));
}

#[test]
fn snippets_use_matching_lines_including_headings_and_late_unicode_hits() {
    let repo = TestRepo::new(None);
    write(
        &repo,
        "docs/pathneedle.md",
        "---\nstatus: active\n---\n\n# Ordinary\n\nNothing relevant here.\n",
    );
    let path_only = query(&repo, "pathneedle");
    assert_eq!(path_only["candidates"][0]["snippet"], "");
    assert_eq!(path_only["candidates"][0]["snippet_location"], "");
    write(&repo, "docs/title.md", "# Titlemarker\n\nUnrelated body.\n");
    let title = query(&repo, "titlemarker");
    assert_eq!(title["candidates"][0]["snippet"], "# Titlemarker");
    assert_eq!(title["candidates"][0]["snippet_location"], "line 1");
    write(
        &repo,
        "docs/long.md",
        &format!(
            "# Long text\n\n{}精灵蛋需要照料。\n",
            "无关前缀".repeat(100)
        ),
    );
    let long = query(&repo, "精灵蛋");
    assert!(long["candidates"][0]["snippet"]
        .as_str()
        .unwrap()
        .contains("精灵蛋"));
    assert_eq!(long["candidates"][0]["snippet_location"], "line 3");
    write(
        &repo,
        "docs/case-expansion.md",
        &format!("# Unicode\n\n{} needleafterexpansion\n", "İ".repeat(300)),
    );
    let expanded = query(&repo, "needleafterexpansion");
    assert!(expanded["candidates"][0]["snippet"]
        .as_str()
        .unwrap()
        .contains("needleafterexpansion"));
    write(
        &repo,
        "docs/id.md",
        "# Ordinary\n\nSolid prose is unconnected.\n\nID 42 identifies the record.\n",
    );
    let short = query(&repo, "id");
    assert_eq!(
        short["candidates"][0]["snippet"],
        "ID 42 identifies the record."
    );
}

#[test]
fn neighbors_are_managed_focused_documents_limited_to_five() {
    let repo = TestRepo::new(Some("standard"));
    let mut text =
        "# Hubneedle\n\n[entry](../../AGENTS.md) [index](index.md) [self](hub.md)\n".to_string();
    for index in 0..10 {
        write(
            &repo,
            &format!("docs/design-docs/neighbor{}.md", index),
            "# Neighbor\n\nEvidence.\n",
        );
        text.push_str(&format!("[evidence](neighbor{}.md)\n", index));
    }
    write(&repo, "docs/design-docs/hub.md", &text);
    let report = query(&repo, "hubneedle");
    let neighbors = report["candidates"][0]["neighbors"].as_array().unwrap();
    assert_eq!(neighbors.len(), 5);
    assert!(neighbors.iter().all(|path| {
        let path = path.as_str().unwrap();
        path.starts_with("docs/design-docs/")
            && !path.ends_with("index.md")
            && !path.ends_with("hub.md")
    }));
}

#[test]
fn both_fts_indexes_follow_edits_deletions_and_reused_rowids() {
    let repo = TestRepo::new(None);
    write(
        &repo,
        "docs/change.md",
        "# Old\n\n前缀精灵蛋后缀以及ultraviolet。\n",
    );
    assert_eq!(paths(&query(&repo, "精灵蛋")), ["docs/change.md"]);
    write(
        &repo,
        "docs/change.md",
        "# New\n\n羁绊成长规则以及chartreuse。\n",
    );
    assert!(paths(&query(&repo, "精灵蛋")).is_empty());
    assert!(paths(&query(&repo, "violet")).is_empty());
    assert_eq!(paths(&query(&repo, "绊成长规")), ["docs/change.md"]);
    fs::remove_file(repo.root.join("docs/change.md")).unwrap();
    write(
        &repo,
        "docs/reused.md",
        "# Reused\n\n橙色温度计现在上线。\n",
    );
    assert!(paths(&query(&repo, "绊成长规")).is_empty());
    assert_eq!(paths(&query(&repo, "温度计")), ["docs/reused.md"]);
    let db = rusqlite::Connection::open(repo.root.join(".harnesskit/state/facts.sqlite")).unwrap();
    db.execute_batch("INSERT INTO docs_fts(docs_fts) VALUES ('integrity-check'); INSERT INTO docs_trigram(docs_trigram, rank) VALUES ('integrity-check', 1);").unwrap();
}

fn tree_bytes(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn collect(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.is_dir() {
                collect(root, &path, out);
            } else {
                out.insert(
                    path.strip_prefix(root)
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .to_string(),
                    fs::read(path).unwrap(),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    collect(root, root, &mut out);
    out
}

#[test]
fn s4_state_rebuild_and_missing_fts_repair_preserve_history_and_old_schema() {
    let repo = TestRepo::new(Some("cjk"));
    write(
        &repo,
        ".harnesskit/schema.yaml",
        include_str!("fixtures/compat/alpha6-schema.yaml"),
    );
    let schema = fs::read(repo.root.join(".harnesskit/schema.yaml")).unwrap();
    repo.run(&["history", "snapshot", "-m", "before S5"]);
    let history_root = repo.root.join(".harnesskit/history");
    let history = tree_bytes(&history_root);
    repo.run(&["index"]);
    let database = repo.root.join(".harnesskit/state/facts.sqlite");
    let db = rusqlite::Connection::open(&database).unwrap();
    // Recreate S4's actual FTS layout and version, leaving its other tables intact.
    db.execute_batch("CREATE TABLE saved_fts AS SELECT * FROM docs_fts;
        DROP TABLE docs_trigram; DROP TABLE docs_fts;
        CREATE VIRTUAL TABLE docs_fts USING fts5(path UNINDEXED, title, headings_text, summary, body_tokens);
        INSERT INTO docs_fts SELECT * FROM saved_fts; DROP TABLE saved_fts;
        UPDATE meta SET value = '4' WHERE key = 'fact_schema_version';").unwrap();
    drop(db);
    assert_eq!(
        paths(&query(&repo, "精灵蛋")),
        ["docs/product-specs/egg-incubation.md"]
    );
    let db = rusqlite::Connection::open(&database).unwrap();
    db.execute_batch("DROP TABLE docs_trigram;").unwrap();
    drop(db);
    assert_eq!(
        paths(&query(&repo, "精灵蛋")),
        ["docs/product-specs/egg-incubation.md"]
    );
    assert_eq!(
        fs::read(repo.root.join(".harnesskit/schema.yaml")).unwrap(),
        schema
    );
    assert_eq!(tree_bytes(&history_root), history);
    assert!(repo.run(&["history", "list"]).contains("before S5"));
    assert!(repo
        .run(&["history", "diff", "latest"])
        .contains("Changed:\n- none"));
    repo.run(&["history", "restore", "latest", "--apply"]);
    assert!(repo.run(&["history", "status"]).contains("dirty=no"));
}
