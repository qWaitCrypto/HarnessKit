"""Deterministic E1 graders, hidden fixture tests, and transcript metrics.

Every scenario grader compares the agent result with the generated repository's
baseline. Behavioural checks run private integration tests in a grading copy;
the subject's repository never receives those tests.
"""

from __future__ import annotations

import difflib
import hashlib
import json
import os
import re
import shlex
import shutil
import subprocess
import tempfile
from dataclasses import asdict, dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Iterable, Mapping, Sequence

from scenarios import S6_FACT_GROUPS


@dataclass
class GradeResult:
    scenario: str
    success: bool
    reasons: list[str] = field(default_factory=list)
    decision_violations: int = 0
    changed_files: list[str] = field(default_factory=list)
    changed_docs: list[str] = field(default_factory=list)
    tests_passed: bool | None = None
    hidden_tests_run: bool = False
    process_metrics: dict[str, bool] = field(default_factory=dict)

    def to_dict(self) -> dict[str, Any]:
        return asdict(self)


def _result(
    scenario: str,
    checks: Iterable[tuple[bool, str]],
    *,
    decision_violations: int = 0,
    changed_files: Iterable[str] = (),
    changed_docs: Iterable[str] = (),
    tests_passed: bool | None = None,
    hidden_tests_run: bool = False,
    process_metrics: Mapping[str, bool] | None = None,
) -> GradeResult:
    reasons = [message for ok, message in checks if not ok]
    return GradeResult(
        scenario=scenario,
        success=not reasons,
        reasons=reasons,
        decision_violations=decision_violations,
        changed_files=sorted(set(changed_files)),
        changed_docs=sorted(set(changed_docs)),
        tests_passed=tests_passed,
        hidden_tests_run=hidden_tests_run,
        process_metrics=dict(process_metrics or {}),
    )


def _read(root: Path, relative: str) -> str:
    try:
        return (root / relative).read_text(encoding="utf-8")
    except (FileNotFoundError, UnicodeDecodeError, OSError):
        return ""


def _exists(root: Path, relative: str) -> bool:
    return (root / relative).is_file()


def _sha256(path: Path) -> str | None:
    if not path.is_file():
        return None
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def snapshot_hashes(root: Path, paths: Iterable[str] | None = None) -> dict[str, str | None]:
    if paths is None:
        paths = (
            path.relative_to(root).as_posix()
            for path in root.rglob("*")
            if path.is_file()
            and ".git" not in path.parts
            and "target" not in path.parts
            and not (".harnesskit" in path.parts and "state" in path.parts)
        )
    return {relative: _sha256(root / relative) for relative in sorted(paths)}


def snapshot_baseline(root: Path) -> dict[str, Any]:
    hashes = snapshot_hashes(root)
    texts = {}
    for relative in hashes:
        path = root / relative
        if path.stat().st_size < 1_000_000:
            try:
                texts[relative] = path.read_text(encoding="utf-8")
            except (UnicodeDecodeError, OSError):
                pass
    return {"hashes": hashes, "texts": texts}


def _baseline_hashes(baseline) -> Mapping[str, str | None]:
    return baseline.get("hashes", baseline) if baseline else {}


def _baseline_text(root: Path, path: str, baseline=None) -> str:
    if baseline and "texts" in baseline:
        return baseline["texts"].get(path, "")
    return _read_from_head(root, path)


def _changed_files(root: Path, baseline: Mapping[str, str | None] | None) -> list[str]:
    if baseline is None:
        try:
            output = subprocess.run(
                ["git", "-C", str(root), "status", "--porcelain", "--untracked-files=all"],
                text=True,
                capture_output=True,
                check=True,
                timeout=20,
            ).stdout
        except (OSError, subprocess.SubprocessError):
            return []
        return sorted({line[3:].strip('"') for line in output.splitlines() if len(line) >= 4})
    current = snapshot_hashes(root)
    hashes = _baseline_hashes(baseline)
    return sorted(path for path in set(hashes) | set(current) if hashes.get(path) != current.get(path))


def _git_diff(root: Path, paths: Iterable[str] = (), baseline=None) -> str:
    if baseline and "texts" in baseline:
        return "\n".join(
            "\n".join(difflib.unified_diff(
                _baseline_text(root, path, baseline).splitlines(), _read(root, path).splitlines(),
                fromfile=f"a/{path}", tofile=f"b/{path}", n=0,
            ))
            for path in paths
        )
    command = ["git", "-C", str(root), "diff", "--no-ext-diff", "--unified=0", "HEAD", "--"]
    command.extend(paths)
    try:
        return subprocess.run(command, text=True, capture_output=True, check=True, timeout=30).stdout
    except (OSError, subprocess.SubprocessError):
        return ""


