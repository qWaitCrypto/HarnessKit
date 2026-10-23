# 0.2.0 设计：Weave，文档与代码织成一张知识网

目标：agent 只问"这是什么、为什么这样、在哪实现"，HarnessKit 一次返回文档和代码交织的答案；从代码出发，也能反查到设定、决策和经验。

## 设计原则

1. **入口不变重**：agent 只多学一种写法（代码锚点）。命令沿用 `query`、`context`、`check`，SKILL.md 保持 35 行以内。
2. **不做代码智能**：只做符号表和关联边，不做调用图、类型推导。读代码交给 agent 自己的 rg、LSP 或 Serena。HarnessKit 只提供它们都没有的"文档↔代码之桥"。
3. **事实源在仓库**：持久的关联状态写进受 git 管理、方便合并的文件里，不藏在只是派生出来的 SQLite 里。
4. **确定性优先**：不依赖 LLM，不联网。可信度可以解释，每条边都带证据。
5. **能量化才保留**：每项功能都要进评测门禁（见 W9）。

## 1. 知识网：一张图，两种节点

继续用现有的 SQLite 索引，在里面新增：

```
nodes(id, kind, path, name, span_start, span_end, fingerprint, lang)
  kind: doc | section | file | symbol
edges(src, dst, kind, confidence, weight, evidence_json)
  kind: anchor | cochange | mention | import(外部)
  confidence: EXTRACTED | INFERRED
code_fts      -- 代码卡片全文索引(见 §4)
```

文档节点细到章节（heading），代码节点细到符号（函数/方法/类型/模块/常量）。

### 符号提取

- 用 tree-sitter 的 `tags.scm` 查询，这是业界标准做法，GitHub 代码导航就用它。
- 默认编译进 Rust、TypeScript/JavaScript、Python、Go；其他语言用 cargo feature 开关。
- 不支持的语言退化为文件级节点，仍能参与同提交统计和锚点关联，适配性不受影响。
- 只扫描 git 跟踪的文件，遵守 `.gitignore` 和 schema 的 ignore。按 mtime+size 增量更新，沿用现有 `file_states` 机制。

### 结构指纹

- 对符号做规范化 token 流：去掉注释和空白、字面量保留，再取 blake3 的前 12 位十六进制。
- 只改格式、改注释不会触发漂移，改逻辑才会。

## 2. 四种边：关联从哪来

| 来源 | 可信度 | 生成方式 |
|---|---|---|
| 显式锚点 | EXTRACTED | 文档中写 `` `src/auth/refresh.rs#refresh_token` ``、`` `src/auth/` ``、`` `Session::renew` `` |
| 同提交 | INFERRED | git 历史中文档和代码文件在同一提交里修改 |
| 标识符提及 | INFERRED | 文档反引号内的名字匹配到符号表 |
| 外部导入 | 按来源标注 | 可选：存在 `graphify-out/graph.json` 等时读入 |

### 锚点语法

沿用 Markdown 行内代码，不发明新语法：

- `` `路径` ``：文件或目录
- `` `路径#符号` ``：限定在某个文件内
- `` `Type::method` ``：全局唯一时自动解析，有歧义时报 info 并列出候选

`#L10-L20` 这种行号锚点会腐烂，只接受，并提示改成符号锚点。

### 同提交边（核心巧思）

- 读取 `git log --name-only --no-merges`，从上次处理到的提交增量读。
- 权重 = `Σ 时间衰减(半衰期 90 天) / sqrt(n_doc · n_code)`，即余弦归一化，防止热门文件和所有东西都相关。
- 一次改动超过 30 个文件的提交（格式化、重命名、vendoring）忽略。
- 默认只保留每个文档权重最高的 8 条边。
- 不用 LLM、零配置。老项目装上就有关联，这是冷启动的关键。

### 标识符提及消歧

候选多于一个时，按两项打分：
1. 路径接近度：`docs/auth/` 优先 `src/auth/`。
2. 同提交权重。

仍有歧义的提及只用于检索加权，不出现在"相关代码"里。

## 3. 知识锁：`.harnesskit/knowledge.lock`

