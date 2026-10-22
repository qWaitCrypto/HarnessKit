# HarnessKit alpha.7 迭代计划（基于 2026-04 ~ 2026-10 半年使用复盘）

本文件是执行计划，按步骤（S0…S11）逐个实施。每一步一个独立提交，提交前必须 `cargo build` 无警告、`cargo test` 全绿。

## 不变的产品原则

- agent 面保持轻：skill 短、入口文件是路由表；**不新增 MCP、daemon、后台进程**，尽量不新增顶层命令，新能力优先放进现有命令的输出和参数。
- CLI 是确定性动作边界，内部可以重：Rust 内部越方便越好，允许引入成熟依赖。
- **CLI 自述**：每个 `--json` 输出都带 `coverage`（管了什么/没管什么）和 `hints`（下一步可以做什么）。skill 不再教命令细节，CLI 自己说。
- 默认零配置自适应，显式 schema 配置兜底；不要求用户搬迁 docs 布局。
- 只报可行动的告警；每条告警都能回答“该改哪个文件、怎么改”。
- 向后兼容：已有 `.harnesskit/history` 快照必须仍可 list/diff/restore；旧 `facts.sqlite` 不兼容时自动重建（state 是派生数据）；已有 schema.yaml 不改也能用。

## 复盘中的关键证据（实现时据此判断）

1. 覆盖：索引/check/history 共用 `collect_managed_doc_paths`，只收 schema collection 目录。一个平铺布局项目 147 篇 docs 只管 28 篇（`docs/decisions.md`、`docs/execution-plan.md`、`docs/reference/` 单数目录等都漏），且所有命令都“成功”、无任何提示；HarnessKit 自身仓库 0/29。
2. 检索：93 条真实查询回放，50 条零命中；全部单个词能命中。原因：多词隐式 AND、unicode61 不切中文（正文中文短词基本命不中）、文档在管辖外。零命中返回空数组，无诊断；agent 随后放弃工具改用 rg。
3. 排序不区分当前/历史：已 superseded 或已移入 completed/archive 的文档与当前文档同等排序；大量文档没有 status。
4. check：stale-explicit-anchor 占全部 warning 的 46%，只比 mtime（checkout/worktree 重置 mtime 即误报，对 decision、completed 计划也报）；没有任何 error 级规则所以永远 exit 0；tracked 模式下 managed-file-not-excluded 误报；canonical-links-reference 对行内路径提及也报，惩罚“设计文档引用证据”；抑制只能按精确路径逐条配；每次全量输出几十 KB，同一组 warning 被重复看几百次。
5. 调用仪式：skill 要求每次先 `--version`、改文档后 `index` 再 `check`，而 check/query 自己会刷新索引；`--version` 占 22–34% 调用。
6. history：快照与索引范围相同，漏掉未纳管文档；断档无提示（某项目快照停止后又新增 111 篇文档）；`host_git_dirty` 在非 git 目录默认 false 造成假象；对 git 忽略 docs 的项目它是唯一版本记录但只在本机。
7. 健壮性：不存在的 target_dir 被静默创建 `.harnesskit/state` 且 exit 0（`check json`、`query x 10` 产生孤立目录）；stdout 管道被 `| head` 截断时 panic；sqlite 通过外部 `sqlite3` 进程调用。
8. 度量缺失：使用情况只能靠刮 agent 会话日志，口径偏差大。

## 步骤

### S0 测试基线与固定语料
- 新建 `tests/fixtures/`，用**合成内容**构造 5 个小型仓库：
  - `standard`：init 模板布局（product-specs/design-docs/decisions/exec-plans active+completed/worklog/references）。
  - `flat`：docs 根目录平铺 `decisions.md`、`execution-plan.md`、`principles.md`，加 `docs/reference/`（单数）若干文档，入口 AGENTS.md 指向它们。
  - `cjk`：正文中文为主（如“玩家获得精灵蛋后需要孵化精灵蛋，孵化周期为三天”），标题中英混合，含编号 `EV-01`、`GD-02`。
  - `lifecycle`：一个 decision 被另一个 `supersedes`；一个计划在 completed/ 且首行横幅“Superseded/已被取代”；一个文档正文开头有“历史证据，已被新路线取代”横幅但 frontmatter 是 active。
  - `external`：docs 引用 docs 外的 `research_runs/x/result.json`。
- 写集成测试（`tests/` 下按主题分文件），先锁定**当前**行为中需要保持的部分（init、index、history snapshot/list/diff/restore 往返）。
- 新建 `tests/fixtures/queries.json`：每条含 fixture、query、期望命中的路径；后续 S5 用它度量。先写 25 条以上，覆盖：多词其中一词不存在、中文短词、中英混合、编号、已取代文档不应排第一。

### S1 拆分 main.rs（纯重构，行为不变）
- 将 `src/main.rs` 拆为模块：`cli`（参数解析与分发）、`schema`、`discover`（文档收集）、`parse`（markdown 解析）、`index`（fact store 构建/增量）、`query`、`context`、`checks/`（按规则分文件）、`history`、`render`（文本与 json 输出）、`git`、`templates`、`update`。
- 不改任何输出格式；S0 测试全绿。

