# S2：结算摘要缓存

## 场景

- 实验组：A、B
- Prompt（A/B 逐字相同）：billing 切到 v8 之后订单列表读不到新写入的缓存，修一下
- 考点：两个真实消费者必须一起迁移缓存命名空间

## 关键文件

- src/cache/keys.rs
- src/cache/settlement.rs
- docs/notes/cache-observation.md

## Grader 判定

正确解让订单缓存与结算缓存一起迁移，并保留历史报表工具的 v7；只改一个消费者或一律改为 v8 都会失败。

grader 以场景初始快照为基线，只检查相对 diff 和隐藏 cargo 测试；被试看不到隐藏测试。
