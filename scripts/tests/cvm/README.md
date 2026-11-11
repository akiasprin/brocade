# CVM 验收

`kvm_acceptance.py` 在明确选择的一次性 KVM 宿主上运行真实 Console／PostgreSQL、生产 Rust Cube 执行器和 Guest。测试记录真实 VM FD、设备、系统盘、SSH 身份和物理清理；未知结果保留本轮资源及私有证据。不得把生产机器用于故障、格式化或掉电测试。

网卡验收使用 `--remote-nics --network-probe-binary /absolute/probe --xray-binary /absolute/current-xray`，与当前 Console、Agent 测试程序、Cubelet／Shim 同源构建。参数见 `--help`。它从管理主网卡创建开始，通过网卡发布 API 挂载同一 P 的两个独立 N，验证实际三张 virtio 设备、地址与 MAC、双栈 TCP／UDP、指定网段、协议端口和默认路由、健康默认卡存在时选中卡故障仍不回落、源地址绑定不能绕开 R、两种承载切换及稳定 Guest 身份。独立入口另经发布与分配 API 验证共享 IPv4 的 TCP／分片 UDP、整 IP 的 TCP／UDP／ICMP 均投递主网卡，Guest 来源为宿主网关，且同机 NAT 出口使用整 IP 入口作源地址；不再调用已移除的网卡入站接口，也不声明 IPv6 入口或原始来源保留。最后等待入口撤销回执、修改路由依赖，核对 Guest、TAP、两端网络与分配清理，再删除发布项。

网络收敛运行在本轮独占的测试服务中，调用生产 Agent 的 `kvm_network_service`；计算收敛与其共享已校验的期望数据，分别保留执行边界。仅 Root 受限目录中的本轮私有标记可停止服务；日志不得公开令牌、密钥或完整节点期望。

`--smoke` 仍验证真实 Guest 开机、SSH、数据及 SSH 身份保留的关机／开机和物理删除，省略扩展计算矩阵。网络验收在 smoke 返回前完成。完整计算矩阵额外验证同系统镜像的 CPU、内存、磁盘改配、重装和独立监控；`--guest-terminal` 使用真实 Console／Agent／Guest PTY，控制端需安装 `websockets`。

`install_kvm.py` 创建自己独占的外层 KVM、存储与服务，再安装同源节点运行包并执行上述网卡及 smoke 验收。显式传入 OS 镜像、运行包与 SHA-256；`--storage-filesystem xfs|ext4` 分别验证 reflink／plain。`--installation-only` 仅验证发行版安装、同包重装、重启恢复和既有服务保留，不代表内层 Guest 或网卡验收。`--prepare-host-only` 保留本轮新宿主供独立验收，不能算作测试通过。

PVM 使用 `--host-virtualization pvm`，在本轮外层 VM 中隐藏 VMX／SVM，验证配对内核安装、待重启状态、手动重启后的 PVM 模式与真实 Guest。普通 KVM 结果不代表 PVM 已通过。包内元数据、低 glibc 基线、存储边界与构建入口见 [Cube 运行栈](../../../docs/cube-runtime.md)。

资源默认值变更必须另外指定 `--host-memory-mib 1024 --host-cpus 1`，验证真实小宿主安装与 Guest/双承载网络。默认 6 GiB 宿主继续用于完整兼容矩阵，不能代替 1 GiB 验收。BPF 表容量升级用同源 `CubeNet/cubevs` 测试二进制在一次性宿主的私有 bpffs 中验证复制、重复执行和容量不足时保留原 pin。

旧本地 LAN、主网卡交付、旧交付 API 和旧容器接管验收已移除。本版只接受 `cubevs-attachments-v2`；网络模型与发布／挂载步骤见 [CVM](../../../docs/cvm.md)。

普通快速检查为本目录的 `test_*.py`、Rust Cube 客户端测试、Store PostgreSQL CVM 测试和 Cubelet `network/attachments` 测试。数据面隔离测试使用 Agent 中显式 ignored 的 `real_ip_backend`，仅在独立 user／mount／network namespace 中运行；需要 ip、nft、tc、conntrack、wg 和匹配 Xray，不能在宿主网络直接执行。
