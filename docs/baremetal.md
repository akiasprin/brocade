# 裸机实例

裸机后端把当前操作系统纳管为一个 CVM 实例。它与宿主共享进程、文件系统、内核和网络空间，没有容器或虚拟机隔离。一台机器只能绑定一个实例，整机容量计入所属用户额度；纳管不创建系统盘，不安装系统，也不改写 SSH 密钥。

## 前提和启用

Console 使用 `cvm` 变体，节点使用匹配的 `cvm` Agent（协议 71 或以上）。节点不需要 Cube、containerd 或 KVM；KVM 只用于开发验收。先按[安装文档](install.md)安装 `--agent-flavor cvm`，不要为裸机安装 Cube 运行栈。

节点须具备 Linux、systemd、iproute2、nftables、conntrack、WireGuard 工具，以及可读取的 `/etc/machine-id`、根文件系统 UUID 和 boot ID。根 UUID 和 machine ID 的摘要共同绑定机器身份；重装系统或更换根盘后必须重新确认身份，不能把旧绑定自动移交给另一台机器。

创建 root 所有、权限 `0600` 的 `/etc/brocade/cvm-host.json`，示例：

```json
{
  "terminal_user": "operator",
  "primary_device": "enp1s0",
  "protected_destinations": ["192.0.2.0/24"]
}
```

`operator` 必须是本机已有的登录账号，WebShell 使用它的 UID、组、home 和 shell。权限等同于该账号的实际权限。`primary_device` 是保留管理联络的原有网卡。保护目标应包含管理网络和必须经原有路径访问的服务；Agent 另外保护实际 Console 地址及传输端点。原有默认路由和 DNS 由操作系统维护。控制面使用域名时，保护目标也应包含实际的上游 DNS。

在 Agent 的 systemd drop-in 中设置：

```ini
[Service]
Environment=BROCADE_CVM_BACKEND=baremetal
Environment=BROCADE_CVM_CONFIG=/etc/brocade/cvm-host.json
Environment=BROCADE_CVM_STATE_DIR=/var/lib/brocade/cvm-host
```

为独立终端服务创建 `brocade-host-terminal.service`（二进制路径须与实际安装一致）：

```ini
[Unit]
Description=Brocade native host terminal
After=local-fs.target

[Service]
Type=simple
Environment=BROCADE_CVM_BACKEND=baremetal
Environment=BROCADE_CVM_CONFIG=/etc/brocade/cvm-host.json
Environment=BROCADE_CVM_STATE_DIR=/var/lib/brocade/cvm-host
ExecStart=/opt/brocade/brocade-agent cvm-host-terminal
Restart=on-failure
RestartSec=3
UMask=0077

[Install]
WantedBy=multi-user.target
```

执行 `systemctl daemon-reload`，启用并启动终端服务，再重启实际 Agent 服务。修改私有配置后两个服务都要重启。终端服务未就绪时不宣告 WebShell 能力；它独立于 Agent 保存 PTY，Agent 重启后可重连。整机重启或绑定变更会撤销旧终端身份。

在同样环境变量下执行 `brocade-agent cvm-check` 检查能力。系统管理员在“容器”页或“宿主”页的标题栏选择“纳管裸机”，选择机器后核对 OS、整机资源和所属用户；也可直接从裸机宿主卡片进入。入口在尚无裸机能力上报时仍可见，打开后说明准备条件；Cube/PVM 宿主不会作为裸机候选。已纳管、能力过期或执行异常的机器不可再次纳管。纳管接口为 `POST /cvm/adopt`，要求新鲜机器身份和明确的无隔离确认。

## 能力边界

支持整机纳管、额度准入、监控、WebShell、业务网卡挂卸、独立路由、访问入口和已有网络控制器的网卡流量计量。网卡新规格需要改变地址时，管理员须明确允许重新分配；裸机在运行中替换业务地址，现有业务连接会中断，原有管理联络保留。业务设备在宿主网络空间中，应用不需要进入容器；传输控制器的辅助网络空间仅承载通道，不隔离业务应用。入口投递到原有管理地址的授权端口。

裸机不提供虚拟开关机、重启、重装系统、调整 CPU/内存/系统盘或删除物理盘。DNS 保留操作系统配置；CubeVS 连接数额度不宣告为可用。容量额度是独占准入和记账，不是 cgroup 资源隔离。

选中业务出口而通道未就绪时保持不可达，不回落到原有默认路由。未匹配业务路由的流量仍使用宿主网络。保护目标优先于业务路由；已有宿主服务和管理员配置的防火墙仍可能限制流量。启动后的 Agent 会从持久意图离线恢复，但不能承诺 Agent 启动前的任意操作系统服务已受业务路由约束；需要启动顺序约束的工作负载应在 Agent 恢复后启动。

## 解除纳管和恢复

使用实例的“解除纳管”，不要使用 Cube 的删除接口。Agent 撤销终端、业务网卡、路由和入口，确认后释放绑定和额度。失败时保留资源归属并允许重试；不会删除用户文件、系统盘、账号或 SSH 配置。换用户之前应按运维流程处理遗留数据；解除纳管不会代为清除。已有系统进程继续由操作系统管理。

切换后端或降级之前先完成解除纳管，停止 Agent 后在原有环境下运行 `brocade-agent cvm-check --empty --config /etc/brocade/cvm-host.json`。不要删除持久状态来绕过清理检查。原有记录用于拒绝过期意图和跨绑定终端，必须保留。旧 Agent 不具备裸机恢复契约时拒绝自更新接管。

开发验收使用独立 KVM 虚拟机中的真实 Linux，测试记录区分静态、数据库、网络命名空间与整机功能结果；未执行的功能不能以单元测试替代。
