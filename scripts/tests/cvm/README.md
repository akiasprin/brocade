# CVM 验收

`kvm_acceptance.py` 在明确选择的一次性 KVM 宿主上运行真实 Console／PostgreSQL、生产 Rust Cube 执行器和 Guest。测试记录真实 VM FD、设备、系统盘、SSH 身份和物理清理；未知结果保留本轮资源及私有证据。不得把生产机器用于故障、格式化或掉电测试。

网卡验收使用 `--remote-nics --network-probe-binary /absolute/probe --xray-binary /absolute/current-xray`，与当前 Console、Agent 测试程序、Cubelet／Shim 同源构建。参数见 `--help`。它从管理主网卡创建开始，通过网卡发布 API 挂载同一 P 的两个独立 N，验证实际三张 virtio 设备、地址与 MAC、双栈 TCP／UDP、指定网段、协议端口和默认路由、健康默认卡存在时选中卡故障仍不回落、源地址绑定不能绕开 R、两种承载切换及稳定 Guest 身份。独立入口另经发布与分配 API 验证共享 IPv4 的 TCP／分片 UDP、整 IP 的 TCP／UDP／ICMP 均投递主网卡，Guest 来源为宿主网关，且同机 NAT 出口使用整 IP 入口作源地址；不再调用已移除的网卡入站接口，也不声明 IPv6 入口或原始来源保留。最后等待入口撤销回执、修改路由依赖，核对 Guest、TAP、两端网络与分配清理，再删除发布项。

网络收敛运行在本轮独占的测试服务中，调用生产 Agent 的 `kvm_network_service`；计算收敛与其共享已校验的期望数据，分别保留执行边界。仅 Root 受限目录中的本轮私有标记可停止服务；日志不得公开令牌、密钥或完整节点期望。

`--smoke` 仍验证真实 Guest 开机、SSH、数据及 SSH 身份保留的关机／开机和物理删除，省略扩展计算矩阵。网络验收在 smoke 返回前完成。完整计算矩阵额外验证同系统镜像的 CPU、内存、磁盘改配、重装和独立监控；`--guest-terminal` 使用真实 Console／Agent／Guest PTY，控制端需安装 `websockets`。

`install_kvm.py` 创建自己独占的外层 KVM、存储与服务，再安装同源节点运行包并执行上述网卡及 smoke 验收。显式传入 OS 镜像、运行包与 SHA-256；`--storage-filesystem xfs|ext4` 分别验证 reflink／plain。`--installation-only` 仅验证发行版安装、同包重装、重启恢复和既有服务保留，不代表内层 Guest 或网卡验收。`--prepare-host-only` 保留本轮新宿主供独立验收，不能算作测试通过。

幂等与中断恢复的定向验证也在一次性 KVM 内运行：

- `python3 crates/brocade-agent/tests/entries_netns.py <Agent 测试程序>` 创建独立网络命名空间，覆盖真实 nftables、WireGuard、同机与跨机入口；保持 TCP／UDP 连接跨越重复收敛、撤销和分批清理恢复，核对未变更入口仍可用。
- `BROCADE_RUN_CVM_KVM=1 python3 crates/brocade-agent/tests/selfupdate_replay.py <旧 Agent> <新 Agent>` 使用私有安装目录与 loopback 发布夹具，执行真实二进制替换、回执确认前强杀、重启重报与同目标重放。两个程序须为不同摘要的 CVM 变体，候选大小须满足更新器限制。
- `unshare -n env BROCADE_RUN_CVM_KVM=1 <Agent 测试程序> --exact cvm::chain_egress::tests::interrupted_restore_releases_open_tuns_before_retry --ignored --nocapture` 验证多个持久 TUN 部分恢复失败时释放句柄，修复检查点后可重新附着。

这些定向测试不替代 Console 发布链路或其他架构、PVM 模式的验收。

PVM 使用 `--host-virtualization pvm`，在本轮外层 VM 中隐藏 VMX／SVM，验证配对内核安装、待重启状态、手动重启后的 PVM 模式与真实 Guest。普通 KVM 结果不代表 PVM 已通过。包内元数据、低 glibc 基线、存储边界与构建入口见 [Cube 运行栈](../../../docs/cube-runtime.md)。