def _added_lines(root: Path, paths: Iterable[str], baseline=None) -> dict[str, list[str]]:
    """Return only lines added relative to the recorded fixture baseline.

    Inspecting added lines is deliberate: a legal retry in an unrelated module
    must not be mistaken for a new retry in a changed scenario path.
    """

    wanted = set(paths)
    result: dict[str, list[str]] = {path: [] for path in wanted}
    current = ""
    for line in _git_diff(root, sorted(wanted), baseline).splitlines():
        if line.startswith("+++ b/"):
            current = line[6:]
            continue
        if current in wanted and line.startswith("+") and not line.startswith("+++"):
            result[current].append(line[1:])
    return result


def _changed_text(root: Path, path: str, baseline=None) -> str:
    diff = _git_diff(root, [path], baseline)
    return "\n".join(
        line[1:]
        for line in diff.splitlines()
        if line.startswith(("+", "-")) and not line.startswith(("+++", "---"))
    )


def _frontmatter_body(text: str) -> str:
    if text.startswith("---\n"):
        end = text.find("\n---", 4)
        if end >= 0:
            return text[end + 4 :]
    return text


_HIDDEN_TESTS: dict[str, str] = {
    "S1": """
        use harbourline_order_service::auth::refresh::{refresh_token, RefreshState};
        use harbourline_order_service::auth::session::renew_session;

        #[test]
        fn e1_refresh_uses_one_provider_operation() {
            let mut state = RefreshState::default();
            assert_eq!(refresh_token(&mut state, "tenant-a"), Ok("session:tenant-a".to_string()));
            assert!(!state.in_flight);
            state.in_flight = true;
            assert!(refresh_token(&mut state, "tenant-a").is_err());
            assert_eq!(state.provider_calls, 1);
            for operation in [refresh_token, renew_session] {
                let mut failed = RefreshState { failures_remaining: 3, ..Default::default() };
                assert!(operation(&mut failed, "tenant-a").is_err());
                assert_eq!(failed.provider_calls, 1);
                assert_eq!(failed.failures_remaining, 2);
                assert!(!failed.in_flight);
            }
        }
    """,
    "S2": """
        use harbourline_order_service::cache::{keys, settlement};

        #[test]
        fn e1_both_cache_consumers_use_namespace_eight() {
            assert_eq!(keys::CACHE_NAMESPACE_VERSION, 8);
            assert_eq!(settlement::CACHE_NAMESPACE_VERSION, 8);
            assert_eq!(keys::namespace_key("cedar", "order-3"), "orders:v8:cedar:order-3");
            let mut entries = harbourline_order_service::cache::entries::Entries::default();
            settlement::refresh_summary(&mut entries, "cedar", "2026-10-18", 1944);
            assert_eq!(entries.get("settlement:v8:cedar:2026-10-18").unwrap(), "1944");
        }
    """,
    "S3": """
        use harbourline_order_service::settlement::window::settlement_window_seconds;

        #[test]
        fn e1_settlement_batch_uses_the_follow_up_window() {
            assert_eq!(settlement_window_seconds(), 90);
            assert_eq!(harbourline_order_service::settlement::window::batch_bucket(179), 1);
            assert_eq!(harbourline_order_service::settlement::window::batch_bucket(180), 2);
        }
    """,
    "S4": """
        use harbourline_order_service::gateway::rate_limit::GATEWAY_REQUESTS_PER_MINUTE;

        #[test]
        fn e1_gateway_allowance_is_the_current_edge_value() {
            assert_eq!(GATEWAY_REQUESTS_PER_MINUTE, 120);
            assert!(harbourline_order_service::gateway::rate_limit::allowed(120));
            assert!(!harbourline_order_service::gateway::rate_limit::allowed(121));
        }
    """,
    "S5": """
        use harbourline_order_service::queue::admission::full_response;

        #[test]
        fn e1_saturated_queue_returns_service_unavailable() {
            assert_eq!(full_response(), 503);
            use harbourline_order_service::queue::admission::{admit, QUEUE_CAPACITY};
            assert_eq!(admit(QUEUE_CAPACITY), Err(503));
            assert_eq!(admit(QUEUE_CAPACITY - 1), Ok(()));
            let mut store = harbourline_order_service::domain::idempotency::IdempotencyStore::default();
            let result = harbourline_order_service::gateway::ingress::submit("cedar", "k", 0, 1, QUEUE_CAPACITY, &mut store);
            assert_eq!(result.unwrap_err(), 503);
        }
    """,
    "S6": """
        #[test]
        fn e1_service_fixture_remains_executable_after_doc_reorg() {
            assert!(harbourline_order_service::process_order(7) > 7);
        }
    """,
    "S7": """
        #[test]
        fn e1_migrated_fixture_still_builds() {
            assert!(harbourline_order_service::process_order(9) > 9);
        }
    """,
}


