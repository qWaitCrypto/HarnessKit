mod support;

use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path};
use support::TestRepo;

#[test]
fn query_corpus_has_valid_targets_and_covers_future_retrieval_cases() {
    let corpus: Value = serde_json::from_str(include_str!("fixtures/queries.json")).unwrap();
    let cases = corpus.as_array().unwrap();
    assert!(cases.len() >= 25);
    let fixtures = ["standard", "flat", "cjk", "lifecycle", "external"];
    let mut seen_fixtures = BTreeSet::new();
    let mut categories = BTreeSet::new();
    let mut queries = BTreeSet::new();
    for case in cases {
        let fixture = case["fixture"].as_str().unwrap();
        assert!(fixtures.contains(&fixture), "unknown fixture {}", fixture);
        let query = case["query"].as_str().unwrap();
        assert!(!query.trim().is_empty());
        assert!(
            queries.insert((fixture, query)),
            "duplicate query {}",
            query
        );
        seen_fixtures.insert(fixture);
        let category = case["category"].as_str().unwrap();
        categories.insert(category);
        let expected = case["expected_paths"].as_array().unwrap();
        assert!(!expected.is_empty(), "no expected paths for {}", query);
        let mut paths = BTreeSet::new();
        for path in expected
            .iter()
            .chain(case["not_first_paths"].as_array().into_iter().flatten())
        {
            let path = path.as_str().unwrap();
            assert!(Path::new(path)
                .components()
                .all(|part| matches!(part, Component::Normal(_))));
            assert!(path.ends_with(".md"));
            assert!(
                paths.insert(path),
                "duplicate or conflicting target {}",
                path
            );
            assert!(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures")
                    .join(fixture)
                    .join(path)
                    .is_file(),
                "missing {} in {}",
                path,
                fixture
            );
        }
        if category == "lifecycle" {
            assert!(!case["not_first_paths"].as_array().unwrap().is_empty());
        }
        if category == "relaxed" {
            assert!(query.ends_with("absentterm") || query.ends_with("missingword"));
        }
    }
    assert_eq!(seen_fixtures, BTreeSet::from(fixtures));
    for category in ["relaxed", "cjk", "mixed", "identifier", "lifecycle"] {
        assert!(categories.contains(category), "missing {} cases", category);
    }
}

#[test]
fn each_fixture_supports_a_current_managed_query_control() {
    for (fixture, query, expected) in [
        ("standard", "backoff", "docs/design-docs/lantern.md"),
        ("flat", "Flat Courier Project", "docs/project.md"),
        ("cjk", "EV-01", "docs/product-specs/egg-incubation.md"),
        (
            "lifecycle",
            "indigo",
            "docs/decisions/0002-relay-routing.md",
        ),
        ("external", "quartz", "docs/design-docs/probe.md"),
    ] {
        let repo = TestRepo::new(Some(fixture));
        let result: Value = serde_json::from_str(&repo.run(&["query", query, "--json"])).unwrap();
        assert!(
            result["candidates"]
                .as_array()
                .unwrap()
                .iter()
                .any(|candidate| { candidate["path"] == expected }),
            "missing query control {} in {}",
            query,
            fixture
        );
        assert!(repo.root.join(".harnesskit/state/facts.sqlite").is_file());
        assert!(!fs::read_to_string(repo.root.join(expected))
            .unwrap()
            .is_empty());
    }
}
