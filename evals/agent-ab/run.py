#!/usr/bin/env python3
"""Run the isolated A/B evaluation or its deterministic offline dry-run."""

from __future__ import annotations

import argparse
import concurrent.futures
import json
import os
import re
import shlex
import shutil
import signal
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any

SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))

from grader_support import extract_metrics, read_jsonl, snapshot_baseline  # noqa: E402
from scenarios import (  # noqa: E402
    SCENARIOS,
    Scenario,
    adopt_harnesskit,
    apply_recorded_solution,
    generate_repo,
    scenario_ids,
    write_definitions,
)


MAX_TIMEOUT_SECONDS = 45 * 60
# S7 starts from a genuinely old CLI, built once from this release tag.
OLD_HARNESSKIT_TAG = "v0.1.0-alpha.6"
FORBIDDEN_HOME_PARTS = re.compile(
    r"^(?:memories.*|sessions|skills|history.*|state.*\.sqlite(?:-shm|-wal)?|goals.*|thread_history.*)$",
    re.IGNORECASE,
)


@dataclass
class PreparedEnvironment:
    shell_home: Path
    codex_home: Path
    path_bin: Path
    env: dict[str, str]
    harnesskit_binary: Path | None
    install_prefix: Path
    shell_harnesskit_path: str
    start_git_status: str


def _repo_root() -> Path:
    return SCRIPT_DIR.parent.parent


def _quote_toml_string(value: str) -> str:
    return json.dumps(value, ensure_ascii=False)


def sanitize_config(raw: str, scenario_repo: Path) -> str:
    """Copy model/provider settings, then trust only this scenario repository."""

    output: list[str] = []
    allowed_section = True
    model_keys = {
        "model", "model_provider", "model_reasoning_effort", "model_reasoning_summary",
        "model_verbosity", "model_context_window", "model_max_output_tokens",
        "model_supports_reasoning_summaries", "service_tier", "preferred_auth_method",
        "disable_response_storage", "chatgpt_base_url",
    }
    in_provider = False
    header = re.compile(r"^\s*\[\[?([^\]]+)\]\]\s*(?:#.*)?$|^\s*\[([^\]]+)\]\s*(?:#.*)?$")
    for line in raw.splitlines():
        match = header.match(line)
        if match:
            section = (match.group(1) or match.group(2) or "").strip()
            in_provider = section == "model_providers" or section.startswith("model_providers.")
            allowed_section = in_provider
        if allowed_section and (in_provider or line.split("=", 1)[0].strip() in model_keys):
            output.append(line.rstrip())
    while output and not output[-1].strip():
        output.pop()
    output.extend(
        [
            "",
            f"[projects.{_quote_toml_string(str(scenario_repo))}]",
            'trust_level = "trusted"',
            "",
        ]
    )
    return "\n".join(output)


def _copy_file(source: Path, target: Path) -> None:
    target.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(source, target)


def _copy_config(source_home: Path, target_home: Path, scenario_repo: Path) -> None:
    source = source_home / "config.toml"
    if not source.is_file():
        raise RuntimeError(f"missing provider config: {source}")
    target_home.joinpath("config.toml").write_text(
        sanitize_config(source.read_text(encoding="utf-8"), scenario_repo),
        encoding="utf-8",
    )


def _link_original_path(original_path: str, destination: Path) -> None:
    """Flatten PATH into symlinks while preserving first-directory precedence."""

    seen_names: set[str] = set()
    for raw_directory in original_path.split(os.pathsep):
        if not raw_directory:
            continue
        directory = Path(raw_directory)
        if not directory.is_dir():
            continue
        try:
            entries = sorted(directory.iterdir(), key=lambda path: path.name)
        except OSError:
            continue
        for entry in entries:
            name = entry.name
            if name == "harnesskit" or name in seen_names:
                continue
            try:
                executable = entry.is_file() and os.access(entry, os.X_OK)
            except OSError:
                executable = False
            if not executable:
                continue
            seen_names.add(name)
            link = destination / name
            try:
                link.symlink_to(entry)
            except FileExistsError:
                pass


def _old_harnesskit(repo_root: Path) -> Path:
    """Build (once) and return the old release binary that S7 starts from."""

    cache = repo_root / "target" / "e1-old" / OLD_HARNESSKIT_TAG
    binary = cache / "target" / "release" / "harnesskit"
    if binary.is_file():
        return binary
    source = cache / "src"
    if not source.exists():
        subprocess.run(["git", "-C", str(repo_root), "worktree", "add", "--detach", str(source), OLD_HARNESSKIT_TAG],
                       check=True, capture_output=True, text=True, timeout=120)
    subprocess.run(["cargo", "build", "--release", "--offline", "--target-dir", str(cache / "target")],
                   cwd=source, check=True, capture_output=True, text=True, timeout=1200)
    if not binary.is_file():
        raise RuntimeError(f"old harnesskit build produced no binary: {binary}")
    return binary


