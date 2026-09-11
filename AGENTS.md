# Brocade Agent 开发指南

本文是仓库级开发约定，适用于人类贡献者与编码 Agent。任务中的明确要求优先于本文；若子目录以后增加更具体的 `AGENT.md` 或 `AGENTS.md`，以离目标文件最近的规则为准。

## 开始工作前

1. 先阅读本文件、`README.md`、相关 crate 的 `Cargo.toml`，以及准备修改的实现和测试。
2. 执行 `git status --short`，把已有改动视为用户工作；不要覆盖、回退、格式化或顺手整理无关内容。
3. 先定位数据流和既有抽象，再动手。优先使用 `rg` / `rg --files` 搜索，避免凭文件名猜实现位置。
4. 让改动保持小而完整：解决根因，补回归测试，只更新受影响的文档和夹具，不做无关重构。
5. 未经明确要求，不创建提交、不改写历史、不推送远端，也不执行破坏性的 Git 命令。

## 项目不变量

Brocade 的主链路是：

```text
ModelSnapshot -> IR -> 节点产物 -> 期望状态 -> Agent 收敛
```

修改代码时必须维护这些性质：

- `brocade-core` 是无数据库、文件系统和外部服务访问的纯编译层；相同版本和输入必须产生字节一致的输出。遍历无序集合后输出时必须显式排序。
- 修订是不可变的，发布与回滚必须可审计；回滚创建新的发布，不篡改历史记录。
- Agent 接收期望状态并反复收敛，不接收一次性操作脚本。收敛操作应幂等、可重试，并能从中断中恢复。
- 配置发布和授权发布是风险、代价不同的路径，不要为了复用代码模糊两者边界。
- Console/Agent 协议需要支持安全升级。协议形状变化时，同时检查 `brocade-deployment`、Console、Agent、协议版本和前端契约测试。
- 本项目会在其他机器上以系统权限执行生成结果。外部输入、路径、命令参数和下载内容必须校验；敏感值不得写入日志、错误信息、夹具或提交。

## 仓库导航

| 路径 | 职责与常用入口 |
| --- | --- |
| `crates/brocade-core` | 纯模型编译器。入口见 `src/model.rs`、`src/compile.rs`、`src/ir/`、`src/artifacts/`；确定性产物在 `tests/golden/`。 |
| `crates/brocade-store` | PostgreSQL 持久化、草稿、修订、发布、凭据、配额与用量；schema 在 `migrations/`。 |
| `crates/brocade-console` | 管理端和节点端 HTTP API、实时通道、证书与内嵌资源；主要路由在 `src/http.rs`，构建嵌入逻辑在 `build.rs`。 |
| `crates/brocade-deployment` | 发布计划、热切换和 Console/Agent 共享协议；协议入口为 `src/protocol.rs`。 |
| `crates/brocade-agent` | 节点侧收敛、系统配置、观测、探测、自更新与离线缓冲。 |
| `crates/brocade-probe` | Console 与 Agent 共用的临时 Xray 客户端和端到端拨测。 |
| `crates/brocade-launcher` | Console、可选受管 PostgreSQL 和 Cloudflare Tunnel 的本机编排。 |
| `crates/brocade-preview` | 基于 Docker 的本地集群预览环境。 |
| `frontend/src/api.ts` | 前端 API 类型与请求入口；后端响应变化时从这里检查调用方。 |
| `frontend/src/panes/` | 页面级功能；`ui/` 是复用界面组件，`forge/` 是草稿/产物工作区，`topo/` 是拓扑，`wm/` 是窗口与导航状态。 |
| `frontend/tests/` | Node 契约测试和 Vitest/Testing Library 组件测试。 |
| `components/xray-core` | Brocade 维护的 Xray fork；上游基线记录在 `BROCADE_UPSTREAM.toml`。只做明确需要的数据面改动。 |
| `.github/workflows/ci.yml` | CI 的最终检查定义；本地命令与 CI 有冲突时以这里为准。 |
| `scripts/`、`install.sh` | 构建辅助脚本、资源准备和 POSIX 安装器。 |
| `console.env.example` | Console 配置的公开清单和部署示例，新增/修改环境变量时同步更新。 |

跨层功能通常按以下方向排查：

