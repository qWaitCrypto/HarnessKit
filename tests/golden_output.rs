use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;

fn assert_success(output: &Output, args: &[&str]) {
    assert!(
        output.status.success(),
        "{}\nstdout:\n{}\nstderr:\n{}",
        args.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn complete_golden_outputs_meet_signal_density_and_metadata_gates() {
    let output = Command::new("python3")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/evaluate-output.py"))
        .env("HARNESSKIT_BIN", env!("CARGO_BIN_EXE_harnesskit"))
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .output()
        .unwrap();
    assert_success(&output, &["evaluate-output.py"]);
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["query_snapshots"], 11);
    assert_eq!(report["snapshots"], 21);
    assert!(report["min_signal_density"].as_f64().unwrap() >= 0.70);
    assert!(report["max_metadata_share"].as_f64().unwrap() <= 0.35);
    eprintln!(
        "Output gates: {}",
        String::from_utf8(output.stdout).unwrap()
    );
}

#[test]
fn output_metric_counts_full_paths_and_rejects_metadata_only_lines() {
    let output = Command::new("python3")
        .arg("-c")
        .arg("import importlib.util; from pathlib import Path; spec = importlib.util.spec_from_file_location('output_eval', Path('scripts/evaluate-output.py')); module = importlib.util.module_from_spec(spec); spec.loader.exec_module(module); path = 'src/usage/log.rs'; sample = module.metrics('symbol ' + path + ' [current] [accepted] [warning] [info]', ['usage', 'log']); assert sample['signal_density'] == 0, sample; assert sample['metadata_chars'] >= len(path) + len('[current]'), sample; assert sample['metadata_share'] > 0.35, sample; assert module.metrics('pub fn usage_log() {}', ['usage', 'log'])['signal_density'] == 1")
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .output().unwrap();
    assert_success(&output, &["output metric negative controls"]);
}
