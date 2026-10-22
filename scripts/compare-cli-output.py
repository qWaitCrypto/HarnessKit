#!/usr/bin/env python3
"""Compare two Linux CLI builds byte for byte, including stderr and exit codes.

Usage: python3 scripts/compare-cli-output.py BEFORE AFTER [--allow-help-changes]
Requires Python 3, cc, git and sqlite3 for the old build. All writes stay in a
temporary directory. --allow-help-changes only exempts help/usage text; exit codes
and file outputs are always compared.
Both builds run at the same executable/repository paths with identical fixtures.
A test-only LD_PRELOAD shim fixes snapshot IDs, timestamps and PIDs; output is never
normalized, parsed or rewritten before comparison.
"""

import argparse
import gzip
import hashlib
import io
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile


FIXTURES = Path(__file__).resolve().parents[1] / "tests/fixtures"
CLOCK_SOURCE = r"""
#define _GNU_SOURCE
#include <dlfcn.h>
#include <stdlib.h>
#include <time.h>
#include <unistd.h>

pid_t getpid(void) { return 4242; }

int clock_gettime(clockid_t clock, struct timespec *value) {
    const char *fixed = getenv("HARNESSKIT_COMPARE_TIME");
    if (clock == CLOCK_REALTIME && fixed != NULL) {
        value->tv_sec = strtol(fixed, NULL, 10);
        value->tv_nsec = 123456789;
        return 0;
    }
    int (*real_clock)(clockid_t, struct timespec *) =
        dlsym(RTLD_NEXT, "clock_gettime");
    return real_clock(clock, value);
}
"""


def fixed_mtimes(root):
    for path in sorted(root.rglob("*"), reverse=True):
        os.utime(path, ns=(1700000000000000000, 1700000000000000000))
    os.utime(root, ns=(1700000000000000000, 1700000000000000000))


def file_outputs(repo):
    return {
        str(path.relative_to(repo)): path.read_bytes()
        for path in sorted(repo.rglob("*"))
        if path.is_file()
        and (".git" not in path.parts or path.relative_to(repo) == Path(".git/info/exclude"))
        and path.name not in ("facts.sqlite", "facts.sqlite-wal", "facts.sqlite-shm")
    }


def run_scenario(binary, sandbox, clock, fixture, steps, git=False):
    repo = sandbox / "repo"
    if repo.exists():
        shutil.rmtree(repo)
    repo.mkdir()
    if fixture:
        shutil.copytree(FIXTURES / fixture, repo, dirs_exist_ok=True)
    if git:
        subprocess.run(["git", "init", "-q", str(repo)], check=True)
    fixed_mtimes(repo)
    executable = sandbox / "harnesskit"
    shutil.copy2(binary, executable)
    env = os.environ.copy()
    env["LD_PRELOAD"] = str(clock)
    env["PWD"] = str(repo)
    results = []
    for index, step in enumerate(steps):
        if callable(step):
            step(repo)
            fixed_mtimes(repo)
            continue
        env["HARNESSKIT_COMPARE_TIME"] = str(1800000000 + index)
        output = subprocess.run(
            [str(executable), *(arg.replace("{repo}", str(repo)) for arg in step)],
            cwd=repo, env=env, capture_output=True
        )
        results.append((step, output.returncode, output.stdout, output.stderr,
                        file_outputs(repo)))
        fixed_mtimes(repo)
    return results


def change_docs(repo):
    (repo / "docs/project.md").write_text("# Changed Project\n\n合成修订。\n")
    (repo / "docs/design-docs/added.md").write_text("# Added\n\nNew route.\n")
    (repo / "docs/decisions/0001-queue.md").unlink()


def block_state(repo):
    (repo / ".harnesskit").mkdir(exist_ok=True)
    (repo / ".harnesskit/state").write_text("not a directory")


def add_suppression(repo):
    with (repo / ".harnesskit/schema.yaml").open("a") as schema:
        schema.write(
            "\nsuppressions:\n  known:\n    rule_id: history-missing\n"
            "    path: .harnesskit/history\n    reason: synthetic baseline\n"
        )


def prepare_release(repo):
    # A local synthetic release exercises the real downloader/checksum/extractor.
    # Omitting SKILL.md keeps this test from writing to installed agent skills.
    package = "harnesskit-x86_64-unknown-linux-gnu"
    payload = b"#!/bin/sh\nexit 0\n"
    archive = io.BytesIO()
    with tarfile.open(fileobj=archive, mode="w") as tar:
        entry = tarfile.TarInfo(f"{package}/harnesskit")
        entry.size = len(payload)
        entry.mode = 0o755
        tar.addfile(entry, io.BytesIO(payload))
    directory = repo / "releases"
    directory.mkdir()
    content = gzip.compress(archive.getvalue(), mtime=0)
    (directory / f"{package}.tar.gz").write_bytes(content)
    (directory / "SHA256SUMS").write_text(
        f"{hashlib.sha256(content).hexdigest()}  {package}.tar.gz\n"
    )


