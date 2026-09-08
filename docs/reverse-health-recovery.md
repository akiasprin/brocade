# VLESS reverse 健康检查与 NAT 故障恢复

2026-09-08。实现位于 `feat/reverse-health-recovery` worktree；未部署、未合并。基线 `6473d94c` 是原工作区的完整快照，原工作区保持不变。此前官方 main 的复核与方案见 `/mnt/edw/Handbook/brocade-xray-reverse-health-recovery-design-2026-09-08.md`。

## 已实现的行为

每条 reverse worker 使用同一传输内的双向 Ping/Pong，包括有 TCP/UDP 业务时。只有本端获得匹配 nonce 的应答且收到对端验证通知，才进入 READY。周期探测等待期间仍可承载业务；截止时间到达立即隔离为 SUSPECT，派发路径自行检查时间，不能依赖定时器及时调度。确认失败关闭传输；恢复需要两次新应答，过期或重复应答不能复活旧 worker。

默认探测间隔 1000 ms（±10% 抖动），探测截止 750 ms、确认窗口 750 ms、健康租约 3000 ms、建连验证窗口 2000 ms。控制帧使用独立有界队列，在完整业务帧之间优先发送，排队时间计入预算；阻塞写不能阻止故障关闭。初次验证使用完整建连窗口以容纳 TLS/REALITY 握手。

Bridge 维护至少 2 条健康隧道，忙时补 1 条备用，空闲 READY 上限 2，每对同时验证上限 2，节点同时建连上限 32。健康池目标上限 32；失败使用带抖动指数退避，最大 2 秒，首个缺失主连接优先于其他 pair 的备用。DRAINING 停止接新业务并等待在途业务退出。长期持有业务的 DRAINING worker 不受健康池目标上限约束；大量主动轮换下的总存活 worker 容量仍需压力验证。

TCP 故障后关闭旧连接，由应用重新连接，不重放字节或承诺迁移已有 TCP 流。UDP 故障后销毁旧 association，新 worker 建立新 association；reverse 不复用跨 worker 的 XUDP 后端状态，防止旧回复串入新代。覆盖同一 UDP socket/source port 跨故障继续发送、多个目标端口、代际载荷隔离与双向关闭。

修复了 bridge State 读写、会话发布早于链路绑定、启动 goroutine 早于配置发布、共享 inbound 元数据修改等竞态，以及无可用 outbound 时派发不返回的错误路径。Close 取消启动、探测、重连与 canary，不能延迟复活。

## 配置与可观测性

完整策略从 ModelSettings 全局默认或 `(chain, from, to)` 定向覆盖，经过编译器生成到 reverse 两端。覆盖采用完整策略，不逐字段继承；发布验证拒绝无对应 reverse 边的覆盖。数据库增量迁移为 `0002_reverse_health.sql`。控制台设置页可编辑策略。

StatsService 新增 GetReverseHealthSnapshot 和 WatchReverseHealth，CLI：

```sh
xray api reversehealth --server=127.0.0.1:10085
xray api reversehealth --server=127.0.0.1:10085 -watch -timeout=60
```

快照包含 boot ID、序号、worker 状态与原因、RTT、ACK 年龄、会话数、受影响会话、控制队列与调度延迟，以及最近 256 条状态事件。worker ID 使用字符串避免 JavaScript 精度丢失。QueryStats 暴露固定 portal/bridge 角色标签的累计计数和 RTT 桶，名称前缀 `reverse>>>`；单项 GetStats 不提供这些虚拟计数。状态日志为 INFO，默认 warning 日志级别下应使用 API 或显式开启 INFO。

业务 canary 通过真实 reverse 路由访问配置的 HTTP(S) 探测地址，单次超时 750 ms、每秒探测，单独报告 UNKNOWN/AVAILABLE/FAILED。连续 20 次成功且跨度至少 10 秒后回溯标记稳定开始时间。隧道 READY 不代表目标应用可用；canary 失败也不直接杀死健康隧道。

Agent 当前采用每秒读取本地快照的轮询路径；管理上报沿用 1/2/5 秒采样配置。节点详情页显示隧道和业务状态，断连、无数据或超过 15 秒未更新显示 UNKNOWN。Watch 服务端每 100 ms 检查变更并周期发送完整快照，但 Agent 尚未消费 Watch，不能宣称 Console 已达到 100 ms 或 p99 ≤2 秒。事件尾部有界，当前 UI 未提供事件缺口告警。API registry 为进程级；快照为实时尽力读取，不是跨 worker 原子事务。

## 故障实验与证据解释

可复现实验：Docker 隔离网络内 bridge/NAT/portal 三个 network namespace，真实 conntrack 删除与 SNAT 地址切换，并对旧五元组注入单向/双向 DROP。链路 RTT 150 ms。脚本 `scripts/reverse-review/run.py` 可选 RAW、TLS、REALITY、循环次数和 `--race`；保留配置、故障时刻、conntrack、逐次 TCP/UDP 样本、健康快照和二进制/源码哈希。

