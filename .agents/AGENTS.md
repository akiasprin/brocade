# Brocade Agent 开发指南

本文是仓库级开发约定，适用于人类贡献者与编码 Agent。任务中的明确要求优先于本文；若从子目录启动任务且该目录存在更具体的 `AGENTS.md` 或 `AGENTS.override.md`，以更接近当前工作目录的规则为准。

## 开始工作前

1. 先阅读本文件、下方“按任务读取”列出的相关文档，以及准备修改的实现和测试。需要项目全貌、公开行为或环境配置时再阅读 `README.md` 和相关 crate 的 `Cargo.toml`。
2. 执行 `git status --short`，把已有改动视为用户工作；不要覆盖、回退、格式化或顺手整理无关内容。
3. 先定位数据流和既有抽象，再动手。优先使用 `rg` / `rg --files` 搜索，避免凭文件名猜实现位置。
4. 让改动保持小而完整：解决根因，补回归测试，只更新受影响的文档和夹具，不做无关重构。
5. 未经明确要求，不创建提交、不改写历史、不推送远端，也不执行破坏性的 Git 命令。

## 按任务读取

`.agents/` 是本仓库面向人类贡献者与编码 Agent 的工程知识唯一入口。不要在 Skill、README 或代码注释中复制同一套规则；引用权威文档并在原处维护。

| 任务 | 继续阅读 |
| --- | --- |
| 理解系统边界或跨层数据流 | `.agents/architecture/system-overview.md` |
| 修改前端视觉、交互或组件 | `.agents/standards/frontend.md` |
| 修改 Rust、PostgreSQL、协议或 Xray | `.agents/standards/backend.md` |
| 选择或执行验证 | `.agents/standards/testing.md` |
| 部署 Console 或变更宿主机服务 | `.agents/runbooks/console-deployment.md` |
| 发布、回滚 Agent/Xray/配置/授权 | `.agents/runbooks/release-and-rollback.md` |
| 排查生产异常 | `.agents/runbooks/incident-response.md` |
| 记录长期架构决策 | `.agents/adr/README.md` |
| 执行较长、跨层或需交接的改造 | `.agents/plans/TEMPLATE.md` |

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

领域规范不在这里重复维护。前端任务遵循 `.agents/standards/frontend.md`；Rust、PostgreSQL、协议、Shell 与 Xray 任务遵循 `.agents/standards/backend.md`。

## 验证

先运行与改动最相关的快速测试，再按影响面扩大范围。具体矩阵和命令以 `.agents/standards/testing.md` 为入口，CI 的最终定义以 `.github/workflows/ci.yml` 为准。不要声称未实际运行的检查已经通过；因环境限制未运行的检查必须在交付说明中列出。

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
