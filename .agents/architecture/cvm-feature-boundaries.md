# 容器扩展编译与发行边界整理

- 状态：draft
- 最后更新：2026-11-10
- 产品基线：按全新、未发布产品设计，不接续任何既有实验版本或数据；`tat.ac` 的 CVM 实验记录另按限定范围清理。
- 范围：基础版与容器版的职责、依赖、数据、安装、升级和验证边界，作为后续实施的设计基线。
- 代码基线：当前工作树，HEAD 为 `07039a4e04cdb85c14e17e7c860393be821a4e67`。当前 CVM 实现包含未提交和未跟踪文件，不能仅凭该提交重建本次分析对象。
- 关联：[系统边界](system-overview.md)、[后端标准](../standards/backend.md)、[验证标准](../standards/testing.md)、[CVM 产品行为](../../docs/cvm.md)、[Cube 运行栈](../../docs/cube-runtime.md)。

目标是让默认发行包不编译、不分发、不启动容器业务能力。需要容器时，通过安装扩展发行包切换为容器版 Console，再按节点用途安装带 CVM 能力的 Agent 和宿主运行栈。采用同一套源码的两个编译变体，保留 Console、Store、Deployment 和 Agent 的职责划分。

这里的“两包”指用户选择的两个发行产品，不等于两个 Rust crate，也不等于所有文件必须塞入两个压缩包。Cube 节点运行包继续按架构独立分发。本文记录产品边界与实现契约；实施及验证状态见文末。

## 目标与产品范围

### 两个发行产品

`base` 和 `cvm` 是产物变体标识，共用同一个软件版本号，不维护两条功能分支。

| 对象 | 基础版 base | 容器版 cvm |
| --- | --- | --- |
| Console | 普通模型、代理、授权、用户、转发、观测、发布等既有能力 | 基础能力加 CVM 管理和节点接口 |
| 前端 | 不构建容器页面、终端、容器导航和专属样式 | 构建容器页面及其机器页集成 |
| 可分发 Agent | 双架构基础 Agent | 双架构基础 Agent 及双架构 CVM Agent，按节点选择 |
| 容器执行依赖 | 不进入所选 Console 和 Agent 的生产依赖闭包 | CVM Agent 包含 Rust Cube 客户端 |
| 宿主运行栈 | 不安装、不下载 | 仅由需要计算能力的节点安装 |
| 数据库 | 同一条迁移链；保留共享保护和历史数据 | 同一条迁移链；开放 CVM 业务读写 |
| 默认安装 | 安装基础 Console 和基础 Agent | 必须明确选择扩展发行包或容器版 |

容器扩展包的控制面安装动作是替换为容器版 Console，并保持数据库、配置、服务身份和普通功能。直接替换容器版 Console 与扩展包安装使用同一套预检和切换逻辑。安装控制面扩展不会自动发布节点 Agent、更新 Xray 或变更宿主内核。

### 首发基线与实验环境

按用户确定的范围，本产品视为未曾发布。首版直接建立完整的变体身份、能力字段、协议、分发清单和数据库结构，不为已有 Console、Agent、安装器、Cube 执行状态或预发布 schema 设计接管、迁移与兼容分支。实验容器、磁盘、网络和操作历史不作为需要保留的升级输入。

现有代码中的旧格式探测、历史 checksum 转换、缺少变体字段的推断、实验协议回退和对应专属夹具，应在实施时沿引用检查并清理；不把这些路径搬入新的基础公共层。共同事务、权限、原子替换、幂等收敛和新产品自身的故障恢复仍保留。

