#!/usr/bin/env python3
"""Aggregate E1 run.json files into a Markdown table and JSON report."""

from __future__ import annotations

import argparse
import json
import statistics
from pathlib import Path
from typing import Any, Iterable


def _numbers(rows: Iterable[dict[str, Any]], key: str) -> list[float]:
    values: list[float] = []
    for row in rows:
        value = row.get("metrics", {}).get(key)
        if isinstance(value, (int, float)) and not isinstance(value, bool):
            values.append(float(value))
    return values


def _summary(values: list[float]) -> dict[str, float | None]:
    if not values:
        return {"mean": None, "median": None, "min": None, "max": None}
    return {
        "mean": round(statistics.fmean(values), 3),
        "median": round(statistics.median(values), 3),
        "min": min(values),
        "max": max(values),
    }


def load_runs(root: Path) -> list[dict[str, Any]]:
    runs: list[dict[str, Any]] = []
    for path in sorted(root.glob("*/[AB]/rep-*/run.json")):
        try:
            value = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            continue
        if isinstance(value, dict):
            runs.append(value)
    return runs


def aggregate(runs: list[dict[str, Any]]) -> dict[str, Any]:
    groups: dict[tuple[str, str], list[dict[str, Any]]] = {}
    for row in runs:
        key = (str(row.get("scenario", "?")), str(row.get("arm", "?")))
        groups.setdefault(key, []).append(row)
    rows: list[dict[str, Any]] = []
    for (scenario, arm), values in sorted(groups.items()):
        passed = sum(bool(value.get("success")) for value in values)
        metrics = {
            key: _summary(_numbers(values, key))
            for key in (
                "tool_calls",
                "first_correct_action_before_tool_calls",
                "docs_read_count",
                "wall_time_seconds",
                "decision_violations",
            )
        }
        token_values = [
            value.get("metrics", {}).get("tokens", {}).get("total_tokens")
            for value in values
        ]
        metrics["total_tokens"] = _summary([float(item) for item in token_values if isinstance(item, (int, float))])
        rows.append(
            {
                "scenario": scenario,
                "arm": arm,
                "runs": len(values),
                "successes": passed,
                "success_rate": f"{passed}/{len(values)}" if values else "0/0",
                "metrics": metrics,
                "constraint_read_before_edit": {
                    "read_before_edit": sum(value.get("metrics", {}).get("constraint_read_before_edit") is True for value in values),
                    "runs": len(values),
                },
                "harnesskit_commands": {
                    command: sum(value.get("metrics", {}).get("harnesskit_commands", {}).get(command, 0) for value in values)
                    for command in sorted(
                        {
                            command
                            for value in values
                            for command in value.get("metrics", {}).get("harnesskit_commands", {})
                        }
                    )
                },
            }
        )
    return {"run_count": len(runs), "groups": rows}


def _fmt(value: Any) -> str:
    if not isinstance(value, dict):
        return str(value)
    mean = value.get("mean")
    median = value.get("median")
    low = value.get("min")
    high = value.get("max")
    if mean is None:
        return "—"
    return f"{mean:g} / {median:g} / {low:g}–{high:g}"


def markdown(report: dict[str, Any]) -> str:
    lines = [
        "# Agent A/B evaluation",
        "",
        "Metrics are `mean / median / range`; success is shown as passed/total.",
        "",
        "| 场景 | 组别 | 成功 | 工具调用 | 首个正确动作前 | token 总量 | 读过文档数 | 墙钟秒 | 决策违反 | 先读约束 |",
        "| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |",
    ]
    for row in report.get("groups", []):
        metrics = row["metrics"]
        lines.append(
            "| {scenario} | {arm} | {success_rate} | {tool_calls} | {first} | {tokens} | {docs} | {wall} | {violations} | {constraints} |".format(
                scenario=row["scenario"],
                arm=row["arm"],
                success_rate=row["success_rate"],
                tool_calls=_fmt(metrics["tool_calls"]),
                first=_fmt(metrics["first_correct_action_before_tool_calls"]),
                tokens=_fmt(metrics["total_tokens"]),
                docs=_fmt(metrics["docs_read_count"]),
                wall=_fmt(metrics["wall_time_seconds"]),
                violations=_fmt(metrics["decision_violations"]),
                constraints="{read_before_edit}/{runs}".format(**row["constraint_read_before_edit"]),
            )
        )
    return "\n".join(lines) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description="Summarize E1 run artifacts")
    parser.add_argument("--input", type=Path, required=True)
    parser.add_argument("--markdown", type=Path)
    parser.add_argument("--json-out", "--json", dest="json_out", type=Path)
    args = parser.parse_args()
    report = aggregate(load_runs(args.input))
    table = markdown(report)
    if args.markdown:
        args.markdown.write_text(table, encoding="utf-8")
    if args.json_out:
        args.json_out.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    if not args.markdown:
        print(table, end="")
    if not args.json_out:
        print(json.dumps(report, ensure_ascii=False, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

