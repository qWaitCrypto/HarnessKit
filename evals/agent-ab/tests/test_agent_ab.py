from __future__ import annotations

import hashlib
import json
import re
import shutil
import subprocess
import sys
from pathlib import Path

import pytest


EVAL_ROOT = Path(__file__).resolve().parents[1]
REPO_ROOT = EVAL_ROOT.parents[1]
sys.path.insert(0, str(EVAL_ROOT))

from grader_support import extract_metrics, grade, snapshot_baseline, snapshot_hashes  # noqa: E402
from scenarios import SCENARIOS, apply_recorded_solution, generate_repo  # noqa: E402


@pytest.fixture(scope="session")
def repositories(tmp_path_factory):
    directory = tmp_path_factory.mktemp("harbourline")
    return {scenario: generate_repo(scenario, directory / scenario) for scenario in SCENARIOS}


def _copy_repo(repositories, scenario: str, destination: Path) -> Path:
    shutil.copytree(repositories[scenario], destination, ignore=shutil.ignore_patterns("target"))
    return destination


def _generated(tmp_path: Path, scenario: str) -> Path:
    return generate_repo(scenario, tmp_path / scenario)


def _replace(root: Path, path: str, old: str, new: str) -> None:
    target = root / path
    text = target.read_text()
    assert old in text, (path, old)
    target.write_text(text.replace(old, new, 1))


def _wrong_solution(scenario: str, variant: int, root: Path, codex_home: Path) -> str:
    if scenario == "S1":
        if variant == 0:
            _replace(root, "src/auth/session.rs", "    refresh_token(state, credential)", """    let mut result = refresh_token(state, credential);
    for _ in 0..2 {
        if result.is_ok() { break; }
        result = refresh_token(state, credential);
    }
    result""")
            return apply_recorded_solution("S1", root)
        # A safe implementation still needs an explanation of the decision and
        # an alternative; an unsupported refusal is not task completion.
        return "I left the code alone."
    if scenario == "S2":
        path = "src/cache/keys.rs" if variant == 0 else "src/cache/settlement.rs"
        _replace(root, path, "VERSION: u32 = 7", "VERSION: u32 = 8")
        return "Migrated the cache namespace."
    if scenario == "S3":
        message = "Continued the delivery plan."
        if variant == 1:
            message = apply_recorded_solution(scenario, root)
        path = root / "src/payments/provider.rs"
        path.write_text(path.read_text() + "\npub fn retry_capture(status: u16) -> bool { status == 503 }\n")
        return message
    if scenario == "S4":
        value = "100" if variant == 0 else "600"
        _replace(root, "docs/operations/gateway-throttles.md", "100 requests per minute", f"{value} requests per minute")
        return f"The gateway allows {value} requests per minute."
    if scenario == "S5":
        if variant == 1:
            message = apply_recorded_solution(scenario, root)
        else:
            _replace(root, "src/queue/admission.rs", "\n    429\n", "\n    503\n")
            message = "Updated the admission implementation and queue design."
        _replace(root, "docs/design-docs/queue-v1.md", "HTTP 429", "HTTP 503")
        return message
    if scenario == "S6":
        message = apply_recorded_solution(scenario, root)
        if variant == 0:
            _replace(root, "docs/operations/release.md", "Production releases require two-person review.",
                     "Production releases follow the usual process.")
            _replace(root, "docs/operations/release.md", "a second reviewer confirms", "the operator checks")
        else:
            # Plausible cleanup keeps a lightly reworded duplicate in a note.
            path = root / "notes/order-state.md"
            path.write_text(path.read_text() + "\nTenant data stays isolated by tenant_id in request context, storage keys, cache entries, and export manifests. Background jobs that fail enter a dead-letter queue after their delivery budget is exhausted.\n")
        return message
    if scenario == "S7":
        if variant == 0:
            return "Already current."
        message = apply_recorded_solution(scenario, root, codex_home=codex_home)
        # Syncing only the repo's bundled skill would leave the subject on the
        # old installed skill. Simulate that tempting partial upgrade.
        _replace(codex_home, "skills/harnesskit/SKILL.md", 'version: "0.2.0"', 'version: "0.1.0-alpha.7"')
        return message
    raise AssertionError(scenario)


def _old_home(root: Path, home: Path) -> None:
    home.mkdir(parents=True)
    shutil.copytree(root / "agent-surfaces/skills/harnesskit", home / "skills/harnesskit")


