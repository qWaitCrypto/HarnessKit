#!/usr/bin/env python3
"""W9a release gate: 10,000 tracked corpus files; end-to-end CLI wall time.

Run `cargo build --release`, then `python3 scripts/benchmark-weave.py`.
Cold means no derived index (OS page caches are not flushed). Query samples
use distinct terms and therefore cannot hit the result cache. The report uses
the slowest sample, including process startup, automatic refresh and rendering.
"""

import argparse
import json
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]


def run(command, root):
    started = time.perf_counter()
    result = subprocess.run(command, cwd=root, capture_output=True, text=True,
                            env={**os.environ, "HARNESSKIT_USAGE_LOG": "off"}, timeout=180)
    elapsed = time.perf_counter() - started
    if result.returncode:
        raise RuntimeError(f"{command}: {result.stderr}\n{result.stdout}")
    return elapsed, result.stdout


def generate(root):
    (root / ".gitignore").write_text(".harnesskit/state/\n.harnesskit/knowledge.lock\n")
    (root / "docs").mkdir()
    (root / "src").mkdir()
    sources = [
        ("rs", "/// Token refresh keeps credentials valid.\npub fn refresh_token_{n}(age: u64) -> u64 {{ age + 60 }}\n"),
        ("ts", "/** Token refresh keeps credentials valid. */\nexport function refreshToken{n}(age: number): number {{ return age + 60; }}\n"),
        ("py", "def refresh_token_{n}(age):\n    \"\"\"Token refresh keeps credentials valid.\"\"\"\n    return age + 60\n"),
        ("go", "package tokens\n// Token refresh keeps credentials valid.\nfunc refreshToken{n}(age int) int {{ return age + 60 }}\n"),
    ]
    for n in range(9600):
        extension, source = sources[n % len(sources)]
        (root / "src" / f"token_{n:05}.{extension}").write_text(source.format(n=n))
    for n in range(400):
        target = n * 4
        (root / "docs" / f"rule_{n:04}.md").write_text(
            f"# 令牌刷新 {n}\n\nRefresh credentials deterministically with a fixed lifetime.\n\n"
            f"## Refresh policy {n}\n\nUse `src/token_{target:05}.rs#refresh_token_{target}` "
            "to renew a token; 令牌刷新必须保留输入的过期时间并使用固定增量。\n")
    run(["git", "init", "-q", "--template=", "--initial-branch=main"], root)
    run(["git", "add", "."], root)
    run(["git", "-c", "user.name=Benchmark", "-c", "user.email=bench@example.invalid",
         "-c", "commit.gpgSign=false", "commit", "-qm", "synthetic import"], root)


def benchmark(binary, samples):
    with tempfile.TemporaryDirectory(prefix="harnesskit-weave-bench-") as directory:
        root = Path(directory)
        generate(root)
        timings = {key: [] for key in ("cold_index", "incremental_index", "query")}
        for _ in range(samples):
            shutil.rmtree(root / ".harnesskit/state", ignore_errors=True)
            (root / ".harnesskit/knowledge.lock").unlink(missing_ok=True)
            elapsed, _ = run([str(binary), "index"], root)
            timings["cold_index"].append(elapsed)
        for n in range(samples):
            path = root / f"src/token_{9000 + n * 4:05}.rs"
            path.write_text(path.read_text().replace("+ 60", "+ 61"))
            elapsed, _ = run([str(binary), "index"], root)
            timings["incremental_index"].append(elapsed)
        for n in range(samples):
            elapsed, output = run([str(binary), "query", f"refresh_token_{n * 4}", "--code", "--json"], root)
            payload = json.loads(output)
            if not payload["bundles"]:
                raise AssertionError("benchmark query returned no bundles")
            timings["query"].append(elapsed)
        with sqlite3.connect(root / ".harnesskit/state/facts.sqlite") as db:
            counts = {kind: count for kind, count in db.execute("SELECT kind, COUNT(*) FROM nodes GROUP BY kind")}
        limits = dict(cold_index=10.0, incremental_index=1.0, query=0.2)
        report = dict(corpus_files=10000, languages=["Rust", "TypeScript", "Python", "Go"],
                      build="release", query_kind="distinct exact code symbols; result cache misses", cold="derived state removed; OS cache retained", nodes=counts,
                      samples_seconds=timings, max_seconds={key: max(values) for key, values in timings.items()},
                      limits_seconds=limits)
        report["passed"] = all(report["max_seconds"][key] < limit for key, limit in limits.items())
        return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/harnesskit")
    parser.add_argument("--samples", type=int, default=3)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    if args.samples < 1:
        parser.error("--samples must be positive")
    report = benchmark(args.binary.resolve(), args.samples)
    encoded = json.dumps(report, ensure_ascii=False, indent=2) + "\n"
    if args.output:
        args.output.write_text(encoded)
    print(encoded, end="")
    raise SystemExit(0 if report["passed"] else 1)


if __name__ == "__main__":
    main()
