# HY2 单 IP、端口段与独立 conntrack 验证

## 正式 Agent/Xray 路径

使用 `--agent-tests` 时运行仓库实际 Agent 的启动、重启、恢复和收敛入口，Xray 使用正式 CLI，
没有实验 FD controller。先用本地源码构建 Agent 测试程序及 Xray，再显式指定二进制：

```sh
python3 scripts/tests/hy2-netns/run.py --mode docker \
  --xray /绝对路径/xray --agent-tests /绝对路径/brocade_agent-test \
  --output target/experiments/hy2-production-docker

python3 scripts/tests/hy2-netns/run.py --mode kvm \
  --vm-image /绝对路径/noble-server-cloudimg-amd64.img \
  --xray /绝对路径/xray --agent-tests /绝对路径/brocade_agent-test \
  --output target/experiments/hy2-production-kvm
```

正式路径断言 37 项客户端/网络检查，另由 Rust 场景核验旧模式迁移、同 PID 规则修复、
链路丢失后的重建、再次重启、同配置字节下失败恢复、关闭并重新启用跳跃和新启动失败回滚。
配置内不含静态用户，真实 HY2 认证成功也验证了动态授权恢复。两侧分别查 conntrack，
只比较新流，不把迁移前尚未到期的旧主空间条目当作新泄漏。ICMPv6 PTB 注入必须更新
HY2 空间的 PMTU，且同一 QUIC 会话继续传输；IFB 队列必须经过实际业务数据。
IPv4／IPv6 另从节点本机连接本机公网地址，验证 Agent 本机拨测不会因回环路由而失效。
额外验证自定义源策略路由、现有源 NAT 与默认 DROP 的 FORWARD 链共存；私有链路的实际占用在停机前拒绝；
普通本机 UDP 与回环端口段 UDP 在已启用跟踪时保持正向对照；实际 VPN 对端仍可经 NAT 出站，旧 `/30` 规则在线收窄后不再修改主机地址发出的报文。
新增安全回归拒绝第三方逻辑监听口和跳跃段占用，并确认失败时旧 PID 与旧规则保持服务。
第三方防火墙、legacy 规则、策略路由及 QoS 不作为迁移准入条件。另验证保留端口合并／回收、外部修改保留，
以及禁用公网与全局 IPv6 后真实 IPv4 HY2 TCP／UDP 仍可通信。
旧路径恢复时将宿主机两项 UDP 超时设为非默认值并单独绑定为只读，断言服务能启动且原值
保持；携带旧 Xray 时，对实际发行 Agent 的回迁 CLI 再验证同一边界。

传入 `--agent /绝对路径/正式brocade-agent` 后，迁移、再次重启与 IPv4-only 启动通过该发行
Agent 的真实 `repair` CLI 执行；清单保存 Agent 与 Xray 的字节摘要。用于发行验收时应传入
本次固定入口构建的实际产物，不能以另编调试二进制替代。
追加 `--legacy-xray /绝对路径/旧Xray --legacy-agent /绝对路径/旧Agent` 后，另验证新 Agent
先回迁到旧 Xray、旧 Agent 重启旧服务、再升级回新空间，共 40 项业务检查。公网出口额外配置
TBF 与默认 FQ，在接收抓包启用时验证双栈回包、不产生 FQ horizon 丢包且原队列参数不变；
删除专用接口的时间戳过滤器后，Agent 同 PID 补齐。旧字节摘要与
来源应在发布记录中核验；仅复制二进制，不复制生产凭据或配置。

运行状态使用容器/客体本地文件系统，保证计量 journal 的分配和 mmap 行为；KVM 的 9p
共享目录仅导出证据。私有网络内开启抓包，所有地址和用户凭据均为实验数据。
内核与实际 sysctl、nft、iproute2 版本留在运行清单和结果中；QMP 验证硬件 KVM。
这不证明 ARM64 原生运行、所有旧内核、复杂第三方网络策略或吞吐容量。

## 同机真实 Docker 的 KVM 验证

