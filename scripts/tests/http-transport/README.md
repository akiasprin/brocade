# Agent HTTP Docker / KVM 验收

同一份静态 Agent、测试可执行文件和显式选择的 Xray 在两种独立环境运行：

- Docker 使用独立网络命名空间，只增加 `NET_ADMIN`，不挂 Docker socket，不使用宿主机网络或 PID。
- KVM 使用经过 SHA-256 校验的 Ubuntu cloud image、新建 qcow2 overlay 和 `/dev/kvm`。QMP 必须确认硬件加速已启用。

`guest.py` 只允许在一次性容器或客机内运行。不要直接在宿主机执行。每轮输出必须选择新目录；失败记录和日志会保留，结束后只删除本轮创建的容器。

## 输入与执行

先构建 Linux x86_64 musl Agent 和单元测试可执行文件。后者由 `cargo test -p brocade-agent --release --target x86_64-unknown-linux-musl --no-run --message-format=json` 的 `compiler-artifact` 中 `profile.test=true` 的 Agent 目标确定，不传入 `tests/self_test.rs` 的可执行文件。Xray 从当前 fork 用固定 Go 工具链构建；旧版本 Xray 用于检查旧 RouterService 的兼容退路，必须显式传入。

```sh
python3 scripts/tests/http-transport/run.py \
  --runtime both \
  --agent /absolute/path/brocade-agent \
  --tests /absolute/path/brocade_agent-test-executable \
  --xray /absolute/path/current-xray \
  --legacy-xray /absolute/path/legacy-xray \
  --vm-image /absolute/path/verified-ubuntu-minimal.img \
  --vm-sha256 VERIFIED_SHA256 \
  --output /absolute/path/new-output-directory
python3 scripts/tests/http-transport/verify.py /absolute/path/new-output-directory
```

默认工具镜像为 `brocade-forwarding-continuity:local`（Python 3.12、iproute2、curl、procps、CA 证书）和 `cube-qemu-lab:24.04`（QEMU、cloud-localds、Python 3.12）。可通过 `--docker-image`、`--qemu-image` 覆盖。输入文件哈希及镜像 ID 保存在 `manifest.json`。

Docker 工具镜像可用 `docker build -t brocade-forwarding-continuity:local scripts/tests/forwarding-continuity` 构建，其定义见 [Dockerfile](../forwarding-continuity/Dockerfile)。KVM 需要可访问的 `/dev/kvm`，并在客机初始化时联网安装测试工具；不会退回纯软件模拟。

## 验收内容

每种环境先执行 HTTP、期望状态投递、磁盘队列测试，以及测试可执行文件列出的真实进程集成用例。唯一排除的是需要 Docker daemon 的流量限制内核测试；应在宿主机显式选择工具镜像另跑：

```sh
BROCADE_TRAFFIC_LIMIT_NET_IMAGE=brocade-forwarding-continuity:local \
cargo test -p brocade-agent --locked \
  traffic_limit::tests::traffic_limit_system_real_kernel_reconciles_caps_and_preserves_external_qos \
  -- --ignored --exact --test-threads=1
```

随后启动生产 Agent 可执行文件、真实 Xray、持续 VLESS TCP 连接和 UDP 回显，使用本地模拟 Console 注入故障：

- 运行状态响应完全卡住，或持续缓慢发送响应体，均应在 30 秒总期限后关闭连接并自动恢复。Agent 和 Xray 的 PID、启动时间不变，计费与期望状态请求仍可前进，恢复后的在线来源快照必须包含持续连接的 VLESS 用户和来源。使用仅存在于客机网络中的 `198.18.0.2` 源地址，避免 Xray 对回环源 `127.0.0.1` 的主动过滤。
- 计费已经持久记录后丢失 ACK、截断响应、返回 503、卡住响应体，磁盘记录保留，并以同一实例、序号和原始请求字节重传。完整成功响应后才删除。
- SIGKILL Agent，验证存量 TCP/UDP 连续、Xray 进程不变、队列恢复并补报。
- 离线积压后真正重启 Docker 容器，或重启 KVM 客机；检查原记录保留、恢复后全部收到。KVM 的 boot ID 必须变化。

`verify.py` 独立检查输入哈希、测试日志、用例完整性、期限、进程身份、业务进展和补报凭证。各环境保留 Agent 日志、原始测试日志、证据、模拟服务收据和最终结果。

这套验收的 Console 是故障注入模拟服务，收据去重只验证 Agent 重传行为；数据库的持久幂等性另由真实 PostgreSQL 集成测试覆盖：

```sh
BROCADE_RUN_PG_TESTS=1 cargo test -p brocade-store --test pg_integration --locked \
  usage_report_receipt_is_exactly_once_under_retry_and_concurrency -- --ignored --exact
```

HTTPS、证书、TLS 截断和 WebSocket 握手边界由同一客机中的 `http::` 测试覆盖；生产 Agent 的故障注入使用本地 HTTP。测试证书仅编入测试可执行文件。此验收不会连接生产 Console，也不执行生产发布。
