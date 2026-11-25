# 物理宿主实例

Brocade 可以把 Cube 节点当前运行的物理操作系统纳管为一个宿主实例。宿主实例与本机系统共享进程、文件系统、内核和网络空间，没有容器或虚拟机隔离；Cube 容器仍由同一个 Agent 的 Cube 执行器独立管理。两类工作负载可以同时存在，不需要重装 Agent 或切换执行模式。

一台机器同一时间只能有一个受管物理宿主实例。纳管不创建系统盘、不安装系统、不修改 SSH 密钥，也不会停止已有 Cube 容器。

## 安装与动态纳管

节点使用匹配 Console 的 `cvm` Agent。以系统权限执行安装命令并带上 `--enable-cvm`：

```sh
sh brocade-install.sh --server https://console.example/agent --enable-cvm
```

安装器一次准备 Cube、物理宿主管理和宿主终端服务。它为两套执行状态使用独立目录：

- Cube 配置与状态：`/etc/brocade/cvm-cube.json`、`/var/lib/brocade/cvm-cube`
- 物理宿主配置与状态：`/etc/brocade/cvm-host.json`、`/var/lib/brocade/cvm-host`

运行时没有“裸机模式”开关。Console 的期望状态决定物理宿主是否被纳管；解除纳管后 Agent 继续承载 Cube，不需要重新安装。旧版 `BROCADE_CVM_BACKEND=baremetal` 独占配置不再受支持；重新执行带 `--enable-cvm` 的安装命令会保留宿主私有配置和恢复记录，并把 systemd drop-in 转换为组合执行栈。旧独占 Agent 不通过自更新静默跨越这项转换。

安装器会根据唯一默认路由网卡探测管理网络与 DNS。无法唯一确定管理网卡时，先创建 root 所有、权限 `0600` 的 `/etc/brocade/cvm-host.json`：

```json
{
  "terminal_user": "operator",
  "primary_device": "enp1s0",
  "protected_destinations": ["192.0.2.0/24"]
}
```

`terminal_user` 必须是本机已有账号，WebShell 使用它的 UID、组、home 和 shell。`primary_device` 是保留管理联络的原有网卡。保护目标应包含远程运维网络和必须走原路径的服务；Agent 还会保护实际 Console 地址及受管传输端点。原默认路由和 DNS 继续由操作系统维护。

可分别检查 Cube 与宿主配置：

```sh
brocade-agent cvm-check --config /etc/brocade/cvm-cube.json
brocade-agent cvm-check --host-config /etc/brocade/cvm-host.json
```

Agent 上报组合能力后，系统管理员在“容器 → 纳管裸机”选择机器，核对系统身份、整机资源和使用用户。机器身份由 machine ID 与根文件系统 UUID 的摘要共同绑定；重装系统或更换根盘后必须重新确认，旧绑定不会自动移交。

## 能力与隔离边界

物理宿主实例支持监控、WebShell、业务网卡挂卸、独立路由、访问入口、流量计量，以及将使用者已授权的线路作为本机应用的 TCP／UDP 出口。它不提供虚拟开关机、重启、重装系统、调整 CPU／内存／系统盘或删除物理盘。整机容量用于归属和用量展示，不是 cgroup 隔离。

Cube 与宿主实例共用控制面发布，但 Agent 按每个实例的 `execution.kind` 分派到对应执行器：Cube 期望只进入 Cube，物理宿主期望只进入宿主执行器。观察、终端和线路出口同样按实例归属分派，不使用全局后端选择。

业务网卡接管前，承载必须进入 Active，且当前 Agent 已观察到配置可用。管理保护目标、Console 联络、已有入站连接回复和 Agent 受管传输优先保留原路径。已生效的业务出口不可达时，匹配流量保持阻断，不回落到原默认路由；未匹配业务路由的流量继续使用宿主网络。

线路客户端上游 socket 使用独立绕过标记，防止递归进入自身。线路不可达、进程退出、授权撤销或额度耗尽时，对应流量保持阻断。IPv6 仍要求线路具有已确认的 IPv6 落地，详见[线路作为出口](cvm.md#线路作为出口)。

## 解除纳管和清理

使用实例的“解除纳管”。Agent 撤销物理宿主终端、业务网卡、路由和入口，确认后释放绑定；不会删除用户文件、系统盘、账号或 SSH 配置。失败时保留资源归属并允许重试。

停止 Agent 后可独立验证两套执行状态已经清空：

```sh
brocade-agent cvm-check --empty --config /etc/brocade/cvm-cube.json
brocade-agent cvm-check --empty --host-config /etc/brocade/cvm-host.json
```

不要删除持久状态来绕过清理检查；这些记录用于拒绝过期意图和撤销跨进程终端。解除物理宿主纳管不要求 Cube 为空，也不影响仍在运行的 Cube 容器。

开发验收应使用独立 KVM 虚拟机中的真实 Linux，并分别验证 Cube 容器和物理宿主实例可同时收敛。未执行的整机网络测试不能由单元测试代替。