### S2 依赖现代化
- 用 `rusqlite`（features = ["bundled"]）替换 `sqlite_cli.rs` 的外部进程调用；SQL 用参数绑定，删除手写字符串转义。
- 用 `serde`/`serde_json` 替换手写 JSON 渲染与解析（history 快照读写必须兼容现有格式，加往返测试）。
- 可选：`clap`（derive）重写参数解析，保持现有命令、位置参数和选项完全兼容；`--help` 自动生成。
- 单二进制、无运行时依赖；更新 README 安装说明（不再需要 sqlite3）。

### S3 健壮性
- target_dir 不存在 → 报错（exit 2），提示“是否把参数当成了 target_dir”；只有 `init` 可以创建目录。
- 发现嵌套孤立 `.harnesskit`（目标目录下无 docs 也无入口）时 `doctor`/`check` 给出提示。
- 正确处理 SIGPIPE/BrokenPipe：静默退出 0，不 panic。
- 退出码约定写入 README：0 正常、1 有 error 级发现（或 --strict 下有 warning）、2 用法/环境错误。
- 所有命令在 target 不是 HarnessKit 项目时给出一行 hint（建议 `harnesskit init --preview`）。

### S4 自适应覆盖（最高优先）
- `discover` 扫描 managed_root 下全部 Markdown（排除 templates、`.harnesskit`、隐藏目录），区分：managed（落在 collection/core/entrypoint）、unmanaged、excluded（schema 显式 ignore）。
- 内置别名与推断：`reference/`→references、`adr/`/`adrs/`→decisions、`plans/`→exec_plans、`design/`→design_docs、`specs/`→product_specs、`runbooks/`→operations；docs 根目录平铺的 `*.md` 默认归入新的 `notes` 角色（authority: supporting，参与索引、检索、history，不参与 collection-index 要求）。文件名启发：`decisions.md`/`*decision*`→decisions 角色，`*plan*`→exec_plans，`architecture*.md`→design_docs。推断结果可被 schema 覆盖。
- schema 新增可选字段：`include`（glob → collection）、`ignore`（glob）。旧 schema 不写也能工作。
- `index`、`check`、`history snapshot`、`list-docs` 输出 `coverage`：`{markdown_total, managed, unmanaged, excluded, by_dir:[{dir,count,status}], ratio}`；文本模式一行摘要。
- 新规则 `unmanaged-docs`（warning，按目录聚合，一个目录一条，而非一个文件一条），hint 给出可粘贴的 schema `include` 片段。
- `unresolved-managed-link` 拆分：目标文件不存在 → `broken-link`（**error**）；目标存在但未纳管 → 并入 unmanaged-docs，不再重复报。
- `doctor --layout`：打印推断出的映射与覆盖率，`--write-schema` 写入 `.harnesskit/schema.yaml`（已存在时先 preview，需 `--force`）。
- 外部指针（巧思）：文档中指向 docs 外的相对链接或行内路径，若文件存在则记录其 hash（存入 fact store 的 `external_refs`）；`check` 在其内容变化后报 `external-ref-changed`（info，附哪篇文档引用了它）。不把这些文件纳入 history。

### S5 检索内核
- FTS 改为两张表：unicode61（拉丁文、编号）+ trigram（SQLite ≥3.34 自带，用于 CJK 与子串）。CJK 少于 3 字的词用 `LIKE` 兜底于正文。
- 查询策略：先严格 AND；若结果 < 3，则按“命中词数 desc、bm25”放宽为 OR，并标记 `match: {matched_terms, missing_terms, mode: "strict"|"relaxed"}`。
- 字段权重：title > path > headings > summary > body（bm25 列权重）。
- 状态感知排序：superseded、completed/archive 路径、首段横幅含 “superseded/已被取代/历史/history only/不再适用” → 降权并在结果中标 `lifecycle: "historical"`；被 `supersedes` 指向者给出 `superseded_by`。
- 零结果诊断（json 字段 `diagnostics`）：每个词单独命中数、管辖外是否存在含该词的文件（用 S4 的 unmanaged 集合做纯文本扫描，列出前 5 个路径）、target_dir/索引是否为空；文本模式给 hint：“试试 `<最有区分度的 1–2 个词>`”。
- 相近标题建议（did-you-mean，基于标题 trigram 相似度）。
- `--limit N`（默认 8）；snippet 只取真正命中的行，未命中时不伪造 snippet；neighbors 默认最多 5 个且去掉入口与索引页。
- 用 S0 的 `queries.json` 写度量测试：输出零命中率与 top-3 命中率，测试断言不低于设定阈值（实现完成后把阈值设为实际值，防止回退）。