def _assert_commands(env: dict[str, str], arm: str, binary: Path | None, repo: Path,
                     expected_version: str = "harnesskit 0.2.0") -> str:
    harnesskit_result = subprocess.run(
        ["bash", "-lc", "command -v harnesskit; harnesskit --version"],
        cwd=repo, env=env, text=True, capture_output=True, timeout=30, check=False,
    )
    tools_result = subprocess.run(
        ["bash", "-lc", "command -v codex cargo"],
        cwd=repo, env=env, text=True, capture_output=True, timeout=30, check=False,
    )
    lines = harnesskit_result.stdout.splitlines()
    harnesskit_path = lines[0] if lines else ""
    tool_paths = tools_result.stdout.splitlines()
    if tools_result.returncode != 0 or len(tool_paths) != 2 or any(
        Path(path).parent != Path(env["PATH"]) for path in tool_paths
    ):
        raise RuntimeError(f"{arm} login shell does not expose isolated codex and cargo: {tool_paths!r}")
    if arm == "A":
        if harnesskit_path or harnesskit_result.returncode != 127:
            raise RuntimeError(f"A PATH unexpectedly exposes harnesskit: {harnesskit_path}")
    else:
        if (binary is None or not harnesskit_path
                or Path(harnesskit_path) != Path(env["PATH"]) / "harnesskit"
                or Path(harnesskit_path).resolve() != binary.resolve()):
            raise RuntimeError(f"B PATH does not put release harnesskit first: {harnesskit_path!r}")
        if harnesskit_result.returncode != 0 or lines[1:] != [expected_version]:
            raise RuntimeError(f"B login shell does not run {expected_version}: {lines!r}")
    return harnesskit_path


def _check_home_isolation(home: Path, arm: str) -> None:
    for path in home.rglob("*"):
        if not path.is_file():
            continue
        relative_parts = path.relative_to(home).parts
        allowed_skill = arm == "B" and relative_parts[:2] == ("skills", "harnesskit")
        if not allowed_skill and any(FORBIDDEN_HOME_PARTS.match(part) for part in relative_parts):
            raise RuntimeError(f"temporary CODEX_HOME copied forbidden state: {path}")
        if len(relative_parts) == 1 and relative_parts[0] not in {
            "auth.json",
            "config.toml",
            "models_catalog_override.json",
        }:
            raise RuntimeError(f"unexpected file in temporary CODEX_HOME: {path}")
    skill = home / "skills" / "harnesskit"
    if arm == "A" and skill.exists():
        raise RuntimeError("A CODEX_HOME unexpectedly contains HarnessKit skill")
    if arm == "B" and (not (skill / "SKILL.md").is_file() or not (skill / "references").is_dir()):
        raise RuntimeError("B CODEX_HOME skill is incomplete")


