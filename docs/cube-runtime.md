# Cube 节点运行栈

CVM 的 Cube 源码位于 `components/cube/`，由 Brocade Git 直接管理。上游来源和基线固定在 `BROCADE_UPSTREAM.toml`；保留原模块路径与许可证。原盘恢复扩展直接维护在 Cubelet／CoW 源码中，构建前无需额外应用补丁。Agent 内的 `brocade-cube` Rust 库从同一份 `pkgs/proto` 生成客户端，Guest I/O 通过 Cubelet Tasks 接口和有界私有 FIFO／PTY 传输。

## 构建与产物

从固定、独占的持久工作树调用统一发行准备入口：

```sh
python3 scripts/build-release.py --cube-runtime-only \
  --cube-arch x86_64 \
  --cube-sysroot-libs /absolute/target-sysroot/lib \
  --cube-bpf-include /absolute/bpf-development/include \
  --cube-glibc 2.35
```

运行栈构建使用 Go 1.27.3、Rust 1.96、Clang 21 和仓库 Zig 包装器，默认 GNU libc 基线为 2.35。sysroot 中必须包含对应目标架构的 `libcap-ng.so` 和 `libseccomp.so`，与选定基线匹配；BPF include 目录需包含 libbpf 开发头文件。它们属于构建输入，摘要随产物记录。可通过 `BROCADE_GO` 指定固定 Go 编译器。`--cube-arch aarch64` 需要同架构 sysroot 与 Rust 目标，并另做该架构 KVM 验收。

构建前检查外部库的架构及实际导入的 `GLIBC_*` 版本，构建后检查所有宿主 ELF，打包前再次核验；超过声明基线时拒绝产物。仅降低 `--cube-glibc` 或包内 `min_glibc` 不能修复高版本依赖。Debian 12 的官方 `libcap-ng0=0.8.3-1+b3` 和 `libseccomp2=2.5.4-1+deb12u1` 可作为 x86_64 sysroot 输入：从经过 APT 签名和包摘要校验的仓库下载 `.deb`，使用 `dpkg-deb -x` 提取，将实际共享库复制为上述两个链接输入名；保留原包、版本、来源和 SHA-256，不从开发宿主直接复制库。

固定缓存为 `target/cube/`。产物位于 `target/cube/artifacts/<arch>/`：

| 文件 | 用途 |
| --- | --- |
| `cubelet` | 含原盘扩展、网卡持久管理和嵌入式 CubeVS 的静态节点服务 |
| `containerd-shim-cube-rs` | 含 Hypervisor 的 GNU Shim，执行真实设备挂卸与 Guest PTY 窗口调整 |
| `cube-runtime` | 与 Shim 同源码构建的运行工具 |
| `lan-inspect` | 只读查询实际 CubeVS LAN 端点与丢包计数 |
| `manifest.json` | 源码、工具、外部库、协议、阶段时间及产物 SHA-256 |