def break_checksum(repo):
    (repo / "releases/SHA256SUMS").write_text(
        "0" * 64 + "  harnesskit-x86_64-unknown-linux-gnu.tar.gz\n"
    )


def scenarios():
    yield "usage", None, [
        [], ["help"], ["--help"], ["-h"], ["unknown"], ["--version"], ["-V"],
        ["inspect"], ["refs"], ["query"], ["graph"], ["context"], ["history"],
        ["history", "unknown"], ["history", "diff"], ["history", "restore"],
        ["update", "--repo", "invalid"], ["init", "--local", "--tracked"],
        ["index", "--schema", "missing.yaml"],
    ], False
    for git in (False, True):
        for mode in ("--local", "--tracked"):
            yield f"init-{git}-{mode}", None, [
                ["init", "nested", mode, "--preview"], ["init", mode],
                ["init", mode], ["init", mode, "--force"], ["doctor"],
                ["doctor", "--json"], ["index"], ["check"], ["check", "--json"],
            ], git
    yield "custom-root", None, [
        ["init", "--tracked", "--docs-root", "knowledge"], ["index"],
        ["list-docs"], ["query", "architecture", "--json"],
        ["check", "--json"], ["history", "snapshot", "-m", "custom root"],
        ["history", "status"],
    ], False
    yield "explicit-target-and-options", "standard", [
        ["init", "{repo}", "--tracked"],
        ["index", "--schema", ".harnesskit/schema.yaml", "{repo}"],
        ["check", "--rules", "nonexistent-rule", "{repo}", "--json", "--json"],
        ["query", "backoff", "{repo}", "--json"],
        ["inspect", "docs/design-docs/lantern.md", "{repo}"],
        ["refs", "docs/design-docs/lantern.md", "{repo}"],
        ["graph", "docs/design-docs/lantern.md", "{repo}", "--json"],
        ["context", "backoff", "{repo}", "--json"],
        ["history", "snapshot", "-m", "--leading-hyphen", "{repo}"],
        ["history", "snapshot", "--message", "long", "-m", "short", "-m", "last", "{repo}"],
        ["history", "list", "{repo}"], ["history", "status", "{repo}"],
        ["history", "diff", "latest", "{repo}"],
        ["history", "diff", "latest", "latest", "{repo}"],
        ["history", "restore", "latest", "{repo}", "--apply"],
    ], False
    for fixture, query, doc in [
        ("standard", "backoff", "docs/design-docs/lantern.md"),
        ("flat", "Flat Courier Project", "docs/project.md"),
        ("cjk", "EV-01", "docs/product-specs/egg-incubation.md"),
        ("lifecycle", "indigo", "docs/decisions/0002-relay-routing.md"),
        ("external", "quartz", "docs/design-docs/probe.md"),
    ]:
        steps = [["index"], ["index"], ["rank"], ["list-docs"], ["inspect", doc],
                 ["refs", doc], ["refs", "missing.md"], ["inspect", "missing.md"]]
        for json in ([], ["--json"]):
            steps.extend([
                ["check", *json], ["check", "--strict", *json],
                ["check", "--strict", "--no-strict", *json],
                ["check", "--rules", "manifest,history", *json],
                ["check", "--rules", "nonexistent-rule", *json],
                ["query", query, *json], ["query", "absentterm", *json],
                ["query", "--- ?! ::", *json], ["query", "精灵蛋", *json],
                ["query", "backoff absentterm", *json],
                ["query", "quoted \"route\" \\ 合成\n", *json],
                ["graph", doc, *json], ["graph", "missing.md", *json],
                ["context", query, *json], ["context", doc, *json],
                ["context", "absentterm", *json], ["doctor", *json],
            ])
        yield fixture, fixture, steps, False
    yield "history", "standard", [
        ["init", "--tracked"], ["history", "list"], ["history", "status"],
        ["history", "snapshot", "-m", "baseline \"quoted\" \\ 合成\n"],
        ["history", "list"], ["history", "status"],
        ["history", "diff", "latest"], change_docs,
        ["index"], ["check", "--json"], ["history", "status"],
        ["history", "diff", "latest"], ["history", "restore", "latest"],
        ["history", "restore", "latest", "--apply"],
        ["history", "restore", "latest", "--apply", "--force"],
        ["history", "status"], ["history", "snapshot", "--message", "second"],
        ["history", "list"], ["history", "diff", "latest", "latest"],
    ], False
    yield "legacy-history", None, [
        lambda repo: shutil.copytree(FIXTURES / "compat/alpha6-history",
                                    repo / ".harnesskit/history"),
        ["history", "list"], ["history", "diff", "latest"],
        ["history", "restore", "latest"],
        ["history", "restore", "latest", "--apply", "--force"],
        ["history", "status"],
    ], False
    yield "suppression", "standard", [
        ["init", "--tracked"], add_suppression,
        ["check"], ["check", "--json"],
        ["check", "--rules", "history-missing"],
        ["query", "backoff", "--schema", "missing", "--schema",
         ".harnesskit/schema.yaml", "--json"],
    ], False
    yield "blocked-state", "standard", [
        block_state, ["index"], ["check", "--json"], ["doctor", "--json"],
    ], False
    for broken in (False, True):
        steps = [prepare_release]
        if broken:
            steps.append(break_checksum)
        steps.append(["update", "--version", "synthetic", "--asset-base-url",
                      "file://{repo}/releases"])
        yield f"update-checksum-broken-{broken}", None, steps, False


