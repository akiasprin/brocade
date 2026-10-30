# Xray 定制能力端到端验收

使用明确选择的最终 Xray 字节，验证真实客户端连接、Xray 进程、目标服务和统计 RPC。测试不会替换待验二进制；不接受跳过、空匹配或自动重试取绿。源码补丁涉及运行行为时，重新冻结构建并重验。

依赖：仓库钉住的 Go、Docker、`strace`、当前 Agent 测试程序以及旧版 Brocade Xray。Docker 镜像沿用[拓扑验收](../xray-upgrade-topology/README.md)准备的镜像。

```sh
cargo test -p brocade-agent --locked --no-run
python3 scripts/tests/xray-extended-e2e/run.py \
  --binary /absolute/path/current-embedded-xray \
  --old-binary /absolute/path/old-brocade-xray \
  --agent-tests /absolute/path/current-agent-test-executable \
  --go /absolute/path/pinned-go \
  --require-ktls --output /absolute/path/new-evidence-directory
```

`--require-ktls` 要求实际认证 kTLS 及 splice/writev 计数大于零。宿主内核不支持时该运行失败，应按下面的 KVM 入口完成这项验收，不能把跳过记为成功。其余测试使用受控本机端点；测试日志和产物 SHA-256 留在新建证据目录。

覆盖内容：

- 机器预算在两代 Xray 间共享、动态调整以及 TCP/UDP；真实 Agent 用户预算服务跨两节点共享，包含应用隔离、转发、TCP/UDP、旧连接暂停、控制面故障、恢复，以及机器上限与用户上限叠加。
- AnyTLS 分别开启 splice 和 writev，在限速期间检查完整 payload、真实 kTLS 认证与对应快速路径计数。
- VLESS、AnyTLS、Hy2 的来源 IP、在线协议和精确用户计量；用户/节点封禁关闭既有 TCP/UDP，另一用户的原连接继续传输，损坏策略不能解除封禁，解除后允许新连接。
- 网站、其他域名和嗅探失败目标的用户归属与精确计数，中继身份不生成重复网站明细。
- DNS 的 UDP/TCP 第三次发送成功与正缓存、冷/热诊断一致、同解析器与地址策略、4 秒/5 次总预算、关闭首个请求后共享工作继续，以及截断刷新保留过期正结果。地址族拒绝使用 Force 策略，断言 trace 未连接；不允许系统解析回退造成假通过。
- 历史 UDP hop JSON 经真实 Hy2 生效，记录真实 UDP 源端口切换，原应用连接继续完整传输，并以 `sendmsg` 的 `UDP_SEGMENT` 控制消息证明 GSO 没有被包装层丢失。随机算法允许再次选择同一目标端口，因此不把“每次目标端口必须不同”当作判据。
- MASQUE HTTP/2、HTTP/3 的 IPv4/IPv6 TCP、UDP、服务端互连和错误凭据拒绝。
- SS2022 三种算法的长密钥 TCP/UDP，以及新旧 Brocade 的 AES 多用户双向互通；另一端使用独立 SHA-256 规范化后的主/用户密钥。
- MTProxy 真实二进制正常获取 HTTPS 目录，执行加密 middle RPC、payload、用户/网站计量、错误 secret、重放拒绝和来源封禁。对端是隔离协议夹具，不能据此宣称真实 Telegram 账号登录或公网可达性已验证。
- 来源窗口在连接完全离线后保留精确流量；明细 v2 ACK 在容量满时释放已确认记录，观测受限期间业务仍可通信。
- 动态转发删除关闭对应原连接，另一条转发及普通入口不受影响；阻止字面量、localhost 解析结果和映射 IPv4 的 loopback 目标。
- 嗅探失败实际选择独立出站，恢复 HTTP 域名或伪造同名 HTTP 属性不能误选；路由替换失败保留原表，成功仅改变新连接。
- 证书文件轮转、损坏证书保留旧值及既有 TLS 连接连续通信；AnyTLS 默认／严格校验拒绝自签证书，只有显式 `allowInsecure` 放行。
- WebSocket、HTTPUpgrade、XHTTP 的来源防伪及实际 IP 封禁；VLESS、AnyTLS、Hy2 的逐用户 TCP/UDP 撤权和其他用户原连接隔离。
- 使用本地 `resources/geoip.dat` 验证 `cn@ipv4`、`cn@ipv6` 的真实路由选择，目标重定向到隔离本地服务；运行器显式设置 `BROCADE_E2E_ASSETS`。

MTProxy 还执行撤权隔离及机器／用户预算拒绝场景。目前两个预算回归在新旧二进制均失败：同进程 VLESS 对照被限，MTProxy 仍完整转发且没有预算请求。运行器保留失败退出，不把该异常作为预期成功。只读复核记录见[完整对照结果](../../../.agents/plans/xray-fork-review-20261028.md)。

HTTP/3 监听注册、暂停／恢复及连接生命周期已纳入普通源码集成回归。最终二进制场景 `TestBrocadeFrozenHTTP3GenerationHandoff` 验证 XHTTP/3、默认 MASQUE/3 和 MASQUE H2+H3 的真实双进程切换：核对 SCM_RIGHTS TCP 身份，按 reconnect-only 关闭旧 UDP，校验新 H3 TCP/UDP payload，保留混合模式原 H2 连接，关闭后才允许旧代退役。

```sh
go -C components/xray-core test -race ./transport/internet ./transport/internet/tcp \
  -run '^(TestHTTP3|TestPacketHandoffParent)' -count=3 -v
```

同一冻结字节可在一个真实 KVM 内重复上述最终二进制场景三次，要求三个模式各三次通过且无跳过。工具镜像、固定 Ubuntu 镜像与摘要复用[拓扑验收](../xray-upgrade-topology/README.md)；客体没有外网，结果保留内核、二进制／测试程序／镜像身份及原始日志：

```sh
python3 scripts/tests/xray-extended-e2e/run_http3_handoff_kvm.py \
  --binary /absolute/path/frozen-xray --tests /absolute/path/scenarios.test \
  --vm-image /absolute/path/noble-server-cloudimg-amd64.img \
  --output /absolute/path/new-http3-kvm-results
```

OCSP 刷新并发另有显式包级网络回归，使用本地合法响应和并行真实 TLS 握手，要求客户端实际收到多个不同的有效 staple；它不替代最终二进制的文件证书轮转场景：

```sh
BROCADE_RUN_OCSP_REVIEW=1 go -C components/xray-core test -race ./transport/internet/tls \
  -run '^TestReviewOCSPRefreshDuringTLSHandshakes$' -count=1 -v
```

MTProxy 夹具仅在自己的无外网 Docker 网络中，把文档地址 `198.51.100.2` 配到 loopback，以满足无 NAT socket 身份要求。目录 HTTPS 测试 CA 只传给该子进程，生产 URL、证书校验和直接拨号代码均保持正常执行；不加入生产测试开关。

真实 KVM 的共享预算与快速路径验收复用[拓扑运行器](../xray-upgrade-topology/README.md)：在原命令增加 `--shared-budget-tests`，并提供本次编译的 Agent 测试程序和 `--ktls-tests-dir`。该选项仅允许支持真实 kTLS 的 KVM，将原 25 项扩展为 28 项，新增机器预算、用户预算加 kTLS splice、用户预算加 kTLS writev，任何跳过或零匹配均失败。

普通 Go 测试未指定 `BROCADE_XRAY_BIN` 时，显式产物用例会显示 SKIP；上述运行器和 CI 指定产物并校验实际执行。场景覆盖率构建须取消该变量，使用独立插桩二进制；两者不能混用。
