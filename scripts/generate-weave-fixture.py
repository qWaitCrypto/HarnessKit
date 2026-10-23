#!/usr/bin/env python3
"""Generate the synthetic W0 Git history in an empty, disposable directory.

Only this script and the expectation corpus belong in the HarnessKit repository;
the generated worktree and its .git directory must stay in temporary storage.
"""

import argparse
import json
import os
from pathlib import Path
import subprocess


AUTHOR = "Weave Fixture"
EMAIL = "weave@example.invalid"
START_TIME = 1767225600  # 2026-01-01 00:00:00 UTC
LANGUAGES = [
    dict(language="rust", path="src/auth/refresh.rs", moved="src/auth/normalize.rs",
         symbol="refresh_token", moving="normalize_token", deleted="legacy_token",
         heading="刷新策略", topic="令牌刷新", prefix="", comment="//",
         main="pub fn refresh_token(expires_in: u64) -> u64 {\n    expires_in + 60\n}\n",
         move="pub fn normalize_token(token: &str) -> String {\n    token.trim().to_owned()\n}\n",
         delete="pub fn legacy_token() -> u64 {\n    0\n}\n"),
    dict(language="typescript", path="web/cache.ts", moved="web/keys.ts",
         symbol="cacheTtl", moving="normalizeKey", deleted="legacyCache",
         heading="缓存有效期", topic="缓存续期", prefix="", comment="//",
         main="export function cacheTtl(age: number): number {\n    return age + 60;\n}\n",
         move="export function normalizeKey(key: string): string {\n    return key.trim();\n}\n",
         delete="export function legacyCache(): number {\n    return 0;\n}\n"),
    dict(language="python", path="worker/retry.py", moved="worker/queues.py",
         symbol="retry_delay", moving="normalize_queue", deleted="legacy_retry",
         heading="重试退避", topic="队列重试", prefix="", comment="#",
         main="def retry_delay(attempt: int) -> int:\n    return attempt + 60\n",
         move="def normalize_queue(queue: str) -> str:\n    return queue.strip()\n",
         delete="def legacy_retry() -> int:\n    return 0\n"),
    dict(language="go", path="gateway/limit.go", moved="gateway/routes.go",
         symbol="limitRequests", moving="normalizeRoute", deleted="legacyLimit",
         heading="请求限流", topic="网关限流", prefix="package gateway\n\n", comment="//",
         main="func limitRequests(count int) int {\n    return count + 60\n}\n",
         move="func normalizeRoute(route string) string {\n    return route\n}\n",
         delete="func legacyLimit() int {\n    return 0\n}\n"),
]


def git_environment():
    # Ignore caller identity, dates, config, index/worktree redirects and hash
    # defaults. Global/system filters, signing and hooks cannot affect history.
    env = {key: value for key, value in os.environ.items()
           if not key.startswith("GIT_")}
    env.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull,
               GIT_CONFIG_SYSTEM=os.devnull, GIT_ATTR_NOSYSTEM="1",
               GIT_AUTHOR_NAME=AUTHOR, GIT_AUTHOR_EMAIL=EMAIL,
               GIT_COMMITTER_NAME=AUTHOR, GIT_COMMITTER_EMAIL=EMAIL,
               LC_ALL="C", TZ="UTC")
    return env


def git(root, *args, timestamp=None):
    env = git_environment()
    if timestamp is not None:
        date = f"{timestamp} +0000"
        env.update(GIT_AUTHOR_DATE=date, GIT_COMMITTER_DATE=date)
    command = ["git", "-c", "core.autocrlf=false", "-c", "core.filemode=false",
               "-c", f"core.hooksPath={os.devnull}",
               "-c", f"core.attributesFile={os.devnull}",
               "-c", f"core.excludesFile={os.devnull}",
               "-c", "commit.gpgSign=false", "-c", "tag.gpgSign=false", *args]
    result = subprocess.run(command, cwd=root, env=env, check=True,
                            capture_output=True, text=True, encoding="utf-8")
    return result.stdout.strip()


def write(root, path, text):
    destination = root / path
    destination.parent.mkdir(parents=True, exist_ok=True)
    with destination.open("w", encoding="utf-8", newline="\n") as output:
        output.write(text)


def doc_paths(language):
    return (f"docs/decisions/{language}.md", f"docs/notes/{language}-pitfalls.md")


