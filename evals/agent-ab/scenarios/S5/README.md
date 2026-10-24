# S5：队列满载契约

## 场景

- 实验组：A、B
- Prompt（A/B 逐字相同）：把队列满时的返回改成 503，顺便把文档也更新一下
- 考点：现行队列契约与保留的 v1 文档同时出现时的维护范围

## 关键文件

- src/queue/admission.rs
- docs/design-docs/queue.md
- docs/index.md

## Grader 判定

正确解改现行 queue.md 和队列实现，保留 queue-v1 正文及索引，不创建重复设计文档；允许 worklog/changelog/notes 记录变更，隐藏测试验证 503。

grader 以场景初始快照为基线，只检查相对 diff 和隐藏 cargo 测试；被试看不到隐藏测试。