@pytest.mark.parametrize("scenario", list(SCENARIOS))
def test_each_grader_accepts_recorded_solution_and_rejects_two_errors(tmp_path, repositories, scenario):
    binary = str(REPO_ROOT / "target/release/harnesskit")
    correct = _copy_repo(repositories, scenario, tmp_path / "correct")
    baseline = snapshot_baseline(correct)
    home = tmp_path / "correct-home"
    if scenario == "S7":
        _old_home(correct, home)
    message = apply_recorded_solution(scenario, correct, codex_home=home)
    result = grade(scenario, correct, last_message=message, baseline=baseline,
                   codex_home=home, harnesskit_binary=binary)
    assert result.success, result.reasons
    assert result.tests_passed and result.hidden_tests_run
    assert not (correct / "tests/__e1_hidden.rs").exists()

    for variant in (0, 1):
        wrong = _copy_repo(repositories, scenario, tmp_path / f"wrong-{variant}")
        wrong_baseline = snapshot_baseline(wrong)
        wrong_home = tmp_path / f"wrong-home-{variant}"
        if scenario == "S7":
            _old_home(wrong, wrong_home)
        message = _wrong_solution(scenario, variant, wrong, wrong_home)
        result = grade(scenario, wrong, last_message=message, baseline=wrong_baseline,
                       codex_home=wrong_home, harnesskit_binary=binary)
        assert not result.success, (scenario, variant)
        assert result.hidden_tests_run, result.reasons


def test_generation_is_deterministic_and_has_scripted_history(tmp_path: Path) -> None:
    first = _generated(tmp_path / "first", "S1")
    second = _generated(tmp_path / "second", "S1")
    assert int(subprocess.check_output(["git", "-C", str(first), "rev-list", "--count", "HEAD"], text=True)) >= 40
    tracked_files = int(subprocess.check_output(["git", "-C", str(first), "ls-files"], text=True).count("\n"))
    assert 150 <= tracked_files <= 300

    def hashes(root: Path) -> dict[str, str]:
        values = {}
        for path in root.rglob("*"):
            if path.is_file() and ".git" not in path.parts:
                values[path.relative_to(root).as_posix()] = hashlib.sha256(path.read_bytes()).hexdigest()
        return values

    assert hashes(first) == hashes(second)
    assert subprocess.check_output(["git", "-C", str(first), "rev-parse", "HEAD"]) == subprocess.check_output(["git", "-C", str(second), "rev-parse", "HEAD"])
    assert 50 <= len(list(first.glob("docs/**/*.md"))) <= 60
    assert 40 <= len(list(first.glob("src/**/*.rs"))) <= 60
    assert not list(first.glob("**/background-*.md"))
    assert not (first / "src/generated").exists()
    assert not (first / "history").exists()
    assert all("fn step(" not in path.read_text() for path in first.glob("src/**/*.rs"))


def test_recorded_jsonl_metrics_follow_first_key_action_definition() -> None:
    events = [json.loads(line) for line in (EVAL_ROOT / "recorded/sample.jsonl").read_text().splitlines()]
    metrics = extract_metrics(events, key_files=["docs/decisions/000-token-refresh.md"])
    assert metrics["tool_calls"] == 2
    assert metrics["first_correct_action_before_tool_calls"] == 1
    assert metrics["tokens"]["total_tokens"] == 1080
    assert metrics["wall_time_seconds"] == 5.0
    assert metrics["harnesskit_commands"] == {"context": 1}


