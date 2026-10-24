# S3：接手结算改造

## 场景

- 实验组：A、B
- Prompt（A/B 逐字相同）：接着上次没做完的工作继续做
- 考点：两个计划都留在 active 目录时，按生命周期和工作日志选择下一步

## 关键文件

- docs/exec-plans/active/settlement-cutover.md
- src/settlement/window.rs

## Grader 判定

正确解执行现行计划的 90 秒窗口步骤，不推进旧计划或其代码方向；允许归档旧计划或调整其 frontmatter。计划勾选和任意 worklog 交接条目只记为过程指标。

grader 以场景初始快照为基线，只检查相对 diff 和隐藏 cargo 测试；被试看不到隐藏测试。