def _run_hidden_tests(root: Path, scenario: str) -> tuple[bool, bool, str]:
    """Run private tests in a grading copy, and prove that each test executed."""

    if not (root / "Cargo.toml").is_file() or scenario not in _HIDDEN_TESTS:
        return False, False, "场景缺少 Cargo.toml 或隐藏测试"
    expected = re.findall(r"fn (e1_\w+)\(", _HIDDEN_TESTS[scenario])
    with tempfile.TemporaryDirectory(prefix="e1-grade-") as temporary:
        copy = Path(temporary) / "repo"
        shutil.copytree(root, copy, ignore=shutil.ignore_patterns(".git", "target", "release", "state"))
        hidden = copy / "tests/__e1_hidden.rs"
        hidden.parent.mkdir(parents=True, exist_ok=True)
        hidden.write_text(_HIDDEN_TESTS[scenario], encoding="utf-8")
        try:
            result = subprocess.run(
                ["cargo", "test", "--offline", "--test", "__e1_hidden", "--", "--nocapture"], cwd=copy,
                text=True, capture_output=True, timeout=180, check=False,
            )
        except (OSError, subprocess.SubprocessError) as exc:
            return False, False, f"隐藏 cargo test 未运行: {exc}"
        output = result.stdout + result.stderr
        ran = bool(expected) and all(
            re.search(rf"(?m)^test (?:\w+::)*{name} \.\.\. (?:ok|FAILED)$", output)
            for name in expected
        )
        if not ran:
            return False, False, f"cargo test 未运行指定的隐藏测试: {output[-700:]}"
        if result.returncode != 0:
            return False, True, f"隐藏 cargo test 失败: {output[-700:]}"
        try:
            suite = subprocess.run(["cargo", "test", "--offline"], cwd=copy,
                                   text=True, capture_output=True, timeout=180, check=False)
        except (OSError, subprocess.SubprocessError) as exc:
            return False, True, f"场景完整测试未运行: {exc}"
        if suite.returncode != 0:
            return False, True, f"场景 cargo test 失败: {(suite.stdout + suite.stderr)[-700:]}"
        return True, True, ""


def _markdown_links(root: Path, starts: Sequence[str]) -> tuple[set[str], list[str]]:
    link_pattern = re.compile(r"\[[^\]]*\]\(([^)]+)\)")
    visited: set[str] = set()
    missing: list[str] = []
    queue = [Path(start) for start in starts]
    while queue:
        current = queue.pop(0)
        relative = current.as_posix()
        if relative in visited or not (root / current).is_file():
            continue
        visited.add(relative)
        text = _read(root, relative)
        for raw_target in link_pattern.findall(text):
            target = raw_target.strip().split("#", 1)[0]
            if not target or target.startswith(("http://", "https://", "mailto:")):
                continue
            target_path = (root / current.parent / target).resolve()
            try:
                target_relative = target_path.relative_to(root.resolve())
            except ValueError:
                missing.append(f"{relative}: {target}")
                continue
            if not target_path.is_file():
                missing.append(f"{relative}: {target}")
            elif target_relative.as_posix().endswith(".md"):
                queue.append(target_relative)
    return visited, missing


def _paragraphs(text: str) -> list[str]:
    text = re.sub(r"\[([^\]]*)\]\([^)]+\)", r"\1", text)
    return [re.sub(r"\s+", " ", part.strip()) for part in re.split(r"\n\s*\n", text) if part.strip()]


def _similar(a: str, b: str) -> float:
    return difflib.SequenceMatcher(a=a.lower(), b=b.lower()).ratio()


def grade_s1(root: Path, *, last_message: str = "", baseline=None, **_: Any) -> GradeResult:
    changed = _changed_files(root, baseline)
    paths = ("src/auth/refresh.rs", "src/auth/session.rs", "src/auth/credentials.rs")
    added = _added_lines(root, paths, baseline)
    additions = "\n".join(line for lines in added.values() for line in lines)
    additions = re.sub(r"//[^\n]*|/\*.*?\*/", "", additions, flags=re.S)
    additions = re.sub(r'"(?:\\.|[^"\\])*"', '""', additions)
    violation = bool(re.search(r"\b(?:retry\w*|backoff\w*|sleep|loop|while)\b|\bfor\s+\w+\s+in\b", additions, re.I))
    message = last_message.lower()
    explains_decision = (
        ("002-session-renewal.md" in message or "单飞" in message or "single-flight" in message
         or re.search(r"no (?:internal )retr(?:y|ies)|不.{0,4}重试", message))
        and ("调用方" in message or "caller" in message or "request boundary" in message)
        and ("建议" in message or "有界" in message or "bounded" in message or "suggest" in message
             or "instead" in message or "alternative" in message)
    )
    tests, tests_run, detail = _run_hidden_tests(root, "S1")
    checks = [
        (_exists(root, "src/auth/refresh.rs"), "缺少刷新实现"),
        (not violation, "刷新路径新增了重试或退避逻辑"),
        (explains_decision, "最终答复没有指出续期决策并给出替代建议"),
        (tests, detail or "隐藏测试未运行"),
    ]
    return _result("S1", checks, decision_violations=int(violation),
                   changed_files=changed, tests_passed=tests, hidden_tests_run=tests_run)


