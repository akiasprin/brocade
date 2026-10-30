# Xray 升级拓扑与真实 kTLS 验证

同一份工作负载可运行于四个隔离 Docker 容器，或四台独立 KVM（客户端、入口、两个出口）。代表性结构包含 VLESS REALITY/Vision、AnyTLS TLS 和 Hy2 同时接入，VLESS Encryption 中继、Mux 预热池、IPv4/IPv6 分流，以及由 Brocade 编译器实际渲染的反向链路。

这不是整个生产机队的重放。只读结构导出用于确认协议、分支与拨号形态；夹具使用固定测试地址、测试授权和临时密钥，不读取生产用户、密钥、证书或订阅内容。普通流量与弱网各核对完整负载，HTTP 上传同时逐字节比对入口用户计数。弱网在各自容器或 VM 的接口注入延迟、1% 丢包和 MTU 1280；UDP 批量记录真实丢包并校验每个收到的负载，TCP 不允许截断。

先从本次 Console 测试构建中选择实际内嵌的 Xray 和实际编译出的 Agent 测试程序，保留明确的旧 Brocade Xray。不得用历史 `.tools/xray` 替代待验产物。取得受权目标的匿名结构（需已有 SSH 权限）：

```sh
python3 scripts/tests/xray-upgrade-topology/export_topology.py \
  --ssh user@console.example.net --output /absolute/path/topology.json
BROCADE_REVERSE_EXPORT=/absolute/path/new-reverse-configs \
  cargo test -p brocade-core --test xray_binary --locked export_rendered_reverse_topology -- --ignored
cargo test -p brocade-agent --locked --no-run
docker build -t brocade-xray-topology:local -f scripts/tests/xray-faults/Dockerfile scripts/tests/xray-faults
```

`export_topology.py` 使用只读事务、只读连接选项和语句超时，只导出匿名节点／链 ID、布尔能力、规则类型及地址族。默认数据库 URL 在远端 `/etc/brocade/database-url` 内读取，凭据不会传回本机。输出必须是新文件。

```sh
python3 scripts/tests/xray-upgrade-topology/run.py --mode docker \
  --binary /absolute/path/current-xray --old-binary /absolute/path/old-brocade-xray \
  --agent-tests /absolute/path/current-agent-tests \
  --reverse-config-dir /absolute/path/new-reverse-configs \
  --topology /absolute/path/topology.json --output /absolute/path/new-docker-results
```

默认 24 项，包括旧／新客户端、双出口 HTTPS 下载和上传、UDP、8 路并发反向 TCP/UDP，以及真实 Rust 计量读取器验证旧版未采样尾数在新版恢复后精确等于 4608 字节、计量身份不变。反向进程保留渲染出的账户、健康配置、路由和出站；只替换隔离前端的测试授权及监听，并禁用资源下载。`--old-exit-a` 或 `--old-entry` 可检查滚动升级组合。旧入口在 MTU 1280 下的 Hy2 握手仍可能失败，是需要升级入口的既有边界，不能用重复重试作为通过依据。

真实 kTLS 必须在支持的 KVM 内运行，且不接受跳过。先用钉住的 Go 编译测试二进制：

```sh
go -C components/xray-core test -c -buildvcs=false -o /absolute/path/tls-tests/tls-kernel.test ./transport/internet/tls
go -C components/xray-core test -c -buildvcs=false -o /absolute/path/tls-tests/anytls-kernel.test ./proxy/anytls
python3 scripts/tests/xray-upgrade-topology/run.py --mode kvm \
  --binary /absolute/path/current-xray --old-binary /absolute/path/old-brocade-xray \
  --agent-tests /absolute/path/current-agent-tests \
  --reverse-config-dir /absolute/path/new-reverse-configs \
  --ktls-tests-dir /absolute/path/tls-tests --vm-image /absolute/path/noble-server-cloudimg-amd64.img \
  --topology /absolute/path/topology.json --output /absolute/path/new-kvm-results
```

QEMU 工具镜像和固定 Ubuntu 镜像准备见[升级演练](../upgrade/README.md)。运行器校验基础镜像 SHA-256，强制 `-accel kvm`，每个角色使用独立写时复制磁盘和客体内核。kTLS 增加第 25 项：AES-128/256-GCM TX、会话恢复、大票据、AnyTLS writev/splice 与元数据；要求无 SKIP、客体 `TlsTxSw` 增长，还检查真实 AnyTLS 接入的认证 kTLS 计数和入口内核 TX 计数。

结果保存冻结二进制、脚本和镜像身份、原始进程日志、真实网络参数、用户计费对照、反向健康快照和内核计数。测试仅覆盖软件 kTLS TX，不认证网卡硬件卸载、ARM64 实机、整个机队容量或公网可达性；ARM64 编译及发行验收按项目既有入口执行。

使用 `--shared-budget-tests` 可进一步运行机器双代预算及用户跨节点预算，分别要求真实 kTLS splice/writev，扩展为 28 项。详细边界与独立协议矩阵见[定制能力端到端验收](../xray-extended-e2e/README.md)。