资源默认值变更必须另外指定 `--host-memory-mib 1024 --host-cpus 1`，验证真实小宿主安装与 Guest/双承载网络。默认 6 GiB 宿主继续用于完整兼容矩阵，不能代替 1 GiB 验收。BPF 表容量升级用同源 `CubeNet/cubevs` 测试二进制在一次性宿主的私有 bpffs 中验证复制、重复执行和容量不足时保留原 pin。

旧本地 LAN、主网卡交付、旧交付 API 和旧容器接管验收已移除。本版只接受 `cubevs-attachments-v2`；网络模型与发布／挂载步骤见 [CVM](../../../docs/cvm.md)。

普通快速检查为本目录的 `test_*.py`、Rust Cube 客户端测试、Store PostgreSQL CVM 测试和 Cubelet `network/attachments` 测试。数据面隔离测试使用 Agent 中显式 ignored 的 `real_ip_backend`，仅在独立 user／mount／network namespace 中运行；需要 ip、nft、tc、conntrack、wg 和匹配 Xray，不能在宿主网络直接执行。

`entry_transports.py` 验证独立访问入口的跨机器承载。显式提供三个一次性 KVM 的 SSH 清单（`b` 入口、`h1` 裸机目标、`h2` 外部客户端）、测试私钥、同源 Agent 测试程序、Xray 与 Phantun 运行包。`prepare` 创建受限测试目录、校验上传摘要并启动持久测试执行器，导出两端公开证书；不修改物理宿主网络。

随后将两个公开证书路径分别传入 `BROCADE_ENTRY_CERTIFICATE_N1` / `_N2`，指定 `BROCADE_ENTRY_ASSIGNMENTS` 为权限 0700 的空目录，并按实验 LAN 设置 `BROCADE_ENTRY_ENDPOINT_V4` / `_V6`、`BROCADE_ENTRY_TARGET_V4` / `_V6`。执行 `BROCADE_RUN_PG_TESTS=1 cargo test -p brocade-store --features cvm --test pg_integration cvm_entry_xray_and_phantun_assignments_and_capability_gates -- --ignored`。它通过生产 Store 生成 WireGuard UDP、Xray、WireGuard Phantun 的双监听方向、双外层地址族共 12 组私有期望，验证端口占用、旧 Agent 拒绝、新旧能力与错误状态显示。私有期望包含通道密钥，不能放入日志或提交。

最后以相同参数执行 `run`：真实验证 L3／L4 TCP、SSH、分片 UDP、ICMP、宿主自身回包、进程故障恢复、与原组网 Phantun 并存及重复重载、撤销后恢复原服务和清理所有入口设备。结果写入受限输出目录；`stop` 等待测试执行器退出，调用方继续负责关闭自己创建的 KVM。上述 IP、角色与命令只用于隔离测试，不得把生产节点作为该故障注入入口。

入口通道验收的默认 `run` 包含真实 120 秒 Phantun 阻断、明确超时日志和解除阻断后的自动恢复验证。对只影响启动状态文案的后续改动，可用 `--skip-timeout-test` 重跑完整数据面矩阵；须保留此前的完整阻断验收证据，并在报告中记录本轮未执行该注入。两端进程日志、凭据隐藏与可用性仍应分别验证。

`crates/brocade-cube/tests/artifacts_kvm.rs` 验证实例产物与真实 Cube 执行器的边界。
在明确选择的一次性 KVM 内执行该测试二进制，设置 `BROCADE_CVM_ARTIFACT_KVM_CONFIG` 指向该测试宿主的已校验配置，
并显式选择 ignored 用例 `cube_artifact_real_guest_lifecycle`。它使用独立的私有执行记录和临时 SSH 身份，
验证四份产物的确认、真实 SSH、DNS 覆盖与恢复、保盘停机／改配／开机、数据及 SSH 主机身份保持、篡改拒绝和物理清理。
默认 Guest 内存为 512 MiB，可用 `BROCADE_CVM_ARTIFACT_KVM_MEMORY_MIB` 显式选择合法档位；
测试失败或清理不明时保留私有目录并打印位置，不自动重建或猜测磁盘归属。
该用例不替代 Console/Store 的 HTTP 与 PostgreSQL 验证，也不验证运行栈发行安装或入口／隧道承载。

