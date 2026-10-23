#!/usr/bin/env python3
"""W9a output gates: full golden snapshots, actual content and path/badge cost.

Snapshot files are read-only inputs. Intentional output changes require reviewing
and editing their complete text; this runner never updates expected output.
"""

import argparse
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
GOLDEN = ROOT / "tests/golden"
PATH = re.compile(r"(?:[\w.-]+/)+[\w./:#-]*|\b[\w.-]+\.(?:md|rs|py|ts|tsx|go|sh|json|yaml)(?:#[\w:]+)?|<repo>")
BADGE = re.compile(r"\[(?:new|resolved|warning|error|info|suppressed|current|accepted|historical|doc|file|symbol|section|added|modified|deleted|renamed|moved|[^\]]*⚠[^\]]*)(?:; (?:covered|uncovered|mixed))?\]|⚠drift|✗broken|superseded|[↳~]")
STRUCTURE = re.compile(r"^(?:Severity summary:|Baseline:|Reading order:|Incoming refs:|Outgoing refs:|Collection siblings:|Same-scope docs:|Entrypoints:$|Project map:$|Current decisions:$|Active plans:$|Truncated:|symbol\s*$|file\s*$)", re.I)


def mask_metadata(line):
    plain = line.replace("«", "").replace("»", "")
    mask = [False] * len(plain)
    for pattern in (PATH, BADGE):
        for match in pattern.finditer(plain):
            mask[match.start():match.end()] = [True] * len(match.group())
    return "".join(" " if hidden else ch for ch, hidden in zip(plain, mask)), sum(mask), len(plain)


def metrics(output, terms):
    lines = [line for line in output.splitlines() if line.strip()]
    signals = metadata_chars = total_chars = 0
    for line in lines:
        content, meta, total = mask_metadata(line)
        metadata_chars += meta
        total_chars += total
        content = content.strip(" `-:0123456789.")
        lower = content.lower()
        queried = any(term.lower() in lower for term in terms)
        signature = bool(re.search(r"\b(?:fn|def|func|function|struct|enum|trait)\s+\w", content))
        prose = not STRUCTURE.match(content) and (
            len(re.findall(r"[\u3400-\u9fff]", content)) >= 4
            or len(re.findall(r"[A-Za-z]+", content)) >= 4)
        signals += bool(queried or signature or prose)
    return dict(lines=len(lines), signal_lines=signals,
                signal_density=signals / max(len(lines), 1), metadata_chars=metadata_chars,
                total_chars=total_chars, metadata_share=metadata_chars / max(total_chars, 1))


def run(binary, root, args):
    result = subprocess.run([str(binary), *args], cwd=root, capture_output=True, text=True,
                            env={**os.environ, "HARNESSKIT_USAGE_LOG": "off"})
    if result.returncode or result.stderr:
        raise RuntimeError(f"{args}: {result.stderr}\n{result.stdout}")
    return result.stdout


def repo_fixture(root):
    root.mkdir()
    for path in ["docs/commands.md", "CHANGELOG.md", "src/skills.rs"]:
        target = root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(ROOT / path, target)
    # Exercise a populated handoff packet as well as the original sparse repo.
    (root / "AGENTS.md").write_text(
        "# Agent context\n\nRead the command reference before changing retrieval behavior; "
        "check the current project constraints and validate the resulting command output.\n\n"
        "[Commands](docs/commands.md)\n")
    (root / "docs/project.md").write_text(
        "# Project context\n\nContext retrieval connects documentation to implementation, "
        "preserves established document ranking, and presents actionable evidence within a bounded output budget.\n")


def usage_fixture(root):
    """A populated local-usage corpus for the fenced-KWIC and relevance gates."""
    root.mkdir()
    for path in [
        "docs/commands.md",
        "README.md",
        "CHANGELOG.md",
        "ITERATION_PLAN.md",
        "docs/migration-alpha.7.md",
        "docs/design-0.2.0-weave.md",
        "agent-surfaces/skills/harnesskit/references/cli.md",
        "src/usage.rs",
    ]:
        target = root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(ROOT / path, target)
    (root / "AGENTS.md").write_text(
        "# Agent context\n\nRead the command reference before changing usage metrics.\n"
    )


def evaluate(binary, verify_snapshots=True):
    cases = json.loads((GOLDEN / "w5b-cases.json").read_text())
    assert sum(case["kind"] == "query" for case in cases) >= 10
    reports = []
    with tempfile.TemporaryDirectory(prefix="harnesskit-output-eval-") as directory:
        base = Path(directory)
        w0, repo, usage, changed = base / "w0", base / "repo", base / "usage", base / "changed"
        subprocess.run(["python3", str(ROOT / "scripts/generate-weave-fixture.py"), str(w0)],
                       check=True, capture_output=True)
        subprocess.run(["git", "checkout", "-q", "baseline"], cwd=w0, check=True)
        repo_fixture(repo)
        usage_fixture(usage)
        subprocess.run(["python3", str(ROOT / "scripts/generate-weave-fixture.py"), str(changed)], check=True, capture_output=True)
        subprocess.run(["git", "checkout", "-q", "baseline"], cwd=changed, check=True)
        run(binary, changed, ["index"])
        source = changed / "src/auth/refresh.rs"
        source.write_text(source.read_text().replace("+ 60", "+ 90"))
        for case in cases:
            root = (
                repo
                if case["fixture"] == "repo"
                else usage
                if case["fixture"] == "usage"
                else changed
                if case["fixture"] == "changed"
                else w0
            )
            output = run(binary, root, case["args"]).replace(str(root), "<repo>")
            output = re.sub(r"(working-tree at )\d+", r"\1<time>", output)
            measured = metrics(output, case["terms"])
            if verify_snapshots:
                assert measured["signal_density"] >= 0.70, (case["name"], measured, output)
                assert measured["metadata_share"] <= 0.35, (case["name"], measured, output)
                expected = (GOLDEN / case["snapshot"]).read_text()
                assert output == expected, f"{case['name']}: complete snapshot changed\n{output}"
            reports.append(dict(name=case["name"], kind=case["kind"], snapshot=case["snapshot"],
                                **measured, output=output))
    return dict(cases=reports, snapshots=len(reports),
                query_snapshots=sum(case["kind"] == "query" for case in cases),
                min_signal_density=min(case["signal_density"] for case in reports),
                max_metadata_share=max(case["metadata_share"] for case in reports))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path(os.environ.get("HARNESSKIT_BIN", ROOT / "target/debug/harnesskit")))
    args = parser.parse_args()
    report = evaluate(args.binary.resolve())
    for case in report["cases"]:
        case.pop("output")
    print(json.dumps(report, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