def _prepare_environment(
    *,
    arm: str,
    scenario_id: str,
    run_dir: Path,
    scenario_repo: Path,
    source_home: Path,
    repo_root: Path,
) -> PreparedEnvironment:
    shell_home = run_dir / "home"
    codex_home = run_dir / "codex-home"
    path_bin = run_dir / "bin"
    install_prefix = run_dir / "install-prefix"
    shell_home.mkdir(parents=True)
    codex_home.mkdir(parents=True)
    path_bin.mkdir(parents=True)
    install_prefix.mkdir(parents=True)
    auth = source_home / "auth.json"
    if not auth.is_file():
        raise RuntimeError(f"missing Codex auth file: {auth}")
    _copy_file(auth, codex_home / "auth.json")
    _copy_config(source_home, codex_home, scenario_repo)
    models = source_home / "models_catalog_override.json"
    if models.is_file():
        _copy_file(models, codex_home / "models_catalog_override.json")

    original_path = os.environ.get("PATH", "")
    release_binary: Path | None = None
    adopt_binary: Path | None = None
    expected_version = "harnesskit 0.2.0"
    if arm == "B":
        release_binary = repo_root / "target" / "release" / "harnesskit"
        if not release_binary.is_file() or not os.access(release_binary, os.X_OK):
            raise RuntimeError(f"missing release binary: {release_binary}; run cargo build --release")
        adopt_binary = release_binary
        if scenario_id == "S7":
            # The subject must upgrade this old CLI itself; adoption still
            # indexes with the current release so the tracked state is valid.
            old_binary = _old_harnesskit(repo_root)
            installed_binary = install_prefix / "bin/harnesskit"
            _copy_file(old_binary, installed_binary)
            release_binary = installed_binary
            expected_version = "harnesskit " + OLD_HARNESSKIT_TAG.removeprefix("v")
        (path_bin / "harnesskit").symlink_to(release_binary)
    _link_original_path(original_path, path_bin)
    # Login shells read these files after /etc/profile. Never copy the real
    # user's startup files: they can restore a PATH containing another CLI.
    profile = f"export PATH={shlex.quote(str(path_bin))}\n"
    for name in (".profile", ".bashrc", ".bash_profile"):
        (shell_home / name).write_text(profile, encoding="utf-8")
    (shell_home / ".gitconfig").write_text(
        "[user]\n\tname = Harbourline Platform Team\n\temail = harbourline-e1@example.invalid\n",
        encoding="utf-8",
    )
    env = os.environ.copy()
    env["HOME"] = str(shell_home)
    env["CARGO_HOME"] = os.environ.get("CARGO_HOME", str(Path.home() / ".cargo"))
    env["RUSTUP_HOME"] = os.environ.get("RUSTUP_HOME", str(Path.home() / ".rustup"))
    for name in ("BASH_ENV", "ENV", "ZDOTDIR"):
        env.pop(name, None)
    env["CODEX_HOME"] = str(codex_home)
    # The flattened directory is the complete PATH. Keeping the original
    # directories after it would re-expose the real harnesskit in arm A (and
    # could make command lookup bypass the release binary in arm B).
    env["PATH"] = str(path_bin)
    env["HARNESSKIT_USAGE_LOG"] = "off"
    env["HARNESSKIT_INSTALL_PREFIX"] = str(install_prefix)

    if arm == "B":
        adopt_harnesskit(scenario_repo, scenario_id, adopt_binary, env=env)
        # S7 deliberately starts with the old skill shipped in the scenario.
        # Other B runs use the release skill as their pre-installed surface.
        source_skill = (
            scenario_repo / "agent-surfaces" / "skills" / "harnesskit"
            if scenario_id == "S7"
            else repo_root / "agent-surfaces" / "skills" / "harnesskit"
        )
        target_skill = codex_home / "skills" / "harnesskit"
        if not (source_skill / "SKILL.md").is_file():
            raise RuntimeError(f"missing shared skill: {source_skill / 'SKILL.md'}")
        shutil.copytree(source_skill, target_skill)
    _check_home_isolation(codex_home, arm)
    harnesskit_path = _assert_commands(env, arm, release_binary, scenario_repo, expected_version)
    status = subprocess.run(
        ["git", "status", "--porcelain"], cwd=scenario_repo, env=env,
        text=True, capture_output=True, timeout=30, check=True,
    ).stdout
    if status:
        raise RuntimeError(f"{arm} prepared workspace is dirty: {status!r}")
    return PreparedEnvironment(shell_home, codex_home, path_bin, env, release_binary,
                               install_prefix, harnesskit_path, status)


def _sample_events(scenario: Scenario, arm: str) -> list[dict[str, Any]]:
    events, malformed = read_jsonl(SCRIPT_DIR / "recorded" / "sample.jsonl")
    if malformed or not events:
        raise RuntimeError("recorded/sample.jsonl is missing or invalid")
    recorded_key = "docs/decisions/000-token-refresh.md"
    replacement = scenario.constraint_files[0]

    def replace(value: Any) -> Any:
        if isinstance(value, str):
            value = value.replace(recorded_key, replacement)
            return value.replace("harnesskit context --current", "rg --files") if arm == "A" else value
        if isinstance(value, list):
            return [replace(child) for child in value]
        if isinstance(value, dict):
            return {key: replace(child) for key, child in value.items()}
        return value

    return [replace(event) for event in events]


def _write_jsonl(path: Path, events: list[dict[str, Any]]) -> None:
    path.write_text("".join(json.dumps(event, ensure_ascii=False) + "\n" for event in events), encoding="utf-8")


def _load_grader(scenario_id: str):
    from grader_support import grade

    return grade


def _codex_command(scenario: Scenario, repo: Path, prepared: PreparedEnvironment, last_message: Path) -> list[str]:
    command = ["codex", "exec", "--json", "--skip-git-repo-check", "-C", str(repo), "-s", "workspace-write"]
    if scenario.scenario_id == "S7":
        roots = json.dumps([str(prepared.codex_home / "skills"), str(prepared.install_prefix)], ensure_ascii=False)
        command.extend(["-c", f"sandbox_workspace_write.writable_roots={roots}"])
    command.extend(["-o", str(last_message), scenario.prompt])
    return command


