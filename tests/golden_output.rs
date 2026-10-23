mod support;

use std::fs;
use std::path::Path;
use std::process::Command;

use serde_json::Value;
use support::{assert_success, TestRepo};

fn w0_fixture() -> TestRepo {
    let repo = TestRepo::new(None);
    let output = Command::new("python3")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/generate-weave-fixture.py"))
        .arg(&repo.root)
        .output()
        .unwrap();
    assert_success(&output, &["generate-weave-fixture.py"]);
    let output = Command::new("git")
        .args(["checkout", "-q", "baseline"])
        .current_dir(&repo.root)
        .output()
        .unwrap();
    assert_success(&output, &["git", "checkout", "baseline"]);
    repo
}

fn repo_style_fixture() -> TestRepo {
    let repo = TestRepo::new(None);
    for path in ["docs/commands.md", "CHANGELOG.md", "src/skills.rs"] {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join(path);
        let target = repo.root.join(path);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::copy(source, target).unwrap();
    }
    repo
}

fn metrics(output: &str, terms: &[String]) -> (f64, f64) {
    let lines = output.lines().filter(|line| !line.trim().is_empty()).collect::<Vec<_>>();
    let signal = lines
        .iter()
        .filter(|line| {
            let lower = line.to_lowercase();
            terms.iter().any(|term| lower.contains(&term.to_lowercase()))
                || line.split_whitespace().count() >= 4
        })
        .count();
    let total_chars = lines.iter().map(|line| line.chars().count()).sum::<usize>();
    let metadata_chars = lines
        .iter()
        .map(|line| {
            line.chars()
                .filter(|ch| matches!(ch, '[' | ']' | '(' | ')' | '#' | '↳' | '~' | '⚠' | '✗'))
                .count()
        })
        .sum::<usize>();
    (
        signal as f64 / lines.len().max(1) as f64,
        metadata_chars as f64 / total_chars.max(1) as f64,
    )
}

#[test]
fn w5b_golden_outputs_meet_signal_density_and_metadata_gates() {
    let cases: Vec<Value> = serde_json::from_str(include_str!("golden/w5b-cases.json")).unwrap();
    assert_eq!(cases.len(), 10);
    let w0 = w0_fixture();
    let repo_style = repo_style_fixture();
    for case in cases {
        let repo = if case["fixture"] == "repo" {
            &repo_style
        } else {
            &w0
        };
        let args = case["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|arg| arg.as_str().unwrap())
            .collect::<Vec<_>>();
        let output = repo.run(&args);
        let terms = case["terms"]
            .as_array()
            .unwrap()
            .iter()
            .map(|term| term.as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        let (density, metadata) = metrics(&output, &terms);
        eprintln!(
            "golden {}: signal_density={density:.3} metadata_share={metadata:.3}",
            case["name"]
        );
        assert!(density >= 0.70, "{}\n{}", case["name"], output);
        assert!(metadata <= 0.35, "{}\n{}", case["name"], output);
        let snapshot = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/golden")
            .join(case["snapshot"].as_str().unwrap());
        let expected = fs::read_to_string(snapshot).unwrap();
        let first = output.lines().find(|line| !line.trim().is_empty()).unwrap_or("");
        assert!(first.starts_with(expected.trim()), "{}: {first}", case["name"]);
    }
}
