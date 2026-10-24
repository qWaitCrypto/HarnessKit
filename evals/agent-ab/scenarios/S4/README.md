# S4：网关运行说明

## 场景

- 实验组：A、B
- Prompt（A/B 逐字相同）：网关限流阈值是多少？写给运维一段说明
- 考点：代码曾在文档之后调整，运维说明需要以当前实现为准

## 关键文件

- src/gateway/rate_limit.rs
- docs/operations/gateway-throttles.md

## Grader 判定

正确解在最终答复报告代码当前值，并更新运维说明或指出旧值已漂移；搜索到的其他限流数字不能替代网关值。

grader 以场景初始快照为基线，只检查相对 diff 和隐藏 cargo 测试；被试看不到隐藏测试。