## 发行路径与线路出口

`release_kvm_lab.py`、`release_kvm_services.py`、`release_kvm_deploy.py` 和
`release_kvm_acceptance.py` 共同使用本轮独占的三个外层 KVM（计算、网络提供者、独立客户端），
安装固定构建入口生成的真实 Console、Agent 和 Cube 运行包，通过正式 API 发布配置并核对实际回执。
OS 镜像和运行包须显式提供摘要。主流程还需要同源 `probe.go` 构建的静态 Linux amd64 探针，由发行编排器在创建 KVM 前构建并固定摘要。手动部署主流程时，向 `release_kvm_deploy.py` 传入 `--probe` 与 `--probe-sha256`。受限输出目录包含节点令牌、会话和测试密钥，不能提交或公开。
故障注入期间会断开本轮代理的既有 Agent 通道并拒绝重新升级，使用生产 HTTP 回退路径截获真实回执；
恢复后继续透传实际 WebSocket 通道。不能把只拦截 HTTP、却仍放行通道 RPC 的脚本当作故障验收通过。
实验网卡的整 IP 回程与探测网段使用持久静态路由；跨月改时钟后仍核对客户端回程，避免 DHCP 更新清除临时路由。
普通停机前创建测试文件，停机后对同一 ext4 原盘运行完整只读检查，并在再次启动后确认文件仍在。

### Cube Stack B 冷切换

仅在本轮独占的 x86_64 原生 KVM／ext4 (`plain`) 实验中执行。先冻结带 Stack B 的 Console、Agent 和同次构建的 `scripts/cvm_stack.py`。`BUILD` 的 `target/cube/artifacts/x86_64/bundle.json` 必须指向源包 `A` 并记录 `A_SHA`；目标 `B` 和 `B_SHA` 必须来自另一个真实构建，Cubelet、Shim 字节和 Shim 不可变版本均须不同。两包须兼容同一状态 ABI，不允许简单改包名或伪造二进制。用新的数据库和实验目录，不将既有产品库作为夹具。

```sh
# 在 KVM 控制主机的仓库根目录；变量均指向本轮已固定的绝对路径或摘要。
install -d -m 0700 "$RUN"
CVM=scripts/tests/cvm
python3 "$CVM/release_stack_acceptance.py" catalog --output "$RUN/catalog" \
  --source "$A" --source-sha256 "$A_SHA" --target "$B" --target-sha256 "$B_SHA"
python3 "$CVM/release_kvm_lab.py" prepare --output "$RUN/lab-3" \
  --image "$OS_IMAGE" --image-sha256 "$OS_SHA"
python3 "$CVM/release_kvm_services.py" --lab "$RUN/lab-3" --output "$RUN/prerequisites"
python3 "$CVM/release_kvm_deploy.py" "$RUN/lab-3" "$BUILD" "$RUN/services"
python3 "$CVM/release_kvm_acceptance.py" "$RUN" bootstrap
python3 "$CVM/release_stack_acceptance.py" prepare --lab-output "$RUN" \
  --catalog "$RUN/catalog" --active alpha --idle stack-idle --seed-missing
python3 "$CVM/release_stack_acceptance.py" run --lab-output "$RUN" --late-receipt
```

`prepare` 保留原 ext4 backing image 和 Agent 执行记录，仅在本轮计算 VM 内将旧实验 Agent 转换为可重启的 `brocade-agent.service`，启用持久存储挂载，配置 Console 与 Agent 的本轮签名信任。已有实验可选择同一个 compute 上已登记的运行 Guest 与停机 Guest，省略 `--seed-missing`；计算 VM 不得有其他实例。未准备现有停机 Guest 时，脚本只在已确认归属的实验中创建并停机。运行前不要让其他脚本并发修改该实验，且两 VM 时钟须与当前维护窗口一致。