`build_complete` 表示构建完成；`deploymentReady` 保持 false，需另做对应架构、升级和恢复验收。系统镜像、Guest 内核和 Guest Agent 按独立制品准备，不绑定 CPU、内存或硬盘大小。Agent 安装器从固定运行包安装 Cube 栈；节点管理员仍需准备持久 XFS/reflink 或 ext4 和上游网络。原生 KVM/PVM 的选择、换核和切换流程见 [CVM 安装与运行包](cvm.md#原生-kvm-与-pvm)。

### 打包 PVM 内核配对

统一构建入口的 `--cvm-package-assets` 继续指定固定系统制品目录。基础清单包含 `system.json`、`root.ext4`、`vmlinux`、`guest.img`、`agent.ext4`。新构建的 x86_64 包必须另含 `vmlinux-pvm` 和 `pvm-host.deb`，两者同样列入 `manifest.json` 的 `files` 摘要映射；缺少配对制品时拒绝构建，避免默认 auto 的包在线上仍无法回退。在该清单增加：

```json
{
  "pvm": {
    "host_kernel_release": "REPLACE_WITH_EXACT_RELEASE_CONTAINING_pvm.host",
    "boot_args": ["nopti", "pti=off"]
  }
}
```

`host_kernel_release` 必须等于 `.deb` 内核主包名称中 `linux-image-` 后的完整版本和实际 `uname -r`。这些字段合入原有制品清单，不能用此片段替代架构、系统名称、版本与全部文件摘要。发布者从经过来源和摘要核验的上游固定版本或自建内核准备制品，不在节点安装时拉取 latest。PVM Guest 目录具有独立的内容版本与系统镜像 ID，原生 Guest 内核仍保留；不能把普通内核改名冒充 PVM。

打包器把恢复程序与内核配对一并纳入不可变清单。自动回退和模式切换须另在缺少嵌套虚拟化的一次性宿主中验证换核、手动重启、自动继续、真实 Guest 创建与清理、切回原生 KVM 和已有宿主服务保留；常规 KVM 验收不代表 PVM 验收通过。

## 节点配置与启动顺序

当前安装器依赖 Python 3.11 或更新版本（标准库 `tomllib`），要求 Linux 5.15 及以上；GNU Shim 的最低 glibc 版本由节点包 manifest 的 `min_glibc` 指定，不能仅按发行版名称判断。

默认资源预算按 1 GiB 宿主设计。下列安装器计算是未分配网络额度时的计算能力上界；支持用户网络额度的 Console 会再按共享物理预算收紧实际可分配内存。安装时为内核、BPF、Agent、Cubelet 与虚拟机额外开销预留 `max(384 MiB, 物理内存 / 8)`，剩余内存按 64 MiB 向下对齐后提供给容器；1 GiB 可见内存对应最多 640 MiB，958 MiB 对应最多 512 MiB。升级会收紧旧的过大容量；已有实例与磁盘保留，超出新容量时应先减小已有资源占用再创建或改配。

| 默认项 | 预算与行为 |
| --- | --- |
| cgroup 池 | 每套只预创建 1 个，恢复在用 ID，其余按需创建；回收旧空闲组不发送杀进程信号 |
| 主网卡 TAP 池 | 预热 1 个，最多保留 1 个空闲 TAP；按批回收旧空闲池，保留 Active、Cleaning 和已保留的设备 |
| NAT 连接表 | 入站、出站使用按需分配的普通 Hash，硬容量随内存模型选择（8192–1048576）；端口映射各 4096 条 |
| 临时网络缓存 | DNS 请求 2048 条，主网卡回包许可 4096 条，邻居缓存 1024 条 |
| 双栈 LAN | 512 个端点、8192 条回包状态；IP 路由和访问控制语义保持一致 |
| Cubelet Go 内存 | `GOMEMLIMIT=128MiB` 软上限；不包含 BPF、内核或 Guest 内存，也不是 OOM 硬上限 |
| Go CPU 与 GC | CPU 并行度采用 Go 自动检测，不再固定 32；GC 目标恢复为 100% |
| 并发 | 创建、销毁、镜像下载各 1；资源采样请求最多 2 个 |
| 日志与后台任务 | Cubelet 日志最多 4 × 16 MiB；节点状态 10 秒，清理 30 秒；每轮镜像回收最多 1 个 |

受管资源预算在升级时从新运行包前向更新，网络设备、DNS、系统镜像与存储身份保留。缩小已有 BPF 表前校验布局并有界复制；已有条目超过新容量时明确失败并保留原表。升级过程中已有连接可能需要重连，不删除路由策略或系统盘。详细资源验收使用一次性的 1 GiB KVM/PVM 宿主，不能仅以构建成功证明内存预算成立。

2026-11-05 的 glibc >= 2.35 x86_64 GNU 候选包，已使用同一静态 Rust Agent 在下列隔离 KVM 环境通过安装、GNU Shim/runtime 实际执行、同包重装、重启恢复及已有 HTTP 服务/网卡/nftables 规则保留检查：

| 外层宿主系统 | 存储 | 结果 |
| --- | --- | --- |
| Debian 12 | ext4 | 通过 |
| Debian 13 | XFS/reflink | 通过 |
| Ubuntu 24.04 | ext4 | 通过 |
| Ubuntu 26.04 | XFS/reflink | 通过 |

早期 glibc >= 2.38 候选包不能用于 Debian 12；需更换低基线包，不能只修改包声明。Debian 11 的默认 Python 3.9 缺少 `tomllib`，且内核 5.10、glibc 2.31 低于当前要求，未纳入此次兼容性修复。上述结果针对外层宿主安装，不等同于完整 Guest 生命周期或其他构建基线的兼容性承诺。

安装兼容性夹具使用 `scripts/tests/cvm/install_kvm.py --installation-only`，保留镜像摘要、实际系统版本、安装输出和重启后证据；不兼容的拒绝不计为安装通过。

1. 准备持久 XFS/reflink 或 ext4、KVM、TAP、bpffs、iproute2、nftables 和 conntrack，使持久状态能够在开机时恢复。
2. 将匹配的 Shim 与 `cube-runtime` 安装到 `/data/cubelet/root/component_versions/cube-shim/<shimVersion>/bin/`。版本与摘要取自同次构建的 manifest，保留旧版本目录。
3. 按 [Cubelet 网络配置示例](../crates/brocade-cube/examples/cube-networks.example.json)准备私有 JSON，文件由 Cubelet 有效 UID 持有，权限 `0600`。在 Cubelet 服务设置 `CUBE_NETWORK_ATTACHMENTS_CONFIG=/absolute/cube-networks.json`。
4. 启动匹配的 Cubelet，为 Agent 设置 [私有 Cube 配置](../crates/brocade-cube/examples/cvm-cube.example.json)与原执行记录目录，再启动 Agent。托管安装器写入 `brocade-agent.service.d/brocade-cvm.conf`，通过 `brocade-agent cvm-check` 核对实际能力。

业务地址由 Store 分配，Cubelet 不读取本地业务网桥或地址池目录。Agent 的 Cube 配置保留镜像、容量和管理主网卡来源；业务桥由 Agent 按 N 身份创建，实际设备槽位和受管网卡契约由 Cubelet 上报。Cubelet 网络配置的 `state_dir` 和 Agent 的 `BROCADE_CVM_STATE_DIR` 必须位于持久存储，不能使用 tmpfs。

主机 Shim 是执行栈组件。创建实例时固定使用 Cubelet 公开且已核对摘要的 Shim 版本；Guest 镜像内容和实例容量不因此改变。只有实际完成设备挂卸、Guest 观测和宿主状态核对后才确认操作。

## 网卡执行契约

`cubevs-attachments-v2` 只接收 Store 已分配的业务 NIC：稳定 N ID、MAC、IP、资源域、MTU 和独立路由版本。Cubelet 持久保存分配结果和实际设备，校验所属桥／TAP、Guest 与 Hypervisor 状态，执行真实热挂卸。准备操作名为 `prepare`，不在执行器中选择业务地址。

管理主网卡保留 CubeVS 原生路径，默认通过宿主机 NAT 出网，优先级最低；交给业务网卡的显式路由前缀在主网卡阻断，合法管理回复独立保留。每个业务 N 使用受归属约束的独立 bridge 和 nftables 身份规则，检查 MAC、IP、ARP 和邻居通告；跨机承载为 IP 层。当前真实槽位上限为三张（含主网卡），同一 P 可以反复挂载，模型不按来源数限制槽位。

R 在 Guest 中配置独立业务表、终止型不可达规则和逐 N 回程表；Agent 在宿主执行对应的路由授权、管理网隔离和承载选择，禁止 Guest 自行绕开。Xray 和 WireGuard 实现、地址模式及生命周期见 [CVM 网卡](cvm.md#发布挂载与路由)。

Brocade 不创建 Cube 原生宿主端口映射（包括 Guest 22），也不从镜像继承 `ExposedPorts`。外部访问通过入站 N；WebShell 使用独立管理通道。

本版移除本地业务网桥和旧交付契约，默认执行目录为 `/data/cubelet/network-attachments-v2`。不导入旧网卡记录，不自动接管旧 bridge、TAP 或数据库资源。运行栈升级前确认旧容器与网络资源已清理；这与保留已验证系统制品和其他宿主服务的边界分别处理。

## 执行状态与恢复

Rust 执行库与 Cubelet 各自持久化所负责的执行状态。重启后核对原物理盘、设备归属和地址预约，再继续同一操作；回执缓存不能代替真实运行证据。运行栈更新后，实例通过保盘关机／开机绑定新的 Shim。验收入口见 [Docker／KVM 流程](../scripts/tests/cvm/README.md)。

同一 Agent 的网卡准备、网卡收敛与网络额度变更共用有界串行队列，排队时间计入 RPC 期限；能力查询独立执行。Cubelet 拒绝请求时不要求附带成功证据，Agent 按操作与返回码报告失败；只有成功响应缺失或损坏证据时才报告 `invalid Cube network evidence`。失败不会释放已分配资源，也不会自动重启实例。

Guest PTY 的使用与策略见 [CVM 实例终端](cvm.md#实例终端webshell)。Rust 客户端负责交互终端，打开时验证真实窗口尺寸；Cubelet 的 `pkgs/nodeexec` 仅提供有界命令观测。

本地卷可选 `local_backend = "plain"`（ext4）或默认 `xfs`（reflink）。对应对象分别位于 `<storage-work>/plain/objects` 与 `<storage-work>/xfs/objects`，不能对已有池切换后端。普通文件卷只实现当前 CVM 所需的持久卷和核查；具体能力、容量与升级边界见 [ext4 兼容边界](cvm.md#ext4-兼容边界)。

## Rust 客户端与升级边界

`crates/brocade-cube/` 负责 Cube 对接，`crates/brocade-agent/src/cvm/` 负责与 Console 的收敛、观测和终端通道。Cubelet RPC 源码来自本树的 `pkgs/proto`；containerd Tasks 协议以固定版本保存在 Rust crate 的 `proto/` 中。构建使用纯 Rust 协议编译器，不依赖宿主安装 `protoc` 或调用 Go SDK。

安装器在变更服务之前检查 `brocade-agent cvm-check --version` 和维护契约。运行包统一使用 `brocade-cvm-stack-v2`，每个包都包含模式管理与恢复程序。Agent 更新与 Cube 运行栈更新分别执行；回退须满足持久状态 ABI，并保留仍被实例引用的系统制品。CVM 开发阶段的执行器、状态和运行包不属于公开升级支持范围。

### 网络额度内存模型

`network-state-budget-v1` 先取物理内存与可见 cgroup v2 各层 `memory.max` 的最小值 M，宿主服务保留 `R=max(448 MiB,M/16)`，共享预算 `B=M−R`。硬容量 C 为满足 `16 MiB+C×(48+1536) ≤ B` 的最大二次幂，范围 8192–1048576。固定预算 `F=16 MiB+48×C`；每状态预算为 1536 字节，系统保留 4096 条。

Store 使用已分配 Guest 内存 G（含保留的关机实例），计算有效总状态数 `T=min(C,floor(max(B−G−F,0)/1536))`，再扣除系统保留和其他用户仍持有的容量。新增 Guest 的总内存反过来不能超过 `B−F−1536×(系统保留+全部已分配网络额度)`。用户的 Guest 授权上限不会被当成已经消耗的物理内存。

1536 字节是容量预算，不是每条连接的实时内存读数。1 GiB KVM/PVM 隔离内核在 16384、65536、131072 条占用下，三张真实布局表（两个 NAT 索引及额度流状态）的 Slab 增量约 514–518 字节/条；生产两种内核 conntrack 对象分别为 256/320 字节，模型按两个 320 字节对象再加 50% 余量计 960 字节。最小测试内核没有 netfilter，后者来自生产只读 Slab 观测，不能解释为测试机已经实测了两份 conntrack。宿主服务峰值保留独立于此每状态预算。

额度在现有硬容量内动态分配，不在正常收敛中整体扩换 BPF 表。运行栈升级改变硬表容量或前向启用 `NO_PREALLOC` 时仍须受控迁移、校验条目及验收；不承诺繁忙 NAT 表切换期间所有连接连续。额度 Guard 在 TAP 上先计量，允许后继续原有 LAN/主网卡安全策略，不能绕过原路由、源地址或入站规则。
