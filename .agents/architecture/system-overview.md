# 系统结构与边界

## 主链路

```text
ModelSnapshot -> IR -> 节点产物 -> 期望状态 -> Agent 收敛
```

`brocade-core` 把完整模型快照纯编译成确定性产物；Console 通过 Store 保存草稿、不可变修订和发布记录；Deployment 定义 Console 与 Agent 共用的计划和协议；Agent 持续比较期望状态与本机实际状态，并以幂等操作收敛。

## 层次与所有权

| 层 | 权威状态 | 不应承担的职责 |
| --- | --- | --- |
| `brocade-core` | 模型、IR、确定性节点产物 | 数据库、文件系统、网络或外部服务访问 |
| `brocade-store` | 草稿、修订、发布、授权、凭据、配额和用量 | HTTP 表示和节点侧执行 |
| `brocade-console` | 管理端与节点端 API、实时通道、证书、内嵌资源 | 本机系统配置收敛 |
| `brocade-deployment` | 发布计划、热切换、Console/Agent 共享协议 | 数据库持久化和 UI 状态 |
| `brocade-agent` | 本机期望状态、实际状态、收敛结果、离线缓冲 | 编辑模型或重写发布历史 |
| `brocade-probe` | Console/Agent 共用的临时 Xray 拨测 | 长期运行的数据面服务 |
| `frontend` | 操作者交互、草稿工作区、发布与观测呈现 | 建立第二份服务端权威状态 |
| `components/xray-core` | Brocade 数据面 fork | 控制面业务规则 |

## 跨层数据流

```text
frontend -> brocade-console -> brocade-store
                         \-> brocade-deployment -> brocade-agent
brocade-core -> artifacts -> embedded Brocade Xray
```

变更跨越边界时按数据流检查调用方和契约，而不是只修改最先报错的一层：

- 后端响应变化：检查 `frontend/src/api.ts`、页面调用方和前端契约测试。
- 协议形状变化：检查 `brocade-deployment`、Console、Agent、协议版本和兼容测试。
- 编译产物变化：检查确定性排序、Golden 产物、订阅格式和嵌入式 Xray。
- 数据库变化：检查事务边界、约束、并发语义、越权测试和公开配置。

## 发布域

Brocade 有多个不能混为一谈的发布域：

1. 模型配置发布：以不可变修订生成节点期望状态。
2. 授权发布：更新访问主体，风险和执行代价低于配置发布。
3. Agent 发布：更新节点收敛程序，需要协议兼容和分批推进。
4. Xray 发布：更新数据面二进制，需要摘要校验、真实启动检查和失败恢复。
5. Console 部署：替换控制面二进制；不会自动触发节点 Agent 或 Xray 更新。

各域的操作与回滚规则见 `../runbooks/release-and-rollback.md`。

## 核心不变量

始终生效的不变量保存在 `../AGENTS.md`，包括确定性编译、不可变修订、可审计回滚、Agent 幂等收敛、协议升级和系统权限边界。本页解释结构，不另建一份会漂移的规则副本。