完整顺序为 `bad-hash`、`bad-size`、`cancel`、`activation-failure`、`manager-death`、`success`、`rollback`。可用重复的 `--case` 显式选取子集。哈希和长度故障须先于任何合法目标暂存，回滚须先有真实目标成功。默认检查独立 provider/client 私有 LAN。若已部署现有发行网络探针和 TCP 业务网卡，可额外指定 `--provider-echo` 验证真实跨宿主业务 TCP。

夹具只提交宿主与清单 ID，核验服务器以 Unix 秒返回创建后五小时的开始截止时间。代理在认领前暂时拒绝来自实验计算节点的真实 Stack HTTP 认领请求，使用其根摘要核对 `ready` 预检证据和独立宿主观测。`cancel` 在认领送达 Console 前取消，并核验 Cubelet 进程和原盘。其他用例布置好对应故障后放行自动认领。`success` 在拦截真实终态回执时核验 `applying` 的取消请求返回冲突。加上 `--late-receipt` 会保持该拦截直到实际重启 Agent 并核验本地回执仍在，再放行重报。其他 HTTP 请求和控制面通道继续可用。

保留 `$RUN/stack-acceptance/state.json` 和隔离日志。出错不删除实例、盘、虚拟机和待恢复记录，也不补造回执。全部完成或人工收集失败现场后，可用 `python3 "$CVM/release_kvm_lab.py" stop --output "$RUN/lab-3"` 停止本轮拥有的外层 VM。此基线不代表 XFS、PVM 或其他架构已验收。

### Cube Stack B 空机首装

在另一套独占的 KVM 实验与空数据库中，只为计算 VM 接入 CVM Agent，保留准备好的 ext4 `plain` 存储。`catalog` 也会生成签向真实源包 `A` 的独立首装清单，供两套实验复用。清单沿用 v1 标签，载荷使用当前签名字段。`FRESH_RUN` 使用新的绝对路径。

```sh
install -d -m 0700 "$FRESH_RUN"
python3 "$CVM/release_kvm_lab.py" prepare --output "$FRESH_RUN/lab-3" \
  --image "$OS_IMAGE" --image-sha256 "$OS_SHA"
python3 "$CVM/release_kvm_services.py" --lab "$FRESH_RUN/lab-3" --output "$FRESH_RUN/prerequisites"
python3 "$CVM/release_kvm_deploy.py" --agent-only-compute \
  "$FRESH_RUN/lab-3" "$BUILD" "$FRESH_RUN/services"
python3 "$CVM/release_kvm_acceptance.py" "$FRESH_RUN" bootstrap
python3 "$CVM/release_stack_acceptance.py" first-install \
  --lab-output "$FRESH_RUN" --catalog "$RUN/catalog"
```

脚本核验 Agent 自行部署安装器并上报正向空机证据。篡改下载归档后，确认预检失败，存储身份与 Agent 进程保持原样。使用合法归档时，脚本先拦截真实认领请求，核验预检给出的空盘摘要和宿主状态，再放行自动安装。最后核验目标组件、持久安装回执和首个真实 KVM Guest。证据保存在 `$FRESH_RUN/stack-acceptance/first-install.json`，失败时保留本轮环境。使用 `release_kvm_lab.py stop --output "$FRESH_RUN/lab-3"` 停止本轮实验。

`release_chain_acceptance.py` 在发行路径实验状态上逐阶段执行：`prepare` 校验并上传 `chain_probe.go`
构建的独立端点；执行 `chain_icmp` 时，另将同源 `attachment_probe.go` 产物作为 `prepare` 的第二个文件参数，
供受限 Guest I/O 正向对照使用。`configure` 发布真实线路、授权与普通 WireGuard 下一跳，`primary` 为仅有主网卡的
实例选择线路。`traffic`、`mixed_routes`、`source_guard`、`faults`、`quotas`、`grant_revocation`
分别核对双栈 TCP／UDP、线路内部域名／IP／协议／端口分流、网卡和线路的共同优先级、绑定业务源地址的绕过尝试、
进程和上游故障、真实用量／限速／额度及撤销授权。阻断验收同时检查独立目的地的新 SYN／UDP 计数，不能仅以客户端超时当作不回落的证明。
`ingress_guards` 保留 DNS 正向对照，验证现有 SMTP、UDP 放大端口和私网防护；`chain_icmp` 验证线路不会伪造 ICMP 支持。
`unauthorized_selection` 在真实 Console 中核对未授权线路不可见，编译预览与直接提交均返回 403，且不改变实例、分配或修订。
`nic_ipv6_pmtu` 在独立客户端清空 IPv6 路径缓存，抓取普通 WG／1400 路径的真实 Packet Too Big，
验证随后显式发送的 4096 字节数据报完整返回。冷路径的首个大 UDP 数据报可能丢失，不能把应用重传当作隐式保证；
混合选路用小于该 MTU 的 IPv6 UDP 数据报单独验收，主网卡线路 TUN 的大数据报另由 `traffic` 验证。