```sh
python3 scripts/reverse-review/run.py --case change_ip_silent \
  --transport raw --transport tls --transport reality \
  --rtt-ms 150 --window 35 --race --output target/reverse-health-race-verified
```

最终竞态修复后的真实进程 `-race` 三种传输均通过，未报告 DATA RACE。双向静默 NAT 换 IP，固定 UDP socket 持续发送：

| 传输 | TCP 稳定恢复 | UDP 稳定恢复 | 旧 TCP 结束 |
| --- | ---: | ---: | ---: |
| RAW | 2.132 s | 3.287 s | 2.289 s |
| TLS | 2.138 s | 3.304 s | 2.491 s |
| REALITY | 2.136 s | 3.294 s | 2.452 s |

上述每种仅 1 次。portal 事件显示故障后隔离 SUSPECT 为 0.701–1.798 秒，DEAD 为 1.450–2.523 秒。旧 TCP 的客户端观察时间和 portal 状态时间来自不同端点，不能等同。恢复以业务探测的连续成功尾部判定，探测间隔约 0.5 秒会引入采样量化；UDP 超时与 race 插桩也影响测量。

开发阶段的 12 场景矩阵（4 故障 × 3 传输）全部通过：同 IP 删表 TCP 0.172–0.678 s、UDP 0.322–1.225 s；换 IP 允许 RST TCP 1.147–1.667 s、UDP 1.052–1.848 s；双向静默 TCP 2.128–2.146 s、UDP 1.793–2.545 s；仅下行静默 TCP 2.170–2.650 s、UDP 1.825–2.572 s。该矩阵 UDP 每次使用新 socket，后续另有固定 socket 的 6 场景验证。矩阵在最后两项竞态修复前运行，不能冒充完全相同源码的最终构建。

100 轮 RAW / RTT 150 ms / 双向静默换 IP / 固定 UDP socket 全部恢复：TCP 中位数 2.154 s、经验 p95 2.653 s、最大 2.706 s；UDP 中位数 2.574 s、经验 p95 3.333 s、最大 3.407 s。这批构建位于最后两项竞态修复之前；修复后另执行上表三传输真实进程 race 验证。完整数值见 `reverse-health-experiment-summary.json`。

更早官方 main 双向静默换 IP 单轮约 593.601 秒，单向静默约 453.206 秒；拓扑与负载细节不同，因此这里是恢复量级对照，不是严格控制变量的性能倍率。

第一次真实进程 race 实验虽然业务恢复，但抓到配置发布和 inbound 元数据竞态，判定失败，修复后重跑得到上表。缺少 sysctl、旧 TLS 参数等实验环境失败不计入通过样本。

## 验证与交付边界

Go 相关包 `go test -race`、真实进程三传输 race NAT 实验、TCP/UDP 代际与截止时间回归；Rust 核心/Agent/部署测试；真实 PostgreSQL 设置往返与校验、两端编译配置测试、本地 Xray gRPC 健康 API 集成；前端 39 个测试文件 238 项及生产构建已执行。最终 Console 单元测试 83 项、Core 31 项、Deployment 26 项通过；Agent 215 项通过，另运行平时忽略的本地 Xray gRPC 集成 1 项通过。Frontend 238 项重跑通过，生产构建通过（已有 chunk 大小警告），修改文件 ESLint 通过。最终 Go 生产源码哈希与三传输 race 验证一致，之后仅新增 shared inbound 回归测试并通过 race。具体 100 轮记录见同目录实验摘要和 Handbook 交付记录。

协议新增健康帧及验证/排空通知，默认启用，无旧版本回退。必须成对升级 portal/bridge；旧端点无法完成 READY 验证。先在测试 pair 发布，再灰度完整 pair，观察 canary、超时、RTT、拒绝派发与重连；回滚也应成对切回匹配二进制和旧配置。不要只更新单端。未发布到生产。

尚未完成 1000 轮 p99 验收、100000 worker-hour 误杀率、极端丢包/高 RTT、高并发容量和管理网络端到端延迟门槛。默认短超时适合本次 RTT 150 ms 环境；高 RTT 场景需显式扩大策略并重新验收。本文不把有限实验解释为生产恢复时间上界。


## 配置补全与设置页位置（2026-09-08 后续实现）

设置入口为 **设置 → 连接策略 → 反向隧道**。基础 9 项及新增高级 11 项一起使用“保存这一段”，加入同一份草稿，再发布到两端。定向 `(chain, from, to)` 覆盖也在此处维护。保存其他设置段会保留已写入草稿的反向隧道策略；只读用户不能编辑。

高级参数位于 `reverse_health.tuning`（定向覆盖则为 `health.tuning`）。旧策略没有 tuning 时，整组采用下表默认值；一旦指定 tuning，必须完整提供该组，不逐字段继承。已有 JSONB 数据无需数据库结构迁移，旧数据读取后自动补齐默认组，保存时写出完整策略。生成的 Xray JSON 两端一致。