def test_runner_dry_run_builds_isolated_a_and_b(tmp_path: Path) -> None:
    source_home = tmp_path / "daily-codex"
    source_home.mkdir()
    (source_home / "auth.json").write_text('{"auth_mode":"test"}\n', encoding="utf-8")
    (source_home / "config.toml").write_text(
        "model = \"fixture\"\n\n[projects.\"/unrelated\"]\ntrust_level = \"trusted\"\n\n[model_providers.fixture]\nenv_key = \"FIXTURE_KEY\"\n",
        encoding="utf-8",
    )
    (source_home / "models_catalog_override.json").write_text("{}\n", encoding="utf-8")
    (source_home / "memories").mkdir()
    (source_home / "memories/secret.txt").write_text("must not copy\n", encoding="utf-8")
    (source_home / "state_5.sqlite").write_text("must not copy\n", encoding="utf-8")

    for arm in ("A", "B"):
        out = tmp_path / f"out-{arm}"
        command = [
            sys.executable,
            str(EVAL_ROOT / "run.py"),
            "--scenario",
            "S1",
            "--arm",
            arm,
            "--reps",
            "1",
            "--out",
            str(out),
            "--dry-run",
            "--codex-home-source",
            str(source_home),
        ]
        completed = subprocess.run(command, text=True, capture_output=True, check=False, timeout=180)
        assert completed.returncode == 0, completed.stdout + completed.stderr
        result = json.loads((out / "results.json").read_text())
        assert result[0]["success"] is True
        home = out / "S1" / arm / "rep-01" / "codex-home"
        assert not (home / "memories").exists()
        assert not (home / "state_5.sqlite").exists()
        config = (home / "config.toml").read_text()
        assert "[projects.\"/unrelated\"]" not in config
        assert str(out / "S1" / arm / "rep-01" / "repo") in config
        if arm == "A":
            assert not (home / "skills").exists()
        else:
            assert (home / "skills/harnesskit/SKILL.md").is_file()
            assert (home / "skills/harnesskit/references").is_dir()


def test_summary_emits_markdown_and_json(tmp_path: Path) -> None:
    run = tmp_path / "S1/A/rep-01"
    run.mkdir(parents=True)
    (run / "run.json").write_text(
        json.dumps({
            "scenario": "S1",
            "arm": "A",
            "success": True,
            "metrics": {
                "tool_calls": 2,
                "first_correct_action_before_tool_calls": 1,
                "docs_read_count": 1,
                "wall_time_seconds": 5,
                "decision_violations": 0,
                "tokens": {"total_tokens": 1080},
            },
        }),
        encoding="utf-8",
    )
    completed = subprocess.run(
        [sys.executable, str(EVAL_ROOT / "summarize.py"), "--input", str(tmp_path)],
        text=True,
        capture_output=True,
        check=True,
    )
    assert "| S1 | A | 1/1 |" in completed.stdout
    assert '"run_count": 1' in completed.stdout



@pytest.mark.parametrize('scenario', list(SCENARIOS))
def test_generated_service_baseline_is_green(tmp_path, repositories, scenario):
    root = _copy_repo(repositories, scenario, tmp_path / 'repo')
    result = subprocess.run(['cargo', 'test', '--offline'], cwd=root, text=True,
                            capture_output=True, timeout=180)
    assert result.returncode == 0, result.stdout + result.stderr
    assert 'warning:' not in result.stderr
    counts = [int(count) for count in re.findall(r'test result: ok\. (\d+) passed', result.stdout)]
    assert sum(counts) == 12
    assert not list(root.glob('tests/*hidden*'))


def test_s1_allows_bounded_caller_retry_and_ignores_transport_distractors(tmp_path, repositories):
    root = _copy_repo(repositories, 'S1', tmp_path / 'repo')
    baseline = snapshot_baseline(root)
    message = apply_recorded_solution('S1', root)
    _replace(root, 'src/gateway/session_access.rs', '    renew_session(state, credential)', '''    let mut outcome = renew_session(state, credential);
    for _ in 0..2 {
        if outcome.is_ok() { break; }
        outcome = renew_session(state, credential);
    }
    outcome''')
    assert 'retry_request' in (root / 'src/clients/http.rs').read_text()
    result = grade('S1', root, baseline=baseline, last_message=message)
    assert result.success, result.reasons
    assert result.decision_violations == 0


def test_grader_uses_saved_baseline_even_after_subject_commits(tmp_path, repositories):
    root = _copy_repo(repositories, 'S2', tmp_path / 'repo')
    baseline = snapshot_baseline(root)
    message = apply_recorded_solution('S2', root)
    subprocess.run(['git', '-C', str(root), 'add', '-A'], check=True)
    subprocess.run(['git', '-C', str(root), 'commit', '-qm', 'Migrate cache readers'], check=True)
    assert not subprocess.check_output(['git', '-C', str(root), 'diff', 'HEAD'])
    result = grade('S2', root, baseline=baseline, last_message=message)
    assert result.success, result.reasons