`preview_lifecycle` 通过真实编译预览验收挂载、卸载、停机改配和无变化保存：预览事务回滚、摘要校验、
保存与发布的产物一致；卸载等待期间实际设备仍在，不能提前释放分配或误报发布失败。
`entry_existing` 验证原有跨宿主入口的双向大块 SSH、分片 UDP 和整 IP ICMP。
`period` **仅推进本轮三个一次性 KVM 的系统时钟**，验证真实跨月账期及线路恢复，不改宿主时钟或用 SQL 重写账本。
最后运行 `cleanup_owned`，确认 Guest、TAP、线路设备／防火墙和两端分配均清理，再关闭本轮外层 KVM。
Cube 的无主 `Ready` TAP 复用池会保留；验收要求没有活动所有者、端口映射、VMM 描述符或 BPF 实例身份，
而非强制销毁复用池。
这些阶段依赖输出目录中明确记录的实验资源，不得对生产服务或未经本轮创建的虚拟机执行。

`release_chain_ui.mjs` 在实际 KVM Console 上检查实例清单与详情跳转、网络草稿的真实编译预览和输入清单，覆盖桌面及窄屏，
不拦截 API 响应；`release_kvm_ui.mjs` 检查混合发布与原生产物。它们补充前端组件和截图比较，不能替代真实网络流量验收。

`release_baremetal_chain_acceptance.py <私有实验目录> <阶段>` 复用上述独占 KVM、已发布的线路与独立目的端，
将计算 VM 的现有操作系统作为真实裸机执行器接管。先完成并清理 Cube 实例，保留线路与网卡发布项，
安装支持 `cvm-check --baremetal-chain-contract` 的同源发行 Agent，再运行 `adopt`。
该阶段确认旧 Cube 实例与宿主网络状态完成清理，写入本轮服务专属的裸机配置，验证双栈 TCP／UDP、线路内部
域名／IP／端口／协议分流、管理进程与入站回包保护，以及保存、重放、撤回和恢复发布。
`verify` 重跑已接管实例的完整正向矩阵；`mixed` 挂载真实业务网卡，比较最长前缀、协议端口和同前缀优先级，
随后发布卸载并恢复原线路。`faults` 注入客户端退出、入口失联、策略表丢失、授权撤销和 Agent 重启，
同时核对独立目的端的新连接计数，验证不可用时不直连回退。`quotas` 使用真实用量核对已有用户限速和额度。

`release_baremetal_chain_ui.mjs <私有实验目录>` 在真实 Console 的 1440／390／320 px 页面验证裸机线路可选，
并实际执行手机端保存、撤回和恢复发布。上述阶段涉及写操作，须串行运行。
最后运行裸机 `cleanup`，经正式释放宿主流程确认原操作系统、启动身份和标记数据保留，
检查本轮线路设备、策略规则与防火墙清理，再移除本轮 backend drop-in，恢复原 Cube 执行器。
恢复 Cube 执行器后可继续其发行回归；所有阶段完成后关闭本轮外层 KVM。不得用此脚本切换生产 Agent 后端。

容器由管理员创建并授权，不再准备用户配额。真实 Guest 的 SSH 验收通过独立凭据接口取得自动密码，使用私有密码文件和 SSH askpass，不把密码写入命令参数、实例规格或验收日志；重装后重新取得生效版本。
