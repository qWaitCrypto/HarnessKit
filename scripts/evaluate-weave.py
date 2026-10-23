#!/usr/bin/env python3
"""Validate the W0 corpus and report pending Weave evaluations as JSON.

This runner generates history in temporary storage. It checks fixture inputs,
not Weave behavior: no retrieval, drift or cochange score is reported until the
corresponding implementation steps supply an evaluator.
"""

import argparse
import json
import os
from pathlib import Path, PurePosixPath
import subprocess
import sys
import tempfile


ROOT = Path(__file__).resolve().parents[1]
CORPUS = ROOT / "tests/fixtures/weave/cases.json"
REQUIRED_STEPS = dict(retrieval=["W1", "W2", "W5"],
                      drift=["W1", "W2", "W3"], cochange=["W4"])


def require(condition, message):
    if not condition:
        raise ValueError(message)


def nonempty(text):
    return isinstance(text, str) and bool(text.strip())


def validate_fixture(corpus, root, revisions):
    require(corpus["schema_version"] == 1 and corpus["fixture"] == "weave",
            "unsupported Weave corpus")
    require(corpus["as_of"] == "2026-01-11T00:00:00Z", "unexpected evaluation clock")
    ids = set()
    contents = {}
    env = {key: value for key, value in os.environ.items()
           if not key.startswith("GIT_")}
    env.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull,
               GIT_CONFIG_SYSTEM=os.devnull, LC_ALL="C")

    def read(revision, path):
        require(revision in revisions, f"unknown revision: {revision}")
        require(nonempty(path) and not PurePosixPath(path).is_absolute()
                and all(part not in ("", ".", "..") for part in path.split("/")),
                f"invalid fixture path: {path}")
        key = (revision, path)
        if key not in contents:
            result = subprocess.run(["git", "show", f"{revision}:{path}"],
                                    cwd=root, env=env, check=True, capture_output=True,
                                    text=True, encoding="utf-8")
            contents[key] = result.stdout
        return contents[key]

    def symbol(revision, anchor):
        path, separator, name = anchor.partition("#")
        require(separator and nonempty(name) and "#" not in name,
                f"invalid symbol target: {anchor}")
        require(name in read(revision, path), f"missing fixture target: {anchor}")

    for kind in REQUIRED_STEPS:
        require(isinstance(corpus[kind], list) and corpus[kind], f"empty {kind} corpus")
        for case in corpus[kind]:
            case_id = case["id"]
            require(nonempty(case_id) and case_id not in ids, f"duplicate/empty id: {case_id}")
            ids.add(case_id)
            if kind == "retrieval":
                revision = case["revision"]
                require(nonempty(case["query"]), f"empty query: {case_id}")
                require(case["top_k"] > 0, f"invalid retrieval budget: {case_id}")
                require(case["expected_sections"] and case["expected_symbols"],
                        f"missing retrieval expectations: {case_id}")
                for section in case["expected_sections"]:
                    text = read(revision, section["path"])
                    require(f"## {section['heading']}\n" in text,
                            f"missing heading: {case_id}")
                for anchor in case["expected_symbols"]:
                    symbol(revision, anchor)
            elif kind == "drift":
                before, after = case["before"], case["after"]
                text = read(before, case["doc"])
                read(after, case["doc"])
                require(f"`{case['anchor']}`" in text, f"missing doc anchor: {case_id}")
                symbol(before, case["anchor"])
                rule = case["expected_rule"]
                require(rule in (None, "code-drift", "anchor-moved", "anchor-broken"),
                        f"unknown rule: {case_id}")
                if rule == "anchor-moved":
                    symbol(after, case["expected_target"])
                elif rule != "anchor-broken":
                    symbol(after, case["anchor"])
            else:
                revision = case["revision"]
                read(revision, case["doc"])
                require(case["top_k"] > 0, f"invalid cochange budget: {case_id}")
                expected, forbidden = case["expected_paths"], case["forbidden_paths"]
                require(expected or forbidden, f"missing cochange expectations: {case_id}")
                require(not set(expected) & set(forbidden), f"conflicting paths: {case_id}")
                for path in expected + forbidden:
                    read(revision, path)


def evaluate(corpus):
    with tempfile.TemporaryDirectory(prefix="harnesskit-weave-eval-") as tmp:
        repo = Path(tmp) / "repo"
        result = subprocess.run([sys.executable, str(ROOT / "scripts/generate-weave-fixture.py"),
                                 str(repo)], check=True, capture_output=True, text=True)
        manifest = json.loads(result.stdout)
        validate_fixture(corpus, repo, manifest["revisions"])
    cases = [dict(id=case["id"], kind=kind, status="pending",
                  required_steps=REQUIRED_STEPS[kind])
             for kind in REQUIRED_STEPS for case in corpus[kind]]
    return dict(schema_version=1, fixture="weave", fixture_validated=True,
                fixture_commits=len(manifest["revisions"]), as_of=corpus["as_of"],
                summary=dict(total=len(cases), executed=0, pending=len(cases),
                             by_kind={kind: len(corpus[kind]) for kind in REQUIRED_STEPS}),
                cases=cases)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--corpus", type=Path, default=CORPUS)
    args = parser.parse_args()
    corpus = json.loads(args.corpus.read_text(encoding="utf-8"))
    print(json.dumps(evaluate(corpus), ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