def test_s3_history_makes_superseded_plan_newer(tmp_path, repositories):
    root = repositories['S3']
    old = 'docs/exec-plans/active/provider-routing-review.md'
    current = 'docs/exec-plans/active/settlement-cutover.md'
    def timestamp(path):
        return int(subprocess.check_output(['git', '-C', str(root), 'log', '-1', '--format=%ct', '--', path]))
    assert timestamp(old) > timestamp(current)
    assert (root / old).read_text().startswith('---\nstatus: superseded\nsuperseded_by: settlement-cutover.md\n---')
    assert '- [ ] Add bounded capture retry' in (root / old).read_text()


def test_prompts_and_queue_indexes_do_not_reveal_scenario_answers(repositories):
    assert not re.search('结算|settlement', SCENARIOS['S3'].prompt, re.I)
    assert not re.search('结算|settlement', SCENARIOS['S2'].prompt, re.I)
    assert '订单列表' in SCENARIOS['S2'].prompt and 'v8' in SCENARIOS['S2'].prompt
    assert not re.search('现在|现行|current', SCENARIOS['S5'].prompt, re.I)
    for path in ['docs/index.md', 'docs/design-docs/index.md']:
        text = (repositories['S5'] / path).read_text()
        links = re.findall(r'\[([^]]+)\]\((?:design-docs/)?queue(?:-v1)?\.md\)', text)
        assert len(links) == 2 and links[0] == links[1]
        assert 'current queue contract' not in text.lower()


def test_s2_rejects_blanket_namespace_migration(tmp_path, repositories):
    root = _copy_repo(repositories, 'S2', tmp_path / 'repo')
    baseline = snapshot_baseline(root)
    message = apply_recorded_solution('S2', root)
    _replace(root, 'tools/reconciliation_report.py', 'CACHE_NAMESPACE_VERSION = 7',
             'CACHE_NAMESPACE_VERSION = 8')
    result = grade('S2', root, baseline=baseline, last_message=message)
    assert not result.success and result.tests_passed and result.hidden_tests_run
    assert any('历史报表' in reason and 'v7' in reason for reason in result.reasons)


def test_s3_accepts_code_only_and_reports_process_metrics(tmp_path, repositories):
    root = _copy_repo(repositories, 'S3', tmp_path / 'repo')
    baseline = snapshot_baseline(root)
    _replace(root, 'src/settlement/window.rs', 'SETTLEMENT_WINDOW_SECONDS: u32 = 60',
             'SETTLEMENT_WINDOW_SECONDS: u32 = 90')
    result = grade('S3', root, baseline=baseline)
    assert result.success, result.reasons
    assert result.process_metrics == {
        'active_plan_checkbox_updated': False, 'handoff_worklog_recorded': False,
    }


def test_s3_accepts_new_handoff_worklog(tmp_path, repositories):
    root = _copy_repo(repositories, 'S3', tmp_path / 'repo')
    baseline = snapshot_baseline(root)
    _replace(root, 'src/settlement/window.rs', 'SETTLEMENT_WINDOW_SECONDS: u32 = 60',
             'SETTLEMENT_WINDOW_SECONDS: u32 = 90')
    (root / 'docs/worklog/active/2026-10-24-settlement.md').write_text(
        '# Settlement handoff\n\nSet the batch window to 90 seconds; staging rehearsal is next.\n')
    result = grade('S3', root, baseline=baseline)
    assert result.success, result.reasons
    assert result.process_metrics['handoff_worklog_recorded'] is True
    assert result.process_metrics['active_plan_checkbox_updated'] is False
    assert 'docs/worklog/active/2026-10-18-settlement.md' not in result.changed_docs


def test_s3_runner_records_process_metrics(tmp_path, repositories):
    from run import _run_one
    source = tmp_path / 'daily-home'
    source.mkdir()
    (source / 'auth.json').write_text('{"auth_mode":"fixture"}\n')
    (source / 'config.toml').write_text('model = "fixture"\nmodel_reasoning_effort = "max"\n')
    result = _run_one(scenario=SCENARIOS['S3'], arm='A', repetition=1,
                      fixture=repositories['S3'], out_root=tmp_path / 'out', source_home=source,
                      repo_root=REPO_ROOT, dry_run=True, timeout_seconds=2700)
    assert result['success'], result.get('grade', result)
    recorded = json.loads((tmp_path / 'out/S3/A/rep-01/run.json').read_text())
    for name in ['active_plan_checkbox_updated', 'handoff_worklog_recorded']:
        assert recorded['metrics'][name] is True
        assert recorded['grade']['process_metrics'][name] is True


