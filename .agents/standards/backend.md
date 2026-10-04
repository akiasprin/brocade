# 后端与数据面标准

本标准补充 `../AGENTS.md` 中始终生效的规则，用于选择跨 crate 边界、持久化语义、协议兼容和系统操作的实现方式。

## Rust 与 crate 边界

- 使用仓库固定的 Rust 1.96、edition 2021 和默认 `rustfmt`；所有 Clippy 警告都按错误处理。
- 让类型和所有权表达合法状态；有明确枚举语义时不使用裸字符串或布尔组合。
- 公共边界验证外部输入，内部函数接收已验证类型；整数转换优先使用检查转换。
- `unsafe` 块保持最小，并用 `// SAFETY:` 写明调用者与实现共同维持的条件。
- `brocade-core` 保持纯编译层。影响输出的无序集合必须在序列化前显式排序。
- `brocade-deployment` 拥有共享协议形状；Console 和 Agent 不复制协议结构。
- Console 负责鉴权、HTTP 表示和编排，不把数据库事务或系统收敛逻辑塞进路由处理器。
- Agent 接收期望状态并收敛，不引入只能执行一次、无法重试的命令脚本模型。
- 单元测试靠近实现；跨模块、数据库和协议行为放在 crate 的 `tests/`。缺陷修复补最小回归测试。

## PostgreSQL

- 当前只维护 `crates/brocade-store/migrations/0001_init.sql` 这一份全新安装定义；除非产品明确引入原地升级策略，否则不新增增量迁移。
- 多表业务操作在同一事务中完成，并通过锁、约束、幂等键或冲突处理表达并发策略。
- 租户、角色、凭据、授权和发布查询必须证明作用域，补越权与并发回归测试。
- 参数绑定是唯一允许的外部输入传递方式；不得拼接 SQL。

## 协议与兼容

- 协议变化必须同时检查 Console、Agent、`brocade-deployment`、版本协商和前端契约。
- 新旧 Console 与 Agent 的升级顺序必须可解释；不能依靠“一次全部更新”保证正确。
- 未识别字段、缺失能力和旧节点状态要有显式行为，不能静默猜测。

## 文件、进程与网络

- 文件落盘遵循临时文件、权限、校验、原子替换的顺序。
- 网络和子进程必须有超时、取消和有界重试；日志、队列、缓存和离线缓冲必须有上限。
- 外部路径、URL、命令参数、摘要和下载内容在系统权限边界前完成校验。
- 敏感值不进入日志、错误、测试夹具或生成产物。

## Shell、配置与生成物

- `install.sh` 保持 POSIX `sh` 兼容；变量加引号，临时文件可靠清理，下载后先校验再执行或安装。
- `Cargo.lock` 和 `frontend/package-lock.json` 随有意的依赖变更提交，不手工编辑。
- `target/`、`frontend/dist/`、`node_modules/`、`.tools/`、`brocade-agent-state/`、本地 GeoIP 数据和临时基准目录不得进入提交。
- `crates/brocade-core/tests/golden/` 只在产物语义有意改变时更新，并逐项审查差异。
- workspace 版本只在根 `Cargo.toml` 的 `[workspace.package]` 维护。

## Xray fork

- 只修改 Brocade 数据面需要的范围，保持上游模块路径和 `BROCADE_UPSTREAM.toml` 基线。
- 生成文件从源定义重新生成，不直接手改。
- 上游导入与 Brocade 补丁保持可区分；只格式化实际修改的 Go 文件。
- 数据面更改按 CI 中的确定性、fuzz-seed、race 和 vet 范围验证。

验证矩阵见 `testing.md`，发布边界见 `../runbooks/release-and-rollback.md`。
