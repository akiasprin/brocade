# 用户限速真实进程验收

使用独立 Docker 网络与 PostgreSQL，运行显式指定的真实 Console、Agent、Xray。不会连接线上数据库或修改宿主机网络。镜像需要 Python 3 和可运行候选 Console 的 GNU libc；数据库镜像为 `postgres:16-bookworm`。依赖镜像须预先准备，夹具不安装系统软件。

按 `.agents/runbooks/console-deployment.md` 的固定入口准备构建，再运行：

```sh
python3 scripts/tests/user-traffic-limit/repro.py \
  --output /absolute/new-result-directory \
  --console /absolute/candidate/brocade-console \
  --agent /absolute/candidate/brocade-agent \
  --xray /absolute/verified/xray \
  --image prepared-python-linux-image
python3 scripts/tests/user-traffic-limit/verify.py /absolute/new-result-directory
```

输出目录必须尚不存在；产物复制后记录 SHA-256，凭据仅用于隔离实验，私有文件权限为 0600。所有正常与失败出口清理自建容器和网络，保留日志、UTC 时间、单调时钟、逐请求耗时及实际 payload 字节数。中断的实验不得声明成功。

覆盖 1 毫秒小请求、无主动间隔、同机多入口、四机器、80 毫秒预算响应延迟、同池大流量与小请求公平性、用户/app 隔离、TCP/UDP 满载聚合上限、策略热变更、暂停/恢复及控制面故障恢复。验证器要求场景完整且样本非空，分别检查冷请求与已建立连接的延迟，验证聚合吞吐和过期后不绕过上限。限速正常排队不以业务客户端超时掩盖；故障/暂停场景允许并验证预期拒绝。

工程验证标准仍以 `.agents/standards/testing.md` 为准。本夹具只验收指定产物，不替代部署、数据库迁移或版本升级验收。

`many_users.py` 单独回归多个用户池共享续领执行器的长尾：隔离 Docker 中运行实际 Agent，连接健康但预算响应延迟 80 毫秒的模拟 Console，96 个独立用户各每毫秒申请 16 字节；前 5 秒预热后，每个用户必须有至少 100 个样本，所有用户无错误、最长预算等待低于 200 毫秒。该夹具验证 Agent 预算通道，不能替代上面的真实 Console/PostgreSQL/Xray 验收。可传入修复前产物进行对照，并用 `--candidate-first` 反向执行；报告包含逐用户尾延迟、Agent CPU/PSS/RSS 和线程数。

```sh
python3 scripts/tests/user-traffic-limit/many_users.py \
  --output /absolute/new-many-users-results \
  --agent /absolute/candidate/brocade-agent \
  --baseline /absolute/previous/brocade-agent \
  --image prepared-python-linux-image
```
