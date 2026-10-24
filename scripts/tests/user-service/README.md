# 用户账期与有效期 Docker 验收

在独立的 Docker 内部网络运行真实 Console、PostgreSQL 16 和 Console 分发的 Agent/Xray。
使用已有工具镜像，不安装虚拟机。拓扑及节点凭据是一次性种子，用户、授权、策略、配额及配置发布走真实 HTTP API。
不伪造 Agent 回执、已应用状态或用量账本，不连接生产 Console；Agent 的网络操作限制在节点容器命名空间，不调整宿主机时间或宿主服务。

```sh
docker build -t brocade-user-service:local scripts/tests/user-service
BROCADE_GO=/path/to/pinned/go cargo build -p brocade-console --locked
python3 scripts/tests/user-service/run.py \
  --console target/debug/brocade-console \
  --geoip /path/to/geoip.dat --geosite /path/to/geosite.dat \
  --output /path/to/new-private-results
python3 scripts/tests/user-service/verify.py /path/to/new-private-results \
  --xray-build-record target/debug/build/brocade-console-BUILD/out/xray-x86_64.build.json \
  --embedded-agent target/debug/build/brocade-console-BUILD/out/brocade-agent-x86_64
```

`BUILD` 对应被选 Console 的构建输出目录。工具镜像默认为 `brocade-user-service:local`，
其 Dockerfile 位于本目录，可用 `--image` 指定；数据库镜像默认为 `postgres:16-alpine`，可用 `--database-image` 指定。
所有容器和网络使用本轮随机名称；不发布宿主端口，不挂 Docker socket，不使用 privileged 或宿主 PID／网络。
节点仅增加 NET_ADMIN，在自己的网络命名空间中配置本地回显地址。SQL 种子及只读账本核验由宿主测试器对本轮数据库执行，不给节点挂载数据库管理工具。

场景覆盖真实 VLESS/REALITY/Vision TCP 和端口转发：

- 真实初始配置发布，两位用户维持原始 TCP 连接并不断核对载荷。
- 正常后台计时到期关闭原有接入与转发连接、拒绝新连接；另一用户不重连且持续收发。
- 续期恢复两条路径，手动关闭的授权和规则不会被续期重开。
- 实际载荷产生账本、耗尽接入和转发额度，并触发真实暂停。
- 修改重置日前按已有账本预览，不清空已用流量。
- 取消配额后后台恢复额度，仍不能解除有效期限制；再次续期后恢复真实访问。
- 预约开始及 Console/Agent 进程重启后，策略仍正确，其他用户的原始连接继续工作。
- Xray PID/start ticks 全程不变，历史账本不减少，数据库迁移仅为 0001、0002。

月末、闰年和账期边界另由完整 PostgreSQL 集成测试检查同一账期函数。
Docker 共享宿主时钟，因此本套测试不修改时间，不把取消配额的场景冒充为真实跨月重置。
验证器独立核对冻结输入摘要、实际分发二进制、容器隔离、完整场景矩阵、进程身份和载荷进展。

结果目录必须是新目录，保留私有日志与摘要；日志可能包含一次性测试凭据，勿公开上传。
退出时只删除本轮创建的容器和网络。不要直接在宿主机运行 `node.py`。
这套验证不等同于生产机队、WAN、其他发行版或 ARM64 验收。