这一节解决"文档悄悄过期"，是本设计的核心。

```
# doc-anchor  ->  被确认时的代码指纹（排序、一行一条，git 好合并）
docs/decisions/0007-token.md  src/auth/refresh.rs#refresh_token  3f9a1c0be27d
docs/notes/auth-pitfalls.md   src/auth/                          dir:8c21e0aa41f3
```

- **封印**：文档锚定代码时，记录"文档最后一次被确认时代码的样子"。目录锚点的指纹是其下文件指纹的 Merkle 汇总。
- **漂移**：当前指纹和锁里的不一致 → `check` 报 `code-drift`（warning），附上改动提交和时间，以及 `git diff` 命令。
- **共改即复核**：同一工作区改动或同一提交里，锚定该代码的文档也被修改了 → 视为已复核，index 时自动重新封印。正常开发无需任何额外动作。
- **显式确认**：代码改了、文档确实不用改时，执行 `harnesskit check --ack <doc>` 重新封印。agent 和人都可以操作，确认过程在 git diff 中可见。
- **锚点迁移（第一个可自动修复的规则）**：符号找不到了，就在全库按指纹搜索。命中唯一一处 → 报 `anchor-moved`，fixable=true，`check --fix` 改写文档里的锚点并更新锁。
- **断链**：既没找到也没迁移 → `anchor-broken`（warning）。
- **为什么用锁文件**：与 Cargo.lock 同理。文档正文保持干净，状态跨机器、跨 agent 共享，合并冲突是逐行的、可读的。

## 4. 代码卡片与词汇桥接：用中文问出 Rust 函数

每个符号生成一张确定性的"卡片"写入 `code_fts`：

```
name: refresh_token        split: refresh token
path: src auth refresh
signature: pub fn refresh_token(session: &Session) -> Result<Token>
doc_comment: ...
bridged: <锚定或强关联它的文档章节标题与摘要>
```

- **标识符切分**：camelCase、snake_case、路径分段都拆开。
- **词汇桥接**：文档的自然语言（包括中文）沿边"流"到代码卡片上。查 "token 刷新" 能直接命中 `refresh_token`，即使代码里一个中文都没有。文档成了代码的自然语言索引，代码成了文档的落地证据。
- 桥接只沿 EXTRACTED 边，以及权重 ≥ 阈值的同提交边，避免污染。

## 5. 检索：答案以"知识束"返回

`query` 默认同时检索文档和代码：

1. 分别用 BM25 召回章节和代码卡片（中文沿用 S5 的 trigram 索引）。
2. 用倒数排名融合（RRF）合并。
3. 沿图做一跳传播加权：命中的文档给它锚定的代码加分，反之亦然。可以看成只走一步的个性化 PageRank。
4. 聚合成**知识束（bundle）**：一个主节点，加上 ≤3 个关联节点，加上新鲜度。

```
harnesskit query "token 刷新"

① 决策 docs/decisions/0007-token.md §刷新策略            [accepted]
   ↳ code  src/auth/refresh.rs#refresh_token             锚点 · ⚠ 漂移：3 天前 a1b2c3 修改
   ↳ code  src/auth/session.rs#Session::renew            同提交 ×4
② 经验 docs/notes/auth-pitfalls.md §并发刷新
   ↳ code  src/auth/refresh.rs#refresh_token
```

- `--json` 输出结构化的 bundle，每条边带 kind、confidence 和 evidence。
- `--docs` / `--code` 只看一侧；默认混合。
- 输出预算：默认 ≤5 个 bundle、≤40 行，保护 agent 的上下文。

### 5.1 输出契约：信号优先，结构服务于内容

关联做得越多，越容易把输出变成一堆元数据。所有面向 agent 的输出（query、context、check、hook）都必须遵守以下契约，并纳入评测。