```text
frontend -> brocade-console -> brocade-store
                         \-> brocade-deployment -> brocade-agent
brocade-core -> artifacts -> embedded Brocade Xray
```

## 通用编码规范

- 遵循 `.editorconfig`：UTF-8、LF、文件末尾换行、无行尾空格；Rust 使用 4 空格，其余项目文件通常使用 2 空格。
- 命名表达领域含义，不使用无信息量缩写。函数只承担一个清晰职责；优先提前返回，避免深层嵌套。
- 复用现有模块和类型，不复制协议结构、校验逻辑或状态来源。一个事实只保留一个权威来源。
- 注释解释“为什么”、不变量和非显然约束，不逐行翻译代码。公共 API 或复杂状态机使用文档注释。
- 生产代码不使用静默兜底隐藏错误。错误应保留上下文并在正确边界处理；只有已证明的不变量才可 `expect`，消息要说明为何不可能失败。测试代码可合理使用 `unwrap`。
- 新增依赖前确认标准库和现有依赖不能满足需求，并评估二进制体积、跨平台/静态链接、许可证和供应链成本。
- 不引入无界队列、缓存、日志或重试。网络和进程操作应设置合理超时，并考虑取消、退避和重复执行。
- 改变用户可见行为、配置、API、协议、数据库或生成产物时，同步更新测试和相关文档。

### Rust

- 使用仓库固定的 Rust 1.96、edition 2021 和默认 `rustfmt`；所有 Clippy 警告都按错误处理。
- 类型和所有权应表达状态约束；避免用裸字符串或布尔组合表示有明确枚举语义的状态。
- 公共边界进行校验，内部函数接收已验证类型。整数转换优先使用检查转换，避免可能截断的强制转换。
- `unsafe` 块必须尽可能小，并用 `// SAFETY:` 写明调用者与实现共同维持的条件。
- 文件落盘和运行时替换优先采用临时文件、权限设置、校验、原子替换的顺序；不要留下半写入状态。
- 单元测试放在实现附近；跨模块行为、数据库和协议行为放在 crate 的 `tests/`。修复缺陷时先补能复现问题的最小测试。

### TypeScript / React / CSS

- TypeScript 保持 strict；不要用 `any` 绕过模型问题。未知边界使用 `unknown`，经过收窄或校验后再进入业务逻辑。
- 使用函数组件和 Hooks；渲染过程必须纯，不在渲染中修改 ref、读取时钟或触发状态更新。副作用需要完整依赖和清理逻辑。
- 服务端数据由既有 API/查询层管理，草稿状态走 `draft` / `forge` 现有流程；不要在组件内建立第二份权威状态。
- 优先复用 `frontend/src/ui/` 中的组件和 `styles.css` 中的设计令牌。新增交互需包含键盘操作、可见焦点、语义标签以及加载/空/错误状态。
- 格式由 Prettier 管理：单引号、分号、2 空格、120 列。ESLint 例外集中在 `eslint.config.js`，不要散落 inline disable；新增例外必须说明机制原因和退出条件。
- API 类型或交互语义改变时，补相邻的 `.test.tsx` / `.test.mjs`；不要只用快照替代行为断言。

### PostgreSQL / SQL

- 当前仓库故意只维护一份全新安装定义 `crates/brocade-store/migrations/0001_init.sql`；直接修改最终 `CREATE` 定义，不新增增量迁移，也不写 `ALTER`、`IF EXISTS` 等升级步骤。`tests/schema_source.rs` 会强制这一约束；若产品开始支持原地升级，必须连同迁移策略和该测试一起显式变更。
- 多表业务操作必须处于同一事务；并发路径明确锁、唯一约束、幂等键或冲突处理策略。
- 能由数据库表达的不变量使用 `NOT NULL`、`CHECK`、`UNIQUE` 和外键，同时在 API 边界返回可理解的错误。
- 查询必须绑定参数，不拼接外部输入。涉及租户、角色、凭据和发布状态的查询要补越权与并发回归测试。

### Go / Xray fork

- 只格式化实际修改的 Go 文件，使用 `gofmt`，并对受影响包执行 `go test` 和 `go vet`。
- 保持 `components/xray-core/go.mod` 的上游模块路径；不要对整个 fork 做无关格式化或批量重写。
- `*.pb.go` 等生成文件不单独手改；修改源定义后使用项目对应生成流程，并一并审查生成差异。
- 更新上游基线时同步维护 `BROCADE_UPSTREAM.toml`，把“上游导入”与“Brocade 补丁”分成易审查的提交。