### S6 知识生命周期
- 状态推断：frontmatter status > 路径（active/completed/archive）> 首段横幅关键词 > 默认。exec-plans、worklog 允许 `status` frontmatter。
- schema 允许字段新增 `superseded_by`、`supersedes_parts_of`（部分取代）；frontmatter-field-not-allowed 不再对这些报。
- `context` 无参数（或 `context --current`）返回“当前状态包”：入口路由、project map、active 计划（按 mtime）、最近的 decisions、各自 lifecycle、最新快照与其后变化的文件、未验证项（文档中 `[ ]` 未勾选的验收项计数）。这是跨会话接手的默认第一步。
- `context --since <snapshot|latest|日期>`（巧思：交接包）：列出自该点后新增/修改的受管文档及其标题/首段，用于接手长任务。
- 新规则（均为 info，可行动）：
  - `entrypoint-too-long`：AGENTS.md/CLAUDE.md 超过 120 行（阈值可在 schema 配）；hint 建议把进度移到 project map。
  - `duplicated-current-pointer`：同一“当前计划/current plan”指向在 ≥3 个文件出现，或不同文件指向不同的 active 计划。
  - `active-but-historical`：frontmatter active 但横幅/路径表明历史。
  - `superseded-not-marked`：被 supersedes 指向的文档没有标 superseded。
  - `stale-active-plan`：active 计划超过 N 天（默认 30）未修改。

### S7 check 变成维护助手
- 默认输出：按规则聚合的计数 + 相对基线的“新增/已解决”条目；`--all` 展开全部。基线自动保存在 `.harnesskit/state/check-baseline.json`（派生数据），`--update-baseline` 显式更新；首次运行即为基线。
- 每条发现带 `hint`（修复建议）与 `fixable` 标记。
- `stale-explicit-anchor` 改为：有 git 时用 `git log -1 --format=%ct -- <path>` 比较文档与 scope 的最后提交时间（批量一次调用，缓存）；无 git 回退 mtime 且降为 info；跳过 decisions、completed、archive、lifecycle=historical 的文档；同一文档多个 scope 合并为一条。
- `managed-file-not-excluded`：检测 tracked/local 模式（docs 是否被 git 跟踪、`.gitignore` 是否已覆盖），tracked 模式不报；同时读 `.gitignore` 与 `.git/info/exclude`。
- `canonical-links-reference` 只对 markdown 链接触发，降为 info。
- `orphan-managed-doc` 按目录聚合。
- error 级规则：`broken-link`、`missing-entrypoint-doc`（全部入口都缺时）、schema 解析错误。有 error 时 exit 1。
- 抑制：schema `suppressions` 支持 `rule` + `path` glob + `reason`。
- `--format summary|full|json`；json 输出体积默认受控（列表截断并给出 `truncated: n`）。

### S8 history
- 快照包含 S4 的全部 managed（含推断的 notes/reference），并在快照元数据中写 `coverage`。
- `history snapshot` 内容无变化时跳过并提示（`--allow-empty` 强制）。
- `history status` 增加：距最近快照的天数、自快照后变化的文件数、未纳管文件数、docs 是否被 git 跟踪（若未被跟踪，提示“history 是这些文档唯一的版本记录”）；`host_git_dirty` 在非 git 目录输出 null 而非 false。
- `check` 中 history-dirty 规则改为：仅当“距快照 > 7 天且有变化”或“变化文件 > 20”时报，并给出 hint。
- `history export <file.tar.gz>` / `history import <file>`：跨设备迁移快照（合并对象与快照，不覆盖已有 id）。
- `history show <id> <path>`：打印某快照中某文件内容（便于复核历史结论，不必整树 restore）。

### S9 本地使用度量（巧思，替代刮日志）
- 每次命令追加一行到 `~/.harnesskit/usage.jsonl`（或 `$HARNESSKIT_HOME`）：时间、版本、命令、target 根的 hash、耗时、退出码、query 的命中数与 mode、是否零结果、coverage ratio、check 的新增发现数。默认不记录查询原文（`HARNESSKIT_USAGE_LOG=full` 才记）；`HARNESSKIT_USAGE_LOG=off` 关闭。
- `doctor --usage [--days N]`：汇总各项目的调用次数、零命中率、放宽命中率、常见无效模式（index 后紧跟 check、频繁 --version）。

### S10 skill 与入口瘦身
- 重写 `agent-surfaces/skills/harnesskit/SKILL.md` 为 ≤60 行：何时用、接手先跑 `harnesskit context --current`、找依据用 `query`（1–3 个关键词，零结果按 hints 换词，再用 rg）、改完文档跑一次 `check`、阶段节点 `history snapshot -m`。去掉 `--version` 预检、`index && check`、命令参考大全（移到 `references/cli.md`）。
- 明确告诉 agent：`check` 无 error ≠ 交付验证；不要把 CLI 统计抄进 worklog。
- skill 带版本头（与 CLI 版本一致）；`doctor` 检测 `~/.codex/skills`、`~/.claude/skills` 等处已安装 skill 与内置版本是否一致并提示 `harnesskit update`/同步脚本。
- 更新 init 模板：入口模板保持路由表，增加“当前状态只写在 docs/project.md”的约定一行；project map 模板增加“当前焦点（带日期）”与“已结束/历史”分区。

### S11 文档与发布
- 更新 README、changelog、`docs` 内的命令说明；版本号 `0.1.0-alpha.7`；补齐迁移说明（旧 state 自动重建、旧 schema 兼容、新增字段）。
- 最终全量 `cargo test`、`cargo clippy -- -D warnings`（如可用）。