def compare_results(name, expected, actual, allow_help_changes=False):
    assert len(expected) == len(actual), name
    count = 0
    for left, right in zip(expected, actual):
        assert left[0] == right[0], name
        help_text = allow_help_changes and name == "usage" and left[0] in (
            [], ["help"], ["--help"], ["-h"], ["unknown"],
        )
        differences = []
        for label, old, new in zip(("exit", "stdout", "stderr", "files"),
                                   left[1:], right[1:]):
            if old == new or (help_text and label in ("stdout", "stderr")):
                continue
            differences.append(label)
            if label == "files":
                for path in sorted(old.keys() | new.keys()):
                    if old.get(path) != new.get(path):
                        print(f"file differs: {path}")
            else:
                print(f"{label} before: {old!r}\n{label} after:  {new!r}")
        if differences:
            raise SystemExit(f"FAIL {name}: {left[0]!r}")
        count += 1
    print(f"PASS {name}: {count} commands")
    return count


def compare_interop(writer, reader, sandbox, clock, name):
    def steps(next_binary):
        return [
            ["init", "--tracked"], ["index"],
            ["history", "snapshot", "-m", "interop \"合成\" \\ 😀\n\t\b\f\r\x01"],
            # Replace only the executable; keep the writer's database and history.
            lambda repo: shutil.copy2(next_binary, sandbox / "harnesskit"),
            ["history", "list"], ["history", "status"],
            ["query", "backoff", "--json"], ["history", "diff", "latest"],
            change_docs, ["index"], ["history", "diff", "latest"],
            ["history", "restore", "latest"],
            ["history", "restore", "latest", "--apply", "--force"],
            ["history", "status"], ["history", "snapshot", "-m", "second"],
            ["history", "list"],
        ]

    expected = run_scenario(writer, sandbox, clock, "standard", steps(writer))
    actual = run_scenario(writer, sandbox, clock, "standard", steps(reader))
    assert all(result[1] == 0 for result in expected), f"{name}: baseline failed"
    assert all(result[1] == 0 for result in actual), f"{name}: cross-version operation failed"
    return compare_results(name, expected, actual)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("before", type=Path)
    parser.add_argument("after", type=Path)
    parser.add_argument("--allow-help-changes", action="store_true")
    args = parser.parse_args()
    before, after = args.before.resolve(), args.after.resolve()
    count = 0
    with tempfile.TemporaryDirectory(prefix="harnesskit-cli-compare-") as temporary:
        sandbox = Path(temporary)
        source = sandbox / "clock.c"
        source.write_text(CLOCK_SOURCE)
        clock = sandbox / "clock.so"
        subprocess.run(["cc", "-shared", "-fPIC", str(source), "-ldl", "-o", str(clock)],
                       check=True)
        for name, fixture, steps, git in scenarios():
            expected = run_scenario(before, sandbox, clock, fixture, steps, git)
            actual = run_scenario(after, sandbox, clock, fixture, steps, git)
            count += compare_results(name, expected, actual, args.allow_help_changes)
        count += compare_interop(before, after, sandbox, clock, "old-to-new-history-and-state")
        count += compare_interop(after, before, sandbox, clock, "new-to-old-history-and-state")
    exception = " (except help/usage text)" if args.allow_help_changes else ""
    print(f"PASS: {count} command comparisons; stdout, stderr, exit codes and file outputs identical{exception}")


if __name__ == "__main__":
    main()
