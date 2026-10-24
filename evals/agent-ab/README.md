# E1 agent A/B 评测

本目录只实现 E1：用离线生成的 Harbourline 多租户订单／支付服务，比较普通 Codex（A）与带 HarnessKit release CLI、skill、索引的 Codex（B）。不修改 HarnessKit 产品实现，不启动真实 provider 评测。Weave 设计只列 W0–W9；E1 的实验定义以 [SPEC.md](SPEC.md) 及本轮返工要求为准。

## 语料与隐藏方式

每个仓库有 45 个固定时间、固定作者的可重建 Git 提交、约 150 个文件、53 个 Rust 源文件。普通场景有 58 篇 `docs/` 文档，包含设计、8 个决策、运维、事故笔记、计划及 worklog，入口和索引互相链接。代码使用真实的 tenant/order/line-item/money 数据，checkout 经过 admission、支付 capture、队列 worker 与 settlement；配置刷新、缓存失效、退款和发货也有调用及测试。基线 `cargo test --offline` 运行 12 项公开测试，无依赖下载。

历史记录保存具体商户案例、运维测量、接口约定与交接证据；没有 background-NN、module_NNN、fixture_NNN、根目录 history 填充物。关键决策不会在其他笔记里重复成为“解题提示”。场景仓库中只有自然英文产品文档；本目录里的 prompt、grader 和解答说明不会复制给被试。

归档的八月 rehearsal 在 8 月 31 日 bootstrap 中加入；带完整日期的 worklog 和标注 Updated 日期的文档在对应日期的提交中新增，包括 10 月 12 日网关 runbook、10 月 18 日结算交接、10 月 22 日队列复核。总提交数仍为 45，旧计划的最后修改仍晚于现行计划。

| 场景 | 隐性陷阱及关键信息位置 | Grader |
| --- | --- | --- |
| S1 | HTTP/队列允许重试；续期决策只在 `docs/decisions/002-session-renewal.md` 说明单飞、一次失败返回调用方，调用方自行决定后续请求。 | 相对基线检查 refresh/session/credentials 路径新增的重试、循环及退避；隐藏测试注入 provider 失败并检查一次调用；最终答复须指出决策及调用方替代建议。允许不改代码，也允许调用方有界重试。 |
| S2 | prompt 只描述订单列表症状；`docs/notes/cache-observation.md` 说明 v8 新数据、两个尚未迁移的读取端及保留 v7 的历史报表。 | 两个 Rust 消费者的 diff 都须改变；隐藏测试校验 v8 订单 key 与真实 settlement summary 写入，Python 报表必须保留 v7。只修一个端或全部改为 v8 都失败。 |
| S3 | prompt 不含工作主题；两个计划同在 `exec-plans/active/`，旧计划的最后修改更晚，只有 frontmatter 标记 superseded 和 superseded_by。 | 成功只看 90 秒窗口步骤及隐藏测试、未执行旧代码方向、未勾选旧计划待办；允许归档旧计划或仅改其 frontmatter。计划勾选与任意 worklog 交接条目只作为过程指标。 |
| S4 | `docs/operations/gateway-throttles.md` 自信写着 100 并标更新日期；之后历史中的网关值提交只改代码。 | 最终答复写当前 120；原文档 diff 更新到 120，或答复中 100 与过时／旧／漂移／stale／outdated 等出现在同句或相隔不超过 80 字符。隐藏测试验证 allowance 与边界。 |
| S5 | `queue.md` 与 `queue-v1.md` 都写 429，v1 仅 frontmatter 标 superseded；两级索引用相同标题中性列出两篇。 | 改实现及现行文档，允许只改 v1 frontmatter，禁止改 v1 正文；新增队列设计／契约文档才判重复（设计／参考目录，或标题／首段相似度 ≥0.8）。worklog/changelog/notes 可复用主题标题记录 503 变更，仅复制契约首段才算重复；链接有效，隐藏测试通过。 |
| S6 | 长 README、53 篇零散现场笔记、重复真实段落和失效链接；事实散落在 README、notes 和 ops。 | AGENTS 必须修改且 ≤120 行；从 AGENTS 递归遍历 Markdown 链接，匹配五组关键事实；用段落相似度（0.86）检查重复，无失效链接，隐藏测试通过。不规定目录或文件名。 |
| S7（仅 B） | alpha.6 schema、已装 `e0deab9` 的原始 alpha.7 skill（未改写的历史快照），仓库旁有真正 0.2.0 release 资产。 | 检查 schema 版本及迁移待办消除、CODEX_HOME skill 0.2.0 与非空 references、docs 正文不变、`check` 0 error；隐藏测试保持服务可跑。录制正确解实际调用本地 installer 和 migrate。 |