def grade_s2(root: Path, *, baseline=None, **_: Any) -> GradeResult:
    paths = ("src/cache/keys.rs", "src/cache/settlement.rs")
    changed = _changed_files(root, baseline)
    tests, tests_run, detail = _run_hidden_tests(root, "S2")
    checks = [
        (all(path in changed for path in paths), "缓存命名空间的两个消费者没有一起变更"),
        (re.search(r"(?m)^CACHE_NAMESPACE_VERSION[ \t]*=[ \t]*7[ \t]*(?:#.*)?$",
                   _read(root, "tools/reconciliation_report.py")) is not None,
         "历史报表工具必须保留 v7 缓存命名空间"),
        (tests, detail or "隐藏测试未运行"),
    ]
    return _result("S2", checks, changed_files=changed, tests_passed=tests, hidden_tests_run=tests_run)


def grade_s3(root: Path, *, baseline=None, **_: Any) -> GradeResult:
    changed = _changed_files(root, baseline)
    active_plan = "docs/exec-plans/active/settlement-cutover.md"
    old_plan = "docs/exec-plans/active/provider-routing-review.md"
    old_body = _frontmatter_body(_baseline_text(root, old_plan, baseline))
    pending = set(re.findall(r"(?m)^\s*- \[ \] (.+)$", old_body))
    old_advanced = False
    for path in changed:
        if not path.endswith(".md"):
            continue
        body = _frontmatter_body(_read(root, path))
        if Path(path).name == Path(old_plan).name or re.search(r"(?m)^# Provider routing review\s*$", body):
            checked = set(re.findall(r"(?mi)^\s*- \[x\] (.+)$", body))
            old_advanced |= bool(pending & checked)
    plan_added = _added_lines(root, (active_plan,), baseline)[active_plan]
    worklogs = [path for path in changed if path.endswith(".md") and (
        {"worklog", "worklogs"} & set(Path(path).parts) or Path(path).stem.lower() == "worklog"
    )]
    process_metrics = {
        "active_plan_checkbox_updated": any(
            "- [x]" in line and ("window" in line or "窗口" in line) for line in plan_added
        ),
        "handoff_worklog_recorded": any(
            line.strip() and not line.lstrip().startswith(("#", "---"))
            for lines in _added_lines(root, worklogs, baseline).values() for line in lines
        ),
    }
    trap_added = "\n".join(
        line
        for lines in _added_lines(root, ("src/payments/provider.rs", "src/settlement/reconciliation.rs"), baseline).values()
        for line in lines
    )
    tests, tests_run, detail = _run_hidden_tests(root, "S3")
    checks = [
        ("src/settlement/window.rs" in changed, "没有执行现行计划的结算窗口步骤"),
        (not old_advanced, "勾选了已取代计划的待办事项"),
        (not trap_added.strip(), "执行了旧 provider routing 方向的代码变更"),
        (tests, detail or "隐藏测试未运行"),
    ]
    return _result("S3", checks, changed_files=changed, changed_docs=[p for p in changed if p.endswith(".md")],
                   tests_passed=tests, hidden_tests_run=tests_run, process_metrics=process_metrics)


def grade_s4(root: Path, *, last_message: str = "", baseline=None, **_: Any) -> GradeResult:
    changed = _changed_files(root, baseline)
    additions = _added_lines(root, ("docs/operations/gateway-throttles.md",), baseline).get(
        "docs/operations/gateway-throttles.md", []
    )
    tests, tests_run, detail = _run_hidden_tests(root, "S4")
    current_value = r"(?<!\d)120(?!\d)"
    doc_updated = "docs/operations/gateway-throttles.md" in changed and bool(re.search(
        rf"{current_value}.{{0,50}}(?:request|req|请求)|(?:request|请求).{{0,50}}{current_value}",
        " ".join(additions), re.I
    ))
    old_value = r"(?<!\d)100(?!\d)"
    drift = r"(?:\b(?:outdated|stale|old(?:er)?|obsolete|out[- ]of[- ]date|drift(?:ed|ing)?)\b|过时|旧|过期|漂移)"
    gap = r"(?:[^。！？.!?\n]*|[\s\S]{0,80})"
    drift_explained = re.search(
        rf"{old_value}{gap}{drift}|{drift}{gap}{old_value}", last_message, re.I
    ) is not None
    checks = [
        (doc_updated or drift_explained, "没有更新运维说明或指出旧文档的 100 与当前值不一致"),
        (re.search(current_value, last_message) is not None
         and ("网关" in last_message or "gateway" in last_message.lower()), "最终答复没有报告当前网关值"),
        (tests, detail or "隐藏测试未运行"),
    ]
    return _result("S4", checks, changed_files=changed,
                   changed_docs=[p for p in changed if p.endswith(".md")],
                   tests_passed=tests, hidden_tests_run=tests_run)


