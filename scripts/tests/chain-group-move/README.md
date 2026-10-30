# 跨分组移动的真实 Docker 验收

复用用户服务的 Docker 运行器和基础设施，运行真实 Console、PostgreSQL、由该 Console 分发的 Agent/Xray。使用独立内部网络、随机容器名和私有结果目录；不连接生产、不发布宿主端口。

先按[固定构建入口](../../../.agents/runbooks/console-deployment.md#固定位置双架构与耗时记录)冻结源码、准备发行产物，再运行：

```sh
docker build -t brocade-chain-move-lab:local scripts/tests/user-service
python3 scripts/tests/user-service/run.py \
  --console /absolute/path/to/selected/brocade-console \
  --node-fixture scripts/tests/chain-group-move/node.py \
  --support-fixture scripts/tests/user-service/node.py \
  --node-arg default --image brocade-chain-move-lab:local \
  --geoip /absolute/path/to/geoip.dat --geosite /absolute/path/to/geosite.dat \
  --output /absolute/path/to/new-private-results
python3 scripts/tests/chain-group-move/verify.py /absolute/path/to/new-private-results \
  --xray-build-record /absolute/path/to/selected/xray-x86_64.build.json \
  --embedded-agent /absolute/path/to/selected/brocade-agent-x86_64
```

`default` 保持进程交接 Beta 关闭；另用新的结果目录把参数改为 `--node-arg handoff`，仅在本轮隔离容器中启用交接。两种发布策略独立验收，不能把其中一种的通过当成另一种的保证。

测试从真实初始发布开始，建立四条不重连的 TCP 连接（两位接入用户、两条用户转发），持续创建真实 HTTP 请求，同时让一条约 40 秒的 HTTP 响应跨过换组发布。模型预览必须回滚；换组、新建目标组和提交走 HTTP API，配置应用必须由真实 Agent 执行和回执。任何原连接 EOF、载荷截断、HTTP 请求失败、隐式重连，或长请求未跨过发布时刻，均使测试失败。流量归属必须由真实计量报告进入历史和目标分组，不能插入伪造用量。

GeoIP/GeoSite 仅放在原 Xray 可执行文件旁，不在候选临时目录或系统回退目录预置副本。交接必须保持原有资源查找语义，不能通过改变夹具资源布局掩盖候选启动失败。

证据记录初始 Xray PID/start ticks、前后请求数、长响应的完整 800 个块、连接次数、发布计划和回执、真实账本。所有输入与二进制摘要被冻结。结果可能含一次性测试凭据，目录须私有保存。

本测试只证明所选策略、所选二进制与此 Docker TCP 场景。交接的强制排空期限、UDP 的 reconnect-only 语义、跨机器中继、WAN 和其他架构需分别验证，不能由本测试推断。
