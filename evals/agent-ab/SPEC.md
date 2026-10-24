# E1：有无 HarnessKit 的 agent 工作对照评测

目的：量化 HarnessKit 对 agent 日常工作的便捷度与精准度影响；暴露 skill 提示词的缺口，作为改进 SKILL.md / references 的依据。对应设计文档 §11 的“交接”评测行，并扩展到更多场景。

## 实验设计

- 被试：`codex exec`（与日常同模型、同 provider），每个场景 × 每个实验组 × 重复 5 次；被试使用日常配置(推理强度 max),不降配。汇总时给出每项指标的均值、中位数与范围,成功率给出 x/5。
- 两组只差 HarnessKit 本身：
  - **对照组 A**：仓库内容完全相同（相同文档），但 PATH 中没有 harnesskit，没有 skill，AGENTS.md 中不提 harnesskit，无 `.harnesskit/`。
  - **实验组 B**：harnesskit（本仓库 release 构建）在 PATH，skill 安装到被试的 CODEX_HOME，AGENTS.md 中按 `harnesskit init` 的写法提及，`.harnesskit/` 已索引。
- 隔离：每次运行使用全新的假 HOME（仅生成设置扁平 PATH 的 shell 启动文件和最小 Git 身份）及临时 CODEX_HOME（只复制 auth.json、config.toml 中的 provider/model 配置、models catalog；**不复制 memories、sessions、skills**），CARGO_HOME、RUSTUP_HOME 保留真实工具缓存位置。以被试同样的 `bash -lc` 断言 CLI 路径和版本，使用全新的场景仓库副本。被试 prompt 两组逐字相同。
- 起始状态：共享的 45 个提交没有 HarnessKit 内容或路径；B 在共享 HEAD 后增加一个适配提交，纳入 schema、knowledge.lock 和 AGENTS 路由，运行态目录忽略。S7 的旧 skill 与 README release 路由也在该提交中，release 缓存忽略。所有场景×组别 prepare 后 `git status --porcelain` 必须为空，B 的父提交树与 A 的 HEAD 树一致。
- 任务 prompt 模拟真实用户：简短、不提示文档位置、不提 harnesskit。

## 场景仓库

一个中等规模的合成项目（约 150–300 个文件，Rust 或 TS + Python 脚本均可，需要能跑测试），带脚本化 git 历史（≥40 提交），文档包含：AGENTS.md、ARCHITECTURE.md、docs/index.md、docs/project.md、decisions（≥6）、notes/pitfalls（≥5）、exec_plans（含 1 个 active、1 个 superseded、1 个 completed）、worklog。文档量要足够大，使“全部读一遍”成本明显（约 50–60 篇实际内容的文档、相互链接）。生成脚本确定性，可重复生成。

## 场景（每个都要有确定性的 grader）

| ID | 用户 prompt（示意） | 考点 | 判定 |
|---|---|---|---|
| S1 决策约束 | “token 刷新偶尔失败，给它加个自动重试” | decisions 里有“刷新必须单飞、失败不重试，由调用方处理” | refresh.rs、session.rs 无新增重试／退避且隐藏测试通过即成功；允许调用方有界重试；答复引用决策、改写决策正文只作指标 |
| S2 已知陷阱 | 一个 bug 报告 | notes 记录了该陷阱的两处必须同时改的位置 | 隐藏测试覆盖两处 |
| S3 接手工作 | “继续上次没做完的工作” | active plan 的下一步 vs superseded plan 的冲突下一步 | 完成现行计划的 90 秒窗口且隐藏测试通过；未执行旧代码方向、未勾选旧计划待办；允许归档／改元数据，过程记录不作硬条件 |
| S4 文档漂移 | “网关限流阈值是多少？写给运维一段说明” | 代码近期改了阈值，文档未更新 | 回答代码当前值，并指出/修正文档 |
| S5 改代码后维护文档（熵） | 一个行为改动需求 | 应更新现行文档而非已被取代的文档；不新建重复文档；索引同步 | grader 检查改动的文档集合、无重复新文档、链接有效 |
| S6 初始化 | 一个文档散乱的仓库（README 塞满、notes 零散、重复内容）：“帮这个项目把文档体系建起来” | 结构化、不丢信息、不重复、入口精简 | AGENTS 被修改且 ≤120 行；关键事实经其链接全部可达；无重复段落；链接有效；隐藏测试通过；不规定目录名 |
| S7 版本更新（仅 B 组） | 一个 alpha.6 风格 schema 的项目，已装旧版 skill：“把 harnesskit 升级到最新并处理迁移” | update/migrate/skill 同步流程 | schema 迁移完成、原有产品文档正文未改（允许 frontmatter 和 AGENTS 路由变化，排除 skill、运行态与 release）、check 0 error、skill 版本已同步；新增 docs 文档作指标 |

## 指标（从被试 session jsonl 自动提取）

- 成功率（grader）
- 工具调用总数；**首个正确动作前的工具调用数**（首次读到/改到该场景的关键文件，关键文件清单由场景定义）
- token 用量、墙钟时间
- 违反决策次数、读过的文档篇数（广度）、改动的文档是否正确
- B 组额外：harnesskit 命令使用分布、是否按 skill 推荐路径使用
- S3 过程指标：现行计划 checkbox 是否更新、是否写入任意 worklog 交接条目；记录到 run.json，不影响成功判定
- S1 过程指标：`decision_cited`、`decision_body_edited`；S7：`new_product_docs`（新增 docs Markdown 路径）、`new_product_docs_count`。
- 隔离及故障指标：`start_git_status`、`shell_harnesskit_path`、`harnesskit_failed_commands`（非零退出的 HarnessKit 命令数，以及首条错误的命令、退出码、输出）。

## 交付物

- `evals/agent-ab/`：场景生成脚本、每个场景的 prompt 与 grader、runner（`run.py --scenario S1 --arm A|B --reps 5 --out <dir>`；负责建临时 CODEX_HOME、复制场景仓库、调用 codex exec、收集 jsonl、调 grader）、汇总脚本（输出 markdown 表 + json）。
- runner 本身由调度者在沙箱外执行；构建者只需保证 `--dry-run`（不调用 codex，用一份录制的假 transcript 走通提取与评分）可在测试中运行。
- grader 必须自测：每个场景至少一份“正确解”和两份“典型错误解”（包括一份看似合理但踩坑的解），grader 分别判对/判错（纳入 cargo test 或 pytest）。

返工细则、隐性陷阱、基线 diff 与隐藏测试规则见 [README.md](README.md)。S7 必须离线使用真实 release，并核验 CODEX_HOME 中实际安装的 skill。