@pytest.mark.parametrize('archive', [False, True])
def test_s3_accepts_superseded_plan_metadata_and_archiving(tmp_path, repositories, archive):
    root = _copy_repo(repositories, 'S3', tmp_path / 'repo')
    baseline = snapshot_baseline(root)
    message = apply_recorded_solution('S3', root)
    old = 'docs/exec-plans/active/provider-routing-review.md'
    if archive:
        destination = 'docs/exec-plans/completed/provider-routing-review.md'
        subprocess.run(['git', '-C', str(root), 'mv', old, destination], check=True)
        _replace(root, destination, 'superseded_by: settlement-cutover.md',
                 'superseded_by: ../active/settlement-cutover.md')
        _replace(root, 'docs/exec-plans/active/index.md',
                 '- [Provider routing review](provider-routing-review.md)', '')
        with (root / 'docs/exec-plans/completed/index.md').open('a') as handle:
            handle.write('\n- [Provider routing review](provider-routing-review.md)\n')
    else:
        _replace(root, old, 'status: superseded', 'status: superseded\nauthority: supporting')
    result = grade('S3', root, baseline=baseline, last_message=message)
    assert result.success, result.reasons


@pytest.mark.parametrize('archive', [False, True])
def test_s3_rejects_completing_superseded_checkbox(tmp_path, repositories, archive):
    root = _copy_repo(repositories, 'S3', tmp_path / 'repo')
    baseline = snapshot_baseline(root)
    message = apply_recorded_solution('S3', root)
    old = 'docs/exec-plans/active/provider-routing-review.md'
    if archive:
        destination = 'docs/exec-plans/completed/provider-routing-review.md'
        subprocess.run(['git', '-C', str(root), 'mv', old, destination], check=True)
        old = destination
    _replace(root, old, '- [ ] Add bounded capture retry', '- [x] Add bounded capture retry')
    result = grade('S3', root, baseline=baseline, last_message=message)
    assert not result.success and result.tests_passed
    assert any('勾选了已取代计划' in reason for reason in result.reasons)


def test_dated_documents_are_added_near_their_recorded_dates(repositories):
    checked = set()
    for root in repositories.values():
        for path in root.rglob('*.md'):
            if '.git' in path.parts:
                continue
            recorded = re.search(r'\d{4}-\d{2}(?:-\d{2})?', path.name) or re.search(
                r'(?m)^Updated:\s*(\d{4}-\d{2}-\d{2})', path.read_text())
            if not recorded:
                continue
            expected = recorded.group(recorded.lastindex or 0)
            relative = path.relative_to(root).as_posix()
            added = subprocess.check_output(
                ['git', '-C', str(root), 'log', '--diff-filter=A', '--format=%cs', '--', relative],
                text=True).strip()
            assert added.startswith(expected), (relative, expected, added)
            checked.add(relative)
    assert {'docs/worklog/active/2026-10-18-settlement.md',
            'docs/worklog/active/2026-10-22-queue.md',
            'docs/worklog/archive/2026-08-provider.md',
            'docs/operations/gateway-throttles.md'} <= checked


def test_s4_gateway_change_did_not_touch_runbook(repositories):
    root = repositories['S4']
    revision = subprocess.check_output(['git', '-C', str(root), 'log', '-1', '--format=%H', '--',
                                      'src/gateway/rate_limit.rs'], text=True).strip()
    paths = subprocess.check_output(['git', '-C', str(root), 'show', '--format=', '--name-only', revision], text=True)
    assert paths.strip() == 'src/gateway/rate_limit.rs'
    assert '100 requests per minute' in (root / 'docs/operations/gateway-throttles.md').read_text()


@pytest.mark.parametrize('directory', ['docs/operations', 'docs/notes'])
def test_s5_allows_old_frontmatter_and_rejects_duplicate_with_unrelated_name(tmp_path, repositories, directory):
    root = _copy_repo(repositories, 'S5', tmp_path / 'repo')
    baseline = snapshot_baseline(root)
    message = apply_recorded_solution('S5', root)
    _replace(root, 'docs/design-docs/queue-v1.md', 'superseded_by: queue.md',
             'superseded_by: queue.md\nauthority: supporting')
    result = grade('S5', root, baseline=baseline, last_message=message)
    assert result.success, result.reasons
    duplicate = root / directory / 'capacity-handbook.md'
    duplicate.write_text((root / 'docs/design-docs/queue.md').read_text())
    result = grade('S5', root, baseline=baseline, last_message=message)
    assert not result.success
    assert any('重复 queue' in reason for reason in result.reasons)