def _run_one(
    *,
    scenario: Scenario,
    arm: str,
    repetition: int,
    fixture: Path,
    out_root: Path,
    source_home: Path,
    repo_root: Path,
    dry_run: bool,
    timeout_seconds: int,
) -> dict[str, Any]:
    run_dir = out_root / scenario.scenario_id / arm / f"rep-{repetition:02d}"
    if run_dir.exists():
        shutil.rmtree(run_dir)
    run_dir.mkdir(parents=True)
    scenario_repo = run_dir / "repo"
    shutil.copytree(fixture, scenario_repo)
    try:
        prepared = _prepare_environment(
            arm=arm,
            scenario_id=scenario.scenario_id,
            run_dir=run_dir,
            scenario_repo=scenario_repo,
            source_home=source_home,
            repo_root=repo_root,
        )
        baseline = snapshot_baseline(scenario_repo)
        (run_dir / "baseline.json").write_text(json.dumps(baseline, ensure_ascii=False, indent=2), encoding="utf-8")
        events_path = run_dir / "events.jsonl"
        last_message_path = run_dir / "last-message.txt"
        started = time.time()
        return_code = 0
        timed_out = False
        if dry_run:
            last_message = apply_recorded_solution(
                scenario.scenario_id,
                scenario_repo,
                codex_home=prepared.codex_home,
                install_prefix=prepared.install_prefix,
                env=prepared.env,
            )
            last_message_path.write_text(last_message + "\n", encoding="utf-8")
            (run_dir / "final_message.txt").write_text(last_message + "\n", encoding="utf-8")
            _write_jsonl(events_path, _sample_events(scenario, arm))
        else:
            command = _codex_command(scenario, scenario_repo, prepared, last_message_path)
            try:
                with events_path.open("w", encoding="utf-8") as output:
                    # codex exec reads extra prompt text from a non-TTY stdin until EOF, so an
                    # inherited stdin that never closes hangs it before the first event.
                    # The npm shim spawns the native binary as a child; kill the whole
                    # session on timeout or the orphan keeps stderr open and blocks us.
                    process = subprocess.Popen(
                        command, env=prepared.env, stdin=subprocess.DEVNULL, stdout=output,
                        stderr=subprocess.PIPE, text=True, start_new_session=True,
                    )
                    try:
                        _, stderr = process.communicate(timeout=min(timeout_seconds, MAX_TIMEOUT_SECONDS))
                    except subprocess.TimeoutExpired:
                        timed_out = True
                        try:
                            os.killpg(process.pid, signal.SIGKILL)
                        except ProcessLookupError:
                            pass
                        _, stderr = process.communicate()
                    return_code = process.returncode
                (run_dir / "stderr.log").write_text(stderr or "", encoding="utf-8")
            except OSError as exc:
                return_code = 127
                (run_dir / "stderr.log").write_text(str(exc), encoding="utf-8")
            if not last_message_path.exists():
                last_message_path.write_text("", encoding="utf-8")
            shutil.copy2(last_message_path, run_dir / "final_message.txt")
        finished = time.time()
        events, malformed = read_jsonl(events_path)
        last_message = last_message_path.read_text(encoding="utf-8", errors="replace")
        grade = _load_grader(scenario.scenario_id)(
            scenario.scenario_id,
            scenario_repo,
            last_message=last_message,
            baseline=baseline,
            harnesskit_binary=(str(prepared.harnesskit_binary) if prepared.harnesskit_binary else None),
            codex_home=prepared.codex_home,
        )
        metrics = extract_metrics(
            events,
            key_files=scenario.key_files,
            constraint_files=scenario.constraint_files,
            repo_root=scenario_repo,
            started_at=None if dry_run else started,
            finished_at=None if dry_run else finished,
        )
        metrics["malformed_events"] = malformed
        metrics["decision_violations"] = grade.decision_violations
        metrics["start_git_status"] = prepared.start_git_status
        metrics["shell_harnesskit_path"] = prepared.shell_harnesskit_path
        metrics.update(grade.process_metrics)
        result = {
            "scenario": scenario.scenario_id,
            "arm": arm,
            "repetition": repetition,
            "dry_run": dry_run,
            "prompt": scenario.prompt,
            "return_code": return_code,
            "timed_out": timed_out,
            "success": grade.success,
            "grade": grade.to_dict(),
            "metrics": metrics,
            "repo": str(scenario_repo),
            "codex_home": str(prepared.codex_home),
            "shell_home": str(prepared.shell_home),
            "start_git_status": prepared.start_git_status,
            "shell_harnesskit_path": prepared.shell_harnesskit_path,
            "harnesskit_failed_commands": metrics["harnesskit_failed_commands"],
            "install_prefix": str(prepared.install_prefix),
            "harnesskit_binary": str(prepared.harnesskit_binary) if prepared.harnesskit_binary else None,
            "events": str(events_path),
            "last_message": str(last_message_path),
            "final_message": str(run_dir / "final_message.txt"),
        }
    except Exception as exc:  # Keep one failed repetition inspectable.
        result = {
            "scenario": scenario.scenario_id,
            "arm": arm,
            "repetition": repetition,
            "dry_run": dry_run,
            "prompt": scenario.prompt,
            "success": False,
            "error": f"{type(exc).__name__}: {exc}",
        }
    (run_dir / "run.json").write_text(json.dumps(result, ensure_ascii=False, indent=2), encoding="utf-8")
    return result