def _queue_design_duplicate(root: Path, relative: str) -> bool:
    text = _frontmatter_body(_read(root, relative))
    if not re.search(r"queue|队列", text, re.I) or not re.search(r"503|saturat|full|满", text, re.I):
        return False
    design_directory = bool({"design", "design-docs", "design_docs", "reference", "references"}
                            & set(Path(relative).parts[:-1]))
    current = _paragraphs(_frontmatter_body(_read(root, "docs/design-docs/queue.md")))
    candidate = _paragraphs(text)
    current_prose = next((part for part in current if not part.startswith("#")), "")
    candidate_prose = next((part for part in candidate if not part.startswith("#")), "")
    similar_prose = bool(current_prose and candidate_prose) and _similar(current_prose, candidate_prose) >= 0.8
    change_record = bool({"worklog", "worklogs", "changelog", "changelogs", "notes"}
                         & set(Path(relative).parts[:-1])) or re.search(
        r"worklog|changelog", Path(relative).stem, re.I
    ) is not None
    # A change record can reuse the topic's title. Only copied contract prose
    # makes a record a duplicate; a new contract elsewhere may match the title.
    if change_record:
        return similar_prose
    similar_title = bool(candidate and current) and _similar(candidate[0], current[0]) >= 0.8
    return design_directory or similar_title or similar_prose


def grade_s5(root: Path, *, baseline=None, **_: Any) -> GradeResult:
    changed = _changed_files(root, baseline)
    current_added = "\n".join(_added_lines(root, ("docs/design-docs/queue.md",), baseline).get("docs/design-docs/queue.md", []))
    old = _read(root, "docs/design-docs/queue-v1.md")
    old_baseline = _baseline_text(root, "docs/design-docs/queue-v1.md", baseline)
    hashes = _baseline_hashes(baseline)
    new_docs = [path for path in changed if path.endswith(".md") and path not in hashes
                and not _baseline_text(root, path, baseline)]
    duplicates = [path for path in new_docs if _queue_design_duplicate(root, path)]
    links_seen, missing = _markdown_links(root, ("AGENTS.md", "docs/index.md"))
    tests, tests_run, detail = _run_hidden_tests(root, "S5")
    checks = [
        ("src/queue/admission.rs" in changed, "队列实现没有把满载响应改为 503"),
        ("docs/design-docs/queue.md" in changed and re.search(r"\b503\b", current_added) is not None, "现行 queue 文档没有同步 503"),
        ("docs/design-docs/queue-v1.md" not in changed or _frontmatter_body(old) == _frontmatter_body(old_baseline),
         "修改了已取代 queue-v1 的正文"),
        (not duplicates, f"新增了重复 queue 设计文档: {duplicates}"),
        ("design-docs/queue.md" in _read(root, "docs/index.md"), "docs/index 没有现行 queue 链接"),
        (bool(links_seen) and not missing, f"从入口遍历发现失效链接: {missing[:3]}"),
        (tests, detail or "隐藏测试未运行"),
    ]
    return _result("S5", checks, changed_files=changed,
                   changed_docs=[p for p in changed if p.endswith(".md")],
                   tests_passed=tests, hidden_tests_run=tests_run)


def _read_from_head(root: Path, relative: str) -> str:
    try:
        result = subprocess.run(
            ["git", "-C", str(root), "show", f"HEAD:{relative}"],
            text=True, capture_output=True, check=True, timeout=20
        )
        return result.stdout
    except (OSError, subprocess.SubprocessError):
        return ""


def grade_s6(root: Path, *, baseline=None, **_: Any) -> GradeResult:
    changed = _changed_files(root, baseline)
    reachable, missing = _markdown_links(root, ("AGENTS.md",))
    fact_paragraphs = [part for path in sorted(reachable)
                       for part in _paragraphs(_frontmatter_body(_read(root, path)))]
    paragraphs: list[str] = []
    for path in sorted(reachable):
        paragraphs.extend(part for part in _paragraphs(_frontmatter_body(_read(root, path)))
                          if len(part) >= 60 and not part.startswith(("#", "- ", "```")))
    duplicates = [
        (first, second)
        for index, first in enumerate(paragraphs)
        for second in paragraphs[index + 1:]
        if _similar(first, second) >= 0.86
    ]
    tests, tests_run, detail = _run_hidden_tests(root, "S6")
    missing_facts = [
        fact for fact, concepts in S6_FACT_GROUPS.items()
        if not any(all(any(term in paragraph.lower() for term in alternatives) for alternatives in concepts)
                   for paragraph in fact_paragraphs)
    ]
    checks = [
        (_exists(root, "AGENTS.md") and "AGENTS.md" in changed, "没有修改 AGENTS.md 文档入口"),
        (len(_read(root, "AGENTS.md").splitlines()) <= 120, "AGENTS.md 超过 120 行"),
        (not missing_facts, f"关键产品事实没有从 AGENTS 可达: {missing_facts}"),
        (not duplicates, "入口可达文档包含高度相似的重复段落"),
        (not missing, f"入口可达文档有失效链接: {missing[:3]}"),
        (tests, detail or "隐藏测试未运行"),
    ]
    return _result("S6", checks, changed_files=changed,
                   changed_docs=[p for p in changed if p.endswith(".md")],
                   tests_passed=tests, hidden_tests_run=tests_run)


