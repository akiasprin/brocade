# 从源码构建

构建 Console 与 launcher。Console 二进制内嵌前端、双架构 Agent 与 Brocade Xray。

## 前置条件

仓库通过 [`rust-toolchain.toml`](../rust-toolchain.toml) 固定 Rust 工具链和 agent 所需的 musl targets。完整构建还需要：

- Node.js 22 与 npm，用于构建前端；
- [`components/xray-core/.go-version`](../components/xray-core/.go-version) 指定的精确 Go 版本，用于构建或校验 Brocade Xray；
- Zig 0.16，用于交叉编译静态 agent；
- Docker，用于执行 PostgreSQL 集成测试；
- 目标平台的交叉链接器，仅在控制面本身需要交叉编译时使用。

## 发行构建

CI 固定在 Ubuntu 22.04 构建发行包，并拒绝依赖高于 glibc 2.35 符号的 GNU 产物。本地 GNU 构建继承本机链接环境；在较新发行版编译后，不能仅凭相同的 Rust target 宣称兼容旧版 glibc。交付前应检查实际 ELF 的版本符号，并在目标基线运行验证。musl 发行包为静态链接，可在同架构 GNU 与 musl 系统运行；受管 PostgreSQL 按实际宿主机的 libc 选择。

构建脚本会在开始阶段检查必要工具，并在缺失时报告对应的安装方式。标准发行构建为：

```sh
cargo build --release --locked -p brocade-console -p brocade-launcher
```

输出位于 `target/release/brocade-console` 与 `target/release/brocade-launcher`，部署 launcher 时应保持二者同目录。构建脚本会执行 `npm ci` 与前端生产构建，并分别生成两种架构的 Agent 和 Brocade Xray。若前端已由其他流水线构建，可通过绝对路径指定待嵌入目录，从而跳过 npm：

```sh
BROCADE_CONSOLE_ASSETS_DIR=/absolute/path/to/frontend/dist \
  cargo build --release --locked -p brocade-console
```

## 本地双架构增量构建

在长期保留、构建期间无人修改的独立工作树运行：

```sh
python3 scripts/build-release.py
```

该入口面向 x86_64 Linux 构建机，检查精确 Go 版本、Zig、Rust targets 和 ARM GNU 链接器，以一次 Cargo 调用调度 x86_64 / aarch64 Console 与 launcher；嵌入 Agent 也以双目标 Cargo 图调度。默认 jobs=4，可用 `--jobs 2` 至 `--jobs 8` 调整。

编译输入位置和缓存保持稳定，归档目录只保存完成后的交付物。前端按源码、工具版本和产物摘要校验缓存，仅在需要时构建一次，避免双目标重复安装依赖。构建没有总时限或阶段超时；构建和各阶段在开始、结束时输出带时区的 UTC 时间、结果与耗时，每 30 秒报告进度。`target/brocade-release/preparation.json` 同步记录总体与各阶段的 `started_at`、`ended_at`、结果、耗时以及来源和产物摘要，运行中的结束时间为 null。耗时使用单调时钟，不受系统时间调整影响。构建日志在同目录的 `cargo.log`。输入变化或实际错误仍使构建失败，人工取消会清理本次进程组并记录 `cancelled`；发布须完成与本次产物对应的验收。

缓存布局、独占、失败处理和耗时记录见[部署规范](../.agents/runbooks/console-deployment.md#固定位置双架构与耗时记录)。比较构建耗时时注明缓存状态和验证范围；本次失败不能用上一轮产物替代。

## 内嵌产物与 Xray 构建身份

当前内置 Xray 基线为 `v26.10.21`（`1b1647cd9ddf0abc34b981d4611fc03bcac1ca74`），Go 最低补丁版本为 `1.27.3`，具体要求见源码中的 `.go-version`。REALITY 依赖通过仓库内相对路径 `third_party/reality` 引入，来源与兼容补丁记录在两级 `BROCADE_UPSTREAM.toml` 中；该依赖的实际编译文件也纳入构建输入指纹。替换成官方 Xray 会失去 Brocade 的协议、计费、交接和指纹兼容能力。

生产构建会把前端资源以及 `x86_64`、`aarch64` 两种架构的静态 Agent 和 Brocade Xray 一并嵌入 `brocade-console`。因此，控制面部署只需分发一个二进制文件，前端、API 与节点发行物也不会因独立部署而发生版本漂移。Xray 的发布身份取自实际内嵌字节；构建时关闭 Go VCS 元数据并使用钉住的上游基线作为 banner build id，避免 README 或前端提交制造一次没有数据面变化的 Xray 发布。

Xray 构建会固定 Go 参数与环境，并为双架构产物自动生成 `xray-<arch>.build.json`（位置打印在构建日志中）。记录包含实际编译输入指纹、编译配方和二进制摘要；相同输入出现不同摘要会中止构建。首次遇到一组输入时，会使用独立 Go 缓存再次编译核对。`BROCADE_XRAY_BIN_X86_64` / `BROCADE_XRAY_BIN_AARCH64` 只接受与当前源码、配方及摘要匹配的预编译文件，必须同时提供 `<二进制路径>.build.json`，仍需精确版本的 Go 来验证输入；无记录的旧文件不能继续混入构建。这些构建检查不会触发节点 Xray 升级。

## 本地前端开发

前端开发服务器与生产构建分别使用：

```sh
cd frontend
npm ci
npm run dev
```

```sh
cd frontend
npm run build
```

生产控制面默认使用编译时内嵌的前端。仅在本地迭代或紧急替换静态资源时，才应通过 `BROCADE_CONSOLE_DIST=/absolute/path/to/frontend/dist` 改为运行时读取目录；这种模式无法保证页面与 API 来自同一次构建。

打开的控制台遇到页面模块加载失败时，会保留可读的错误提示并尝试重新加载。仍有未保存且不能自动进入草稿的表单，或正在执行写入时，会暂缓重载；先完成当前操作后再重试。持续失败时，一分钟内不会反复自动刷新，可在网络恢复后点击“重新加载界面”。

## 验证

格式、lint、单元测试与 PostgreSQL 集成测试的命令见[验证标准](../.agents/standards/testing.md)。
