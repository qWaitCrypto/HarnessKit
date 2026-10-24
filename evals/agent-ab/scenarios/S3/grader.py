from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))
from grader_support import grade as _grade  # noqa: E402

SCENARIO = Path(__file__).resolve().parent.name


def grade(repo: Path, *, last_message: str = "", baseline=None, **kwargs):
    return _grade(SCENARIO, repo, last_message=last_message, baseline=baseline, **kwargs)


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("repo", type=Path)
    parser.add_argument("--last-message", default="")
    parser.add_argument("--baseline", type=Path)
    parser.add_argument("--codex-home", type=Path)
    parser.add_argument("--harnesskit-binary")
    args = parser.parse_args()
    baseline = json.loads(args.baseline.read_text()) if args.baseline else None
    print(json.dumps(grade(args.repo, last_message=args.last_message, baseline=baseline,
                           codex_home=args.codex_home, harnesskit_binary=args.harnesskit_binary).to_dict(), ensure_ascii=False, indent=2))
