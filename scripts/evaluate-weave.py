#!/usr/bin/env python3
"""Validate the W0 corpus and execute the W3/W4/W5 Weave evaluations."""

import argparse
import json
import os
import sqlite3
from pathlib import Path, PurePosixPath
import subprocess
import sys
import tempfile
import shutil


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
        drift_cases = run_drift_cases(corpus, repo)
        cochange_cases, cochange_coverage = run_cochange_cases(corpus, repo)
        retrieval_cases, retrieval_metrics = run_retrieval_cases(corpus, repo)
    cases = []
    for kind in REQUIRED_STEPS:
        for case in corpus[kind]:
            if kind == "drift":
                cases.append(drift_cases[case["id"]])
            elif kind == "cochange":
                cases.append(cochange_cases[case["id"]])
            else:
                cases.append(retrieval_cases[case["id"]])
    executed = sum(case["status"] != "pending" for case in cases)
    return dict(schema_version=1, fixture="weave", fixture_validated=True,
                fixture_commits=len(manifest["revisions"]), as_of=corpus["as_of"],
                summary=dict(total=len(cases), executed=executed,
                             pending=len(cases) - executed,
                             by_kind={kind: len(corpus[kind]) for kind in REQUIRED_STEPS}),
                cases=cases, cochange_coverage=cochange_coverage,
                retrieval_metrics=retrieval_metrics)


def harnesskit_command():
    configured = os.environ.get("HARNESSKIT_BIN")
    if configured:
        return [configured]
    binary = ROOT / "target" / "debug" / "harnesskit"
    if binary.is_file():
        return [str(binary)]
    return ["cargo", "run", "--quiet", "--"]


def run_cli(repo, *args, expect_success=True):
    command = harnesskit_command() + list(args) + [str(repo)]
    # Target directory is last for compatibility with the existing CLI.  The
    # check command accepts options before that positional argument.
    result = subprocess.run(command, cwd=ROOT, capture_output=True, text=True,
                            env={**os.environ, "HARNESSKIT_USAGE_LOG": "off"})
    if expect_success and result.returncode != 0:
        raise ValueError(f"HarnessKit command failed: {' '.join(command)}\n{result.stderr}\n{result.stdout}")
    return result


def reset_derived_state(repo):
    subprocess.run(["git", "reset", "--hard", "--quiet"], cwd=repo, check=True,
                   capture_output=True)
    lock = repo / ".harnesskit" / "knowledge.lock"
    if lock.exists():
        lock.unlink()
    state = repo / ".harnesskit" / "state"
    if state.exists():
        shutil.rmtree(state)


def bundle_nodes(payload):
    for bundle in payload["bundles"]:
        yield bundle["primary"]
        for related in bundle["related"]:
            yield related["node"]
            yield from (symbol["node"] for symbol in related.get("additional_symbols", []))


def distinct(values):
    return list(dict.fromkeys(values))


def run_retrieval_cases(corpus, repo):
    """Score sections and symbols independently in the mixed bundles.

    Related nodes are real answers in the mixed bundles. The --code check also
    requires direct code-card recall, so propagation cannot mask missing Chinese
    vocabulary bridges. Neither the corpus nor its expected targets are changed.
    """
    results = {}
    section_hits = symbol_hits = chinese_hits = chinese_count = 0
    for case in corpus["retrieval"]:
        reset_derived_state(repo)
        subprocess.run(["git", "checkout", "--quiet", "--detach", case["revision"]],
                       cwd=repo, check=True)
        mixed = json.loads(run_cli(repo, "query", case["query"], "--json").stdout)
        docs = json.loads(run_cli(repo, "query", case["query"], "--docs", "--json").stdout)
        code = json.loads(run_cli(repo, "query", case["query"], "--code", "--json").stdout)
        require(len(mixed["bundles"]) <= 5, f"bundle budget exceeded: {case['id']}")
        require(all(len(bundle["related"]) <= 3 for bundle in mixed["bundles"]),
                f"related-node budget exceeded: {case['id']}")
        mixed_nodes = list(bundle_nodes(mixed))
        mixed_sections = distinct((node["path"], node["heading"])
                                  for node in mixed_nodes if node["kind"] == "section")
        mixed_symbols = distinct(f"{node['path']}#{node['name']}"
                                 for node in mixed_nodes if node["kind"] == "symbol")
        sections = distinct((bundle["primary"]["path"], bundle["primary"]["heading"])
                            for bundle in docs["bundles"]
                            if bundle["primary"]["kind"] == "section")
        symbols = distinct(f"{bundle['primary']['path']}#{bundle['primary']['name']}"
                           for bundle in code["bundles"]
                           if bundle["primary"]["kind"] == "symbol")
        top_k = case["top_k"]
        section_ok = all((target["path"], target["heading"]) in mixed_sections[:top_k]
                         for target in case["expected_sections"])
        symbol_ok = all(target in symbols[:top_k] and target in mixed_symbols[:top_k]
                        for target in case["expected_symbols"])
        section_hits += section_ok
        symbol_hits += symbol_ok
        chinese = any("\u4e00" <= ch <= "\u9fff" for ch in case["query"])
        chinese_count += chinese
        chinese_hits += chinese and symbol_ok
        results[case["id"]] = dict(
            id=case["id"], kind="retrieval", status="passed" if section_ok and symbol_ok else "failed",
            required_steps=REQUIRED_STEPS["retrieval"], observed_sections=mixed_sections,
            docs_only_sections=sections,
            observed_symbols=symbols, mixed_sections=mixed_sections, mixed_symbols=mixed_symbols,
            section_top3=section_ok, symbol_top3=symbol_ok,
            score=float(symbol_ok),
        )
    count = len(corpus["retrieval"])
    metrics = dict(queries=count, section_top3_hits=section_hits,
                   section_top3_rate=section_hits / count, symbol_top3_hits=symbol_hits,
                   symbol_top3_rate=symbol_hits / count, chinese_queries=chinese_count,
                   chinese_symbol_top3_hits=chinese_hits,
                   chinese_symbol_top3_rate=chinese_hits / chinese_count if chinese_count else 1.0)
    require(metrics["symbol_top3_rate"] >= 0.8, f"W5 symbol top3 below 80%: {metrics}")
    require(section_hits == count, f"W5 section retrieval regressed: {metrics}")
    return results, metrics