1. **每行都要有内容**：每个结果至少带一行真实文本，元数据只能附在后面。真实文本指命中片段、函数签名或决策原文，不能只有路径加标签。
2. **片段跟着查询走**：摘要从命中查询词的句子中截取（KWIC），没有命中才回退到章节首句。查询词高亮用 `«»`，以纯文本形式输出。
3. **默认值不显示，只显示异常**：`current`、`EXTRACTED`、`anchor` 这类正常状态不打印。只有漂移 `⚠drift`、断链 `✗broken`、推断 `~`（INFERRED）、已取代 `superseded` 才显示徽章。
4. **不凑数**：关联节点必须超过相关度阈值才显示，宁可只给 1 条，也不为填满 3 个槽位放进弱关联。主结果同理：分数低于首条一定比例就截断，并提示还有 N 条。
5. **去重合并**：同一文件的多个符号合并为一行（`src/usage.rs: log_path, record, …`）。同一文档的多个命中章节，合并为文档加章节列表。
6. **按 token 预算，而不是按条数**：`--budget <tokens>`，默认约 1500。按"主结果 → 关联 → 提示"的优先级装填，装不下的给出计数和续取方法。
7. **渐进展开**：默认是概览，`--expand <n>` 展开第 n 个知识束（完整章节、关联代码的签名和 doc comment），`--page <k>` 或 `--cursor <token>` 取下一页。
8. **结果缓存**：查询结果集按"规范化查询 + 参数 + 索引代际号"缓存在 `.harnesskit/state/query-cache/`。翻页和展开直接复用，无需重算；index 变化时代际号加一，旧缓存自动失效。缓存采用 LRU，上限 64 条。
9. **JSON 一样节制**：`--json` 默认只给要点字段，`--json --full` 才给证据明细。所有 JSON 输出都带 `next_cursor` 和 `truncated` 计数。

示意，默认输出：

```
1. 决策 docs/decisions/0007-token.md §刷新策略
   刷新必须单飞：并发请求共享同一个 «refresh» 结果，失败不重试。
   ↳ src/auth/refresh.rs: refresh_token  ⚠drift 3天前 a1b2c3d
   ↳ ~ src/auth/session.rs: Session::renew
2. 经验 docs/notes/auth-pitfalls.md §并发刷新
   两个 tab 同时 «refresh» 会让旧 token 失效……
… 另有 4 条（harnesskit query --page 2）
```

**评测（加入门禁）**：
- **信号密度**：输出行中包含实际文本或查询词的比例 ≥ 70%。
- **元数据占比**：徽章和路径字符占比 ≤ 35%。
- **金标对照**：维护 10 条固定查询的期望输出快照，行为改动必须人工确认差异。

## 6. 双向入口

| 命令 | 作用 |
|---|---|
| `context --for <path\|symbol>` | 从代码出发：讲过它的文档、约束它的决策、进行中的计划、常一起改的文件、漂移状态 |
| `context --diff [<rev>]` | 从改动出发：当前工作区（或指定提交以来）改过的代码涉及哪些文档、决策；哪些锚点会漂移。给 review agent 和提交前自检用 |
| `context --current` | 增加一节"上个快照以来改动的代码区域：已覆盖 / 未覆盖 / 漂移"，供交接用 |

## 7. 原生融入 agent：hook

- `harnesskit hook pre-edit <path>`：只输出 ≤3 行，例如 "此文件受决策 0007 约束：刷新必须单飞；参见 docs/decisions/0007-token.md"。
  - 去噪：同一 session 中每个文件只提示一次。session id 取自 hook 输入，状态存在 `.harnesskit/state/`（不入 git）。
  - 没有关联文档时什么也不输出。
- `harnesskit hook post-edit <path>`（可选）：改完后若某锚点漂移，提示一句"决策 0007 锚定的代码已改，请更新或 `check --ack`"。
- `install` 提供 Claude Code PreToolUse/PostToolUse（Edit|Write|MultiEdit）的配置片段。Codex 等其他 agent 给通用调用方式，由 skill 指导调用 `context --for`。
- hook 默认关闭，用 `harnesskit install --hooks` 显式开启。

## 8. 写侧：让关联几乎零成本地长出来

