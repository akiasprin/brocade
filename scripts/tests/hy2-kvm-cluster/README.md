# 四客体 KVM HY2 吞吐与网络共存实验

四个独立 QEMU/KVM 客体组成客户端 → 边界路由器 → HY2 节点 → 业务目标。
每个客体有独立内核和磁盘 overlay；QMP 必须确认硬件加速，boot ID 必须互异。
QEMU 全部位于一个 `--network none` 容器中，三个以太网段使用容器私有 TAP/vhost 网桥，
不经过 TCP 隧道、不创建宿主网桥、不修改宿主路由、不访问生产环境。
网络后端见 [QEMU 官方文档](https://www.qemu.org/docs/master/system/invocation.html)。

使用与生产相同的正式 Agent 和 Xray 字节。Agent 的 `repair` 仅在全新实验客体内
重放合成产物，建立正式 HY2 网络和动态授权；此实验不替代已有常驻 Agent 升级验收。
对照使用同一 Xray 的普通主空间 HY2 监听，另有 TCP 直连路径；比较包含空间转发、
NAT 和时间戳处理的整体代价，不能将差值全部归因于某条 BPF 指令。
后续正常再收敛通过实际 `apply-once` CLI 与本地夹具的 204 响应，进入生产的
`OnlyWhenBroken` 路径；不用会按设计强制重启 Xray 的 `repair` 验证 PID 保留。

```sh
docker build -f scripts/tests/hy2-kvm-cluster/Dockerfile.qemu \
  -t brocade-hy2-cluster-qemu:20261101 scripts/tests/hy2-kvm-cluster
CGO_ENABLED=0 go build -trimpath -o /绝对路径/traffic scripts/tests/hy2-kvm-cluster/traffic.go
python3 scripts/tests/hy2-kvm-cluster/run.py \
  --output /新的实验输出目录 \
  --vm-image /已校验的/noble-server-cloudimg-amd64.img \
  --agent /本次正式/brocade-agent --xray /本次正式/xray \
  --traffic /绝对路径/traffic --docker-rootfs /离线/echo-rootfs.tar
```

Go 使用 Xray fork 固定工具链。离线包与 echo 根文件系统复用
[HY2 空间功能验收](../hy2-netns/README.md)，镜像身份和所有输入摘要保存在清单中。
默认每个清洁环境吞吐组合测量 10 秒、交替顺序重复三次，包含单流上行、单流下行、
八流双向及 IPv4/IPv6；组合网络测量至少 12 秒、每项重复两次。
所有测量校验每个完整负载块与接收端确认的字节数，不将本地 socket 写入量当作送达量。

累加组合依次引入真实 dockerd bridge、MASQUERADE、FORWARD DROP 和 TCP/UDP 发布，
默认拒绝的 nft INPUT、源地址/mark 策略路由、独立 legacy NAT、严格反向路径过滤，
TBF/FQ 与接收抓包、IFB 入站限速、上游 SNAT/DNAT、1280 MTU，最后增加双向时延与丢包。
每阶段核验 Docker 出站与发布业务、主空间 HY2 conntrack 为零、专用空间存在正向条目，
以及 Agent 再收敛保留 Xray PID 和第三方规则。最后在完整组合上做双栈各 120 秒、
每个方向 32 条 TCP 流的持续传输。

`results/cases.json` 是逐项结论；每个测量 JSON 保留四台客体的前后 CPU、内存、
链路/队列/softnet 计数、节点 nft 和两侧 conntrack。命令与回复、串口、daemon 日志和
overlay 都保留，运行结束清理本轮容器。所有时间使用 UTC 与单调时钟。

真实 firewalld 与 UFW 使用各自的全新四客体集群，避免两个防火墙相互接管规则。
先准备应用离线包镜像，再对上面的命令增加
`--packages-image brocade-hy2-cluster-packages:20261101 --suite firewalld` 或 `--suite ufw`：

```sh
docker build -f scripts/tests/hy2-kvm-cluster/Dockerfile.packages \
  -t brocade-hy2-cluster-packages:20261101 scripts/tests/hy2-kvm-cluster
python3 scripts/tests/hy2-kvm-cluster/verify.py /实验输出目录
python3 scripts/tests/hy2-kvm-cluster/analyze.py /实验输出目录
```

每种应用组合有 21 项检查：真实服务管理的规则与真实 Docker 并存，然后叠加内核
WireGuard 业务出站、上游 NAT、FQ 限速与接收抓包；在双向负载期间重载防火墙，
核验负载内容、Docker 并行业务、Xray PID、Agent 再收敛和真实 WireGuard 加密字节。
监测器持续记录重载期间的请求失败与恢复，不在首个错误处停止观测；平稳阶段要求零失败，
重载后要求业务可用，并将重载过程中的中断作为独立结果报告，不能宣称无中断。
已完成前置负载后可用 `--app-reload-only` 只重建拓扑、预热并验证在线重载，共 13 项。
`--reload-count 10 --observer-interval 0.05` 可在同一负载期间连续重载十次，
每次间隔两秒；监测器在每次请求完成后等待指定秒数，因此实际采样周期还包括请求耗时。
独立证据审计重读所有测量的 IPv4/IPv6 正向 conntrack 控制，并检查两个方向的
主空间 HY2 泄漏、FQ/IFB 实际包计数和零 horizon 丢包，以及防火墙重载确实发生于负载期间。

1280 MTU 用例的 IPv6 客户端显式设置 `finalMask.quicParams.disableChromeParrot=true`，
保留端口跳跃。默认 ChromeParrot 固定发送 1250 字节 UDP Initial，IPv6 总长 1298，
在 1280 路径会在边界路由器被 PTB 拒绝；同版本 Xray 的主空间直听对照也失败。
关闭该指纹后初始负载为 1200，主空间与专用空间均能通信。这是客户端指纹的已测限制，
不能把兼容参数下的通过描述为默认指纹也兼容最低 IPv6 MTU。清洁网络基准仍使用默认指纹。

这些数值只描述本机共享 CPU、virtio 和 TAP/vhost 后端下的实验，不代表云网卡线速、
ARM 性能或无限并发容量；必须结合直连/主空间对照与各客体 CPU、丢包证据解释。
