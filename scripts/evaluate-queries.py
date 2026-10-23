#!/usr/bin/env python3
"""Replay the frozen query corpus against a CLI binary in disposable repositories.

Capture S4 before editing search:
  python3 scripts/evaluate-queries.py --binary /tmp/harnesskit-s4 --label S4 \
    --output scripts/query-baseline-s4.json
Compare S5 (also enforced by cargo test):
  python3 scripts/evaluate-queries.py --binary target/debug/harnesskit --label S5 \
    --baseline scripts/query-baseline-s4.json --output scripts/query-results-s5.json

Top-k means any expected path occurs in the first k candidates. CJK includes
the corpus's cjk and mixed categories. Sizes are fresh, checkpointed SQLite
files, without VACUUM; all fixture inputs and their hashes are kept unchanged.
The S5 storage gate excludes actual pages belonging to the Weave graph/card tables
and their indexes. Total database size is still reported separately.
"""

import argparse
import hashlib
import json
from pathlib import Path
import shutil
import sqlite3
import subprocess
import tempfile


ROOT = Path(__file__).resolve().parents[1]
FIXTURES = ROOT / "tests/fixtures"


def metrics(cases):
    count = len(cases)
    zero = sum(not case["paths"] for case in cases)
    top1 = sum(case["rank"] == 1 for case in cases)
    top3 = sum(case["rank"] is not None and case["rank"] <= 3 for case in cases)
    return dict(queries=count, zero_hits=zero, zero_hit_rate=zero / count,
                top1_hits=top1, top1_rate=top1 / count,
                top3_hits=top3, top3_rate=top3 / count)


def evaluate(binary, label):
    corpus = json.loads((FIXTURES / "queries.json").read_text())
    fixtures = sorted({case["fixture"] for case in corpus})
    inputs = [FIXTURES / "queries.json"]
    for fixture in fixtures:
        inputs.extend(path for path in sorted((FIXTURES / fixture).rglob("*"))
                      if path.is_file())
    digest = hashlib.sha256()
    for path in inputs:
        digest.update(str(path.relative_to(FIXTURES)).encode() + b"\0")
        digest.update(path.read_bytes() + b"\0")
    cases = []
    sizes = {}
    with tempfile.TemporaryDirectory(prefix="harnesskit-query-eval-") as tmp:
        for fixture in fixtures:
            shutil.copytree(FIXTURES / fixture, Path(tmp) / fixture)
        for case in corpus:
            repo = Path(tmp) / case["fixture"]
            output = subprocess.run([str(binary), "query", case["query"], "--json"],
                                    cwd=repo, check=True, capture_output=True, text=True)
            if output.stderr:
                raise RuntimeError(output.stderr)
            paths = [doc["path"] for doc in json.loads(output.stdout)["candidates"]]
            rank = next((i + 1 for i, path in enumerate(paths)
                         if path in case["expected_paths"]), None)
            cases.append(dict(fixture=case["fixture"], query=case["query"],
                              category=case["category"], paths=paths, rank=rank,
                              forbidden_first=bool(paths and paths[0] in
                                                   case.get("not_first_paths", []))))
        for fixture in fixtures:
            database = Path(tmp) / fixture / ".harnesskit/state/facts.sqlite"
            with sqlite3.connect(database) as db:
                db.execute("PRAGMA wal_checkpoint(TRUNCATE)")
                total_bytes = database.stat().st_size
                weave_bytes = db.execute(
                    "SELECT COALESCE(SUM(pgsize), 0) FROM dbstat WHERE name IN "
                    "('nodes', 'idx_nodes_path', 'edges', 'idx_edges_dst', 'inline_code_spans', "
                    "'cochange_stats', 'idx_cochange_stats_doc', 'cochange_files', 'doc_sections', "
                    "'sqlite_autoindex_doc_sections_1', 'sqlite_autoindex_code_cards_1') "
                    "OR name = 'code_cards' OR name LIKE 'code_fts_%' OR name LIKE 'code_trigram_%' "
                    "OR name LIKE 'sections_fts_%' OR name LIKE 'sections_trigram_%'"
                ).fetchone()[0]
                sizes[fixture] = dict(bytes=total_bytes,
                                     doc_index_bytes=total_bytes - weave_bytes,
                                     weave_bytes=weave_bytes,
                                     docs=db.execute("SELECT COUNT(*) FROM docs").fetchone()[0])
    return dict(label=label, corpus_sha256=digest.hexdigest(),
                metrics=metrics(cases),
                cjk=metrics([case for case in cases if case["category"] in ("cjk", "mixed")]),
                index_sizes=sizes, total_index_bytes=sum(s["bytes"] for s in sizes.values()),
                total_doc_index_bytes=sum(s["doc_index_bytes"] for s in sizes.values()),
                cases=cases)


def compare(before, after):
    assert before["corpus_sha256"] == after["corpus_sha256"], "fixture inputs changed"
    assert len(before["cases"]) == len(after["cases"])
    for old, new in zip(before["cases"], after["cases"]):
        assert (old["fixture"], old["query"]) == (new["fixture"], new["query"])
        if old["rank"] is not None:
            assert new["rank"] is not None and new["rank"] <= old["rank"], new
        assert not new["forbidden_first"], new
    # Frozen S5 acceptance thresholds, mirrored in tests/query_retrieval.rs.
    for group in ("metrics", "cjk"):
        assert after[group]["zero_hit_rate"] == 0.0, after[group]
        assert after[group]["top1_rate"] >= (34 / 35 if group == "metrics" else 1.0), after[group]
        assert after[group]["top3_rate"] == 1.0, after[group]
    for key in ("top1_rate", "top3_rate"):
        assert after["metrics"][key] - before["metrics"][key] >= 0.20
    assert before["metrics"]["zero_hit_rate"] - after["metrics"]["zero_hit_rate"] >= 0.20
    # Bound the added trigram cost on this small corpus (page allocation included).
    before_bytes = before.get("total_doc_index_bytes", before["total_index_bytes"])
    after_bytes = after.get("total_doc_index_bytes", after["total_index_bytes"])
    assert after_bytes <= before_bytes * 1.35


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--label", required=True)
    parser.add_argument("--baseline", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    result = evaluate(args.binary.resolve(), args.label)
    if args.baseline:
        baseline = json.loads(args.baseline.read_text())
        compare(baseline, result)
        delta = result["total_index_bytes"] - baseline["total_index_bytes"]
        result["index_delta_bytes"] = delta
        result["index_delta_ratio"] = delta / baseline["total_index_bytes"]
        before_doc_bytes = baseline.get("total_doc_index_bytes", baseline["total_index_bytes"])
        doc_delta = result["total_doc_index_bytes"] - before_doc_bytes
        result["doc_index_delta_bytes"] = doc_delta
        result["doc_index_delta_ratio"] = doc_delta / before_doc_bytes
    args.output.write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n")
    print(json.dumps({key: value for key, value in result.items() if key != "cases"},
                     ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
