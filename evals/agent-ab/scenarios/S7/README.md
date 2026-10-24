# S7：离线迁移 HarnessKit

## 场景

- 实验组：仅 B
- Prompt（A/B 逐字相同）：把 harnesskit 升级到最新并处理迁移
- 考点：旧 schema、旧 skill、references 和本地 release 的同步

## 关键文件

- .harnesskit/schema.yaml
- agent-surfaces/skills/harnesskit/SKILL.md

## Grader 判定

正确解离线迁移 schema，更新 CODEX_HOME 中实际安装的 skill 与 references，保留产品文档正文，并通过 check；只有 B 组运行。

grader 以场景初始快照为基线，只检查相对 diff 和隐藏 cargo 测试；被试看不到隐藏测试。