这与“在 Docker 容器中运行测试”不同：KVM 客体内启动实际 dockerd，让 Agent 与 Docker
共享同一个私有宿主网络空间。Docker 自己建立默认 bridge、FORWARD DROP、MASQUERADE
和 TCP/UDP 发布规则；不关闭 Docker 防火墙，不手写替代规则。镜像仅离线导入静态 echo
程序，客体没有外网连接。先准备离线包和容器根文件系统：

```sh
docker build -t brocade-hy2-netns:20261031-compat scripts/tests/hy2-netns
docker build -t brocade-hy2-docker-host:20261031 \
  -f scripts/tests/hy2-netns/Dockerfile.docker-host scripts/tests/hy2-netns
mkdir -p target/experiments/hy2-docker-rootfs
CGO_ENABLED=0 go build -trimpath -o target/experiments/hy2-docker-rootfs/echo \
  scripts/tests/hy2-netns/docker_echo.go
tar -C target/experiments/hy2-docker-rootfs -cf target/experiments/hy2-docker-rootfs.tar echo
```

使用上文的真实 KVM 命令，另传入 `--image brocade-hy2-docker-host:20261031`、
`--docker-host-image /绝对路径/hy2-docker-rootfs.tar`、本次发行 `--agent` 和实际旧版
`--legacy-xray`。共 21 项检查覆盖升级前后及 Agent 服务重启后的容器出站 NAT、TCP/UDP 发布、
双栈 HY2、主空间与专用空间 conntrack、本机公网地址拨测，以及 Docker 规则保持不变。
升级复用既有故障验收的冻结工单 HTTP 夹具，常驻 Agent 经正式 binary-release 下载、校验、
安装、重启和回执路径替换真实旧版 Xray，不用 repair 替代发布。systemd 使用与生产一致的
LogNamespace 与 KillMode=process，并在 Agent 服务重启后核验旧 Xray 原 PID 和空间接管。
迁移被拒绝时仍验证原服务与 Docker 保持可用，但测试必须失败，不把安全拒绝计作兼容通过。
结果、daemon 日志及输入摘要留在输出目录；不接触宿主机或生产节点网络。

## 历史机制原型

这是隔离实验，不安装生产规则，也不改变 Agent 或正式 Xray 的启动方式。
运行边界遵循 [测试规范](../../../.agents/standards/testing.md)。

## 验证的路径

```text
client netns
  -> 模拟公网网卡 wan 的 TC ingress：匹配公网 IP + UDP 30000–30099
  -> skbmod 更新下一跳 MAC，mirred 转发到 HY2 veth
  -> HY2 netns 内 nft REDIRECT 30000–30099 -> :30000
  -> 在 HY2 netns 创建的 UDP socket
  -> SCM_RIGHTS 把 FD 传给主空间进程中的 Xray

Xray 通过该 FD 回包
  -> HY2 netns 的反向 NAT 恢复公网 IP 与客户端请求的目标端口
  -> HY2 veth 的主侧 TC ingress 直接转发回 wan
  -> client netns

Xray 新建的 TCP/UDP 出站 socket -> 主空间网络栈
```

主空间保留公网 IP；主空间没有为 HY2 设置 DNAT、REDIRECT 或 Docker 端口映射。
每个地址族只有一个 HY2 监听 socket，监听 `:30000`；端口段由网络层映射。
TC 使用软件分类，入站和回包都在主空间 L3/conntrack 之前转发。

`xray_fd.go` 复用仓库现有的 `RegisterPacketHandoffController` 注入继承的 UDP FD，
实际 HY2、QUIC、TLS 和 freedom 出站都来自冻结的本地 Xray 源码。
它是实验入口，不是生产 FD broker；没有实现热升级代际、Agent 收敛或规则恢复。

## 运行

