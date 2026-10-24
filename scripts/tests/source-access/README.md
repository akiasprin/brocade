# 来源 IP 封禁验收

复用用户服务验收的隔离 Docker 运行器，启动真实 PostgreSQL、Console 及本次 Console 分发的 Agent/Xray。所有策略修改走 HTTP API，执行状态来自真实 Agent；载荷经过 VLESS/REALITY/Vision。容器只增加 NET_ADMIN，不挂 Docker socket，不使用宿主网络，不修改宿主服务。

```sh
BROCADE_GO=/path/to/go1.26.9/bin/go cargo build -p brocade-console --locked
python3 scripts/tests/user-service/run.py \
  --console target/debug/brocade-console \
  --geoip .tools/geoip.dat --geosite .tools/geosite.dat \
  --node-fixture scripts/tests/source-access/node.py \
  --support-fixture scripts/tests/user-service/node.py \
  --output /absolute/path/to/new-private-results
python3 scripts/tests/source-access/verify.py /absolute/path/to/new-private-results \
  --xray-build-record target/debug/build/brocade-console-BUILD/out/xray-x86_64.build.json \
  --embedded-agent target/debug/build/brocade-console-BUILD/out/brocade-agent-x86_64
```

若工作区同时有其他构建，先把本次 Console、内嵌 Agent 和 Xray build record 复制到同一个私有目录，再使用这些固定路径运行与校验，避免其他构建覆盖产物而失去对应关系。

覆盖同 IP 不同用户、同用户不同 IP、节点与用户名单并集、IPv4-mapped IPv6 归一化、IPv6、已有会话关闭、解封后重新接入、离线期间耐久策略、Console/Agent 重启后的选择性访问、乐观并发冲突、输入校验、无配置发布及迁移链仅 0001/0002。

用户流量预算是另一项独立的 Agent 服务。验收在策略修改期间检查其他来源的原始载体持续收发；重启 Agent 前主动结束该对照载体，离线阶段核验耐久文件与 Xray 实际应用回执，服务恢复后再次核验未封禁来源可以访问、被封禁来源仍被拒绝。它不把预算服务暂不可用时的传输失败作为来源封禁生效的唯一证据。

四种接入协议另由 Go race 测试检查真实认证处理：VLESS TCP/Mux、AnyTLS、Hy2 QUIC TCP/UDP，以及 MTProxy 认证后、访问官方目录之前的拒绝。XHTTP 还验证伪造转发头不会替代来源、受管 loopback PROXY v2 前端保留地址。QUIC 活跃对端变化在不修改策略时仍重新检查。上述测试不冒充与 Telegram 公网中间代理的端到端验收，也不等同于生产 WAN、ARM64 或完整配置交接的 Docker 验收。

结果目录包含一次性凭据与私有日志，请勿上传；校验器只输出非敏感摘要。仅清理由本轮创建的容器与网络。
