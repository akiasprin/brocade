# 实时 WebSocket 压缩与机队负载验收

使用同一份冻结 Console 和其内嵌的静态 Agent，在独立 Docker 网络与启用硬件加速的 KVM 客机运行。每种环境拥有独立测试数据库；不连接生产控制面，不挂 Docker socket，不使用宿主网络或 PID。KVM 基础镜像必须预先核验官方 SHA-256，运行器创建新的 overlay 并通过 QMP 确认 KVM 已启用。

```sh
python3 scripts/tests/realtime-transport/run.py \
  --runtime docker --nodes 1000 --real-agents 32 --seconds 90 \
  --console /absolute/path/to/frozen/brocade-console \
  --agent /absolute/path/to/its/embedded/brocade-agent-x86_64 \
  --output /absolute/path/to/new-private-results
python3 scripts/tests/realtime-transport/verify.py /absolute/path/to/new-private-results
```

`--nodes` 是有独立节点凭据的协议模拟节点数量，`--real-agents` 是实际生产 Agent 进程数量，不把它们宣称为同等数量的独立机器。模拟节点使用独立的 Python 标准库 WebSocket/deflate 编解码，连接实际 Console 的节点路由。真实 Agent 使用 `state-dir` 模式，并核对其字节与该 Console 的下载接口一致，不启动或更换 Xray。

所有连接必须协商 `permessage-deflate`，等级、窗口和上下文复用使用默认参数。模拟节点持续发送每条 64 行的用户速率报告，经真实 PostgreSQL 节点认证与 Console 实时处理；另以最多 16 个节点各发送完整 2,048 行、约 260 KiB 的消息，再通过机器快照核对序号、速率、行数和尾行。机队 SSE 必须实际收到样本，长于一分钟的运行还须重新授权并恢复订阅。

真实 Agent 的双向帧通过透明代理检查 RSV1，测量模拟上报的应用字节和线上 WebSocket 数据帧字节。结果不包含 TCP/TLS 开销，也不是整机业务流量节省预测。测试中途主动断开模拟节点和真实 Agent，要求后者自动重新建立压缩连接。记录 Console/夹具内存和每轮耗时，不将短时负载等同长期生产稳定性或计量持久补报验收。

默认复用本机的 `brocade-forwarding-continuity:local`、`cube-qemu-lab:24.04`、`postgres:17-alpine` 镜像；可用对应参数明确覆盖。运行器只清理本轮创建的容器和网络，保留 overlay、日志、输入摘要与失败证据。输出目录包含一次性测试材料，应保持私有，不上传到仓库。

本次优先 RPC 验收额外解码真实 Agent 的压缩帧，只记录方法、路径和计数，不保存 RPC 正文或凭据。要求更新、策略和拨测查询实际通过 RPC，Agent/Xray 发布目标插入和来源策略提交后四秒内出现对应真实工作线程的查询，长于一分钟的运行还要求每个真实 Agent 的 runtime/load 上报走 RPC。发布唤醒夹具使用不可执行的 halted 发布，只验证提交信号和更新查询，不宣称执行了二进制替换。二进制替换的既有安全校验由 Agent 更新测试覆盖。
