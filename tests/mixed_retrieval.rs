mod support;

use std::fs;
use std::path::Path;
use std::process::Command;

use rusqlite::Connection;
use serde_json::{json, Value};
use support::{assert_success, TestRepo};

fn write(repo: &TestRepo, path: &str, text: &str) {
    let path = repo.root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn query(repo: &TestRepo, terms: &str, flags: &[&str]) -> Value {
    let mut args = vec!["query", terms, "--json"];
    args.extend(flags);
    serde_json::from_str(&repo.run(&args)).unwrap()
}

fn nodes(report: &Value) -> Vec<&Value> {
    report["bundles"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|bundle| {
            std::iter::once(&bundle["primary"]).chain(
                bundle["related"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .flat_map(|related| {
                        std::iter::once(&related["node"]).chain(
                            related["additional_symbols"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .map(|symbol| &symbol["node"]),
                        )
                    }),
            )
        })
        .collect()
}

fn has_symbol(report: &Value, name: &str) -> bool {
    nodes(report)
        .iter()
        .any(|node| node["kind"] == "symbol" && node["name"] == name)
}

fn database(repo: &TestRepo) -> Connection {
    Connection::open(repo.root.join(".harnesskit/state/facts.sqlite")).unwrap()
}

#[test]
fn cards_recall_split_names_paths_signatures_and_multilanguage_doc_comments() {
    let repo = TestRepo::new(None);
    for (path, text) in [
        ("src/HTTPServer/session_refresh.rs", "/// Credentialrenewal keeps the session valid.\n#[inline]\npub fn refresh_token(\n    session: &SessionEnvelope,\n) -> SessionEnvelope {\n    session.clone()\n}\n"),
        ("web/cache.ts", "/** Cacheexpiration is deterministic. */\nexport function cacheTtl(age: number): number { return age + 60; }\n"),
        ("worker/retry.py", "def retry_delay(attempt: int) -> int:\n    \"\"\"Queuebackoff uses a fixed offset.\"\"\"\n    return attempt + 60\n"),
        ("gateway/limit.go", "package gateway\n\n// Gatewaythrottle limits requests.\nfunc limitRequests(count int) int { return count + 60 }\n"),
    ] { write(&repo, path, text); }
    for (terms, name) in [
        ("refresh token", "refresh_token"),
        ("http server", "refresh_token"),
        ("SessionEnvelope", "refresh_token"),
        ("credentialrenewal", "refresh_token"),
        ("cache ttl", "cacheTtl"),
        ("cacheexpiration", "cacheTtl"),
        ("queuebackoff", "retry_delay"),
        ("gatewaythrottle", "limitRequests"),
    ] {
        let report = query(&repo, terms, &["--code"]);
        assert!(has_symbol(&report, name), "{}: {}", terms, report);
        assert!(nodes(&report)
            .iter()
            .all(|node| matches!(node["kind"].as_str(), Some("file" | "symbol"))));
        assert!(report["diagnostics"].is_null());
    }
    let db = database(&repo);
    let (signature, comment, split): (String, String, String) = db.query_row(
        "SELECT signature, doc_comment, split_name FROM code_cards WHERE name = 'refresh_token';", [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).unwrap();
    assert_eq!(
        signature,
        "pub fn refresh_token( session: &SessionEnvelope, ) -> SessionEnvelope"
    );
    assert!(comment.contains("Credentialrenewal"));
    assert_eq!(split, "refresh token");
    for table in ["code_trigram_content", "sections_trigram_content"] {
        let count: usize = db
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = ?1;",
                [table],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0, "both tokenizers must share card content");
    }
}

#[test]
fn chinese_bridge_stays_with_its_anchored_chapter_and_excludes_mentions() {
    let repo = TestRepo::new(None);
    write(
        &repo,
        "src/auth.rs",
        "pub fn refresh_token() {}\npub fn migrate_ledger() {}\npub fn mentioned_symbol() {}\n",
    );
    write(&repo, "docs/contracts.md", "# Contracts\n\nShared contract.\n\n## 令牌刷新\n\n刷新令牌需要单飞。`src/auth.rs#refresh_token`\n\n## 账本迁移\n\n迁移账本需要校验。`src/auth.rs#migrate_ledger`\n\n## 冷门术语\n\nOrdinary mention `mentioned_symbol`.\n");
    for term in ["令牌刷新", "牌刷", "刷新令牌", "刷"] {
        let report = query(&repo, term, &["--code"]);
        assert!(has_symbol(&report, "refresh_token"), "{}: {}", term, report);
        assert!(!has_symbol(&report, "migrate_ledger"));
    }
    assert!(!has_symbol(
        &query(&repo, "冷门术语", &["--code"]),
        "mentioned_symbol"
    ));
    let bridged: String = database(&repo)
        .query_row(
            "SELECT bridged FROM code_cards WHERE name = 'refresh_token';",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(bridged.contains("令牌刷新"));
    assert!(!bridged.contains("账本迁移"));
    let mixed = query(&repo, "令牌刷新", &[]);
    assert!(has_symbol(&mixed, "refresh_token"));
    let relation = mixed["bundles"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|bundle| bundle["related"].as_array().unwrap())
        .find(|related| related["edge"]["kind"] == "anchor")
        .unwrap();
    assert_eq!(relation["edge"]["confidence"], "EXTRACTED");
    assert!(relation["edge"]["evidence"]["anchor"]
        .as_str()
        .unwrap()
        .contains("#refresh_token"));
    assert_eq!(relation["freshness"]["status"], "current");
    for (flag, kinds) in [
        ("--docs", vec!["doc", "section"]),
        ("--code", vec!["file", "symbol"]),
    ] {
        let report = query(&repo, "令牌刷新", &[flag]);
        assert!(!nodes(&report).is_empty());
        assert!(nodes(&report)
            .iter()
            .all(|node| kinds.contains(&node["kind"].as_str().unwrap())));
    }
    let output = repo
        .command(&["query", "refresh", "--docs", "--code"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn code_and_bridge_updates_remove_stale_postings_and_preserve_comment_only_fingerprints() {
    let repo = TestRepo::new(None);
    write(
        &repo,
        "src/auth.rs",
        "/// Oldcredentialcomment\npub fn refresh_token() {}\n",
    );
    write(
        &repo,
        "docs/auth.md",
        "# Auth\n\n## 令牌刷新\n\nOldbridgeword `src/auth.rs#refresh_token`.\n",
    );
    assert!(has_symbol(
        &query(&repo, "oldbridgeword", &["--code"]),
        "refresh_token"
    ));
    let before: String = database(&repo)
        .query_row(
            "SELECT fingerprint FROM nodes WHERE name = 'refresh_token';",
            [],
            |row| row.get(0),
        )
        .unwrap();
    write(
        &repo,
        "src/auth.rs",
        "/// Newcredentialcomment\npub fn refresh_token() {}\n",
    );
    write(
        &repo,
        "docs/auth.md",
        "# Auth\n\n## 令牌续期\n\nNewbridgeword `src/auth.rs#refresh_token`.\n",
    );
    assert!(has_symbol(
        &query(&repo, "newcredentialcomment", &["--code"]),
        "refresh_token"
    ));
    assert!(has_symbol(
        &query(&repo, "newbridgeword", &["--code"]),
        "refresh_token"
    ));
    for term in ["oldcredentialcomment", "oldbridgeword", "令牌刷新"] {
        assert!(
            nodes(&query(&repo, term, &["--code"])).is_empty(),
            "{}",
            term
        );
    }
    let after: String = database(&repo)
        .query_row(
            "SELECT fingerprint FROM nodes WHERE name = 'refresh_token';",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(before, after);
    fs::remove_file(repo.root.join("docs/auth.md")).unwrap();
    assert!(nodes(&query(&repo, "newbridgeword", &["--code"])).is_empty());
    fs::remove_file(repo.root.join("src/auth.rs")).unwrap();
    write(
        &repo,
        "src/reused.rs",
        "/// Anothercredential\nfn alternate_token() {}\n",
    );
    assert!(!has_symbol(
        &query(&repo, "refresh_token", &["--code"]),
        "refresh_token"
    ));
    assert!(has_symbol(
        &query(&repo, "anothercredential", &["--code"]),
        "alternate_token"
    ));
    for table in [
        "code_fts",
        "code_trigram",
        "sections_fts",
        "sections_trigram",
    ] {
        database(&repo)
            .execute_batch(&format!(
                "INSERT INTO {table}({table}, rank) VALUES ('integrity-check', 1);"
            ))
            .unwrap();
    }
}

#[test]
fn pure_docs_keep_legacy_candidates_text_and_limits_with_additive_json_fields() {
    let repo = TestRepo::new(None);
    for index in 0..12 {
        write(
            &repo,
            &format!("docs/sample{index}.md"),
            "# Sample\n\nShared sample.\n",
        );
    }
    let report = query(&repo, "sample", &[]);
    let docs = query(&repo, "sample", &["--docs"]);
    assert_eq!(report["candidates"].as_array().unwrap().len(), 8);
    assert_eq!(report["results"], report["candidates"]);
    assert_eq!(report["candidates"], docs["candidates"]);
    assert_eq!(report["bundles"].as_array().unwrap().len(), 5);
    assert_eq!(
        repo.run(&["query", "sample"]),
        repo.run(&["query", "sample", "--docs"])
    );
    assert_eq!(
        query(&repo, "sample", &["--limit", "10"])["candidates"]
            .as_array()
            .unwrap()
            .len(),
        10
    );
    let code = query(&repo, "sample", &["--code"]);
    assert!(nodes(&code).is_empty());
    assert_eq!(code["diagnostics"]["indexed_code_nodes"], 0);
    assert_eq!(code["diagnostics"]["index_empty"], true);
}

#[test]
fn mixed_default_budgets_cover_hints_verbose_and_multiline_text() {
    let repo = TestRepo::new(None);
    for index in 0..16 {
        write(&repo, &format!("src/item{index}.rs"), &format!("/// Budgetneedle description\n/// on multiple lines.\npub fn budget_item_{index}() {{}}\n"));
        let mut doc = "# Budgetneedle\n\nDescription.\n\n".to_string();
        for neighbor in 0..6 {
            doc.push_str(&format!(
                "`src/item{neighbor}.rs#budget_item_{neighbor}`\n\n"
            ));
        }
        write(&repo, &format!("docs/item{index}.md"), &doc);
    }
    let report = query(&repo, "budgetneedle", &[]);
    assert_eq!(report["bundles"].as_array().unwrap().len(), 5);
    assert!(report["bundles"]
        .as_array()
        .unwrap()
        .iter()
        .all(|bundle| bundle["related"].as_array().unwrap().len() <= 3));
    for args in [
        vec!["query", "budgetneedle", "--verbose"],
        vec!["query", "?! missingword", "--verbose"],
        vec!["query", "budgetneedle\nmissingword"],
    ] {
        let text = repo.run(&args);
        assert!(text.lines().count() <= 40, "{}", text);
    }
    assert_eq!(
        query(&repo, "budgetneedle", &["--limit", "1"])["bundles"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(
        query(&repo, "budgetneedle", &["--limit", "10"])["bundles"]
            .as_array()
            .unwrap()
            .len()
            > 5
    );
}

#[test]
fn bundles_expose_current_knowledge_lock_drift_evidence() {
    let repo = TestRepo::new(None);
    let output = Command::new("python3")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/generate-weave-fixture.py"))
        .arg(&repo.root)
        .output()
        .unwrap();
    assert_success(&output, &["generate-weave-fixture.py"]);
    let checkout = |revision: &str| {
        let output = Command::new("git")
            .args(["checkout", "--quiet", "--detach", revision])
            .current_dir(&repo.root)
            .output()
            .unwrap();
        assert_success(&output, &["git", "checkout", revision]);
    };
    checkout("baseline");
    query(&repo, "令牌刷新", &[]);
    checkout("logic");
    let report = query(&repo, "令牌刷新", &[]);
    let issues = report["bundles"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|bundle| {
            std::iter::once(&bundle["freshness"]).chain(
                bundle["related"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|related| &related["freshness"]),
            )
        })
        .flat_map(|freshness| freshness["issues"].as_array().unwrap())
        .collect::<Vec<_>>();
    assert!(issues.iter().any(|issue| issue["kind"] == "code-drift"
        && issue["evidence"]["anchor"] == "src/auth/refresh.rs#refresh_token"));
    let evidence = &issues
        .iter()
        .find(|issue| issue["evidence"]["anchor"] == "src/auth/refresh.rs#refresh_token")
        .unwrap()["evidence"];
    assert!(evidence["commit"].is_string());
    assert!(evidence["git_diff"]
        .as_str()
        .unwrap()
        .starts_with("git diff"));
    assert!(repo.run(&["query", "令牌刷新"]).contains("code-drift"));
    assert_eq!(report["results"], report["candidates"]);
    assert!(report["coverage"].is_object());
    assert!(report["hints"].is_array());
    assert_ne!(report["bundles"], json!([]));
}

#[test]
fn section_summaries_and_bridge_vocabulary_stay_with_their_own_first_paragraph_or_line() {
    let repo = TestRepo::new(None);
    write(&repo, "src/engine.rs", "pub fn engine() {}\n");
    write(&repo, "docs/plan.md", "# Plan\n\nDocumentprelude is shared by neither chapter.\n\n## Prosechapter\n\nOwnparagraph starts here\nand continues here.\n\nLater detail.\n\n## Listchapter\n\n- Ownrequirement is the first line.\n- A second requirement.\n");
    for (term, expected) in [
        (
            "Prosechapter",
            "Ownparagraph starts here and continues here.",
        ),
        ("Listchapter", "- Ownrequirement is the first line."),
    ] {
        let report = query(&repo, term, &["--docs"]);
        assert_eq!(report["bundles"][0]["primary"]["heading"], term);
        assert_eq!(report["bundles"][0]["primary"]["summary"], expected);
    }
    let prelude = query(&repo, "Documentprelude", &["--docs"]);
    assert!(nodes(&prelude).iter().all(|node| {
        !matches!(
            node["heading"].as_str(),
            Some("Prosechapter" | "Listchapter")
        )
    }));
}

#[test]
fn context_and_query_prefer_the_constraint_sentence_within_the_anchored_section() {
    let repo = TestRepo::new(None);
    write(&repo, "src/auth/session.rs", "pub fn renew_session() {}\n");
    write(&repo, "docs/decisions/ownership.md", concat!(
        "---\nstatus: accepted\n---\n# Session ownership\n\nA general introduction.\n\n",
        "## Policy\n\nThe renewal operation handles session requests.\n\n",
        "The renewal operation returns any failure to its caller; the caller owns retry policy.\n\n",
        "It is implemented by src/auth/session.rs#renew_session.\n\n",
        "## Other policy\n\nOther callers must never delete records.\n",
    ));
    let expected =
        "The renewal operation returns any failure to its caller; the caller owns retry policy.";
    let context: Value =
        serde_json::from_str(&repo.run(&["context", "--for", "src/auth/session.rs", "--json"]))
            .unwrap();
    assert_eq!(context["documents"][0]["summary"], expected, "{context}");
    let results = query(&repo, "renewal operation", &["--docs"]);
    let policy = nodes(&results)
        .into_iter()
        .find(|node| node["heading"] == "Policy")
        .unwrap();
    assert_eq!(policy["summary"], expected, "{results}");
    let text = repo.run(&["query", "renewal operation", "--docs"]);
    assert!(text.contains("returns any failure to its caller"), "{text}");
    assert!(!text.contains("handles session requests"), "{text}");
    let hook = repo
        .command(&["hook", "pre-edit", "src/auth/session.rs"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert_success(&hook, &["hook", "pre-edit"]);
    let hook = String::from_utf8(hook.stdout).unwrap();
    assert!(hook.contains("returns any failure to its caller"), "{hook}");
}

#[test]
fn summaries_skip_html_and_badges_and_upgrade_pre_fix_cached_documents() {
    let repo = TestRepo::new(None);
    write(&repo, "src/engine.rs", "pub fn engine() {}\n");
    write(&repo, "README.md", "<div align=\"center\">\n\n# Readme\n\n<!--\nHidden banner.\n-->\n\n[![Status](https://example.invalid/badge.svg)](https://example.invalid/)\n![Build](https://example.invalid/build.svg)\nActual <strong>summaryneedle</strong> remains.\n\n</div>\n");
    for upgraded in [false, true] {
        if upgraded {
            let db = database(&repo);
            db.execute_batch("UPDATE docs SET summary = '<div align=\"center\">';\nUPDATE doc_sections SET summary = 'old document summary';\nUPDATE meta SET value = '11' WHERE key = 'fact_schema_version';").unwrap();
        }
        let report = query(&repo, "summaryneedle", &[]);
        assert_eq!(
            report["candidates"][0]["summary"],
            "Actual summaryneedle remains."
        );
        assert_eq!(
            report["bundles"][0]["primary"]["summary"],
            "Actual summaryneedle remains."
        );
        assert!(!repo.run(&["query", "summaryneedle"]).contains("<div"));
    }
}

#[test]
fn bundle_status_omits_empty_fields_and_preserves_explicit_lifecycle_status() {
    let repo = TestRepo::new(None);
    write(&repo, "src/engine.rs", "pub fn stateprobe() {}\n");
    for (frontmatter, expected) in [
        ("", "docs/status.md §Stateprobe"),
        ("---\nstatus: accepted\n---\n", "docs/status.md §Stateprobe"),
        ("---\nstatus: superseded\n---\n", "[superseded]"),
    ] {
        write(
            &repo,
            "docs/status.md",
            &format!("{frontmatter}# Stateprobe\n\nState summary.\n"),
        );
        let text = repo.run(&["query", "stateprobe", "--docs", "--limit", "1"]);
        assert!(text.contains(expected), "{text}");
        assert!(!text.contains("[;"), "{text}");
    }
    assert!(repo
        .run(&["query", "stateprobe", "--code"])
        .contains("stateprobe"));
}

#[test]
fn chapter_related_nodes_use_its_own_outgoing_edges_without_sibling_anchors() {
    let repo = TestRepo::new(None);
    write(&repo, "src/own.rs", "pub fn own_symbol() {}\n");
    write(&repo, "src/other.rs", "pub fn other_symbol() {}\n");
    write(&repo, "tests/anchors.rs", "fn unrelated_anchor_test() {}\n");
    write(&repo, "docs/plan.md", "# Plan\n\nGeneral introduction.\n\n## Localchapter\n\nLocal summary.\n\nScopeneedle is implemented by `src/own.rs#own_symbol`.\n\n## Otherchapter\n\nOther summary with `src/other.rs#other_symbol` and `tests/anchors.rs#unrelated_anchor_test`.\n");
    let report = query(&repo, "scopeneedle", &[]);
    let bundle = &report["bundles"][0];
    assert_eq!(bundle["primary"]["heading"], "Localchapter");
    let related = bundle["related"].as_array().unwrap();
    assert_eq!(related.len(), 1);
    assert_eq!(related[0]["node"]["name"], "own_symbol");
    assert_eq!(related[0]["edge"]["src"], bundle["primary"]["id"]);
    assert_eq!(related[0]["edge"]["evidence"]["section"], "Localchapter");
    assert!(!serde_json::to_string(bundle)
        .unwrap()
        .contains("tests/anchors.rs"));
}

#[test]
fn related_slots_prefer_production_then_tests_then_docs_and_merge_symbols_by_file() {
    let repo = TestRepo::new(None);
    write(
        &repo,
        "src/z.rs",
        "pub fn alpha() {}\npub fn beta() {}\npub fn gamma() {}\n",
    );
    write(&repo, "tests/a.rs", "fn check_result() {}\n");
    write(&repo, "docs/aaa.md", "# Reference\n\nRelated prose.\n");
    let base = "# Slots\n\nShort summary.\n\nSlotneedle relates to `docs/aaa.md`, `tests/a.rs#check_result`, `src/z.rs#alpha`, `src/z.rs#beta`, and `src/z.rs#gamma`.\n";
    write(&repo, "docs/slots.md", base);
    for extra_production in [false, true] {
        if extra_production {
            write(&repo, "src/x.rs", "pub fn first() {}\n");
            write(&repo, "src/y.rs", "pub fn second() {}\n");
            write(
                &repo,
                "docs/slots.md",
                &format!("{base}\nAlso `src/x.rs#first` and `src/y.rs#second`.\n"),
            );
        }
        let report = query(&repo, "slotneedle", &[]);
        let bundle = &report["bundles"][0];
        assert_eq!(bundle["primary"]["path"], "docs/slots.md");
        let related = bundle["related"].as_array().unwrap();
        let paths = related
            .iter()
            .map(|entry| entry["node"]["path"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            paths,
            if extra_production {
                vec!["src/x.rs", "src/y.rs", "src/z.rs"]
            } else {
                vec!["src/z.rs", "tests/a.rs", "docs/aaa.md"]
            }
        );
        let group = related
            .iter()
            .find(|entry| entry["node"]["path"] == "src/z.rs")
            .unwrap();
        let symbols = std::iter::once(group)
            .chain(group["additional_symbols"].as_array().unwrap().iter())
            .collect::<Vec<_>>();
        assert_eq!(symbols.len(), 3);
        for (entry, name) in symbols.iter().zip(["alpha", "beta", "gamma"]) {
            assert_eq!(entry["node"]["name"], name);
            assert_eq!(
                entry["edge"]["evidence"]["anchor"],
                format!("src/z.rs#{name}")
            );
            assert!(entry["freshness"].is_object());
        }
        let text = repo.run(&["query", "slotneedle", "--limit", "1"]);
        assert_eq!(
            text.lines()
                .filter(|line| line.contains("↳") && line.contains("src/z.rs"))
                .count(),
            1
        );
        assert!(text.contains("src/z.rs: alpha, beta, gamma"), "{text}");
    }
}

#[test]
fn test_file_symbols_are_downweighted_in_both_query_modes_but_stay_searchable() {
    for (path, source) in [
        (
            "tests/engine.rs",
            "/// Rankingneedle\nfn testonlyneedle() {}\n",
        ),
        (
            "src/engine_test.rs",
            "/// Rankingneedle\nfn testonlyneedle() {}\n",
        ),
        (
            "src/tests.rs",
            "/// Rankingneedle\nfn testonlyneedle() {}\n",
        ),
        (
            "src/test_engine.py",
            "def testonlyneedle():\n    \"\"\"Rankingneedle\"\"\"\n    pass\n",
        ),
        (
            "web/engine.spec.ts",
            "/** Rankingneedle */\nexport function testonlyneedle() {}\n",
        ),
        (
            "web/engine.test.tsx",
            "/** Rankingneedle */\nexport function testonlyneedle() {}\n",
        ),
        (
            "web/__tests__/engine.js",
            "/** Rankingneedle */\nexport function testonlyneedle() {}\n",
        ),
        (
            "gateway/engine_test.go",
            "package gateway\n// Rankingneedle\nfunc testonlyneedle() {}\n",
        ),
    ] {
        let repo = TestRepo::new(None);
        write(
            &repo,
            "src/engine.rs",
            "/// Rankingneedle\npub fn implementation() {}\n",
        );
        write(&repo, path, source);
        for flags in [vec![], vec!["--code"]] {
            let report = query(&repo, "rankingneedle", &flags);
            assert_eq!(
                report["bundles"][0]["primary"]["path"], "src/engine.rs",
                "{path}: {report}"
            );
            assert!(has_symbol(&report, "testonlyneedle"), "{path}: {report}");
            let exact = query(&repo, "testonlyneedle", &flags);
            assert_eq!(exact["bundles"][0]["primary"]["path"], path, "{exact}");
        }
    }
}

#[test]
fn broad_directory_anchors_cannot_outvote_direct_query_matches_through_many_test_symbols() {
    let repo = TestRepo::new(None);
    write(
        &repo,
        "src/engine.rs",
        "/// Retrievalneedle\npub fn implementation() {}\n",
    );
    write(
        &repo,
        "docs/answer.md",
        "# Answer\n\nRetrievalneedle is explained here.\n",
    );
    write(
        &repo,
        "docs/broad.md",
        "# Broad\n\nGeneral summary.\n\n## Generic\n\nBroad directory anchor: `tests/`.\n",
    );
    for index in 0..24 {
        write(
            &repo,
            &format!("tests/check{index}.rs"),
            &format!("/// Retrievalneedle\nfn check_{index}() {{}}\n"),
        );
    }
    let report = query(&repo, "retrievalneedle", &[]);
    let paths = report["bundles"]
        .as_array()
        .unwrap()
        .iter()
        .take(2)
        .map(|bundle| bundle["primary"]["path"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(paths.contains(&"docs/answer.md"), "{report}");
    assert!(paths.contains(&"src/engine.rs"), "{report}");
    assert!(report["bundles"]
        .as_array()
        .unwrap()
        .iter()
        .take(2)
        .all(|bundle| bundle["primary"]["match"].is_object()));
}

#[test]
fn bridge_only_code_matches_rank_below_matching_sections_and_native_code_terms() {
    let repo = TestRepo::new(None);
    write(
        &repo,
        "src/actual.rs",
        "/// Detailed lookupneedle contract.\npub fn implementation() {}\n",
    );
    write(&repo, "src/broad.rs", "pub fn unrelated() {}\n");
    write(
        &repo,
        "docs/answer.md",
        "# Answer\n\nLookupneedle is described here.\n",
    );
    write(&repo, "docs/bridge.md", "# General introduction\n\nOrdinary summary.\n\n## Bridge\n\nLookupneedle bridges `src/broad.rs#unrelated`.\n");
    let report = query(&repo, "lookupneedle", &[]);
    let bundles = report["bundles"].as_array().unwrap();
    assert_ne!(bundles[0]["primary"]["path"], "src/broad.rs", "{report}");
    let code = query(&repo, "lookupneedle", &["--code"]);
    assert_eq!(
        code["bundles"][0]["primary"]["path"], "src/actual.rs",
        "{code}"
    );
    assert!(has_symbol(&code, "unrelated"));
}

#[test]
fn directory_edges_do_not_borrow_symbol_matches_bridged_from_another_chapter() {
    let repo = TestRepo::new(None);
    write(&repo, "tests/anchors.rs", "fn other_symbol() {}\n");
    write(&repo, "docs/plan.md", "# Plan\n\nGeneral overview.\n\n## Localchapter\n\nGeneral summary.\n\nProjectionneedle uses the `tests/` directory.\n\n## Otherchapter\n\nProjectionneedle specifically describes `tests/anchors.rs#other_symbol`.\n");
    let report = query(&repo, "projectionneedle", &["--limit", "20"]);
    let bundle = report["bundles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|bundle| bundle["primary"]["heading"] == "Localchapter")
        .unwrap();
    let related = bundle["related"].as_array().unwrap();
    assert_eq!(related.len(), 1, "{bundle}");
    assert_eq!(related[0]["node"]["kind"], "file", "{bundle}");
    assert_eq!(related[0]["node"]["path"], "tests/anchors.rs");
    assert_eq!(related[0]["edge"]["src"], bundle["primary"]["id"]);
    assert_eq!(related[0]["edge"]["evidence"]["anchor"], "tests/");
    assert_eq!(related[0]["edge"]["evidence"]["section"], "Localchapter");
    assert!(related[0]["additional_symbols"].is_null());
}

#[test]
fn merged_chapters_keep_all_signatures_when_related_files_share_the_first_symbol() {
    let repo = TestRepo::new(None);
    write(
        &repo,
        "tests/symbols.rs",
        "pub fn alpha() {}\npub fn beta() {}\npub fn gamma() {}\n",
    );
    write(&repo, "docs/chaptermerge.md", "# Chaptermerge\n\nOverview of the contract.\n\n## First\n\nChaptermerge covers `tests/symbols.rs#alpha` and `tests/symbols.rs#beta`.\n\n## Second\n\nChaptermerge also covers `tests/symbols.rs#alpha` and `tests/symbols.rs#gamma`.\n");
    let output = repo.run(&["query", "chaptermerge", "--expand", "1"]);
    for signature in ["pub fn alpha()", "pub fn beta()", "pub fn gamma()"] {
        assert!(output.contains(signature), "{signature}: {output}");
    }
}