共同干扰项包括 HTTP retry、队列 redelivery、provider 503、cache refresh、session renewal、configuration refresh、网关／租户／DB 三种额度，以及 event/config/provider/cache 多个 version 常量。正文没有“先检查实现”“更新现行文档”等人为提示。S3 的 worklog 只记停点，S5 的版本关系只由 frontmatter 表明。

## Grader 与隔离

评分使用运行前保存的文件 hash 和文本快照，包含新增／删除行及改动文件。即使被试自行提交或新增文件，也仍相对该快照评分。现有文档和源码不会被整文件 retry 关键词扫描；S6 可达性、段落去重及 S7 结构化版本字段按各自内容契约检查。

隐藏 Rust 测试只在评分副本中写入，先运行指定 integration target，再运行整个场景测试集。测试名必须确实出现在 libtest 的通过／失败记录里；未执行、未安装 cargo、禁用 autotests 或只输出“0 tests”均失败。被试仓库不会留下隐藏测试。

每次 repetition 都创建独立 repo、CODEX_HOME 和 PATH。只复制 auth、model/provider 配置与 models catalog；不复制日常 memories、sessions、skills 或数据库。A 的 PATH 没有 harnesskit；B 使用 release，预装当前 skill（S7 特意预装旧 skill），并已索引。B 的 AGENTS 仅增加一句真实路由：`This repository uses HarnessKit for documentation; see the HarnessKit skill.` 源码确认 init 会跳过已有入口，故采用返工要求允许的 fallback，其余文档逐字相同。

S7 将可替换 CLI 放进本次运行的独立安装前缀，通过 `HARNESSKIT_INSTALL_PREFIX` 暴露该位置。Codex 仅额外开放 `CODEX_HOME/skills` 和该安装前缀两个 writable roots；不开放共享 release 或整个用户目录。本地 `release/` 含真实二进制、skill、VERSION、SHA256SUMS 和标准 install.sh；tar/gzip 元数据固定，无联网要求。

## 运行与指标

```bash
cargo build --release
python3 evals/agent-ab/run.py --scenario S1,S2,S3,S4,S5,S6 --arm A --reps 1 --out /tmp/e1-a --dry-run
python3 evals/agent-ab/run.py --scenario all --arm B --reps 1 --out /tmp/e1-b --dry-run
pytest -q evals/agent-ab/tests
cargo build
cargo test
```

正式实验由调度者去掉 `--dry-run`，使用日常 provider/model/reasoning 配置，默认每组重复 5 次；可用 `--parallel` 控制并行。`--codex-home-source` 可指定配置来源。dry-run 只重放 transcript 和正确解，不能作为真实 A/B 成功率或收益结论。

每次运行保存 `events.jsonl`、`last-message.txt`、`final_message.txt`、`baseline.json`、`run.json`。后者包含 `constraint_read_before_edit`、不同 Markdown 的 `docs_read`／`docs_read_count`、`harnesskit_commands` 分布、首个关键文件动作前调用数、token、时间和 grader 结果。文档写入、ls 列名、失败读取不会算作读过；首次代码修改前必须读过该场景列出的约束文档。顺序指标根据显式工具调用及可观察的 CLI 阅读路径提取，动态构造路径的任意脚本不保证可识别。

S3 的 `run.json` 在 `grade.process_metrics` 和 `metrics` 中记录 `active_plan_checkbox_updated`、`handoff_worklog_recorded`；这两个布尔值不决定 success，新建当天 worklog 条目也算交接记录。

```bash
python3 evals/agent-ab/summarize.py --input /tmp/e1-b --markdown /tmp/e1-b/summary.md --json-out /tmp/e1-b/summary.json
```

自测对每个场景运行一份正确解和两份错误解（包括可编译、看似合理的部分修复）；另覆盖 S2 一律改 v8、S3 无过程记录／当天新增 worklog／旧计划归档／错误勾选、S4 漂移措辞及距离、S5 合法变更记录及重复契约、S6 自选 handbook 布局、带日期文件的新增时间，以及原有隔离和隐藏测试执行检查。
