# 清理节点 Agent

`scripts/cleanup-agent.py` 用于卸载 Linux/systemd 节点上的 Brocade Agent，并清理已确认归属的网络残留。需要 Python 3.11+、iproute2、nftables；检查 iptables 规则时自动使用本机存在的 IPv4/IPv6 与 legacy 工具。清理 Cube BPF 时还需要 `bpftool` 和 `tc`。脚本可单独复制到节点执行，无需安装仓库依赖。

先执行只读预览：

```sh
sudo python3 scripts/cleanup-agent.py
```

确认清单后执行：

```sh
sudo python3 scripts/cleanup-agent.py --apply --report /root/agent-cleanup.json
```

默认保留凭据、状态、日志、Cube 运行栈和 Xray 二进制。需要全部移除时，先在 Console 撤下资源、排空实例和磁盘，再使用：

```sh
# 先用相同参数预览，再追加 --apply。
sudo python3 scripts/cleanup-agent.py --cube --remove-xray --purge-data
sudo python3 scripts/cleanup-agent.py --cube --remove-xray --purge-data --apply \
  --report /root/agent-full-cleanup.json
```

`--purge-data` 永久删除选定的凭据、状态、专用日志和 CVM 数据，不自动创建备份。脚本发现仍在运行的 Cube Guest/Shim 或未知挂载时会拒绝相应清理。默认不会停止或卸载独立的 containerd、Docker，也不会卸载共享内核模块、重置系统 sysctl 当前值、清空全局 conntrack 或修改 Console 数据库。控制面退役仍按 [CVM 宿主清理](cvm.md)完成。

## NAT 和防火墙边界

- 删除 Agent 使用的专属 nftables 表，包括 `brocade_cvm_entries`、`brocade_hy2_nat`、`brocade_entry_phantun` 及相关组网、隔离、计量与链路规则。动态 NIC、线路和 bridge 表还核对归属注释。
- 从共享表中逐条删除带有精确 `brocade-entry-phantun` 标记的规则，保留链、默认策略和其他规则。
- 支持 iptables-nft 与 legacy、IPv4 与 IPv6。重复执行时已经消失的规则和接口按实际状态处理。
- 不执行 `nft flush ruleset`、`iptables -F`、清空共享 `nat/filter` 表或恢复整机规则快照。

旧 CubeRouter 的 MASQUERADE 和 FORWARD 规则没有归属注释。必须显式提供原 NAT IPv4 地址；脚本只删除源码中定义的五种精确规则形状。地址从原 Cube 网络配置或卸载前的只读规则清单获取，不能猜测宿主出口地址：

```sh
# 此处是文档示例，替换成该节点原来的 Cube NAT 地址。
sudo python3 scripts/cleanup-agent.py --cube --cube-nat-ip 198.18.64.2
sudo python3 scripts/cleanup-agent.py --cube --cube-nat-ip 198.18.64.2 --apply
```

原规则中的路由接口不是 `cube-router` 时追加 `--cube-router 名称`。可以重复传入 `--cube-nat-ip` 清理多份旧配置的残留；没有明确地址时保留这些无注释规则。

VPN Gate 的旧共享 MASQUERADE 规则也没有注释。核对原命名空间和 veth 后，用 `--namespace`、`--interface` 选定资源，再用 `--nat-source` 明确选定原 SNAT 源，例如：

```sh
sudo python3 scripts/cleanup-agent.py --namespace brocade-vg-0001 \
  --interface bvg0001h --nat-source 169.254.128.2/32
```

旧版若使用整个 `/30` 而非 peer `/32` 作为源，按实际规则额外传入对应 CIDR。此参数支持双栈，源范围不得宽于 IPv4 `/24` 或 IPv6 `/64`；只删除精确的 POSTROUTING/MASQUERADE 形状。上面的地址及槽位都是示例，不能直接套用于其他节点。

## 无标签接口和命名空间

带有 Brocade alias 的接口自动识别；NIC 命名空间还检查内部接口归属，HY2 根据规范状态路径计算身份。脚本不会按 IP 形状猜测 TAP 归属，也不会自动删除没有标签的 `wg0`、`cube-dev`、`cube-router` 或旧数字命名 TAP。

确认归属后通过可重复的 `--interface`、`--namespace` 显式选定；这也适用于先前卸载已丢失所有权标签的残留。命名空间中仍有进程时会停止并报告，不强行删除未知进程。例如：

```sh
sudo python3 scripts/cleanup-agent.py --cube \
  --interface cube-dev --interface cube-router --interface 192.168.0.2 \
  --cube-nat-ip 198.18.64.2
```

自定义安装可通过 `--state-dir`、`--config-dir`、`--bin-dir` 指定原路径；数据目录必须包含以 `brocade` 开头的目录名，各目录不能互为父子或使用符号链接。只读预览不会停止服务、修改规则、卸载挂载或删除文件；显式使用 `--report` 时会新建权限为 `0600` 的 JSON 报告，已有文件不会被覆盖。报告包含开始/结束 UTC 时间、单调耗时、执行清单和结果。错误以非零退出码返回，排除错误后可重跑。

## 验证脚本

```sh
python3 -m unittest discover -s scripts/tests -p test_agent_cleanup.py
BROCADE_RUN_CLEANUP_NETNS_TESTS=1 \
  python3 -m unittest discover -s scripts/tests -p test_agent_cleanup.py
```

第二条命令在独立用户及网络命名空间内建立真实双栈 NAT 和混合归属规则，验证清理和重复执行；不会调用宿主的 systemd 停机或文件删除流程。