def generate(root):
    root = root.resolve()
    root.mkdir(parents=True, exist_ok=True)
    if any(root.iterdir()):
        raise ValueError(f"fixture destination must be empty: {root}")
    git(root, "init", "-q", "--template=", "--initial-branch=main",
        "--object-format=sha1")
    revisions = {}

    def commit(label):
        git(root, "add", "--all")
        git(root, "commit", "-q", "-m", f"weave: {label}",
            timestamp=START_TIME + len(revisions) * 86400)
        git(root, "tag", "--no-sign", label)
        revisions[label] = git(root, "rev-parse", "HEAD")

    write(root, ".gitignore", ".harnesskit/*\n!.harnesskit/schema.yaml\n__pycache__/\n")
    # docs/notes is not a default alpha.7 collection. Keep the fixture's
    # experience documents managed through the existing schema include API.
    write(root, ".harnesskit/schema.yaml",
          "schema_version: 0\nmanaged_root: docs\n\nentrypoints:\n"
          "  agents: AGENTS.md\n  manifest: docs/index.md\n"
          "  project_map: docs/index.md\n\ninclude:\n"
          "  'docs/decisions/**': decisions\n  'docs/notes/**': notes\n")
    write(root, "AGENTS.md", "# Weave fixture\n\nRead [the knowledge map](docs/index.md).\n")
    index = "# Weave knowledge map\n\nAll contents and history are synthetic.\n\n"
    for item in LANGUAGES:
        decision, notes = doc_paths(item["language"])
        write(root, item["path"], item["prefix"] + item["main"] + "\n"
              + item["move"] + "\n" + item["delete"])
        write(root, decision,
              f"---\nstatus: accepted\n---\n\n# {item['topic']}决策\n\n"
              f"{item['topic']}的确定性规则。\n\n## {item['heading']}\n\n"
              f"`{item['path']}#{item['symbol']}` 在输入值上增加 60。\n")
        write(root, notes,
              f"# {item['topic']}经验\n\n{item['topic']}的维护经验。\n\n"
              f"## 规范化\n\n`{item['path']}#{item['moving']}` 保持独立，移动时保持逻辑。\n\n"
              f"## 旧接口\n\n`{item['path']}#{item['deleted']}` 将被删除。\n\n"
              f"## 边界检查\n\n`{item['path']}#{item['symbol']}` 需要覆盖边界值。\n")
        index += f"- [Decision: {item['language']}]({decision[5:]})\n"
        index += f"- [Notes: {item['language']}]({notes[5:]})\n"
    write(root, "docs/index.md", index + "- [Bulk-only notes](notes/bulk-only.md)\n")
    write(root, "docs/notes/bulk-only.md", "# Bulk-only notes\n\nNo semantic relation is implied.\n")
    write(root, "bulk/unrelated.rs", "pub fn unrelated() -> u64 { 1 }\n")
    # Both the initial import and final bulk edit exceed W4's 30-file cutoff.
    # Otherwise the initial import would create every possible doc/code pair.
    for number in range(31):
        write(root, f"bulk/part-{number:02}.txt", f"Synthetic bulk input {number}.\n")
    commit("baseline")

    for item in LANGUAGES:
        path = root / item["path"]
        write(root, item["path"], path.read_text(encoding="utf-8").replace("+ 60", "+ 90"))
    commit("logic")

    for item in LANGUAGES:
        path = root / item["path"]
        text = path.read_text(encoding="utf-8")
        main = item["main"].replace("+ 60", "+ 90")
        lines = main.splitlines(keepends=True)
        lines.insert(1, f"    {item['comment']} Formatting-only checkpoint.\n")
        formatted = "".join(lines).replace("    ", "        ")
        write(root, item["path"], text.replace(main, formatted))
    commit("format")

    for item in LANGUAGES:
        path = root / item["path"]
        text = path.read_text(encoding="utf-8")
        write(root, item["path"], text.replace("\n" + item["move"], ""))
        write(root, item["moved"], item["prefix"] + item["move"])
    commit("moved")

    for item in LANGUAGES:
        path = root / item["path"]
        text = path.read_text(encoding="utf-8")
        write(root, item["path"], text.replace("\n" + item["delete"], ""))
    commit("deleted")

    for item in LANGUAGES:
        path = root / item["path"]
        write(root, item["path"], path.read_text(encoding="utf-8").replace("+ 90", "+ 120"))
        decision, notes = doc_paths(item["language"])
        text = (root / decision).read_text(encoding="utf-8")
        write(root, decision, text.replace("增加 60", "增加 120"))
        text = (root / notes).read_text(encoding="utf-8")
        write(root, notes, text + "\n已复核：输入值增加 120 时仍需检查边界。\n")
        commit(f"reviewed-{item['language']}")

    for number in range(31):
        write(root, f"bulk/part-{number:02}.txt", f"Bulk formatting pass {number}.\n")
    write(root, "bulk/unrelated.rs", "pub fn unrelated() -> u64 { 2 }\n")
    write(root, "docs/notes/bulk-only.md", "# Bulk-only notes\n\nEdited only in a bulk commit.\n")
    commit("bulk")
    return dict(schema_version=1, head=revisions["bulk"], revisions=revisions)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("destination", type=Path, help="empty disposable directory")
    args = parser.parse_args()
    print(json.dumps(generate(args.destination), indent=2))


if __name__ == "__main__":
    main()