def _ensure_release(repo_root: Path) -> Path:
    binary = repo_root / "target" / "release" / "harnesskit"
    if not binary.is_file() or not os.access(binary, os.X_OK):
        subprocess.run(["cargo", "build", "--release"], cwd=repo_root, check=True, timeout=900)
    if not binary.is_file() or not os.access(binary, os.X_OK):
        raise RuntimeError(f"release build did not produce {binary}")
    return binary


def main() -> int:
    parser = argparse.ArgumentParser(description="Run E1 HarnessKit agent A/B evaluation")
    parser.add_argument("--scenario", required=True, help="S1..S7, comma-separated, or all")
    parser.add_argument("--arm", choices=("A", "B"), required=True)
    parser.add_argument("--reps", type=int, default=5)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--parallel", type=int, default=1)
    parser.add_argument("--dry-run", action="store_true", help="build/assert isolation and score a recorded solution; never call codex")
    parser.add_argument("--codex-home-source", type=Path, help="source daily CODEX_HOME (default: $CODEX_HOME or ~/.codex)")
    parser.add_argument("--timeout-seconds", type=int, default=MAX_TIMEOUT_SECONDS, help="per-run timeout, capped at 45 minutes")
    args = parser.parse_args()
    if args.reps < 1 or args.parallel < 1:
        parser.error("--reps and --parallel must be positive")
    if args.timeout_seconds < 1 or args.timeout_seconds > MAX_TIMEOUT_SECONDS:
        parser.error("--timeout-seconds must be between 1 and 2700")
    ids = scenario_ids(args.scenario)
    invalid = [scenario_id for scenario_id in ids if args.arm not in SCENARIOS[scenario_id].arms]
    if invalid:
        parser.error(f"scenario(s) {', '.join(invalid)} do not support arm {args.arm}")
    source_home = args.codex_home_source or Path(os.environ.get("CODEX_HOME", Path.home() / ".codex"))
    source_home = source_home.expanduser().resolve()
    args.out = args.out.expanduser().resolve()
    repo_root = _repo_root()
    if args.arm == "B":
        _ensure_release(repo_root)
    write_definitions()
    fixture_root = args.out / "_fixtures"
    fixture_root.mkdir(parents=True, exist_ok=True)
    for scenario_id in ids:
        generate_repo(scenario_id, fixture_root / scenario_id, force=True)
    args.out.mkdir(parents=True, exist_ok=True)
    jobs: list[tuple[Scenario, int]] = [
        (SCENARIOS[scenario_id], repetition)
        for scenario_id in ids
        for repetition in range(1, args.reps + 1)
    ]
    results: list[dict[str, Any]] = []
    worker = lambda job: _run_one(
        scenario=job[0],
        arm=args.arm,
        repetition=job[1],
        fixture=fixture_root / job[0].scenario_id,
        out_root=args.out,
        source_home=source_home,
        repo_root=repo_root,
        dry_run=args.dry_run,
        timeout_seconds=args.timeout_seconds,
    )
    if args.parallel == 1:
        results = [worker(job) for job in jobs]
    else:
        with concurrent.futures.ThreadPoolExecutor(max_workers=args.parallel) as executor:
            results = list(executor.map(worker, jobs))
    results.sort(key=lambda item: (item.get("scenario", ""), item.get("repetition", 0)))
    (args.out / "results.json").write_text(json.dumps(results, ensure_ascii=False, indent=2), encoding="utf-8")
    print(json.dumps({"runs": len(results), "passed": sum(bool(item.get("success")) for item in results), "out": str(args.out)}, ensure_ascii=False))
    return 0 if all(item.get("success") for item in results) else 1


if __name__ == "__main__":
    raise SystemExit(main())