def run_drift_cases(corpus, repo):
    results = {}
    for case in corpus["drift"]:
        reset_derived_state(repo)
        subprocess.run(["git", "checkout", "--quiet", "--detach", case["before"]],
                       cwd=repo, check=True)
        run_cli(repo, "index")
        subprocess.run(["git", "checkout", "--quiet", "--detach", case["after"]],
                       cwd=repo, check=True)
        report = run_cli(repo, "check", "--rules", "anchors", "--json", "--all")
        payload = json.loads(report.stdout)
        matches = [check for check in payload["checks"]
                   if check["subject_path"] == case["doc"]
                   and check["evidence_json"].get("anchor") == case["anchor"]]
        expected = case.get("expected_rule")
        if expected is None:
            require(not any(check["rule_id"] in ("code-drift", "anchor-moved", "anchor-broken")
                            for check in matches), f"unexpected drift warning: {case['id']}")
        else:
            require(any(check["rule_id"] == expected for check in matches),
                    f"missing {expected}: {case['id']}")
            if expected == "anchor-moved":
                moved = next(check for check in matches if check["rule_id"] == expected)
                require(moved["fixable"] is True, f"move is not fixable: {case['id']}")
                fixed = run_cli(repo, "check", "--fix", "--rules", "anchors", "--json", "--all")
                fixed_payload = json.loads(fixed.stdout)
                require(not any(check["rule_id"] == "anchor-moved"
                                and check["subject_path"] == case["doc"]
                                for check in fixed_payload["checks"]),
                        f"move fix did not clear: {case['id']}")
                target = case["expected_target"]
                require(f"`{target}`" in (repo / case["doc"]).read_text(encoding="utf-8"),
                        f"move fix did not rewrite: {case['id']}")
        results[case["id"]] = dict(id=case["id"], kind="drift", status="passed",
                                    required_steps=REQUIRED_STEPS["drift"],
                                    expected_rule=expected)
    return results


def run_cochange_cases(corpus, repo):
    """Run W4's top-eight cochange checks against the derived SQLite graph."""
    results = {}
    known_expected = 0
    known_matched = 0
    for case in corpus["cochange"]:
        reset_derived_state(repo)
        subprocess.run(["git", "checkout", "--quiet", "--detach", case["revision"]],
                       cwd=repo, check=True)
        run_cli(repo, "index")
        database = sqlite3.connect(repo / ".harnesskit" / "state" / "facts.sqlite")
        rows = database.execute(
            """SELECT t.path
               FROM edges e
               JOIN nodes s ON s.id = e.src
               JOIN nodes t ON t.id = e.dst
               WHERE s.path = ? AND e.kind = 'cochange'
               ORDER BY e.weight DESC, t.path ASC
               LIMIT ?""",
            (case["doc"], case["top_k"]),
        ).fetchall()
        database.close()
        observed = [row[0] for row in rows]
        expected = case["expected_paths"]
        forbidden = case["forbidden_paths"]
        matched = sum(path in observed for path in expected)
        known_expected += len(expected)
        known_matched += matched
        passed = (matched == len(expected)
                  and all(path not in observed for path in forbidden))
        results[case["id"]] = dict(
            id=case["id"], kind="cochange", status="passed" if passed else "failed",
            required_steps=REQUIRED_STEPS["cochange"], expected_paths=expected,
            forbidden_paths=forbidden, observed_paths=observed,
            matched=matched, expected=len(expected),
            score=(matched / len(expected) if expected else 1.0),
        )
        if not passed:
            raise ValueError(
                f"cochange case failed: {case['id']} observed={observed} "
                f"expected={expected} forbidden={forbidden}"
            )
    coverage = known_matched / known_expected if known_expected else 1.0
    if coverage < 0.9:
        raise ValueError(f"W4 cochange coverage {coverage:.3f} is below 0.900")
    return results, coverage


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--corpus", type=Path, default=CORPUS)
    args = parser.parse_args()
    corpus = json.loads(args.corpus.read_text(encoding="utf-8"))
    print(json.dumps(evaluate(corpus), ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