需要 Docker；KVM 模式还需要可用的 `/dev/kvm`、`cube-qemu-lab:24.04` 和已经校验的基础镜像。
基础镜像摘要与仓库现有 KVM 测试一致，运行器再次计算摘要，并通过 QMP
`query-kvm` 验证 `enabled=true` 和 `present=true`，不会退回软件模拟。

先构建实验工具镜像：

```sh
docker build -t brocade-hy2-netns:20261031 scripts/tests/hy2-netns
```

冻结 `components/xray-core` 到独立实验目录，记录各文件 SHA-256；把 `xray_fd.go`
复制到该目录之外，再以冻结目录作为工作目录编译。使用仓库 `.go-version` 指定的 Go，
设置 `CGO_ENABLED=0`，执行 `go build -trimpath -o /绝对路径/xray-fd /绝对路径/xray_fd.go`。
这是测试程序，不是发行构建；不得用它替代固定发行构建入口。

两种环境执行完全相同的冻结工作负载：

```sh
python3 scripts/tests/hy2-netns/run.py --mode docker \
  --xray /绝对路径/xray-fd --output target/experiments/hy2-netns-docker

python3 scripts/tests/hy2-netns/run.py --mode kvm \
  --vm-image /绝对路径/noble-server-cloudimg-amd64.img \
  --xray /绝对路径/xray-fd --output target/experiments/hy2-netns-kvm
```

输出目录必须不存在。省略 `--xray` 仅运行 22 项网络基础检查；完整矩阵必须携带
从本地冻结源码编译的实验二进制，完成全部 30 项检查。
运行器记录输入摘要、镜像 ID、UTC 起止时间和单调时钟耗时。
KVM 使用新建 overlay 与离线安装包，基础镜像只读；两种容器均使用 `--network none`，
没有 host network/PID，也没有连接生产节点。

## 断言与证据

- IPv4/IPv6 端口 `30000/30001/30037/30098/30099` 的回包源 IP 和端口正确，应用看到原始客户端 IP 与端口。
- 范围外 UDP 和同端口 TCP 仍在主空间；正向对照证明主空间 conntrack 查询有效。
- 200 条并发 UDP 流的主空间 CT 为零，HY2 空间有对应条目；1200 字节 UDP 负载无截断。
- 主进程从未进入 HY2 netns，SCM_RIGHTS 接收的 socket 仍使用其出生空间。
- 删除主空间测试用 CT 规则后，HY2 netns 的 NAT 仍工作，也不会启用主空间新 TCP 的自动跟踪。
- 真实 HY2 在 IPv4/IPv6 的 `30000/30037/30099` 上承载 TCP 与 UDP，逐字节校验数据；TCP 出站只出现在主空间 CT。
- 每个地址族维持同一条 TCP 代理流，间隔 5 秒跳 UDP 目标端口，要求观察到至少两个不同端口并继续完成传输。

`results/result.json` 保留逐项结果；`real-hy2-sockets.txt` 记录实际 Xray PID 与两只
监听 socket；两侧 `*-conntrack.txt`、`*-nft.txt` 和 `tc-*.txt` 保留规则、条目与计数。
读取 conntrack 必须成功完成；stderr 的工具摘要不计为条目，查询失败不能当作零条。
进程、容器和网络命名空间有界清理；镜像、overlay 和日志保留用于复核。

## 验证边界

实验使用 veth 模拟公网网卡，静态下一跳 MAC 模拟路由器；没有验证实际云厂商网卡、
网关变化、多网卡策略、NIC offload、ICMP/PMTU 错误投递、IPv4 分片或 IPv6 扩展头。
生产实现还要处理这些路径，以及既有 TC/QoS 规则共存、启动失败恢复与代际 FD 所有权。
netns 隔离跟踪作用域，不增加主机总内存；本实验不提供吞吐、CPU 或容量结论。

相关机制的原始说明：[socket 出生空间](https://www.wireguard.com/netns/)、
[TC flower 范围匹配](https://man7.org/linux/man-pages/man8/tc-flower.8.html)、
[TC mirred 转发](https://man7.org/linux/man-pages/man8/tc-mirred.8.html)。