| 参数 | 含义 | 默认 | 范围 |
| --- | --- | ---: | --- |
| `probe_jitter_percent` | 探活间隔抖动（百分比） | 10 | 0–50 |
| `recovery_successes` | 恢复所需连续应答次数 | 2 | 1–8 |
| `spare_workers` | 忙时备用隧道数 | 1 | 1–8 |
| `max_healthy_workers` | 健康隧道总数上限 | 32 | 1–32 |
| `max_sessions_per_worker` | 每条隧道业务并发上限 | 16 | 1–256 |
| `reconnect_backoff_base_ms` | 重试退避起点（毫秒） | 250 | 50–30000 |
| `reconnect_stable_reset_ms` | 稳定后重置退避（毫秒） | 10000 | 1000–300000 |
| `canary_interval_ms` | 业务探测间隔（毫秒） | 1000 | 100–60000 |
| `canary_timeout_ms` | 业务探测超时（毫秒） | 750 | 50–30000 |
| `canary_successes` | 业务稳定所需连续成功次数 | 20 | 1–1000 |
| `canary_stable_window_ms` | 业务稳定最短观察期（毫秒） | 10000 | 0–300000 |

关联校验：健康租约必须覆盖最大抖动后的间隔加探测超时；备用数 ≤ 空闲上限 ≤ 健康总数上限；退避起点 ≤ 退避上限；canary 超时 < canary 间隔。确认阶段的多个应答仍需在同一个确认窗口内完成，调整成功次数时应同时考虑 RTT 和确认预算。

高级参数直接作用于健康状态机、bridge 容量/退避、portal 业务并发及 canary 定时器/稳定判断。canary 结果有效期随配置变为 `max(15 秒, 2 × canary 间隔 + canary 超时)`；管理采集自身仍使用独立的 15 秒过期判断，不能用较长的业务周期掩盖失联。

仍固定的内部边界已明确：双端探测和双向验证不能关闭；控制队列容量 8、状态事件尾部 256、健康调度 tick 25 ms、bridge 兜底扫描 100 ms、进程级建连并发保护 32；Agent 本地采集间隔 1 秒、管理断连过期 15 秒。它们目前不是此链路策略的可调项，不能通过多个链路覆盖修改进程级边界。启动延迟 2 秒和单 worker 累计 session 上限 4096 也保留原行为。

复现实验可传入自定义策略文件：

```sh
python3 scripts/reverse-review/run.py --case change_ip_silent \
  --case downstream_silent --transport raw --transport tls --transport reality \
  --rtt-ms 150 --window 35 --race --health-policy docs/reverse-health-tuning-example.json \
  --output target/reverse-tuning-verified
```

新增回归覆盖配置兼容与完整性、抖动范围/零抖动、自定义恢复应答次数、容量/退避、canary 连续成功与时间窗口、较长业务周期的 UI 有效期、设置页位置、统一保存、跨设置段草稿保留和无效参数禁止保存。前文恢复结果属于前一提交的实验，不作为任意自定义参数下的恢复承诺。


配置补全后的最终验证：Go 相关包 race 回归通过；Core 完整测试（含 20 项真实 Xray 二进制测试）通过；Agent 215 项、Deployment 26 项通过；显式启用 Docker 的 PostgreSQL 设置往返通过；Frontend 39 文件、241 项通过，生产构建及修改文件 ESLint 通过。Core 第一次执行因 `.tools` 缺少 geodata 资源而失败，补齐忽略的测试资源后完整重跑通过；未把跳过数据库的初次运行计为真实 PostgreSQL 验证。

自定义策略见 `reverse-health-tuning-example.json`，包含 1500 ms 探测间隔、20% 抖动、3 次恢复应答、2 条备用、6 条健康上限、每 worker 4 条业务并发、100 ms 退避起点、2 秒 canary 周期。RTT 150 ms，真实进程 `-race`，固定 UDP socket，以下 6 项均通过且无 DATA RACE：

| 场景 / 传输 | TCP 稳定恢复 | UDP 稳定恢复 |
| --- | ---: | ---: |
| change_ip_silent-raw-0 | 3.074 s | 3.308 s |
| change_ip_silent-reality-0 | 2.147 s | 3.297 s |
| change_ip_silent-tls-0 | 2.300 s | 2.541 s |
| downstream_silent-raw-0 | 2.937 s | 3.316 s |
| downstream_silent-reality-0 | 1.730 s | 2.576 s |
| downstream_silent-tls-0 | 2.179 s | 3.346 s |

每场景仅 1 次，不能视为分位数或上界。测试输出 `target/reverse-tuning-verified` 保留实际两端策略、故障注入记录和源码哈希，生产 Go 源码与该次测试逐文件一致。