def _check_json_errors(root: Path, binary_override: str | None = None) -> tuple[bool, str]:
    binary = binary_override or os.environ.get("HARNESSKIT_BIN") or "harnesskit"
    try:
        result = subprocess.run(
            [binary, "check", str(root), "--json"],
            text=True, capture_output=True, timeout=180, check=False,
            env={**os.environ, "HARNESSKIT_USAGE_LOG": "off"},
        )
    except (OSError, subprocess.SubprocessError) as exc:
        return False, f"harnesskit check 未运行: {exc}"
    try:
        report = json.loads(result.stdout)
    except json.JSONDecodeError:
        return False, "harnesskit check 未返回 JSON"
    summary = report.get("summary", {}) if isinstance(report, dict) else {}
    errors = summary.get("error", summary.get("errors"))
    if isinstance(errors, int):
        return errors == 0 and result.returncode == 0, f"check errors={errors}"
    if not isinstance(report, dict) or "findings" not in report:
        return False, "harnesskit check JSON 缺少 error 计数或 findings"
    findings = report["findings"]
    error_findings = [item for item in findings if item.get("severity") == "error"] if isinstance(findings, list) else []
    return not error_findings and result.returncode == 0, f"check error findings={len(error_findings)}"


def _schema_migrated(root: Path, binary: str | None) -> bool:
    try:
        result = subprocess.run(
            [binary or "harnesskit", "migrate", str(root), "--plan", "--json", "--full"],
            text=True, capture_output=True, check=False, timeout=180,
            env={**os.environ, "HARNESSKIT_USAGE_LOG": "off"},
        )
        report = json.loads(result.stdout)
        return result.returncode == 0 and isinstance(report.get("items"), list) and not any(
            item.get("category") == "schema" for item in report["items"]
        )
    except (OSError, subprocess.SubprocessError, json.JSONDecodeError, AttributeError):
        return False


def _skill_version(text: str) -> str | None:
    match = re.search(r"(?m)^\s*version:\s*[\"']?([^\"'\s]+)", text)
    return match.group(1) if match else None


def grade_s7(
    root: Path,
    *,
    baseline=None,
    harnesskit_binary: str | None = None,
    codex_home: Path | None = None,
    **_: Any,
) -> GradeResult:
    changed = _changed_files(root, baseline)
    schema = _read(root, ".harnesskit/schema.yaml")
    def product_doc(path: str) -> bool:
        return path.endswith(".md") and not path.startswith(("agent-surfaces/", ".harnesskit/"))
    current_docs = sorted(path for path in snapshot_hashes(root) if product_doc(path))
    baseline_docs = sorted(path for path in _baseline_hashes(baseline) if product_doc(path))
    prose_unchanged = bool(baseline_docs) and set(current_docs) == set(baseline_docs) and all(
        _frontmatter_body(_baseline_text(root, path, baseline)) == _frontmatter_body(_read(root, path))
        for path in baseline_docs
    )
    installed = _read(codex_home, "skills/harnesskit/SKILL.md") if codex_home else ""
    references_ok = codex_home is not None and all(
        bool(_read(codex_home, f"skills/harnesskit/references/{name}.md").strip())
        for name in ("cli", "migration")
    )
    check_ok, check_detail = _check_json_errors(root, harnesskit_binary)
    tests, tests_run, detail = _run_hidden_tests(root, "S7")
    release_ok = (
        (root / "release/SHA256SUMS").is_file()
        and (root / "release/harnesskit-x86_64-unknown-linux-gnu.tar.gz").is_file()
    )
    checks = [
        (re.search(r"(?m)^schema_version:\s*1\s*$", schema) is not None, "schema 未迁移到版本 1"),
        (_schema_migrated(root, harnesskit_binary), "schema 仍有未完成的结构迁移"),
        (".harnesskit/schema.yaml" in changed, "没有对旧 schema 进行迁移"),
        (_skill_version(installed) == "0.2.0" and references_ok,
         "CODEX_HOME 中没有 0.2.0 skill 与 references"),
        (prose_unchanged, "迁移改写了产品文档正文"),
        (release_ok, "场景没有可用的离线 release 资产"),
        (check_ok, check_detail),
        (tests, detail or "隐藏测试未运行"),
    ]
    return _result("S7", checks, changed_files=changed,
                   changed_docs=[p for p in changed if p.endswith(".md")],
                   tests_passed=tests, hidden_tests_run=tests_run)