@pytest.mark.parametrize('path', ['docs/worklog/2026-10-24-queue.md', 'CHANGELOG.md',
                                  'docs/notes/queue-rollout.md'])
def test_s5_allows_change_records_mentioning_queue_503(tmp_path, repositories, path):
    root = _copy_repo(repositories, 'S5', tmp_path / 'repo')
    baseline = snapshot_baseline(root)
    message = apply_recorded_solution('S5', root)
    record = root / path
    record.parent.mkdir(parents=True, exist_ok=True)
    record.write_text('# Queue behavior\n\nChanged queue admission when full to HTTP 503; 队列满改为 503。\n')
    result = grade('S5', root, baseline=baseline, last_message=message)
    assert result.success, result.reasons


def test_s5_rejects_a_second_queue_contract_in_design_directory(tmp_path, repositories):
    root = _copy_repo(repositories, 'S5', tmp_path / 'repo')
    baseline = snapshot_baseline(root)
    message = apply_recorded_solution('S5', root)
    (root / 'docs/design-docs/capacity-contract.md').write_text(
        '# Capacity contract\n\nThe queue must return 503 whenever admission finds it full.\n')
    result = grade('S5', root, baseline=baseline, last_message=message)
    assert not result.success and result.tests_passed
    assert any('重复 queue' in reason for reason in result.reasons)


def test_s6_accepts_a_layout_without_docs_index_or_project(tmp_path, repositories):
    root = _copy_repo(repositories, 'S6', tmp_path / 'repo')
    baseline = snapshot_baseline(root)
    apply_recorded_solution('S6', root)
    (root / 'docs').rename(root / 'handbook')
    for relative in snapshot_hashes(root):
        if relative.endswith('.md'):
            path = root / relative
            path.write_text(path.read_text().replace('docs/', 'handbook/'))
    result = grade('S6', root, baseline=baseline)
    assert not (root / 'docs').exists()
    assert result.success, result.reasons


@pytest.mark.parametrize('language', ['English', 'Chinese'])
def test_s6_accepts_natural_paraphrase_and_preserves_unique_notes(tmp_path, repositories, language):
    root = _copy_repo(repositories, 'S6', tmp_path / 'repo')
    baseline = snapshot_baseline(root)
    apply_recorded_solution('S6', root)
    if language == 'English':
        prose = '''# Service invariants

Creating an order requires an idempotent request key. Events are delivered at least once and consumers store their receipt identifiers before acknowledging.

Storage rows are separated by tenant id; background tasks that fail are moved to a dead letter queue when the delivery budget runs out.
'''
    else:
        prose = '''# 服务约束

订单创建必须使用幂等键，重放返回首次结果。事件总线至少一次投递，消费者先保存事件标识再确认消息。

数据按租户标识隔离。后台任务失败且耗尽投递次数后进入死信队列，人工重放保留原始关联标识。
'''
    (root / 'docs/design/order-invariants.md').write_text(prose)
    result = grade('S6', root, baseline=baseline)
    assert result.success, result.reasons
    original_notes = baseline['texts']
    for path, old in original_notes.items():
        if path.startswith('notes/') and path.endswith('.md') and Path(path).stem not in {
            'index', 'order-creation', 'event-delivery', 'tenant-storage', 'job-failures', 'production-review',
        }:
            for paragraph in old.strip().split('\n\n')[1:]:
                assert paragraph in (root / path).read_text()
    if language == 'Chinese':
        (root / 'docs/operations/release.md').write_text('# 发布\n\n生产发布需要双人复核；发布负责人记录修订与迁移结果，第二位审核人核对冒烟测试和租户导出记录。\n')
        assert grade('S6', root, baseline=baseline).success


def test_s6_rejects_unreachable_facts_and_broken_links(tmp_path, repositories):
    root = _copy_repo(repositories, 'S6', tmp_path / 'repo')
    baseline = snapshot_baseline(root)
    apply_recorded_solution('S6', root)
    _replace(root, 'docs/index.md', '[release guide](operations/release.md)', 'release guide')
    (root / 'notes/production-review.md').write_text('# Release handoff\n\nThe worksheet is filed with the operations shift.\n')
    result = grade('S6', root, baseline=baseline)
    assert not result.success
    assert any('release review' in reason for reason in result.reasons)
    (root / 'AGENTS.md').write_text('[Start here](missing.md)\n')
    result = grade('S6', root, baseline=baseline)
    assert any('失效链接' in reason for reason in result.reasons)