- `harnesskit suggest-anchors <doc>`：根据同提交和提及，给出建议锚点（带证据），agent 一键采纳。
- 不加 `<doc>` 时，从 `context --diff` 推断：本次改了哪些代码，就建议写进哪篇文档。
- SKILL.md 只加一句："写决策/经验时，用反引号引用相关代码路径或符号。"
- check 新增 info `undocumented-hotspot`：同提交频繁、改动多，却没有任何文档边的代码区域，提示该补文档。

## 9. 多 agent 与一致性

- 索引使用 SQLite WAL，并加 `.harnesskit/index.lock` 进程锁。并发的 `query` 只读不阻塞，`index` 串行执行。
- knowledge.lock 排序、逐行存储，两个 agent 在不同分支上各自封印，合并时基本不冲突。冲突时 `check` 能识别并给出 `--ack` 建议。
- 所有派生状态可以从仓库和 git 完全重建，锁文件除外，它是事实源。

## 10. 旧文档迁移（与本期一起做）

- 不迁移也能用（底线）。
- `harnesskit migrate --plan [--json]` 输出待办：schema 升级、可写入的推断布局、可能被取代的文档、可添加的锚点（来自 suggest-anchors）。每条带证据和建议动作。
- `migrate --apply` 只改结构化内容（schema、frontmatter），不改正文，执行前自动快照。
- 需要判断的事项交给 AI，按 `references/migration.md` 逐条处理。
- `doctor` 和 `context` 发现 schema 版本落后时，只提示一行。

## 11. 评测门禁

| 评测 | 内容 | 门槛 |
|---|---|---|
| 检索 | 扩展 S5 评测集：查询 → 期望文档章节和期望符号；含中文查代码 | 符号 top3 ≥ 80%，文档指标不回退 |
| 漂移 | fixture 仓库带脚本化 git 历史：改逻辑 / 改格式 / 移动 / 删除 | 精确率 100%，格式改动 0 误报 |
| 同提交 | 合成历史中已知的相关对 | 已知对进入 top8 ≥ 90% |
| 性能 | 1 万文件规模的合成仓库 | 冷 index < 10s，增量 < 1s，query < 200ms |
| 交接（alpha.9） | 新 session 完成任务：有无 HarnessKit 对比首个正确动作前的工具调用数、违反决策次数 | 记录基线 |

## 实施步骤（交给 Codex，每步保持全绿、0 warning）

- **W0** fixture：带 git 历史的多语言仓库生成脚本（Rust/TS/Python/Go，含决策、经验文档），加评测骨架。
- **W1** 代码节点：tree-sitter 符号提取、结构指纹、增量扫描，nodes 表；`doctor --code` 汇报语言和符号数。
- **W2** 锚点：解析、解析失败与歧义报告、EXTRACTED 边。
- **W3** knowledge.lock：封印、共改自动复核、`code-drift`、`check --ack`、`anchor-moved` 加 `check --fix`、`anchor-broken`。
- **W4** 同提交边与提及边：增量 git 读取、余弦和时间衰减、大提交过滤、提及消歧。
- **W5** 代码卡片和词汇桥接：`code_fts`，`query` 中的 RRF、一跳传播、bundle 输出与预算。
- **W5b** 输出契约（§5.1）：KWIC 片段、只显示异常徽章、相关度阈值不凑数、合并去重、token 预算、--expand/--page/--cursor、结果缓存、JSON 节制，加信号密度评测和金标快照；对 query、context、check、hook 统一生效。
- **W6** 双向入口：`context --for`、`context --diff`，`--current` 中的代码节。
- **W7** 写侧与 hook：`suggest-anchors`、`undocumented-hotspot`、`hook pre-edit/post-edit`、`install --hooks`。
- **W8** 迁移：`migrate --plan/--apply`、`references/migration.md`、版本落后提示。
- **W9** 收尾（版本号定为 0.2.0）：评测门禁达标、性能基准、SKILL.md 更新（≤35 行）、CHANGELOG 和 0.2.0 迁移说明、本仓库 dogfood（本文锚点全部有效、0 warning）。

## 不做

- 调用图、跨文件类型推导（交给 LSP/Serena）。
- LLM 语义提取、向量嵌入（违背确定性和离线；以后如需要，只作为可选外部来源接入）。
- 行号锚点作为一等公民。