GRADERS = {
    "S1": grade_s1, "S2": grade_s2, "S3": grade_s3, "S4": grade_s4,
    "S5": grade_s5, "S6": grade_s6, "S7": grade_s7,
}


def grade(scenario: str, root: Path, **kwargs: Any) -> GradeResult:
    try:
        return GRADERS[scenario](root, **kwargs)
    except KeyError as exc:
        raise ValueError(f"unknown scenario: {scenario}") from exc


def _as_number(value: Any) -> int | float | None:
    return value if isinstance(value, (int, float)) and not isinstance(value, bool) else None


def _timestamp(value: Any) -> float | None:
    if isinstance(value, (int, float)):
        number = float(value)
        return number / 1000 if number > 100_000_000_000 else number
    if not isinstance(value, str):
        return None
    try:
        parsed = datetime.fromisoformat(value.strip().replace("Z", "+00:00"))
        if parsed.tzinfo is None:
            parsed = parsed.replace(tzinfo=timezone.utc)
        return parsed.timestamp()
    except ValueError:
        return None


def _walk_strings(value: Any) -> Iterable[str]:
    if isinstance(value, str):
        yield value
    elif isinstance(value, Mapping):
        for key, child in value.items():
            if str(key).lower() in {"aggregated_output", "output", "stdout", "stderr", "result"}:
                continue
            yield from _walk_strings(child)
    elif isinstance(value, Sequence) and not isinstance(value, (bytes, bytearray)):
        for child in value:
            yield from _walk_strings(child)


def _tool_calls(events: Sequence[Mapping[str, Any]]) -> list[tuple[Mapping[str, Any], str]]:
    calls: list[tuple[Mapping[str, Any], str]] = []
    seen: dict[str, int] = {}
    without_ids: set[str] = set()
    tool_types = {"command_execution", "function_call", "tool_call", "computer_call", "shell_command"}
    for event in events:
        item = event.get("item") if isinstance(event.get("item"), Mapping) else event
        event_type = str(event.get("type", "")).lower()
        item_type = str(item.get("type", "")).lower() if isinstance(item, Mapping) else ""
        if item_type not in tool_types and event_type not in tool_types and not any(
            token in event_type for token in ("tool_call", "function_call", "command_execution")
        ):
            continue
        identity = str(item.get("id") or event.get("id") or "")
        text = "\n".join(_walk_strings(item))
        if identity and identity in seen:
            calls[seen[identity]] = (event, text)
            continue
        if identity:
            seen[identity] = len(calls)
        if not identity:
            fingerprint = f"{item_type}\n{text}"
            if fingerprint in without_ids:
                continue
            without_ids.add(fingerprint)
        calls.append((event, text))
    return calls


def _edit_call(text: str) -> bool:
    return bool(re.search(r"(?:apply_patch|git\s+(?:mv|rm)|\b(?:tee|cat)\s+[^\n]*>|sed\s+-i|write_text|write_bytes|perl\s+-pi|\bmv\s+|\bcp\s+)", text, re.I))


def _read_call(text: str) -> bool:
    return not _edit_call(text) and bool(re.search(
        r"\b(?:cat|head|tail|bat|less|read_file|read_text|Get-Content)\b|\bsed\s+-n\b|"
        r"\brg\s+(?!--files)|\bgit\s+show\b|\bharnesskit\s+(?:query|context|inspect|graph)\b",
        text,
    ))


def _harnesskit_commands(text: str) -> Iterable[str]:
    """Count executed shell commands, excluding prose in patches or arguments."""
    for line in text.splitlines():
        try:
            lexer = shlex.shlex(line, posix=True, punctuation_chars=";&|")
            lexer.whitespace_split = True
            words = list(lexer)
        except ValueError:
            continue
        if not words:
            continue
        if Path(words[0]).name in {"bash", "sh", "zsh"} and any(
            flag.startswith("-") and "c" in flag for flag in words[1:-1]
        ):
            yield from _harnesskit_commands(words[-1])
            continue
        at_command = True
        for index, word in enumerate(words):
            if word and all(character in ";&|" for character in word):
                at_command = True
                continue
            if at_command and (re.match(r"\w+=", word) or word in {"env", "exec", "sudo"}):
                continue
            if at_command and Path(word).name == "harnesskit" and index + 1 < len(words):
                name = words[index + 1]
                if re.fullmatch(r"[a-zA-Z][a-zA-Z0-9_-]*|--version|--help", name):
                    yield name.lstrip("-")
            at_command = False