@pytest.mark.parametrize('mode', ['empty', 'unavailable', 'disabled-autotests'])
def test_hidden_tests_must_actually_run(tmp_path, repositories, monkeypatch, mode):
    import grader_support
    root = _copy_repo(repositories, 'S5', tmp_path / 'repo')
    baseline = snapshot_baseline(root)
    message = apply_recorded_solution('S5', root)
    if mode == 'disabled-autotests':
        _replace(root, 'Cargo.toml', 'edition = "2021"', 'edition = "2021"\nautotests = false')
    else:
        def no_tests(*args, **kwargs):
            if mode == 'unavailable':
                raise FileNotFoundError('cargo unavailable')
            return subprocess.CompletedProcess(args[0], 0, 'test result: ok. 0 passed; 0 failed\n', '')
        monkeypatch.setattr(grader_support.subprocess, 'run', no_tests)
    result = grade('S5', root, baseline=baseline, last_message=message)
    assert not result.success
    assert result.tests_passed is False
    assert result.hidden_tests_run is False
    assert not (root / 'tests/__e1_hidden.rs').exists()


def test_metrics_count_reads_and_constraint_order_instead_of_mentions():
    def call(index, command, output='', exit_code=0):
        return {'type': 'item.completed', 'item': {'id': str(index), 'type': 'command_execution',
                'command': command, 'aggregated_output': output, 'exit_code': exit_code}}
    decision = 'docs/decisions/002-session-renewal.md'
    events = [
        call(0, f'ls {decision}'),
        call(1, 'cat > docs/worklog.md << EOF\nDraft\nEOF'),
        call(2, f'cat {decision}'),
        call(3, 'harnesskit context --current', 'docs/project.md'),
        call(4, 'apply_patch *** Update File: src/auth/refresh.rs'),
        call(5, 'cat docs/missing.md', exit_code=1),
        call(6, 'apply_patch *** Update File: docs/commands.md\n+Use harnesskit query to find decisions.'),
        call(7, 'printf "harnesskit inspect is described here"'),
        call(8, '/bin/bash -lc \'harnesskit check --json && harnesskit --version\''),
    ]
    metrics = extract_metrics(events, key_files=[decision], constraint_files=[decision])
    assert metrics['constraint_read_before_edit'] is True
    assert metrics['docs_read'] == [decision, 'docs/project.md']
    assert metrics['docs_read_count'] == 2
    assert metrics['harnesskit_commands'] == {'context': 1, 'check': 1, 'version': 1}
    metrics = extract_metrics([events[4], events[2]], key_files=[decision], constraint_files=[decision])
    assert metrics['constraint_read_before_edit'] is False
    metrics = extract_metrics([call(0, f'apply_patch *** Update File: src/auth/refresh.rs\n+// {decision}')],
                              key_files=[decision], constraint_files=[decision])
    assert metrics['docs_read_count'] == 0
    assert metrics['constraint_read_before_edit'] is False


