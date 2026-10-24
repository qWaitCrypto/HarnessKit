#!/usr/bin/env python3
"""Re-score finished E1 runs with the current graders, keeping the old verdicts."""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT))

from grader_support import grade  # noqa: E402


def _commit() -> str:
    result = subprocess.run(["git", "-C", str(ROOT), "rev-parse", "--short", "HEAD"],
                            text=True, capture_output=True, check=False)
    return result.stdout.strip() or "unknown"


def regrade_run(run_json: Path, commit: str) -> tuple[bool, bool]:
    run = json.loads(run_json.read_text(encoding="utf-8"))
    run_dir = run_json.parent
    baseline = json.loads((run_dir / "baseline.json").read_text(encoding="utf-8"))
    message_path = run_dir / "final_message.txt"
    result = grade(
        run["scenario"],
        run_dir / "repo",
        last_message=message_path.read_text(encoding="utf-8", errors="replace") if message_path.exists() else "",
        baseline=baseline,
        harnesskit_binary=run.get("harnesskit_binary"),
        codex_home=Path(run["codex_home"]) if run.get("codex_home") else None,
    )
    before = bool(run.get("success"))
    run.setdefault("regrades", []).append({"at": time.time(), "grader_commit": commit,
                                           "previous_success": before, "previous_grade": run.get("grade")})
    run["success"] = result.success
    run["grade"] = result.to_dict()
    run.setdefault("metrics", {}).update(result.process_metrics)
    run["metrics"]["decision_violations"] = result.decision_violations
    run_json.write_text(json.dumps(run, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    return before, result.success


def main() -> int:
    parser = argparse.ArgumentParser(description="Re-score E1 runs with the current graders")
    parser.add_argument("--input", type=Path, required=True, help="a run.py --out directory")
    parser.add_argument("--scenario", required=True, help="S1..S7, comma-separated")
    args = parser.parse_args()
    scenarios = {item.strip().upper() for item in args.scenario.split(",") if item.strip()}
    commit = _commit()
    for run_json in sorted(args.input.glob("*/[AB]/rep-*/run.json")):
        if run_json.parts[-4] not in scenarios:
            continue
        before, after = regrade_run(run_json, commit)
        print(f"{'/'.join(run_json.parts[-4:-1])}: {before} -> {after}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