def extract_metrics(
    events: Sequence[Mapping[str, Any]],
    *,
    key_files: Sequence[str],
    constraint_files: Sequence[str] = (),
    repo_root: Path | None = None,
    started_at: float | None = None,
    finished_at: float | None = None,
) -> dict[str, Any]:
    calls = _tool_calls(events)
    normalized_keys = [key.replace("\\", "/").lstrip("./") for key in key_files]
    constraints = [path.replace("\\", "/").lstrip("./") for path in constraint_files]
    first_correct: int | None = None
    docs_read: set[str] = set()
    harnesskit_commands: dict[str, int] = {}
    read_constraints: set[str] = set()
    edit_seen = False
    markdown_pattern = re.compile(r"[\w./*-]+\.md\b")
    for index, (event, command_text) in enumerate(calls):
        normalized_text = command_text.replace("\\", "/")
        if first_correct is None and any(key in normalized_text or key.lstrip("./") in normalized_text for key in normalized_keys):
            first_correct = index
        item = event.get("item", event)
        succeeded = item.get("exit_code", 0) == 0
        if _read_call(normalized_text) and succeeded:
            read_text = normalized_text
            if re.search(r"\bharnesskit\s+(?:query|context|inspect|graph)\b|\brg\s", normalized_text):
                read_text += "\n" + str(item.get("aggregated_output", ""))
            for match in markdown_pattern.findall(read_text):
                paths = [match.lstrip("./")]
                if repo_root is not None:
                    if Path(match).is_absolute():
                        try:
                            paths = [Path(match).relative_to(repo_root).as_posix()]
                        except ValueError:
                            paths = []
                    elif "*" in match:
                        paths = [path.relative_to(repo_root).as_posix() for path in repo_root.glob(match)]
                    paths = [path for path in paths if (repo_root / path).is_file()]
                docs_read.update(paths)
            if not edit_seen:
                read_constraints.update(path for path in constraints if path in read_text)
        code_path = re.search(r"(?:src|tools|tests)/[^\s'\"<>]+\.(?:rs|py|ts|js|go)\b", normalized_text)
        if _edit_call(normalized_text) and code_path:
            edit_seen = True
        for command in _harnesskit_commands(normalized_text):
            harnesskit_commands[command] = harnesskit_commands.get(command, 0) + 1
    usage: dict[str, int] = {"input_tokens": 0, "cached_input_tokens": 0, "output_tokens": 0, "total_tokens": 0}
    usage_seen: set[str] = set()
    timestamps: list[float] = []
    for event in events:
        for key in ("created_at", "timestamp", "time"):
            parsed = _timestamp(event.get(key))
            if parsed is not None:
                timestamps.append(parsed)
        usage_obj = event.get("usage")
        if not isinstance(usage_obj, Mapping):
            continue
        fingerprint = json.dumps(usage_obj, sort_keys=True, ensure_ascii=False)
        if fingerprint in usage_seen:
            continue
        usage_seen.add(fingerprint)
        aliases = {
            "input_tokens": ("input_tokens", "prompt_tokens"),
            "cached_input_tokens": ("cached_input_tokens", "cache_read_input_tokens"),
            "output_tokens": ("output_tokens", "completion_tokens"),
            "total_tokens": ("total_tokens",),
        }
        for target, names in aliases.items():
            for name in names:
                number = _as_number(usage_obj.get(name))
                if number is not None:
                    usage[target] += int(number)
                    break
    if usage["total_tokens"] == 0:
        usage["total_tokens"] = usage["input_tokens"] + usage["output_tokens"]
    if started_at is not None and finished_at is not None:
        wall_seconds = max(0.0, finished_at - started_at)
    elif len(timestamps) >= 2:
        wall_seconds = max(timestamps) - min(timestamps)
    else:
        wall_seconds = None
    return {
        "tool_calls": len(calls),
        "first_correct_action_before_tool_calls": first_correct,
        "docs_read": sorted(docs_read),
        "docs_read_count": len(docs_read),
        "constraint_read_before_edit": bool(constraints) and set(constraints) <= read_constraints,
        "tokens": usage,
        "wall_time_seconds": wall_seconds,
        "harnesskit_commands": harnesskit_commands,
        "malformed_events": 0,
    }


def read_jsonl(path: Path) -> tuple[list[dict[str, Any]], int]:
    events: list[dict[str, Any]] = []
    malformed = 0
    if not path.is_file():
        return events, malformed
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        if not line.strip():
            continue
        try:
            value = json.loads(line)
        except json.JSONDecodeError:
            malformed += 1
            continue
        if isinstance(value, dict):
            events.append(value)
    return events, malformed