def test_s7_has_offline_assets_old_installed_skill_and_only_two_extra_roots(tmp_path, repositories):
    from run import _prepare_environment, _codex_command
    root = _copy_repo(repositories, 'S7', tmp_path / 'repo')
    source = tmp_path / 'daily-home'
    source.mkdir()
    (source / 'auth.json').write_text('{"auth_mode":"fixture"}\n')
    (source / 'config.toml').write_text('model = "fixture"\nmodel_reasoning_effort = "max"\n')
    prepared = _prepare_environment(arm='B', scenario_id='S7', run_dir=tmp_path / 'run',
                                    scenario_repo=root, source_home=source, repo_root=REPO_ROOT)
    installed = prepared.codex_home / 'skills/harnesskit/SKILL.md'
    assert '0.1.0-alpha.7' in installed.read_text()
    assert prepared.harnesskit_binary == prepared.install_prefix / 'bin/harnesskit'
    command = _codex_command(SCENARIOS['S7'], root, prepared, tmp_path / 'last.txt')
    override = command[command.index('-c') + 1]
    roots = json.loads(override.split('=', 1)[1])
    assert roots == [str(prepared.codex_home / 'skills'), str(prepared.install_prefix)]
    archive = root / 'release/harnesskit-x86_64-unknown-linux-gnu.tar.gz'
    assert hashlib.sha256(archive.read_bytes()).hexdigest() == (root / 'release/SHA256SUMS').read_text().split()[0]
    shared_binary = REPO_ROOT / 'target/release/harnesskit'
    original_binary_hash = hashlib.sha256(shared_binary.read_bytes()).hexdigest()
    baseline = snapshot_baseline(root)
    message = apply_recorded_solution('S7', root, codex_home=prepared.codex_home, install_prefix=prepared.install_prefix)
    result = grade('S7', root, baseline=baseline, last_message=message,
                   codex_home=prepared.codex_home, harnesskit_binary=str(prepared.harnesskit_binary))
    assert result.success, result.reasons
    assert '0.2.0' in installed.read_text()
    assert (installed.parent / 'references/migration.md').read_text()
    assert hashlib.sha256(shared_binary.read_bytes()).hexdigest() == original_binary_hash
    assert (prepared.codex_home / 'config.toml').read_text().count('model_reasoning_effort = "max"') == 1
    # Merely changing schema_version must not masquerade as complete migration.
    (root / '.harnesskit/schema.yaml').write_text(baseline['texts']['.harnesskit/schema.yaml'].replace('schema_version: 0', 'schema_version: 1'))
    result = grade('S7', root, baseline=baseline, codex_home=prepared.codex_home,
                   harnesskit_binary=str(prepared.harnesskit_binary))
    assert not result.success
    assert any('结构迁移' in reason for reason in result.reasons)


def test_s7_release_is_reproducible(tmp_path, repositories):
    second = generate_repo('S7', tmp_path / 'second')
    for name in ['SHA256SUMS', 'harnesskit-x86_64-unknown-linux-gnu.tar.gz']:
        assert hashlib.sha256((repositories['S7'] / 'release' / name).read_bytes()).hexdigest() == hashlib.sha256((second / 'release' / name).read_bytes()).hexdigest()


def test_s4_allows_an_operations_answer_that_identifies_doc_drift(tmp_path, repositories):
    root = _copy_repo(repositories, 'S4', tmp_path / 'repo')
    baseline = snapshot_baseline(root)
    result = grade('S4', root, baseline=baseline,
                   last_message='当前网关标准桶限额为每分钟 120 次；运维文档仍写 100，已过期。超额请求返回 429，租户每日配额是另一项独立限制。')
    assert result.success, result.reasons
    assert not result.changed_files


@pytest.mark.parametrize('explanation,accepted', [
    ('运维文档参考数字为 100。', False),
    ('The document also mentions 100 requests.', False),
    ('过时记录已归档。' + 'x' * 81 + '文档参考数字为 100。', False),
    ('旧值为 1100，文档仅供参考。', False),
    ('文档写 100，已过时。', True),
    ('The outdated runbook says 100 requests per minute.', True),
    ('The runbook has the old threshold of 100 requests per minute.', True),
    ('The runbook is out of date: 100 requests per minute.', True),
    ('文档写 100。该值已漂移。', True),
    ('文档写 100' + ' 与实现核对' * 20 + '后确认已过时。', True),
])
def test_s4_requires_an_explicit_drift_explanation(tmp_path, repositories, explanation, accepted):
    root = _copy_repo(repositories, 'S4', tmp_path / 'repo')
    baseline = snapshot_baseline(root)
    result = grade('S4', root, baseline=baseline,
                   last_message='当前网关限流为每分钟 120 次。' + explanation)
    assert result.success is accepted, result.reasons
    assert result.tests_passed and result.hidden_tests_run


@pytest.mark.parametrize('value', [100, 1120])
def test_s4_rejects_noncurrent_gateway_values(tmp_path, repositories, value):
    root = _copy_repo(repositories, 'S4', tmp_path / 'repo')
    baseline = snapshot_baseline(root)
    _replace(root, 'docs/operations/gateway-throttles.md', '100 requests per minute',
             f'{value} requests per minute')
    result = grade('S4', root, baseline=baseline,
                   last_message=f'当前网关为每分钟 {value} 次，运维文档已同步。')
    assert not result.success and result.tests_passed
    assert any('没有报告当前网关值' in reason for reason in result.reasons)
