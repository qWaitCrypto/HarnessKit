#!/usr/bin/env python3
"""CLI wrapper for deterministic E1 scenario generation."""

from __future__ import annotations

import argparse
from pathlib import Path

from scenarios import generate_repo, scenario_ids, write_definitions


def main() -> int:
    parser = argparse.ArgumentParser(description="Generate HarnessKit agent A/B scenario repositories")
    parser.add_argument("--scenario", default="all", help="S1..S7, comma-separated, or all")
    parser.add_argument("--out", type=Path, required=True, help="directory containing generated repositories")
    parser.add_argument("--force", action="store_true", help="replace existing generated repositories")
    parser.add_argument("--definitions", action="store_true", help="write scenarios/<ID>/README.md, prompt.txt and grader.py")
    args = parser.parse_args()
    if args.definitions:
        write_definitions()
    args.out.mkdir(parents=True, exist_ok=True)
    for scenario_id in scenario_ids(args.scenario):
        destination = generate_repo(scenario_id, args.out / scenario_id, force=args.force)
        print(destination)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