### Shell、配置和生成物

- `install.sh` 保持 POSIX `sh` 兼容；不要无意引入 Bash 专用语法。变量加引号，临时文件可靠清理，下载后先校验再执行或安装。
- `Cargo.lock` 和 `frontend/package-lock.json` 必须随有意的依赖变更提交；不要手工编辑 lockfile。
- `target/`、`frontend/dist/`、`node_modules/`、`.tools/`、`brocade-agent-state/` 和本地 GeoIP 数据不是源代码，不得加入提交。
- `crates/brocade-core/tests/golden/` 只在产物语义有意改变时更新；必须逐项审查差异并在测试中说明变化。
- workspace 版本只在根 `Cargo.toml` 的 `[workspace.package]` 维护，不在各 crate 重复写版本。

## 验证

先运行与改动最相关的快速测试，再扩大范围。不要声称未实际运行的检查已经通过。

常用定向命令：

```sh
cargo test -p brocade-core --locked
cargo test -p <crate-name> --locked <test-filter>
cargo clippy -p <crate-name> --all-targets --locked -- -D warnings

cd frontend
node --test tests/<name>.test.mjs
npx vitest run tests/<name>.test.tsx --config vitest.config.ts

cd components/xray-core
go test ./path/to/changed/package
go vet ./path/to/changed/package
```

提交前的标准检查：

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked

cd frontend
npm ci
npm run format:check
npm run lint:strict
npm test
node scripts/route-check.mjs
npm run build

cd ..
sh -n install.sh
git diff --check
```

`brocade-console/build.rs` 会嵌入前端、两个 musl Agent 和 Brocade Xray，因此完整 workspace 构建需要 `README.md` 中列出的 Node.js、Go、Zig 和 Rust targets；缺少这些工具时，仍应完成可运行的定向检查并明确报告遗漏。

PostgreSQL 集成测试需要 Docker，且必须显式开启：

```sh
BROCADE_RUN_PG_TESTS=1 \
  cargo test -p brocade-store --test pg_integration --locked -- --ignored

BROCADE_RUN_PG_TESTS=1 \
  cargo test -p brocade-console --test http_integration --locked -- --ignored
```

改动 `components/xray-core` 时，除受影响包外，还应按 `.github/workflows/ci.yml` 运行对应的确定性、fuzz-seed、race 和 vet 检查。

## Git 提交规范

### 提交边界

- 一个提交只表达一个可独立解释、审查和回退的意图；实现、对应测试和必要文档放在同一提交。
- 不把重命名/全量格式化、依赖升级和行为修改混在一起。不要提交调试输出、临时文件、真实凭据或本地运行状态。
- 提交前检查 `git status --short`、`git diff --check` 和待提交 diff；使用显式路径暂存，避免把他人的工作带入提交。
- Agent 只有在用户明确要求时才提交；禁止自动 push、force-push、rebase、amend 或修改用户已有提交。

### 提交消息

使用 Conventional Commits：

```text
<type>(<scope>): <summary>

<optional body>

<optional footer>
```

- `type` 使用 `feat`、`fix`、`refactor`、`perf`、`test`、`docs`、`build`、`ci`、`chore` 或 `revert`。
- `scope` 可省略；优先使用 `core`、`store`、`console`、`agent`、`deployment`、`probe`、`launcher`、`preview`、`ui`、`xray`、`install`。
- `summary` 使用简洁英文祈使语气，小写开头，不加句号，建议不超过 72 个字符；描述行为结果，不写“update files”之类过程信息。
- 正文说明动机、关键约束和用户可见影响；关联问题放 footer。破坏性变更使用 `!`，并添加 `BREAKING CHANGE:`。

示例：

```text
feat(agent): report gaps in realtime samples
fix(store): serialize concurrent draft commits
test(core): cover deterministic route ordering
docs: clarify managed database startup
build(xray): update embedded upstream baseline
```

## 完成定义

交付前确认：改动符合上述架构不变量；新增行为有测试；格式、lint 和相关测试已运行；API/配置/协议/迁移/README 已按需同步；diff 中没有无关文件、秘密或本地产物；最终说明列出实际验证结果和任何未验证项。