实验环境的实际清理另列为独立操作，目标为 [tat.ac](https://tat.ac)。用户明确限定只清理 CVM 部分，其他现有数据与服务全部保留。该要求不授权重建数据库、重置普通业务或在文档更新时执行清理。新产品发布后自身产生的数据和两种变体之间的正常切换，仍按本文的状态保护与恢复边界处理。

### 节点角色

| 节点用途 | 所需 Agent | 所需运行栈 | 可承担的容器职责 |
| --- | --- | --- | --- |
| 普通节点 | base | 无 | 普通代理、转发和观测 |
| 容器入口或出口节点 | cvm | 不要求 Cube 或 KVM | 入口投递、网络提供侧、承载和清理 |
| 容器宿主 | cvm | 匹配的 Cube 栈与系统制品 | 计算、系统盘、Guest 网卡、额度和终端 |

入口、出口可以来自其他机器。不能只升级运行 Guest 的宿主，也不能为了提供入口、出口而给普通网络节点强制安装 Hypervisor。

每台节点继续只有一个 Brocade Agent 进程和一份节点身份。安装扩展时替换同一 Agent 为 CVM 变体，不并行启动基础 Agent 和 CVM Agent 共同消费期望状态、写同一状态目录或操作同一网络对象。Cubelet 等运行栈服务仍按原边界独立运行。

第一阶段只提供一个完整的 `cvm` feature，不将计算、网卡、入口、额度和终端继续拆成任意组合。网络节点可以使用相同的 CVM Agent，通过实际能力上报区分提供侧与计算宿主；计算工作线程应根据本机角色和能力决定是否运行，不因没有 Cube 配置持续报错。

### 不属于此次拆分的变化

不重写容器产品模型，不迁移到另一套容器引擎，不改变现有网络、配额和原盘语义，不创建第二套用户与认证系统。开发预览使用的 Docker、CI 的数据库容器和部署工具中的“容器”不属于 CVM 产品扩展，不能按关键词一并删除。

## 当前实现与数据流

当前计算链路为：

```text
容器前端
  -> Console 鉴权与 HTTP
  -> Store 的实例规格、操作、容量和资源预约
  -> Deployment 共享期望状态与回执
  -> Agent CVM 工作线程
  -> brocade-cube Rust 客户端
  -> Cubelet 与 containerd Tasks
  -> Shim 与 Hypervisor、CubeVS、卷和 Guest
```

入口和出口另外经过提供节点与消费节点的 Agent，不能归并为本机计算操作。容器规格与操作保存在 CVM 业务表中，与普通代理模型修订、配置发布保持各自的状态来源。

### 已存在的安装边界

[节点安装器](../../crates/brocade-console/templates/install.sh)默认 `BROCADE_ENABLE_CVM=0`，通过 `--enable-cvm` 获取经过摘要校验的运行包。运行包由 [cvm_package.py](../../scripts/cvm_package.py) 组装，包含 Cubelet、Shim、运行工具、系统制品和宿主模式恢复程序；它不编译或交付第二套 Console。

[Cube 文档](../../docs/cube-runtime.md)已经将运行栈定义为独立的可选安装与发布域。普通 Agent 更新不会隐式替换 Cube 栈；KVM、PVM、换核、持久存储和恢复继续属于运行栈域。

### 尚未存在的编译边界

| 现状 | 代码依据 | 拆分影响 |
| --- | --- | --- |
| Agent 非可选依赖 `brocade-cube` | [Agent manifest](../../crates/brocade-agent/Cargo.toml) | 基础 Agent 当前仍编译 Cube RPC、Guest I/O 等依赖 |
| Agent 无条件声明和启动 CVM 模块 | [Agent main](../../crates/brocade-agent/src/main.rs)、[CVM 启动器](../../crates/brocade-agent/src/cvm.rs) | 当前会启动计算、网卡、入口、额度、观测和终端线程 |
| Deployment 非可选依赖 `cube-terminal` | [Deployment manifest](../../crates/brocade-deployment/Cargo.toml)、[终端协议](../../crates/brocade-deployment/src/cvm/terminal.rs) | 只把 Agent 的 Cube 依赖改为 optional 仍未切断全部 Cube 源码依赖 |
| Store 无条件公开 CVM 模块 | [Store lib](../../crates/brocade-store/src/lib.rs)、[CVM Store](../../crates/brocade-store/src/cvm.rs) | 业务层和部分公共路径必须先分离 |
| Console 无条件挂载 CVM API、服务字段和分发资源 | [Console lib](../../crates/brocade-console/src/lib.rs)、[HTTP](../../crates/brocade-console/src/http.rs) | 只裁 HTTP 子模块会留下状态类型、鉴权和安装脚本引用 |
| Console 主程序有 CVM 观测保留任务和历史维护入口 | [Console main](../../crates/brocade-console/src/main.rs) | 编译裁剪需要覆盖后台任务和离线命令 |
| 前端固定注册容器入口和异步模块 | [导航](../../frontend/src/forge/shell.tsx)、[页面注册](../../frontend/src/panes/index.tsx)、[预加载](../../frontend/src/panes/preload.ts) | 隐藏导航不等于排除构建产物 |
| 机器页查询 CVM、展示状态并生成安装参数 | [机器页](../../frontend/src/panes/nodes.tsx) | 非容器页面也需要切开集成点 |
| Agent 构建和分发按架构选择 | [子构建](../../crates/brocade-console/build/agents.rs)、[二进制发布](../../crates/brocade-store/src/binary_release.rs) | 两种 Agent 必须拥有明确的变体身份 |

## 模块所有权与依赖方向

### 目标所有权

| 层或目录 | 基础部分保留 | CVM feature 内包含 | 需要移出的耦合 |
| --- | --- | --- | --- |
| `brocade-core` | 纯模型编译、确定性产物、公共哈希和网络模型 | 不新增容器运行时职责 | 不以裁剪为由把数据库、内核或 Cube 访问放入 Core |
| `brocade-deployment` | 通用协议、Agent 通道、发布协议和公共预算工具 | `cvm.rs`、`cvm/` 及 `cube-terminal` 依赖 | 产品变体身份属于通用发布协议，不能藏在 CVM 协议内 |
| `brocade-store` | 普通业务、统一迁移、资源协调、扩展残留保护、发布目录 | 实例、额度、宿主策略、网卡、入口、监控和终端策略 | 普通代码不能再调用 `crate::cvm` 获取公共锁或删除保护 |
| `brocade-console` | 鉴权、普通 HTTP、实时通道、分发和服务生命周期 | 容器路由、终端会合服务、CVM 分发资源和观测保留任务 | 通用状态和构造器不得强制持有 CVM 服务 |
| `brocade-agent` | 普通收敛、自更新、Xray、转发、系统网络和连接跟踪 | `cvm.rs`、`cvm/`、`cvm-check` 和 Cube 客户端 | conntrack 通过公共接口获取强制跟踪范围 |
| `brocade-cube` | 无；基础发行不选择该依赖 | RPC、执行记录、Guest I/O、原盘核查、网卡和终端 | 不承担用户、认证、业务额度和独立 HTTP 控制面 |
| `components/cube` | 无产品运行依赖 | 可选 Cube 节点执行栈及固定协议源码 | 不把 Cube 全树加入基础 Agent 的源码版本闭包 |
| `frontend` | 普通导航、机器、代理、用户和转发页面 | CVM 页面、API、样式、终端依赖和机器页扩展组件 | 基础机器页不依赖 CVM 查询、类型和组件 |
| Launcher 与根安装器 | 本机 Console 和数据库编排、发行包校验 | 识别并切换到带 CVM 的 Console 产物 | 不新增 Launcher 对 CVM 业务 crate 的依赖 |
| 构建与 CI | 固定构建入口、摘要和双架构验证 | 显式 CVM 变体及 Cube 栈构建、验收 | 缓存、外置产物和发行名称不能继续只按架构区分 |

第一阶段沿现有 crate 组织 feature，不为“两包”机械地复制 Console、Store 或协议 crate。边界稳定后，只有依赖闭包或复用需求证明必要时再拆出新 crate。

### 依赖约束

```text
基础 Console -> 基础 Store -> 公共资源协调与统一 schema
       |              |
       +-----------> 通用 Deployment

CVM Console -> Store/cvm -> Deployment/cvm
CVM Agent   -> Deployment/cvm
            -> brocade-cube -> 固定 Cube 与 containerd 协议
            -> 公共网络接口

基础 Agent  -> 公共网络接口
公共网络接口不得反向依赖 Agent/cvm 或 brocade-cube
```

基础编译保留的是通用协调和防止误操作的最小读取能力，不包含创建实例、分配 CVM 资源、生成容器期望状态或执行 Guest 操作的能力。数据库中存在容器表不代表基础版开放容器业务。

## 必须先整理的共享边界

### 数据库资源锁和端口占用

当前 `cvm::nics::lock` 使用事务 advisory lock `pg_advisory_xact_lock(1112687438,65)`，并要求先于实例、用户和参与节点的行锁取得。普通模型编辑、草稿步骤、转发、节点更新和发布路径都调用该函数。

应把同一把锁的所有权移到中性公共模块，例如 `resource_coordination`，保留事务范围和加锁顺序。两种变体的所有竞争路径必须使用同一锁键；可以沿用当前键值，但不承担与实验版本并行运行的契约。不能在禁用 CVM 时把公共加锁变成空操作。

当前 [port_reservations.rs](../../crates/brocade-store/src/port_reservations.rs)读取 `cvm_nic_resources`，防止普通监听和容器网卡、入口重复占用端口。该冲突检查留在公共保护层；第一阶段继续读取原表，不通过重命名全套表来完成模块拆分。容器分配器调用相同公共检查和同一把锁。

需要同步替换的生产入口包括：

- `brocade-store/src/console.rs` 中的普通模型写入和网络锁计时。
- `brocade-store/src/console/steps.rs` 的草稿步骤。
- `brocade-store/src/console/nodes.rs` 的节点更新、退役和移除。
- `brocade-store/src/forwarding.rs` 的普通端口转发写入。
- `brocade-store/src/deployment.rs` 的发布相关事务。
- `brocade-store/src/port_reservations.rs` 的监听冲突校验。

以 [model_lock_order.rs](../../crates/brocade-store/tests/cases/model_lock_order.rs) 和 [forwarding_lock_order.rs](../../crates/brocade-store/tests/cases/forwarding_lock_order.rs) 的现有行为为回归依据，覆盖跨基础版与容器版的并发，而非只证明单一 feature 内没有死锁。

### 节点退役和历史引用

当前 [节点管理](../../crates/brocade-store/src/console/nodes.rs)会检查实例、系统盘和网卡引用，退役提供节点还会推进网卡与入口撤销。这些语义不能因为 `cvm` 未编译而消失。

公共层保留对活动扩展资源、未完成清理和历史引用的读取保护；CVM 层负责生成与推进容器撤销期望状态。基础版遇到需要 CVM 清理的对象，应返回明确冲突，要求使用容器版完成清理；不能跳过撤销后把节点标成已退役。历史记录继续阻止不允许的物理移除。用户或租户删除也须保留现有外键与引用约束，不放宽级联语义。

### Agent 的 conntrack 与 NAT

[conntrack.rs](../../crates/brocade-agent/src/conntrack.rs)负责 Xray TCP 的 NOTRACK 策略。它读取 CVM 的地址、端口范围；[网卡收敛](../../crates/brocade-agent/src/cvm/nics.rs)反过来刷新强制跟踪范围。这是 NAT 与回复隔离的执行依赖，不只是连接观测。

目标是由公共网络模块持有强制跟踪范围和策略串行化，CVM 提供已验证的范围。基础 Agent 在确认没有 CVM 在用状态后提供空范围；CVM Agent 保持先保护投递流量、再开放网络的顺序。读取失败必须显式失败，不能用空集合掩盖。Bridge 流量、UDP、转发流量和普通 TCP 的既有规则继续保留。

新产品从 CVM Agent 切回基础 Agent 时，候选二进制的预检必须检查当前 CVM 执行记录与实际残留；不能在启动后用空范围覆盖仍在用的策略。网卡期望状态和检查点不是可随意删除的缓存。该检查不承担识别和接管实验版本状态的职责。

### 网络额度与流量计量

CVM 计算内存与网络状态额度共享宿主物理预算，相关代码位于 [网络额度 Store](../../crates/brocade-store/src/cvm/network_quota.rs)、[额度协议](../../crates/brocade-deployment/src/cvm/network_quota.rs)和 CubeVS。它们整体留在 CVM 域，不拆成基础版的普通用户流量功能。

[网卡计量](../../crates/brocade-store/src/cvm/nics/metering.rs)有独立的计数游标、月份汇总与超额执行；它不应替代或重置普通代理用户流量账本。公共时间换算等纯工具可以继续复用。移除扩展时也不能因停止上报而提前释放仍占用的额度、地址和端口。

## Rust 条件编译边界

### feature 传播

以下是目标关系，不是当前可直接使用的构建参数。

| crate | 默认状态 | `cvm` 启用关系 |
| --- | --- | --- |
| `brocade-deployment` | 不含 CVM 协议与 Cube 终端依赖 | 编译 `cvm` 模块，启用 optional `cube-terminal` |
| `brocade-store` | 普通业务与公共保护 | 启用 `brocade-deployment/cvm`，编译 CVM 业务 |
| `brocade-agent` | 不含 CVM 线程和命令 | 启用 `brocade-deployment/cvm` 和 optional `brocade-cube` |
| `brocade-cube` | 不由基础发行选择 | 显式依赖 `brocade-deployment/cvm`，保留自己的协议生成 |
| `brocade-console` | 不含 CVM 服务、路由和后台任务 | 启用 Store 与 Deployment 的 `cvm`，并向子构建传递产物变体 |

各产品 crate 的默认 feature 集合不含 `cvm`；依赖边显式控制 `default-features`。用 `#[cfg(feature = "cvm")]` 裁剪模块和引用，而非只用运行时判断绕过调用。CVM 专属测试、夹具和公共导出同步加边界，基础构建不得靠伪造空 CVM 实现维持编译。

Cargo feature 是累加的条件编译和可选依赖机制；同一次依赖图中启用的特性会合并。基础版以“没有启用 cvm”表达，不创建与 `cvm` 互斥的 `base` feature。两种产物分两次 Cargo 调用；每种变体内部继续遵循双架构构建规范。[Cargo features](https://doc.rust-lang.org/cargo/reference/features.html)、[Cargo resolver](https://doc.rust-lang.org/cargo/reference/resolver.html)。

### 基础构建的证明范围

验证对象是所选生产包的依赖闭包及实际发行文件。仅检查 `Cargo.lock` 是否还有 Cube 条目不能证明裁剪成功；共享锁文件可以保留其他变体需要的依赖。`cargo build --workspace` 也会显式选择 workspace 内的 `brocade-cube`，不能作为基础发行依赖闭包的反证。

基础 Console 和 Agent 的所选依赖图中不得出现 `brocade-cube`、`cube-terminal` 或因 CVM 引入的协议生成链。通用层如果独立使用相同的第三方库，不按库名一概禁止。还需确认没有启动 CVM 线程、访问 Cube socket 或请求 CVM API。

## Console 接口与服务生命周期

### 容器业务接口

以下现有接口归入 `cvm` 路由组，统一从容器版挂载；基础版不注册这些业务处理器。

| 业务 | 现有路径 |
| --- | --- |
| 工作区与计算 | `/cvm`、`/cvm/readings`、`/cvm/{id}`、`/cvm/{id}/monitoring`、`/cvm/{id}/operations` |
| 额度与宿主 | `/cvm/quota`、`/cvm/quota/network/preview`、`/cvm/hosts/{node}` |
| 出口网络发布 | `/cvm/networks`、`/cvm/networks/{id}`、`/cvm/networks/{id}/admission` |
| 实例网卡与路由 | `/cvm/{id}/network`、`/cvm/{id}/interfaces`、`/cvm/{id}/routes`、`/cvm/{id}/interface-versions`、`/cvm/{id}/interfaces/{nic}/transport`、`/cvm/{id}/interfaces/{nic}/version` |
| 访问入口与发布 | `/cvm/entries`、`/cvm/entries/{id}`、`/cvm/entries/{id}/target`、`/cvm/{id}/entries`、`/cvm/entry-publications`、`/cvm/entry-publications/{id}`、`/cvm/entry-publications/{id}/admission` |
| 浏览器终端与策略 | `/cvm/{id}/terminal`、`/cvm/{id}/terminal/{session}/stream`、`/cvm/hosts/{node}/terminal` |
| 节点计算与观测 | `/agent/v1/cvm/inventory`、`/agent/v1/cvm/desired`、`/agent/v1/cvm/report`、`/agent/v1/cvm/observation-targets`、`/agent/v1/cvm/observations` |
| 节点网卡与入口 | `/agent/v1/cvm/nics/inventory`、`/agent/v1/cvm/nics/desired`、`/agent/v1/cvm/nics/report`、`/agent/v1/cvm/entries/desired`、`/agent/v1/cvm/entries/report` |
| 节点额度 | `/agent/v1/cvm/network-quotas/desired`、`/agent/v1/cvm/network-quotas/report` |
| 节点终端 | `/agent/v1/cvm/terminal/control`、`/agent/v1/cvm/terminal/policy`、`/agent/v1/cvm/terminal/{session}/stream` |
| 运行栈安装器 | `/enroll/cvm-stack.py` |

需要同时整理 `AppState`、`ConsoleServices`、构造器、鉴权路径分类、公开读取和用户权限判断。终端服务的票据、会话、连接与退出处理随 feature 一起裁剪，普通实时通道和 HTTP 生命周期仍保留。现有公共 Agent 通道并未自动承载全部 CVM HTTP；此次拆分不顺带改变传输机制。

基础版对上述路径返回 API 层的 404，不能被 SPA fallback 变成 200 HTML。缺少接口不构成资源已清理的证据；CVM Agent 与基础 Console 混接时不得把 404 解读为空期望状态后清空本机资源。

### 通用能力接口

在现有 `/bootstrap` 增加由服务器编译事实派生的能力字段，例如 `features.cvm`，供导航和请求门控使用；具体字段与版本接口统一定义。该字段只说明 Console 支持，不能代替节点实际能力或用户权限。

判断创建权限至少包括：Console 已编译 CVM、Agent 支持相应协议、宿主运行栈实际就绪、宿主策略开放、用户作用域与额度允许。网络提供侧使用独立的提供能力，不要求伪造计算就绪。过期或缺失的能力记录显示未知或未就绪，不沿用无限期成功状态。

首版服务端明确返回能力字段，基础版为 false，容器版为 true。字段缺失或清单不匹配视为协议或资产错误，显示明确状态，不猜测实验版本的能力，不调用 CVM 接口做兼容探测。

### 后台任务与离线命令

CVM 观测保留任务只在容器版启动。普通计费、授权、证书、观测、数据库维护和发布任务保持独立，不能放进同一个 CVM 开关。

`database-check` 和正常 `database-migrate` 两版均保留，检查首发统一 schema。旧实验 schema 转换不进入新产品的维护接口；当前 `BROCADE_CVM_COMPOSITION_RESET`、`BROCADE_CVM_HOST_POLICY_COMPAT` 及其他仅为历史部署保留的转换路径、配置和测试，沿调用链清理。新产品自身的 schema 版本校验和扩展使用状态检查不能依赖完整 CVM 业务模块。

### 配置与安装参数归属

| 配置或参数 | 所有者与拆分行为 |
| --- | --- |
| `BROCADE_CVM_BUNDLE_URL_X86_64`、`BROCADE_CVM_BUNDLE_SHA256_X86_64` 及对应 `AARCH64` 项 | Console 的已校验运行包目录；只由容器版用于扩展分发 |
| `BROCADE_CVM_COMPOSITION_RESET`、`BROCADE_CVM_HOST_POLICY_COMPAT` | 实验兼容遗留，列入移除清单，不进入首发配置 |
| `BROCADE_ENABLE_CVM`、`--enable-cvm` | 节点安装器的计算扩展选择；需同时确保安装的是 CVM Agent |
| `BROCADE_CVM_MODE`、`--cvm-mode` | 节点运行栈的 KVM 或 PVM 模式选择，不能用来判断 Console 编译能力 |
| `BROCADE_CVM_BUNDLE_URL`、`BROCADE_CVM_BUNDLE_SHA256`、`BROCADE_CVM_INSTALLER_URL` 及安装器对应参数 | 节点安装时的运行包输入；保持来源、架构和摘要校验，不覆盖 Agent 变体选择 |
| `BROCADE_CVM_CONFIG`、`BROCADE_CVM_STATE_DIR` | CVM Agent 与 Rust Cube 客户端的私有配置和执行记录，不作为 Console 配置 |
| `BROCADE_CVM_AGENT_BIN` | 运行栈安装器定位维护契约检查使用的 Agent；需核对它确为所选 CVM 产物 |
| `CUBE_NETWORK_ATTACHMENTS_CONFIG` 与其他 Cube 栈配置 | Cubelet 的设备、持久状态和网络契约，继续由运行栈安装器与 Cube 配置管理 |
| CVM 测试开关、观测夹具输入输出与安装模板替换标记 | 验收或脚本内部契约，不推广为产品能力开关 |

Agent 变体选择与计算运行栈安装选择需要两个独立语义。网络提供节点可以选择 CVM Agent 而不启用计算栈；`--enable-cvm` 继续表示安装计算扩展。具体新增参数命名在安装契约中冻结，并同步更新帮助、环境示例和命令生成。已有节点省略参数时保留已知选择，不能用首次安装默认值覆盖升级状态。

## 前端构建与运行边界

基础前端的构建模块图应排除 `panes/cvm/`、`api/cvm*.ts`、容器异步入口和专属终端资源。Vite 的构建常量可以静态替换并帮助裁剪，但是否真正移除要检查模块图和产物，不能凭隐藏按钮判断。[Vite 环境变量与模式](https://vite.dev/guide/env-and-mode.html)。

| 集成点 | 目标处理 |
| --- | --- |
| `forge/shell.tsx` | 桌面与移动导航均按编译能力和服务端能力显示 |
| `forge/state.ts`、`forge/route.ts`、`forge/access.ts` | 容器路由注册与恢复可控；基础版恢复旧 hash、窗口和深链时回到可用页面 |
| `panes/index.tsx`、`panes/preload.ts` | 基础版不保留对 CVM 页面的异步 import |
| `api.ts` 与 `api/cvm*.ts` | 专属请求归扩展入口，不从基础聚合入口强制拉入 |
| `panes/nodes.tsx` | 拆出 CVM 状态、查询、宿主类型、安装选项与命令拼接；基础机器页不发相关请求 |
| `styles.css` | 将 CVM 专属样式划到扩展样式入口，保留普通页面共用令牌和布局 |
| xterm 与终端 CSS | 不进入基础发行 bundle；开发环境仍可用统一 npm 锁文件安装工具依赖 |
| 查询缓存和会话 | 切换版本、能力变化和恢复旧窗口后不继续展示旧 CVM 数据或发起轮询 |
| Vite 开发代理与契约工具 | 覆盖两种模式的路由；基础模式不要求 CVM API 可用 |

Console 的内嵌资产必须与自身变体匹配。`BROCADE_CONSOLE_ASSETS_DIR` 和运行时 `BROCADE_CONSOLE_DIST` 都可能绕过默认前端来源，需要资产变体清单及匹配校验；不能给基础 Console 接上容器版资产后出现假功能。前端门控不替代服务器鉴权。

## 数据库与持久状态边界

### 统一迁移链

推荐两版使用同一 schema 和迁移 checksum，不按 feature 跳过部分 SQL。当前 `0005_node_xray_enabled.sql` 同时包含普通 Xray 状态变化与 CVM 表，不能将整个迁移作为容器专属迁移关闭。基础版初装可以建立空的 CVM 表；它不安装节点运行栈，也不开放 CVM 业务。

首发 schema 可以在现有未发布迁移内整理，不保留实验版本的 checksum 接管与转换路径；新增迁移文件仍遵循[后端标准](../standards/backend.md)的创建门槛。实验 CVM 记录清理与数据库结构设计分别处理，不能为了采用新设计重置 `tat.ac` 的其他业务或改写其迁移回执。新产品正常卸载扩展也不通过重建库、删表或伪造迁移历史完成。

### CVM 表的领域归属

下表覆盖当前 `0005` 创建的 33 张 CVM 表，作为拆分和降级审计清单。

| 领域 | 当前表 |
| --- | --- |
| 宿主能力与身份策略 | `cvm_hosts`、`cvm_host_policies`、`cvm_host_policy_events` |
| 计算额度与宿主网络预算 | `cvm_quotas`、`cvm_quota_changes`、`cvm_network_quota_hosts`、`cvm_user_host_network_quotas`、`cvm_network_quota_events` |
| 实例与不可变执行历史 | `cvm_instances`、`cvm_volumes`、`cvm_interfaces`、`cvm_spec_versions`、`cvm_operations` |
| 观测 | `cvm_observation_state`、`cvm_observation_samples`、`cvm_observation_events` |
| 终端策略 | `cvm_terminal_policies`、`cvm_terminal_policy_events` |
| 出口网络 | `cvm_nic_nodes`、`cvm_nic_publications`、`cvm_nic_publication_versions`、`cvm_nic_requests`、`cvm_nic_allocations`、`cvm_nic_bindings`、`cvm_nic_route_versions` |
| 访问入口 | `cvm_entry_nodes`、`cvm_entry_publications`、`cvm_entry_publication_versions`、`cvm_entry_tunnels`、`cvm_entries` |
| 跨业务资源占用 | `cvm_nic_resources` |
| 网卡流量计量 | `cvm_nic_meter_cursors`、`cvm_nic_monthly_usage` |

CVM 业务模块拥有这些表的领域写入；公共保护层可以读取资源占用和历史引用。现有外键、触发器、`notify_agent_channel_change`、额度修订推进和锁顺序保持有效，不因代码编译开关分成两份事实来源。

### 降级保护的读取范围

基础版必须具备不依赖 CVM 类型的扩展使用状态检查，用于启动、版本切换、节点降级和普通业务保护。只检查“有没有运行中的 Guest”不够。

| 状态 | 切回基础版前的处理 |
| --- | --- |
| 在用或保留实例、未释放系统盘 | 删除或按现有产品语义完成释放，并等待实际证明 |
| 未确认网卡卸载、未释放分配和地址端口预约 | 在容器版完成双方收敛与清理回执 |
| 入口、跨机隧道和提供侧残留 | 撤销后核对入口节点、宿主及提供节点的实际状态 |
| 未完成操作、失败待恢复任务、在途发布和变体切换 | 完成或按现有状态机结清；不能靠换版本跳过 |
| 生效网络额度、Guest 内存关联和本地配额检查点 | 下发撤销并核对容量释放，保留计量历史 |
| 可接受新分配的网络或入口发布项 | 在容器版关闭准入并撤销；不得留下可继续分配的目录 |
| 终端活动会话 | 关闭和排空，保留既有策略审计 |
| PVM 待重启、恢复程序和宿主转换状态 | 先完成或恢复该转换，不能卸载恢复程序中断后续启动 |
| 已清理的历史实例、操作、策略、计量和能力记录 | 可保留；不能因表非空永久禁止降级 |

控制面状态和节点实际状态共同决定是否可降级。离线节点缺少清理证据时保留资源和拒绝降级，不把未知当空。启动检查的失败只拒绝接管该数据库，不执行资源删除或自动重置。

### 节点文件与系统对象

| 对象 | 所有者与保留规则 |
| --- | --- |
| Agent 的普通状态目录、节点凭据与升级记录 | 通用 Agent 保留，安装扩展不重新纳管或轮换身份 |
| `cvm-nics-desired.json`、`cvm-entries-desired.json`、网络额度与计量检查点 | CVM Agent 持久执行状态，离线重放和降级预检都需要 |
| `BROCADE_CVM_CONFIG`、`BROCADE_CVM_STATE_DIR` 指向的对象 | Rust Cube 执行库读取，保留原路径、权限、执行记录和原对象归属 |
| `/opt/brocade/cvm/` 的版本、当前链接与 `host.json` | 运行栈安装器及宿主模式恢复程序管理 |
| `/data/cubelet/` 的卷、组件、网卡与系统制品 | Cube 栈管理；不得用卸载基础 Agent 的方式删除 |
| nftables、bridge、TAP、netns、WireGuard、路由、BPF pin | 按原资源身份和清理证明撤销，不执行宿主全局 flush |
| systemd 服务与 Agent CVM drop-in | 只改扩展拥有的服务和环境；普通 Agent 及宿主已有服务继续保留 |

降为基础 Agent 需同时证明本机执行记录和扩展拥有的实际对象已释放。保留根目录或已清理历史本身不等于扩展仍在用；文件无法解析、归属不明或对象状态未知则不能忽略。

## 构建与产物分发边界

### 统一入口和缓存

继续使用 [build-release.py](../../scripts/build-release.py)及[固定构建入口规范](../runbooks/console-deployment.md)。建议在同一入口增加显式变体参数，具体命令在实现后同步到产品文档，不把本文中的标识当成已支持选项。

需要同时调整以下路径：

1. 固定入口中的前端构建、Agent 构建、Console 构建、产物验收和记录。
2. `brocade-console/build.rs` 的默认子构建，以及 `build/agents.rs` 的 Cargo 命令。
3. `build/source_revision.rs` 的版本闭包：基础 Agent 不因 Cube 专属源码变化而显示升级；产物摘要仍是机器身份。
4. `.github/workflows/ci.yml` 的变体矩阵、打包、校验清单、上传和升级验收输入。
5. 根 `install.sh` 对 `BROCADE_PLATFORM`、`SHA256SUMS`、Console 和 Launcher 的校验及升级流程。

父 Cargo 的 feature 不会自动传到 build.rs 启动的子 Cargo 或 npm。构建脚本必须显式选择 Agent 变体和前端变体；基础与 CVM 产物使用独立输出身份和缓存键，避免共享路径上的最后一次构建覆盖前一次。每个变体继续构建 x86_64 与 aarch64，不退化为单架构。

基础构建仍需要现有 Xray、前端和静态 Agent 的工具链。裁掉 CVM 不表示可以去掉 Xray 所需的 Go、前端所需的 Node 或 Agent 所需的 Zig；Cube 栈的 Clang、BPF、GNU sysroot 与系统制品准备继续只属于独立运行栈构建。

外置 `BROCADE_AGENT_BIN_*`、预构建前端目录等入口必须核对变体元数据和实际文件摘要。若要给两种 Agent 提供外置输入，参数也须显式区分变体。不能靠文件名或一个旧 stamp 接纳产物；前端缓存记录、Agent 源码身份、Console 构建记录和发行校验清单均包含相关变体。

### Console 内嵌内容

基础 Console 只携带基础前端、基础 Agent 和既有 Xray。容器版 Console 携带容器前端及两类 Agent，保持普通节点可以继续使用基础 Agent。大型 Cube 栈、Guest 系统制品和 PVM 内核不内嵌 Console，由现有运行包渠道按需获取。

容器版携带两类 Agent 会增加控制面包体和构建量，但它保持内嵌字节与摘要绑定，并支持混合节点。基础包节省的体积和构建耗时需要测量；不能把原本就未内嵌的系统镜像大小计入新增收益。

### 分发契约

当前 `/enroll/dist` 使用扁平的架构字段，安装器用 shell 解析。首版可以直接统一新的分发形状与解析实现，Console、安装器和测试同时调整，不保留旧字段别名或旧脚本解析兼容。无论最终采用扁平还是嵌套结构，都必须明确返回变体、架构、来源和摘要。

推荐现有普通下载入口在新发行中默认提供基础 Agent，新入口显式选择 CVM 变体；基础 Console 不返回可安装 CVM 的 URL、摘要或运行栈安装器。基础安装器收到 `--enable-cvm`、CVM 模式或专属运行包参数时，应在下载、写服务和消费一次性纳管凭据前明确拒绝，不能静默安装基础 Agent 后宣称成功。

新产品节点重装时读取已安装变体和扩展在用状态。省略选择时保留已知变体；首次安装默认 base；切换变体需要显式意图。首版产物必须能自描述变体，缺少字段时明确拒绝把它作为可接续的新产品节点，不设计实验节点的自动识别与原地转换。

## 发布身份与变体切换

### 产物身份

建议把 Agent 产物定位从 `arch` 扩为 `arch + flavor`，并继续使用实际 `sha256` 标识字节。构建信息包含软件版本、源码闭包版本、变体、协议能力和摘要，各自承担不同含义。

普通协议中的自描述、节点上报、发布 artifact、offer、执行回执、安装 manifest 和界面契约要同时定义。首版候选 Agent 的只读 `self-test` 直接返回可核对的变体与能力，不保留实验 Agent 的 usage 文本回退探测；只证明 ELF 能执行还不能证明扩展选择正确。

每类 Agent 的 build ID 由该变体的架构与摘要有序组成。不要把 base 与 cvm 的所有摘要无条件揉成同一个发布身份，导致仅修改 CVM 就让基础发布失效。普通自动更新保留变体；base→cvm 需要明确的发布意图并检查候选身份。cvm→base 先排空资源并停止 Agent，再由安装器独占执行状态完成检查；在线更新进程持有 Cube 与 NIC 状态锁，因此不允许远程发布承担降级。

### 现有发布账本

[binary_release.rs](../../crates/brocade-store/src/binary_release.rs)当前按架构匹配产物、冻结目标摘要并保留逐机尝试。第一阶段继续复用同一审计账本和每组件一张活动单的限制，发布单明确目标变体并拒绝隐式跨变体更新。新增列或 artifact 形状按统一迁移和协议兼容规则处理。

一次发布可以面向某个变体的双架构节点，不要求为了两种变体增加同时运行的发布单。冻结后的目标变体与摘要不因后续 Console 切换而变化；缺少原产物时明确不可继续领取，保留在途回执和审计，不能改成当前另一种变体的字节。

首发发布记录从一开始就包含变体，不导入实验发布单或补造逐机执行证据。实验 Agent 不参与新产品的升级组合；新产品自身的跨变体发布仍冻结目标身份并保持逐机审计。

### 版本组合

| Console | Agent 与运行栈 | 目标行为 |
| --- | --- | --- |
| base | base | 普通功能正常，无容器业务请求和入口 |
| cvm | base | 普通功能正常，节点不可作为 CVM 参与节点 |
| cvm | cvm，无 Cube 栈 | 可按实际权限作为网络提供侧；计算显示未安装或未就绪 |
| cvm | cvm，匹配 Cube 栈 | 在宿主策略、协议和额度允许时提供完整能力 |
| base | 仍承担 CVM 职责的 Agent 或持有活动资源 | 正常切换预检拒绝；异常混接也不下发清空或基础降级 |
| 新产品 Console | 缺少首发必需身份或能力字段的实验 Agent | 不接管、不推断变体；实验部分另按限定计划清理 |

首版统一建立计算、网卡、入口、额度和终端的协议与能力契约，`flavor=cvm` 不能替代实际运行栈能力。当前常量和格式作为实现盘点，可以同步整理，不要求继续接受实验协议。正式发布后的演进继续遵循共享协议和安全升级原则。

## 安装与移除流程

### 首次基础安装

校验基础发行包和变体，使用原有数据库模式与配置流程安装 Console；节点安装基础 Agent。没有容器运行包下载、Cube 服务、CVM drop-in 或宿主内核变更。普通 OpenVPN、Xray 和已有系统服务按原安装边界处理。

### 控制面启用容器

1. 验证容器版 Console、前端、两类 Agent 和运行包目录的摘要与兼容信息。
2. 按标准数据库检查和部署流程切换 Console，保留原配置、认证、监听和数据。
3. 确认控制面能力及容器入口可用；不因入口出现就认定宿主就绪。
4. 按实际参与节点选择 CVM Agent，创建可审计发布；普通节点保持 base。
5. 仅在计算宿主安装匹配运行栈；缺少原生 KVM 时按既有 PVM 流程处理，等待重启期间仅暂停 CVM。
6. 核对实际能力后开放宿主准入和用户额度，再创建实例与网络分配。

### 已有节点增加扩展

安装器保持节点身份、已有服务、普通状态和持久执行目录。先校验并安装支持维护契约的 CVM Agent，再安装与检查运行栈；需换核时保留待重启状态和恢复程序。网络提供节点只升级 Agent，不下载计算栈。重装、失败重试和中断恢复均须幂等。

### 移除扩展与切回基础版

先关闭新分配准入，在容器版中撤销实例、入口、网卡和额度，收到所有参与节点的实际清理证据，关闭终端并完成宿主转换。随后按节点停止 Agent，使用安装器完成 CVM Agent 降级预检和切换，最后通过数据库使用状态检查切回基础 Console。

卸载计算栈和退回原生内核是独立动作；不把切换基础版等同于删除系统盘、系统镜像或恢复程序。只剩已清理历史数据时允许基础版使用同一数据库，并继续执行端口占用、历史引用和删除保护。

## 失败恢复与回滚边界

| 失败位置 | 恢复责任与限制 |
| --- | --- |
| 构建或摘要校验失败 | 不接纳产物，不更新发行目录，不使用旧缓存假成功 |
| Console 替换前预检失败 | 保留原版本和配置，不创建新数据库，不消费节点纳管凭据 |
| Console 已切换但节点未升级 | 保持普通业务；容器显示未就绪，不自动强推 Agent |
| Agent 更新失败 | 沿用原有候选探测、原子替换和失败恢复；变体与本地状态一并核对 |
| 运行栈或 PVM 切换失败 | 使用运行栈保留版本和固定恢复程序，不让普通 Agent 更新接管换核 |
| 节点离线或清理结果未知 | 保留资源预约与原状态，允许重试，不宣称扩展已移除 |
| 清理后切回基础版失败 | 使用已保留且 schema 兼容的容器版恢复，不反向改写发布历史 |
| 带活动容器的容器版需回滚 | 优先回滚到兼容的容器版；基础版不能接管仍需要 CVM 收敛的环境 |

发布数据库保存元数据和证据，不保存所有历史二进制。可回滚字节必须另行保留；不能假定历史发布单能重新下载旧包。数据库迁移、状态 ABI、运行栈和内核兼容仍按[发布回滚手册](../runbooks/release-and-rollback.md)核验。

## tat.ac 的 CVM 实验环境清理计划

### 目标与执行位置

目标入口为 [tat.ac](https://tat.ac)。清除该环境的 CVM 实验记录，使新产品无需承接实验容器数据；其他现有业务保持原状。本节是待执行计划，本次文档更新不连接数据库执行变更，也不删除线上或节点资源。

执行前通过只读盘点核实该入口实际对应的 Console、数据库、参与节点、CVM 实例和资源归属。前文 33 张表是当前源码的领域清单，不是对目标环境实际表结构或数量的确认。不得凭域名推断数据库位置，也不能将其他环境并入清理。

### 清理白名单与保留范围

| 对象 | 允许的清理范围 | 必须保留的内容 |
| --- | --- | --- |
| 数据库 CVM 记录 | 实际盘点确认的 CVM 实例、系统盘记录、网卡、入口、资源预约、额度、监控、终端策略、操作及实验审计记录 | 普通模型与修订、代理配置、用户与租户、节点、凭据、普通额度与用量、订阅、授权和发布历史 |
| Guest 与系统盘 | 仅限目标清单中仍存在的 CVM 实验实例及其专属磁盘，先确认归属再清理 | 非 CVM 文件、其他工作负载的卷、宿主文件系统和存储池 |
| 网络对象 | 仅限能够对应到目标实例、分配或入口的 CVM bridge、TAP、netns、隧道、路由、规则和 BPF 对象 | 普通 Xray、端口转发、WireGuard、宿主路由、防火墙及其他服务的网络对象 |
| 执行记录与检查点 | 对应目标 CVM 对象的期望状态、执行记录与计量检查点 | 普通 Agent 状态、身份、升级记录和非 CVM 检查点 |
| 共享表、配置与服务 | 只处理已明确列入白名单且能证明属于目标 CVM 对象的专属记录；无法证明时保留 | 节点行、用户行、普通发布账本、认证会话、数据库与服务配置、迁移回执和共享视图 |
| 运行栈和宿主内核 | 本次记录清理不自动卸载 Cube 栈、不删除公共系统制品、不切换或卸载内核 | Agent、Console、Cube 工具、系统镜像库、启动项、运行栈恢复程序和其他宿主服务 |

源码中的 `managed_static_listener_ranges` 是普通监听范围的共享视图，不能因位于包含 CVM 的迁移中而删除。同一迁移内的 Xray 状态、转发触发器和通用二进制发布字段也不属于清理白名单。清除 CVM 占用不会重置普通用户的额度和用量。

数据库清理使用经核实的显式表清单和记录范围，不使用名称通配符直接生成删除，不使用 `DROP DATABASE`、schema 重建或 `TRUNCATE ... CASCADE`。不得全局停用外键或触发器。CVM 不可变历史表可能阻止普通 DELETE，实施脚本须在核对依赖后选择只作用于完整 CVM 白名单的事务性维护方法；遇到非 CVM 引用应失败并保留现场，不扩张删除范围。

### 清理步骤

1. **只读盘点与清单。** 列出目标数据库中实际存在的 CVM 表、记录数、实例与资源 ID、外键和触发器；核对宿主、提供节点、入口节点与实际对象。列出预计删除与明确保留的对象，无法确认归属的对象不进入删除清单。
2. **保留证据与恢复材料。** 保存受限权限的 CVM 记录导出及必要恢复材料，记录非 CVM 稳定业务数据的基线和服务状态。证据中不输出凭据或完整私有期望状态；正常增长的用量按时间窗口核对，不能要求其在在线业务期间完全不变。
3. **仅隔离 CVM 写入和重建。** 形成适用于现场版本的方式，暂停 CVM 新建、分配及相关重试，避免清理后被实验工作线程重新写回。普通代理、转发、用户流量上报和其他后台任务继续运行；无法单独隔离时先停在该步骤，不默认停止整个环境。
4. **处理仍在用的实验对象。** 对清单中实际存在的 CVM 实例、网卡、入口及跨机承载完成专属清理，再核对释放。若现场只有历史记录、没有运行资源，则标明该步骤不需要执行。禁止先清数据库、后让仍在运行的 Guest 和网络成为无主对象。
5. **事务性清理 CVM 记录。** 严格按依赖和白名单处理相关表中的实验数据，包括允许丢弃的 CVM 历史；事务内核对影响行数与非 CVM 引用。清理失败回滚本次事务，不以清空共享表来解决约束冲突，不改写迁移回执。
6. **清理对应执行残留。** 仅处理已确认释放对象的 CVM 检查点和残留规则，避免执行器重放；不递归删除整个 Agent 状态目录、`/data`、存储池或运行栈安装目录，不做全局 nftables、路由或 BPF 清空。
7. **保留验证与交付。** 复查 CVM 目标记录、关联资源和重复写入情况；对比非 CVM 稳定业务数据、凭据与配置的基线，验证普通代理、转发、订阅、用户访问和用量采集。记录每步实际时间、影响行数、资源清单和结果。

只读盘点完成后，执行清单和脚本应能够独立审查，明确标出每个变更对象。清理范围到此为止，不把此次操作扩展成普通业务重置、主机重装、运行栈卸载或新产品部署。

只读盘点可以先行，代码拆分可以在本地独立推进。实际清理应在移除或替换实验 CVM 执行能力之前完成，避免改成基础版后失去清理仍在用对象的能力；它不依赖为新产品补建实验兼容路径，也不自动触发新产品上线。

### 失败与验收条件

某一节点离线、资源归属不明、存在非 CVM 外键引用或非 CVM 对比异常时，停止对应清理步骤并保留已收集证据。数据库事务与外部 Guest 删除无法构成一个原子事务，恢复记录必须区分已删除的实验资源和已回滚的数据库操作，不能把导回记录描述为恢复了物理磁盘。

完成条件为：清单内 CVM 记录及对应实验资源已按范围处理，不再被工作线程写回；其他数据没有因本次清理被删除或重置，普通服务和正常业务仍工作。未执行、跳过或结果未知的项目逐项列出，不能只以容器页面为空判定成功。

## 实施拆分与完成条件

| 阶段 | 工作范围 | 可独立判断的完成条件 |
| --- | --- | --- |
| 共享边界 | 公共事务锁、端口冲突、节点与历史保护、conntrack 接口 | 普通路径不直接依赖 CVM 业务模块；现有并发和 NAT 行为保持 |
| 编译边界 | Deployment、Store、Agent、Console feature 与测试分组 | 所选基础生产图无 Cube 依赖；完整 CVM 图仍可构建 |
| 前端与接口 | 路由组、状态服务、能力字段、机器页扩展和样式入口 | 基础版不包含或请求容器业务；旧窗口和深链可恢复 |
| 构建与发行 | 双变体、双架构、缓存、外置产物、源码身份和包清单 | 混合缓存不能串包，父子构建和前后端变体一致 |
| 安装与发布 | 分发清单、变体保持、转换记录、自更新候选和残留检查 | 重装不隐式换型，跨变体操作有预检和完整回执 |
| 首发与运行验收 | 干净初始化、变体切换、网络提供侧、真实宿主和恢复 | 通过下表对应矩阵后，才具备发行条件 |
| 实验环境清理 | `tat.ac` 的 CVM 定向盘点、清理与非 CVM 保留验证 | 仅清除明确清单中的 CVM 记录及关联实验资源，其他内容保持 |

每阶段实现、测试和受影响文档一起交付，不以全树格式化或大规模重命名代替边界整理。产品操作行为稳定后更新 `docs/build.md`、`docs/install.md`、`docs/cvm.md`、`docs/cube-runtime.md` 和环境清单；长期决策确认后再记录 ADR，不把本计划提前当作已实现功能说明。

## 验收矩阵

下表是验收范围；实际完成情况单独记录，不能把设计要求当作已通过的验证。

| 范围 | 必须验证的结果 |
| --- | --- |
| 基础依赖闭包 | 显式选择基础 Console 与 Agent，依赖图无 Cube 执行链；不依赖 workspace 全选结果判断 |
| 条件编译 | base 与 cvm 两种组合的协议、Store、Console、Agent 编译、定向测试和 Clippy 均通过 |
| 基础 HTTP | CVM 路由和安装器不可用且不落到 SPA；普通 API、登录与 bootstrap 正常 |
| 基础运行 | 无 CVM 线程、Cube socket 访问和容器 API 轮询；普通收敛不受影响 |
| 前端裁剪 | 检查生产模块图、JS/CSS 和异步资源，无 CVM 页面和 xterm；不能只检查 chunk 文件名 |
| 前端恢复 | 两种变体下的桌面与移动导航、机器列表、安装向导、hash 和窗口恢复正确；缺少首发能力字段时明确失败 |
| CVM 界面 | 完整容器功能、角色作用域、过期能力与待重启状态正常；按前端标准截图比较并实际看图 |
| 共享资源并发 | 普通转发、模型变更与 CVM 分配竞争同一端口、地址或节点时保持互斥和锁顺序 |
| 数据删除保护 | 节点退役、节点移除、用户与租户删除不绕过在用资源和历史引用 |
| conntrack | 私有网络命名空间中覆盖 Guest 与入口 NAT、强制跟踪、普通 TCP/UDP、桥接、免跟踪和规则重建 |
| 数据库 | 两版初装 checksum 一致；基础到容器和已清理容器到基础可用；活动资源和未知 schema 被拒绝 |
| 首发契约 | 实验 schema 转换、旧端推断和回退探测已清理；首发必需身份及能力可校验 |
| 构建缓存 | base 到 cvm 再到 base、失败重试、外置 Agent 和外置前端均不接受另一变体的产物 |
| 双架构发行 | 每个承诺分发的变体均有 x86_64 与 aarch64 字节、摘要、源码与变体记录 |
| 二进制发布 | 同架构不同变体不会误匹配；普通更新保持变体；候选和迟到回执按冻结身份处理 |
| 安装重入 | 初装默认 base；扩展安装保持身份；已装 CVM 节点普通重装不降级；拒绝在凭据消费前发生 |
| 网络提供侧 | 无 Cube 和 KVM 的 CVM Agent 可提供真实入口或出口，并完成双方撤销清理 |
| 计算宿主 | 真实 Cube 栈覆盖 Guest 生命周期、原盘、网络、额度、终端与重启恢复 |
| PVM 与存储 | KVM、PVM、XFS/reflink、ext4 按各自声称支持的范围独立验收，不能互相替代 |
| 故障恢复 | 离线、未知回执、待重启、中断安装、失败更新和降级残留均保留正确状态 |
| 普通产品回归 | 模型确定性、配置与授权发布、Xray、用户流量、端口转发、订阅和非容器观测保持既有行为 |
| 首发安装与切换 | 受管与外置 PostgreSQL 的干净安装、base 到 cvm、已清理 cvm 到 base、失败恢复和重装通过；不以旧正式版本为升级基线 |
| 实验清理范围 | `tat.ac` 清理后的 CVM 目标记录及对应残留达到清单目标；非 CVM 稳定数据一致，服务与正常用量采集保持 |

快速检查和实际验收复用[验证标准](../standards/testing.md)、[CVM 验收](../../scripts/tests/cvm/README.md)、[安装与升级夹具](../../scripts/tests/upgrade/README.md)及 CI 的机制，并按本次“全新未发布产品”的明确范围调整历史升级矩阵和旧端夹具；不再要求接续实验或过去发布的数据。禁用 CVM 的构建保留专门的负向验证；启用 CVM 的构建必须确实收集并执行新产品的 CVM 用例，防止因 cfg 或空过滤获得假通过。

## 方案取舍

| 方向 | 对目标的覆盖 | 主要代价 | 采用范围 |
| --- | --- | --- | --- |
| 只保持运行栈可选安装 | 节点不装 Cube 栈，但 Console 和 Agent 仍含完整容器代码 | 改动小，无法实现基础编译裁剪 | 现状，不作为最终拆分 |
| 同源码双变体，扩展安装切换 Console | 基础包裁剪，容器版按需启用，继续复用现有状态所有权 | 构建、分发、自更新和转换需明确变体 | 已选定的第一阶段 |
| 基础 Console 不替换，另装独立扩展进程 | 可以独立安装、升级和卸载扩展进程 | 新增跨进程接口、事务边界、鉴权、通知、终端与故障恢复协议 | 仅在明确要求 Console 字节不变时重新设计 |

动态加载 Rust 库不作为第一阶段路径。现有模块是在进程内静态组合，没有插件生命周期契约；为满足“两包”引入动态加载会新增版本与 ABI 维护成本。今后若必须支持真正独立扩展服务，应先重新确定持久状态、鉴权和事务所有权，不能把现有 Store 方法简单搬到另一个进程。

## 需要在实现时冻结的产品契约

发行产品的名字可以沿用 `brocade` 与 `brocade-cvm`，但需要在实现时统一变体字段、下载路径和安装参数；这些命名不影响上述职责边界。第一阶段按“容器版携带两种 Agent、基础版保留统一 schema、默认更新保持变体、移除扩展需完成清理”推进。

还需在协议设计时冻结首发必需的变体与能力字段、跨变体发布请求的明确意图、候选自描述和资产清单格式。Console、Agent、安装器和前端共享同一契约，不增加历史字段推断和旧端兼容路径。

## 实现文件与验证入口索引

| 领域 | 当前入口 |
| --- | --- |
| Rust 清单 | [workspace](../../Cargo.toml)、[Agent](../../crates/brocade-agent/Cargo.toml)、[Console](../../crates/brocade-console/Cargo.toml)、[Deployment](../../crates/brocade-deployment/Cargo.toml)、[Store](../../crates/brocade-store/Cargo.toml)、[Cube 客户端](../../crates/brocade-cube/Cargo.toml) |
| 共享协议 | [Deployment lib](../../crates/brocade-deployment/src/lib.rs)、[通用协议](../../crates/brocade-deployment/src/protocol.rs)、[Agent 通道](../../crates/brocade-deployment/src/agent_channel.rs)、[CVM 协议](../../crates/brocade-deployment/src/cvm.rs)、[CVM 子协议](../../crates/brocade-deployment/src/cvm/) |
| Console 业务与服务 | [HTTP 路由](../../crates/brocade-console/src/http.rs)、[CVM HTTP](../../crates/brocade-console/src/http/cvm.rs)、[入口接口](../../crates/brocade-console/src/http/cvm/entries.rs)、[网卡接口](../../crates/brocade-console/src/http/cvm/nics.rs)、[终端接口](../../crates/brocade-console/src/http/cvm/terminal.rs)、[终端服务](../../crates/brocade-console/src/cvm/terminal.rs)、[主程序](../../crates/brocade-console/src/main.rs) |
| Store CVM 业务 | [CVM 模块](../../crates/brocade-store/src/cvm.rs)、[业务目录](../../crates/brocade-store/src/cvm/)、[网卡分配](../../crates/brocade-store/src/cvm/nics.rs)、[访问入口](../../crates/brocade-store/src/cvm/nics/entries.rs)、[运行状态回执](../../crates/brocade-store/src/cvm/node_state.rs) |
| Store 公共调用方 | [普通控制面](../../crates/brocade-store/src/console.rs)、[草稿步骤](../../crates/brocade-store/src/console/steps.rs)、[节点管理](../../crates/brocade-store/src/console/nodes.rs)、[转发](../../crates/brocade-store/src/forwarding.rs)、[发布](../../crates/brocade-store/src/deployment.rs)、[端口占用](../../crates/brocade-store/src/port_reservations.rs)、[数据库](../../crates/brocade-store/src/pg.rs) |
| Agent CVM | [启动与检查](../../crates/brocade-agent/src/cvm.rs)、[共享 backend](../../crates/brocade-agent/src/cvm/backend.rs)、[计算收敛](../../crates/brocade-agent/src/cvm/reconcile.rs)、[网卡](../../crates/brocade-agent/src/cvm/nics.rs)、[入口](../../crates/brocade-agent/src/cvm/entries.rs)、[额度](../../crates/brocade-agent/src/cvm/network_quota.rs)、[观测](../../crates/brocade-agent/src/cvm/observer.rs)、[终端](../../crates/brocade-agent/src/cvm/terminal.rs) |
| Agent 通用集成 | [main](../../crates/brocade-agent/src/main.rs)、[conntrack](../../crates/brocade-agent/src/conntrack.rs)、[身份](../../crates/brocade-agent/src/identity.rs)、[自更新](../../crates/brocade-agent/src/selfupdate.rs) |
| Cube 客户端 | [模块与 backend](../../crates/brocade-cube/src/)、[协议生成](../../crates/brocade-cube/build.rs)、[固定 containerd 协议](../../crates/brocade-cube/proto/UPSTREAM.toml)、[配置示例](../../crates/brocade-cube/examples/) |
| 数据库 | [迁移 0005](../../crates/brocade-store/migrations/0005_node_xray_enabled.sql)；实验 composition_reset / host_policy_upgrade 模块和开关已移除 |
| 前端 | [CVM 页面](../../frontend/src/panes/cvm/)、[CVM API](../../frontend/src/api/cvm.ts)、[网卡 API](../../frontend/src/api/cvm-nics.ts)、[入口 API](../../frontend/src/api/cvm-entries.ts)、[机器页](../../frontend/src/panes/nodes.tsx)、[导航](../../frontend/src/forge/shell.tsx)、[路由](../../frontend/src/forge/route.ts)、[样式](../../frontend/src/styles.css)、[Vite](../../frontend/vite.config.ts) |
| 构建与发行 | [统一入口](../../scripts/build-release.py)、[Console build](../../crates/brocade-console/build.rs)、[Agent 子构建](../../crates/brocade-console/build/agents.rs)、[源码版本](../../crates/brocade-console/build/source_revision.rs)、[发布账本](../../crates/brocade-store/src/binary_release.rs)、[CI](../../.github/workflows/ci.yml) |
| 安装与运行栈 | [控制面安装器](../../install.sh)、[Launcher](../../crates/brocade-launcher/src/)、[节点安装模板](../../crates/brocade-console/templates/install.sh)、[环境清单](../../console.env.example)、[Cube 构建](../../scripts/build-cube.py)、[运行包](../../scripts/cvm_package.py)、[栈安装器](../../scripts/cvm_stack.py)、[宿主模式](../../scripts/cvm_host.py)、[ELF 校验](../../scripts/cvm_elf.py) |
| 现有重点测试 | [Store CVM](../../crates/brocade-store/tests/cases/cvm.rs)、[网卡](../../crates/brocade-store/tests/cases/cvm_remote_nic.rs)、[入口](../../crates/brocade-store/tests/cases/cvm_entries.rs)、[宿主策略](../../crates/brocade-store/tests/cases/cvm_host_policy.rs)、[额度](../../crates/brocade-store/tests/cases/cvm_network_quota.rs)、[Console 集成](../../crates/brocade-console/tests/http_integration.rs)、[终端](../../crates/brocade-console/tests/cases/cvm_terminal.rs)、[Cube 执行](../../crates/brocade-cube/tests/)、[前端](../../frontend/tests/cvm/)、[视觉](../../frontend/tests/visual/cvm/)、[安装和宿主](../../scripts/tests/cvm/) |

## 实施与审查状态（2026-11-10）

已实现 leaf crate 的 `cvm` feature、base 默认构建、前端编译裁剪与清单绑定、Console/Agent 自描述身份、按变体分发和缓存、显式发布意图、基础版资源保护及安装器身份核对。两种 Console 保留相同迁移来源；没有新增迁移或重写线上迁移回执。

节点运行时上报缺少 `agent_flavor` 时仅记录为未知，不能作为基础版推断或跨变体授权；新产品 Agent 始终显式上报，发布创建要求有效身份。旧 CVM 安装、schema 和中间协议不接管。

`tat.ac` 已在限定维护事务内清空 33 张 CVM 表的 352 条实验记录，158 张非 CVM 表保留；35 张关键身份/配置表事务指纹一致。CVM 专项逻辑备份与执行回执保存在服务器 `/var/backups/brocade-cvm-cleanup/20261110T095007Z/`。随后按用户选择清理 Agent/Xray 发布历史及无引用配置/授权历史，保留全部关联和 461 条仍有引用的发布；不重置 ID 序列。

review 修复完成后，按用户最新授权于 2026-11-10 部署普通版 Console `0.3.6 / base`。独立工作树冻结全部本地源码，预算模块采用另一任务已验收的版本；共享预算文件保持只读。ARM64 Console SHA-256 为 `9666931a26e11839edbba4799f294ae57d6feb6341ecc82b1b9c8bd283b62dfe`，源码清单摘要为 `65f8ca88fac4a8cc55973f59577a0bc8cae558087281c5d0bdecd36d69c9172b`。

本次通过安装器首次建立 base 身份，完整备份、标准升级和实际 systemd 启动均成功；没有执行 schema 迁移或节点 Agent/Xray 发布。内外网接口、前端资源与分发摘要通过；CVM 接口 404，实验记录保持 0，38 台原在线节点已在升级后重新上报。本任务添加的 Nginx 临时 CVM 隔离已移除，禁用能力由普通版自身执行。详细回执在本地 `.agents/local/cvm-variants-20261110-1707/deployment/`。

本次验收范围是普通版 Console 部署。CVM 版完整双架构发行、受管/外置 PostgreSQL 两种模式的真实双向安装切换，以及 KVM/PVM/物理存储验收仍须分别有对应产物与结果；既有 CVM 视觉差异也须在其发行前完成核验，不能由本次普通版通过推导全部首发门槛已通过。
