# Console 部署手册

## 适用范围

本手册用于首次准备 `brocade-console` 主机，以及之后替换 Console 二进制。它不发布模型配置、用户授权、Agent 或 Xray；这些流程见 `release-and-rollback.md`。

部署、重启、数据库备份/恢复和反向代理变更都会修改外部系统。Agent 可以完成只读预检和生成命令，但只有用户明确授权目标环境与操作后才能执行生产变更。

## 前置条件

- 明确目标环境、主机、CPU 架构、域名和维护窗口。
- 具备受控的 SSH、systemd、Nginx 和 PostgreSQL 管理权限。
- 按[构建文档](../../docs/build.md)的前置条件安装 Node.js、Go、Zig、Rust 工具链和目标架构。
- 准备与 `console.env.example` 同步的环境文件；真实秘密只存在于目标机器的受限文件或秘密管理系统。
- 确认数据库备份位置有足够空间，并验证恢复责任人和停机边界。

首次部署的系统账号、PostgreSQL、systemd、Nginx 与 TLS 示例见 `../../docs/manual-deployment.md`。本手册是操作检查表；配置清单的权威来源是 `../../console.env.example`。

## 发布前预检

1. 检查工作树和待发布提交，确认构建来自预期修订。
2. 按 `../standards/testing.md` 完成与改动相称的检查。
3. 构建目标架构的 release 产物，并记录产物 SHA-256、提交和构建环境。
4. 审查是否涉及环境变量、数据库、HTTP API、Console/Agent 协议、内嵌 Agent 或 Xray。
5. 确认旧二进制和数据库备份的保存位置，写下回滚判据。

复制源码制作发布快照时，应给快照独立的 Git 元数据。不要将指向实际仓库的
`GIT_DIR`、`GIT_WORK_TREE` 或 `GIT_INDEX_FILE` 传给测试进程：Git 语义测试会在临时目录
执行 `init` 和 `commit`，继承这些变量会把操作重定向到实际仓库。构建来源追溯与测试都应
使用快照自己的 `.git`，同时保留实际仓库的工作文件、索引和提交状态。

示例构建和摘要：

```sh
cargo build --release --locked --target <target-triple> -p brocade-console
sha256sum target/<target-triple>/release/brocade-console
```

## 部署前备份

在目标机上备份数据库，并保存当前二进制。占位值必须替换为本次发布的唯一标识；不要把真实地址或凭据写回仓库。

```sh
sudo install -d -o postgres -g postgres -m 0700 /var/backups/brocade
sudo -u postgres pg_dump --format=custom --compress=6 \
  --file=/var/backups/brocade/before-<release-id>.dump brocade

sudo install -d -o root -g root -m 0755 /opt/brocade/releases
sudo install -o root -g root -m 0755 \
  /opt/brocade/brocade-console \
  /opt/brocade/releases/brocade-console.before-<release-id>
sha256sum /opt/brocade/brocade-console
```

确认备份命令成功、文件非空，并把旧二进制摘要写入发布记录。涉及不可逆数据库变化时，仅有备份文件不算完成预检；必须在隔离环境验证恢复。

## 数据库兼容

### 迁移链与历史基线

Console 在构建时嵌入 `crates/brocade-store/migrations/` 中的全部前向迁移，并按数字版本
执行；不限制迁移数量，也不要求逐个运行中间版本的 Console。新增数据库变更追加迁移，
命名和不可变性要求见 `../standards/backend.md`。SQLx 继续校验已应用迁移的 checksum，
不会自动修正不匹配的历史记录。当前未发布功能按用户 2026-10-24 的明确要求统一整合在
`0002`，正式迁移目录仅保留 `0001`、`0002`；已执行旧 `0002` / `0003` 的环境须走下述
一次性转换，不能直接替换二进制。

下文兼容步骤描述过去持续修改 `0001_init.sql` 的历史结构转换，不是后续新增功能的默认
发布流程。旧库只有接入经过验证的迁移基线后才能使用追加迁移；放开数量限制本身不提供
任意历史版本自动升级、备份或失败回退。不得仅为通过启动检查而改写 checksum。

### 转发逐条回执（0002）

`0002_forwarding_rule_results.sql` 为 `forwarding_node_state` 增加可空 `rule_results JSONB`
及对象形状约束，不回填或重写已有规则、节点回执和用量。`0001_init.sql` 保持原有字节。
对于 `0001` 校验值匹配的已有库，先备份并验证恢复，再更新 Console；启动时 SQLx 在同一
事务中应用新增字段与约束、记录版本 2，后续启动直接跳过。不需要手工兼容 SQL，也不改写
版本 1 的 checksum。之后独立更新 Agent v41，此项不需要更新 Xray。

迁移失败时本条 DDL 和版本 2 记录一起回滚；锁或语句超时须排除原因后再启动重试。迁移
完成后，只认识版本 1 的旧 Console 会拒绝版本 2 的历史，回退构建须携带相同迁移链且兼容
新增列。不得通过删除迁移记录、删除字段或覆盖升级后新用量来回退。

### 隧道目标域名解析（整合于 0002）

`0002_forwarding_rule_results.sql` 包含 `external_outbounds.target_strategy` 可空字段和取值约束。
存量行保持 NULL，旧快照缺失字段时保持原有解析行为；不重写隧道凭据、模型修订、授权或节点配置。
页面选项仍需保存到草稿并发布配置才影响节点。回退代码必须保留已保存字段的数据与编译语义。

### 已部署旧 0002 / 0003 的合并转换

按用户明确要求，原目标地址族策略 `0003` 和移除转发总额度的 `0004` 已整合进未发布 `0002`。
`0001` 字节不变；原 SQL 仅在测试夹具和既有发布证据中留存，不再进入正式迁移链。
150 已执行旧 `[1,2,3]`，它的历史不会因源码合并自动改变；直接启动新二进制仍会被 SQLx 拒绝。

`scripts/prepare-consolidated-0002-compat.py` 只接受精确核验的历史：`0001` 为
`794044a4…0947`、旧 `0002` 为 `375a1d3c…ab71a`，以及可选旧 `0003` 为
`1ef92edf…4c13`。目标 `0002` 为 `54300599…552f1`。不接受任意旧库或其它 checksum。

先保存数据库、原迁移回执和二进制，在隔离恢复库演练。生成 SQL 后，以实际应用 owner 执行：

```sh
python3 scripts/prepare-consolidated-0002-compat.py \
  --migration crates/brocade-store/migrations/0002_forwarding_rule_results.sql \
  --output <release-dir>/consolidate-0002.sql
psql -X -v ON_ERROR_STOP=1 -v app_role=<应用角色> -d <恢复库> \
  -f <release-dir>/consolidate-0002.sql > <release-dir>/consolidate-0002.log
```

仅安装上述旧 `[1,2]` 的库，生成时加 `--without-target-strategy`；默认接受 `[1,2,3]`。
SQL 从完整目标 `0002` 提取尚未执行的 DDL，验证接入配额和预算原样保留、转发总额度退役完成，
再在同一事务内将版本 2 checksum 对齐到目标并移除版本 3 回执；原始完整回执输出到转换日志。
版本 1 与版本 2 的其它元数据保持不变。5 秒锁等待和 120 秒语句超时，任何失败整笔回滚。
不允许单独删除版本 3 回执或只修改 checksum。

上线时先停止 Console 写入，执行与演练完全相同的文件并保存输出，再以候选运行
`database-check`，确认已安装 `[1,2]`、无待执行迁移，随后切换已验证二进制。此次源码合并本身
不执行生产转换。旧总额度对象已退役，原二进制不能直接作为回退包；须准备保留当前结构和行为的
兼容回退构建。不得恢复整库覆盖新写入的计费数据。

### 未发布版本 0002 的用户账期兼容

本次用户明确要求未发布版本仍合并在 `0002`，并手工兼容已经运行过旧版 `0002` 的环境。
这是针对已核验字节的一次性转换；后续迁移继续遵守已发布文件不可变规则。
`scripts/prepare-user-service-compat.py` 只接受旧版 SHA-384 `201b4ff0…acd0643` 的完整
前缀和目标 `54300599…552f1`，从目标文件提取早期前缀之后的全部 DDL（包含账期、接入超额处理、
来源准入、目标地址族及转发总额度退役），避免另存一份结构定义。此路径只适用于该早期前缀，
不是上述 `375a1d3c…ab71a` 已部署库的转换方式。

1. 保存原迁移记录、二进制、配置与数据库备份，在隔离恢复库验证恢复。
2. 生成 SQL，先对恢复库以实际应用 owner 执行，核对与全新完整 `0002` 的结构一致、用户原字段、
   账本、计数头及迁移记录元数据不变，再用候选二进制运行 `database-check`。
3. 准备兼容完整目标 `0002` 结构和当前额度行为的回退包，验证它能通过恢复库检查及接口验证。
   依赖已退役转发总额度对象的旧代码不能直接使用；回退代码也须保留已启用的服务有效期与目标策略约束。
4. 核验生成 SQL 与演练摘要一致后，在目标库执行同一文件。事务以有界锁等待锁定 `users`，
   拒绝未知历史、部分执行和重复执行；验证新增结构、owner、默认值及旧用户原字段后，才在同一
   事务内对齐版本 2 的 checksum。版本 1、版本 2 的其它元数据与原始备份均保留。
5. 用候选再次执行 `database-check`，切换 Console 并验证接口、用户页和正常记账。失败时保留
   现场，未知执行结果先检查事务是否提交，不得盲目重放或只改 checksum。

```sh
python3 scripts/prepare-user-service-compat.py \
  --migration crates/brocade-store/migrations/0002_forwarding_rule_results.sql \
  --output <release-dir>/user-service-compat.sql
psql -X -v ON_ERROR_STOP=1 -v app_role=<应用角色> -d <恢复库或目标库> \
  -f <release-dir>/user-service-compat.sql
```

该路径不创建 `0003`，不放宽 Console 的迁移校验，不删除历史用量。旧的原字节二进制在
checksum 对齐后不能直接启动，应使用已验证的回退包；不得整库恢复覆盖兼容之后的新计费数据。

### 校验范围精简

`scripts/compat-validation-limits.sql` 将 geodata cron 的五段形状限制改为非空检查，
并将 REALITY 时间容差上限从一天改为 Xray signed duration 能表示的
`9223372036854` 毫秒。cron 语法由实际 Xray 解析器处理；更大的时间容差仍会导致
Go duration 溢出，不应接受。

该脚本可重复执行，只替换两个命名 CHECK，不改配置值、修订、计费状态和迁移记录。
先在隔离恢复库执行并核对数据；停写及备份后，以应用角色执行：

```sh
psql -X -v ON_ERROR_STOP=1 -v app_role=<应用角色> -d brocade \
  -f scripts/compat-validation-limits.sql
```

脚本不自动更新 SQLx checksum。核对目标 `0001_init.sql` 的其余差异均已兼容后，再按
本手册既有流程单独更新 version 1 的 SHA-384；仅执行 ALTER 并不能消除二进制的迁移
checksum 差异。回退旧二进制前先将 `@daily` 等新 cron 形式恢复成旧版接受的五段表达式，
并恢复旧 checksum；超过一天的时间容差也要先调整回旧版本能接受的值。

### 首次上线向导

新安装在创建系统管理员时会同时写入 `first_run_onboarding` 单例，随后由向导纳管首台机器、
创建链和首个用户，并以真实授权拨测完成。初始化不再创建固定的 `zero` 用户。

既有环境在停写、备份并于隔离恢复库验证后，以应用角色执行：

```sh
psql -X -v ON_ERROR_STOP=1 -d brocade \
  -f scripts/compat-first-run-onboarding.sql
```

脚本只建表、不插入单例；缺少单例明确表示“不适用”，因此升级不会把已经运行的 Console
重定向进入向导，也不会删除历史上已有的 `zero`。核对表、外键和检查约束，并确认本次
`0001_init.sql` 的其它差异都已兼容后，再按目标二进制更新 version 1 的 SQLx SHA-384。
旧 Console 会忽略该表；回退时可以保留，但须恢复旧 checksum。

### 发布工单阶段时间

发布详情的步骤时间来自控制面阶段切换，不再复用 Agent 领取目标时写入且会随租约刷新的
`deployment_target_state.dispatched_at`。新表 `deployment_wave_progress` 按工单和 wave 只保存
一条 `entered_at`：第一阶段使用工单创建时间，后续阶段在前一阶段关闭并使其成为当前阶段
的同一事务内写入。等待人工确认也已经进入该阶段，因此确认和节点领取都不能改写时间。
终态仍以 `deployments.finished_at` 作为「结束」节点。

已有环境部署前停止 Console 写入、备份数据库和旧二进制，并记录 version 1 migration
checksum；先在生产备份的隔离恢复库验证，再以应用角色执行：

```sh
psql -X -v ON_ERROR_STOP=1 -d brocade -v app_role=brocade \
  -f scripts/compat-deployment-wave-progress.sql
```

脚本只新增表并回填历史：首个有效 wave 取工单创建时间，有确认记录的 wave 取确认时间；
缺少控制面事件的旧自动 wave 才以首次 Agent 领取时间作近似。脚本故意拒绝重复或部分执行。
核对表、主键、外键、检查约束、所有权和回填结果，并审查本次 `0001_init.sql` 的其它差异
都已兼容后，才按目标二进制更新 version 1 的 SQLx SHA-384。旧 Console 会忽略新增表；
回退旧二进制时可以保留该表，但必须恢复旧 checksum。无需升级 Agent 或 Xray。

### 用户机器流量

用户统计页的机器 × 时间矩阵使用新增 `usage_user_node_rollups` 保存用户、入口机器的日汇总，
与已接受的接入记账窗口在同一事务中累加。转发复用已有日汇总；不修改计费、Agent/Xray 或协议。
当天/当周从保留的用量明细按小时读取，当月按日读取，均沿用 UTC+8 和窗口起点的账本归属。

停写并备份后，以应用角色运行 `scripts/compat-user-machine-usage.sql`；先在生产备份的隔离
恢复库验证。脚本从尚存的接入明细回填，故意拒绝重复执行，不修改 SQLx checksum。
核对 owner、主键/外键、每日已归属总量不超过原账本，以及未按机器记录的差额；
只有审查目标 0001 全部差异后，才能设置与目标二进制相同的 SHA-384。
历史差额不能均摊到机器或小时；缺失上报证据不能解释为零流量。

回退旧 Console 时可保留新增表并恢复对应 checksum，但旧版本会停止维护此投影。
再次启用前须停写并核对中断期间的明细是否仍在保留期内，按缺失区间补齐；
无法补齐的部分继续作为未按机器记录。不能恢复旧数据库覆盖新账单。

### 机器流入统计

来源表格明细另需新增观测表：停写、备份并在隔离恢复库验证后，以应用角色执行
`scripts/compat-source-traffic.sql`。脚本创建来源窗口、来源日汇总、采集状态及时段查询索引，不回填历史 IP，
拒绝重复执行，也不修改 SQLx checksum。审查本次 `0001_init.sql` 的所有差异后，沿用下述 checksum
核对步骤。先升级 schema/Console，再升级 Agent/Xray；Console 通过 `x-brocade-source-traffic: 1`
声明接收能力，新 Agent 连接旧 Console 时停止该观测，新 Console 连接旧 Agent/Xray 时保留未归属合计。
旧用量统计和计费不依赖此来源观测，回退旧二进制可以保留新增表。

机器详情「流量」页签的流入统计使用新增 `usage_user_ingress_rollups`：按机器、+08 日、用户与入口
累计接入流量，在接受记账窗口的同一事务中写入，并冻结写入时入口所属的接入链。同时为
`user_site_usage_hourly` 增加 `(node_id, bucket)` 索引，供流出统计读取单台机器。端口转发复用
已有日汇总；不修改计费、Agent/Xray 或协议。

机器用量「流出统计」也以独立「端口转发」类型读取该账本，与站点统计在同一只读快照中汇总。
转发总量和主要用户读取日汇总，当天/当周的小时分布读取保留明细并按窗口起点归属（首次读数用
读取时间），当月读取日汇总。已清理的小时明细不均摊，页面提示未分配到图中的账本流量。
站点采集开关、上报覆盖与观测起点只描述认证代理入口；转发账本独立显示，无需额外 schema 或节点升级。

停写并备份后，以应用角色运行（先在生产备份的隔离恢复库验证）：

```sh
psql -X -v ON_ERROR_STOP=1 -d brocade -v app_role=brocade \
  -f scripts/compat-user-ingress-usage.sql
```

脚本从尚存的接入明细回填，故意拒绝重复执行，不修改 SQLx checksum。核对 owner、主键/外键、
索引，以及每日接入链汇总不超过 `usage_user_node_rollups`；明细保留期以外的差额在页面上显示为
「未按接入链记录」，不能均摊到接入链。只有审查目标 0001 全部差异后，才能设置与目标二进制相同的
SHA-384。

回退旧 Console 时可保留新增表与索引并恢复对应 checksum，但旧版本会停止维护此投影；再次启用前
须停写，从仍在保留期内的明细补齐中断区间，无法补齐的部分继续作为「未按接入链记录」。

### 用户实时流量

此功能不改变数据库或计费。先部署 Console，再独立发布 Agent v27；未升级的机器显示
未支持，既有实时网卡和 30 秒用量上报继续正常。只批准 Console 部署不会升级机队。
验证系统/租户管理员的机器与用户卡片、并发订阅隔离、普通用户/访客禁止获取、断线和
计数器回退。旧 Agent 忽略可选选择器，新 Agent 兼容旧 Console 的无选择器命令。

### 转发统一统计兼容

用户总量、用量页每日/月累计、机器 XRAY 和用户归属预测同时纳入转发；按线路的订阅
额度仍只计算该线路接入。物理网卡账本已包含转发，禁止再叠加逻辑字节。统计不改变
ForwardingRuleDesired、不创建配置/授权发布、不重启 Xray。

已有转发数据库先停止 Console 写入并备份，以应用角色执行
`scripts/compat-forwarding-traffic.sql`。`schema_file` 须来自目标 `0001_init.sql` 的
`BEGIN FORWARDING TRAFFIC PROJECTIONS` / `END FORWARDING TRAFFIC PROJECTIONS` 标记之间。
脚本新增计数序号、收据指纹、窗口、历史机器归属与日汇总，并以原始事实回填既有总量；
未知旧区间保持 NULL，不伪造零覆盖或秒速率。核对用户/机器/规则和每日总量一致、
所有权与其余 schema 差异后，单独更新 migration checksum。

顺序为数据库兼容 → Console → 独立批准 Agent 29 灰度；无需更新 Xray。旧 Agent
继续记转发用量，新字段默认缺省，界面明确提示转发实时待升级。新 Agent 每轮按需
读取非重置 inbound 转发计数并与接入速率共同报告；仍仅管理员可见。实时授权由
Console 解析，不写进转发配置；SSE 授权刷新同步撤销与移机的选择范围。
回滚保留事实、汇总和计数头，不能整库恢复覆盖回滚期间的新用量；回退 Console
前恢复对应 checksum，回退 Agent 后只失去新增转发实时能力。

### 配置重启的连续用量

Agent v28 与对应 Xray 共同保留用户 / 转发累计计数，受管 Linux 数据面以私有
`xray-accounting.bin` 共享映射承载计数，不再在进程替换时清零。字典上限 16,384 项，
主文件约 8 MiB；从不复用已存在标签的槽位。计数注册或落盘失败不允许新连接绕过统计；
Agent 采集 / spool 失败阻止破坏性的变更，满队列保留旧检查点而不是删除。
splice 按每次完成的内核传输递增，不再等到 64 MiB 或连接结束才入账。

先部署 Console，再独立发布 Agent 28，最后灰度对应 Xray。旧 Agent / Xray 保持原路径；
只有两者均升级且 journal 生效后，才具备跨进程计数连续性。第一次退出旧 Xray 仍没有
其内存外的尾部副本，不能声称此次升级恢复了历史丢失数据。新旧 Console 最低协议仍为 20。

用量协议可选零窗口表示已经确认停止的区间，不是把累计计数归零。零区间与真实窗口起点
随既有用量回执 JSON 持久化，不增加表或迁移；历史响应保留旧 points 与结束时间字段。
用户流量图按真实窗口的平均速率重采样，确认停机为 0，未知离线仍为空缺，不改写计费。
异常退出无法凭最后联系时间猜零，断电 / 磁盘损坏也不承诺逐字节持久性。

禁止在进程运行时删除、截断或替换 journal。回退到旧 Xray 后再次升级时，Agent 为中断的
连续性保留一份 `xray-accounting.previous.bin`，并创建新计数身份，避免旧绝对值重复计费。
该副本不是可直接覆盖回去的回滚入口；二进制回滚仍创建新的审计发布单。
验证真实小流量长连接、退出前未采集尾部、重试去重、长期停用后重启、零窗口与未知空缺。

### 机器流量配额与容量预测

用量页的机器容量模型在 `node_traffic_policy` 增加可空的 `quota_bytes NUMERIC(20,0)`；
NULL 表示尚未配置机器供应商配额，正整数表示当前重置周期的固定字节额度。预测读取已有
物理网卡日账本、用户日账本和当前授权拓扑，不改写历史数据，也不需要升级 Agent/Xray。

已有环境部署前停止 Console 写入、保存数据库与旧二进制，先在隔离恢复库验证，再以应用
角色执行 `psql -v app_role=<应用角色> -f scripts/compat-traffic-capacity.sql`。脚本只扩列与约束，
故意拒绝重复或部分执行。
核对列、约束和所有本次 `0001_init.sql` 差异后，才按目标二进制更新 version 1 的 SQLx
SHA-384；不能只为绕过 checksum 修改迁移表。旧 Console 会忽略该列；回退旧二进制时
可以保留列，但必须恢复与旧二进制匹配的迁移 checksum。

上线后以系统管理员验证设置/清除配额、P50/P90 风险状态和用户贡献展开；以只读管理员
验证只读视图，以自助用户和 public 验证既不显示入口也无法读取 `/usage/machine-model`。
用户贡献是“当前授权与拓扑上的比例估算”；单个用户的机器占用按
`(uplink_bytes + downlink_bytes) * 2` 换算，因为代理载荷在同一机器的客户端侧和下一跳侧
各经过一次网卡。归因总量以机器物理账本为上限，差额作为未归因流量；物理账本和未归因
流量才是容量核算边界。

#### 预测模型与每日快照

期末预计 P50 = 本期已用 + 近期日均 × 剩余天数：

- 近期日均取最近 28 天中记录完整的日子，每早 4 天权重减半，可以跨越周期边界。
- 跨日失联补报的字节平摊到失联的各天。
- 失联一整天以上后重启、换网卡或更换 Agent 状态时，相关日子不参与日均。

P90 = 本期已用 + 日均 × 剩余天数 × (1 + q)。q 是同一方法在账本历史上「预测之后 h 天」的相对误差 90% 分位，h 为剩余天数。q 按以下顺序取：

1. 本机回测，样本至少 20 个且不少于 8 × h。
2. 全部机器的回测，可退到不短于 h/2 的天数。
3. 默认 +50%。

记账晚于期初与本期校准，或缺口可能改变本期总量时，接口返回 `needs_calibration`；页面不再判「余量充足」，改为「待校准」。

Console 每小时把当天还没有记录的机器预测写入 `node_traffic_forecasts`，每台机器每个 UTC 日一行，当天第一次记录为准，保留 800 天。周期结束后，页面显示该周期每日 P50 的平均偏差和 P90 覆盖次数。

已有环境与上文同样在停写和备份后执行 `psql -v app_role=<应用角色> -f scripts/compat-traffic-forecast.sql`，脚本只新增这张表，故意拒绝重复执行；之后按目标二进制更新 version 1 的 SQLx SHA-384。旧 Console 不读这张表，回退时可以保留，但要恢复旧 checksum。

上线后核对：
- 机器详情的近期日均、P90 误差来源、待校准状态。
- 次日起 `node_traffic_forecasts` 每台已记账机器一行。

### 持久证书签发任务

证书扫描从进程内任务改为数据库持久队列和带代次的租约；新安装的默认续签提前量由
30 天改为 60 天，已有证书域显式保存的值不变。旧库缺少 `certificate_scan_runs` 时，
新 Console 无法安全启动证书扫描，因此 Console 发布前必须先完成兼容。

1. 停止所有 Console 写入者，完成数据库备份和隔离恢复演练。
2. 从目标 `0001_init.sql` 提取 `BEGIN/END CERTIFICATE SCAN SCHEMA` 区块到已核验文件。
3. 以应用角色运行：

   ```sh
   psql -X -v ON_ERROR_STOP=1 -d brocade \
     -v app_role=brocade \
     -v certificate_scan_schema_file=/verified/certificate-scan-schema.sql \
     -f scripts/compat-certificate-scan.sql
   ```

脚本在单事务内把 `cert_domains.renew_before_days` 的列默认值改为 60，并创建持久任务表、
约束和索引；不会改写已有域的续签提前量，也故意拒绝重复或部分执行。执行后逐项核对
列、identity 序列、约束、索引、默认值和所有权与目标 `0001` 一致。确认本次 `0001` 的
所有其它差异也已兼容后，才更新 `_sqlx_migrations` version 1 的 SHA-384；不要仅为消除
checksum 报错而直接更新。完成转换后，旧 Console 不再是安全回滚目标，除非先恢复旧库。

### Telegram MTProxy 入站

MTProxy 在 `control_state` 增加默认 `28800` 的 TCP 起始端口，在 `ingresses` 增加可空
`mtproto_port`，并把 `mtproto` 加入在线来源协议约束。已有行的接入协议、端口和来源历史
保持不变；现有控制行取得 `28800`。部署前停止所有 Console 写入者并完成数据库备份，
随后以应用角色运行 `scripts/compat-mtproxy.sql`。脚本故意不允许重复执行，也不修改
`_sqlx_migrations` checksum。

执行后核对新增列类型、默认值、非空属性和七个受影响约束与目标 `0001` 一致，确认
`control_state.port_mtproto_base = 28800`、已有 `ingresses.mtproto_port` 全为 NULL，并确认
没有活动配置或二进制发布，才按目标文件更新 version 1 的 SQLx SHA-384。旧 Console
会忽略新增列；回滚旧二进制前必须把 checksum 恢复为旧值。Console 部署不会自动发布
Agent/Xray；启用 MTProxy 配置前需另走二进制发布流程，使目标机器具备对应能力。

### 通知中心一键清空

仅修改 `0001_init.sql`：在 `admin_operators` 增加默认 0 的
`notification_cleared_through_event_id`，约束为非负且不大于已读游标。
系统管理员清空会单调推进所有账号的清空/已读位置，其他已登录账号只推进自身；访客不能
主动清空，但跟随系统管理员的全局清空。操作不删除机器事件、公网 IP 历史或 Webhook，
不会替操作者关闭告警。保留清理与新事件上报不变，不升级 Agent/Xray 或协议。

部署前保存数据库、旧二进制和迁移 checksum，在隔离库验证
`scripts/compat-notification-clear.sql`。该脚本只扩列与约束，重复执行会拒绝，
不自动修改 checksum。停止 Console 写入后以应用角色执行，逐项比对目标 `0001`
的新增列、默认值、约束和所有权；确认没有其它未兼容差异后才更新版本 1 checksum。
验证清空后的刷新/跨会话、系统管理员全局清空、普通账号隔离、公网 IP 历史不变和清空后新恢复通知。
生产验证不替真实操作者清空通知；写入回归应使用隔离环境。

旧二进制可忽略新增列，回退时保留该列和全部事件，只恢复对应 checksum 和旧程序。
旧 UI 不理解清空游标，回退期间可能重新展示旧通知；不能通过恢复整库回退界面状态。

### VPN Gate 总开关与任务集合拆分

新安装的 VPN Gate 总开关默认关闭。已有环境部署前停止 Console 写入、备份数据库与旧二进制，
记录 version 1 migration checksum，并先在生产备份的隔离恢复库执行：

```sh
psql -X -v ON_ERROR_STOP=1 -d brocade -v app_role=brocade \
  -f scripts/compat-vpngate-worker-sets.sql
```

脚本把旧的组合集合重命名为 `vpngate_catalog_nodes`，另建
`vpngate_intelligence_nodes`，并把原选择复制到两张表，因此升级不会静默丢失任务分配；新
Console 上线后两套集合可独立修改。脚本同时把总开关当前值和默认值设为关闭，保留目录、候选、
选择及已发布 Chain。核对两表的列、约束、所有者和复制行数，以及总开关为关闭后，确认目标
`0001_init.sql` 的其它差异也已兼容，才更新 version 1 的 SQLx SHA-384。

这是非向后兼容的表形状变更：执行脚本后不能直接回退旧 Console。若新 Console 尚未写入，
可在维护窗口从已验证备份恢复旧数据库与 checksum；新 Console 已写入后，两套集合可能已经
分歧，必须先导出并人工决定合并规则，不能自动覆盖其中一套选择。Agent 与 Xray 协议不变。

### 稀疏用量明细与 VPN Gate 索引

仅改 `0001`：新 `usage_node_windows` 表替代 `node_usage_windows` UNION 视图，按
机器、租户、真实上报时间聚合四种字节量；与日账本、head、回执在同一事务更新。
不保存正常零流量用户明细；非零和缺口明细、链路明细、原始读数、回执保留策略不变。
`inserted_samples` 仍表示接受的记账窗口数，不代表实际明细 INSERT 行数。

1. 备份并恢复演练，停止所有 Console 写入者。
2. 从目标 `0001` 提取 `BEGIN/END USAGE NODE WINDOWS SCHEMA` 区块，以及四个索引
   `vpngate_exit_reputations_due`、`usage_samples_by_user_window`、
   `usage_samples_by_node_window`、`usage_chain_samples_by_node_window` 的完整 CREATE 定义。
3. 以应用角色执行 `scripts/compat-usage-sparse.sql`，传入 `app_role`、
   `node_schema_file`、`index_schema_file`。脚本在单事务内验证旧图表逐点一致，
   保留每日账本和所有非零/缺口事件，拒绝盲目重跑。
4. 兼容事务成功提交后，仍保持写入停止，再以 `app_role` 执行
   `scripts/reclaim-usage-sparse.sql`：对用户明细做一次 CLUSTER 后解除聚簇标记。
   不能与 DELETE 合在同一事务，否则 MVCC 会保留刚删除的行。兼容脚本也会重建链路和
   VPN Gate latest 索引。需要足够磁盘、WAL 空间，事先用真实备份测量时长。
   不是常驻任务或全库 VACUUM FULL。VPN Gate due 索引覆盖过期租约且不含频繁更新的
   last_seen_at；相关高写入表使用 2% vacuum/analyze 阈值。
5. 比较新装/兼容库的列、约束、索引、所有权和 reloptions，验证完整账本与原精度曲线，
   才能更新版本 1 的 SQLx SHA-384。兼容失败回滚后可启动旧 Console；成功后不能直接
   用依赖旧视图的二进制回退，也不能用旧备份覆盖已发生的新计费。

### Load 分钟历史（有损）

仅修改 `0001_init.sql`：`node_load_samples.is_rollup`、原始行部分索引和
`node_load_compaction_state` 水位表。Agent 协议与采样不变；Console 每分钟尝试一轮有界
聚合，最近约 1 小时保留原始窗口，较早历史按分钟封存，仍保留 7 天。旧数据较多时渐进
追赶，每轮最多 16 台 × 240 行、20 秒预算；不是启动时一次性重写。

超过 1 小时的查询（含 24 小时图）也聚合最近的原始窗口，与已封存部分保持同一口径。
CPU/速率按时长加权，CPU peak 保留最大值；事件增量求和，容量/内存组成保持末次快照，
内存 available_min 保留最小值，磁盘 await 按 IOPS × 时长加权。NULL 不冒充零，不跨
重启、gap 或不连续窗口合并；分钟边界附近仍保留实际测量起止，不插值补齐缺失时段。
每台机器最新原始窗口保留用于 `latest_sample`，即使机器离线；7 天清理仍适用。
CPU steal 告警仍消费原始上报，历史聚合不触发或重放告警。Ping/计费用量不变。

1. 停止全部旧 Console 写入实例，备份并在隔离库验证恢复。
2. 提取目标 `0001` 的 `BEGIN/END LOAD ROLLUP SCHEMA` 区块至受限目录，执行：

   ```sh
   psql -X -v ON_ERROR_STOP=1 -d brocade \
     -v app_role=brocade -v load_schema_file=/verified/load-schema.sql \
     -f scripts/compat-load-minute.sql
   ```

3. 该脚本只扩结构，不压缩历史；重跑拒绝且失败回滚。与全新安装比较结构、约束、索引、
   所有权，并核对此次所有 `0001` 变更后，才能按目标二进制更新 SQLx SHA-384。
4. 新 Console 上线后检查最新上报、CPU steal 事件、短/长范围、overview/metrics 时间轴、
   聚合日志与原始积压。每节点事务锁与封存水位防止重试重新插回已聚合原始点；Load
   无离线补报，封存水位及以前的迟到窗口计为 skipped。保留清理与聚合使用共享/排他锁
   协调，避免清理中的过期样本被聚合重新写入。
5. DELETE 后空间通常先变成可复用空间，不会立即返还文件系统；观察 autovacuum、死元组、
   WAL 和磁盘余量，不自动执行阻塞全表的 VACUUM FULL。

恢复边界：扩列本身不损失数据，但新程序开始聚合后无法还原 30 秒点。不能直接启动
忽略封存水位的旧 Console；需要停止压缩时使用理解新结构/水位且禁用后台压缩的补丁。
恢复整库须另行授权维护窗口和数据损失边界，禁止覆盖部署后新数据。无需升级 Agent/Xray。

### Ping 序列字典（无损时序存储）

此项历史变更写入当时的 `0001_init.sql`。结构为
`node_ping_probe_series(id,node_id,target,family)` 与以 `(series_id,probed_at)` 为主键的
`node_ping_probe_samples`；所有时间点、微秒延迟、NULL、丢包/未执行原因、双栈分离和
7 天保留期不变，HTTP/Agent 协议不变。Load、用量结构和保留策略不随本项改变。

1. 停止旧 Console 的所有写入实例，备份数据库和旧二进制。估算同时保留旧表、新表、
   索引、排序临时文件和 WAL 的峰值磁盘，不能用迁移后的最终体积代替峰值。
2. 在隔离库恢复真实备份演练。提取目标 `0001` 的 `BEGIN/END PING SERIES SCHEMA`
   区块为受限目录中的 `ping-schema.sql`，运行：

   ```sh
   psql -X -v ON_ERROR_STOP=1 -d brocade \
     -v app_role=brocade -v ping_schema_file=/verified/ping-schema.sql \
     -f scripts/compat-ping-series.sql
   ```

3. 脚本在一个事务中加锁、生成字典、复制每个点并逐身份/值校验；全部通过后才删除旧表，
   不使用 CASCADE。锁等待 5 秒、单条语句 15 分钟上限。中途失败全部回滚；已转换环境
   再执行会拒绝。PostgreSQL 18 的具名 NOT NULL 约束也会兼容处理。
4. 比对转换后与空库全新安装的表、约束、索引、序列及所有权；验证新版本读取、双栈、
   重复和并发上报、租户隔离、保留清理。只有全部结构变更都核对一致后才能更新 version 1
   SQLx SHA-384。兼容脚本故意不自动更新 checksum。
5. 取得部署授权后再应用生产兼容和替换 Console。此次不是向后兼容扩列；转换后不能
   直接启动旧 Console。需回退时停写，另行验证反向重建或恢复方案，禁止自动恢复整库
   覆盖发布后的新数据。无需升级 Agent/Xray 或提升协议。

可在同一隔离库转换前后运行 `scripts/ping-storage-benchmark.sql`（psql `-X -qAt`），
输出无机器名和目标地址的 JSON。窗口锚定备份最大采样时间，保留第一遍和后续 5 遍结果；
不清缓存、不重启 PostgreSQL，因此第一遍不能称为真实冷启动。不要在生产直接跑全量
计数和 EXPLAIN ANALYZE 基准。

2026-10-07 隔离演练：PG18，11,541,354 点 / 240 序列，恢复后的原结构与新结构分别为
2,062.125 / 1,044.633 MiB（含索引及字典），减少 49.3%。24 小时详情查询预热后中位数
53.549 → 36.259 ms，返回均为 98,532 点；全序列 latest 预热后 2.061 → 0.608 ms。
latest 第一遍为 3.678 → 20.214 ms，缓存条件不同，不能宣称所有冷查询都更快。
这不是生产延迟或最终生产体积承诺，也没有降低采样分辨率。

保留清理改为最多 20 批 × 10,000 行独立事务，每批语句限时 5 秒，总耗时达到 10 秒后
不再开下一批；仍每小时执行，过期积压可跨轮清理。该上限当前高于采样增长量，但机队
大幅增长后须复核；不能把“任务成功”误读为“没有过期积压”。

Load 的 20,000 行无损 TOAST 目标实验（2,040 / 1,024 / 512 字节）没有带来体积收益，
更低目标的 JSON 读取反而变慢，故不修改；用量索引也不依据单次 `idx_scan=0` 直接删除。

### Agent / Xray 统一发布账本兼容

此项历史变更写入当时的 `0001_init.sql`。停止旧 Console 后才可转换旧 Xray 三表为
`binary_releases`、`binary_release_targets`、`binary_release_attempts` 和
`binary_release_events`。转换前必须没有活动 Xray 单；保留完整备份，并在隔离库
恢复实际生产备份演练，不只在空库建表。

权威 DDL 是 `0001_init.sql` 的 `BEGIN/END BINARY RELEASE SCHEMA` 区块。
将该区块提取到本次发布的受限目录，以 psql 运行仓库脚本：

```sh
psql -X -v ON_ERROR_STOP=1 -d brocade \
  -v app_role=brocade -v binary_schema_file=/verified/binary-schema.sql \
  -f scripts/compat-binary-releases.sql
```

脚本在事务内保留旧记录 ID、范围、摘要、事件及实际执行证据，原波次等布局元数据
保留在迁移事件；逐次尝试只导入存在的证据，不补造历史。完成校验后替换旧表。
`control_state` 的 Agent 最后批准保留为只读基线。所有新表和序列必须由应用角色
拥有；不要以 postgres 所有权建表后遗漏授权。核对记录数、身份、约束、索引、
序列和新安装 schema 完全一致后，才更新 version 1 的 SQLx SHA-384。

该转换不是旧 Console 的向后兼容扩列：转换完成后不能直接换回旧二进制。
恢复应优先修复新版本；确需旧版时须停写并另行验证反向适配或恢复方案，不能自动
恢复整库覆盖转换后业务数据。保留部署备份不等于建立历史 Agent/Xray 产物仓库。

#### Xray 平滑交接策略与发布期双进程观测

统一发布账本已存在的环境，升级到支持 `restart` / `handoff` / `handoff-force` 的
Console 前，先停止 Console 写入并备份数据库，再以应用角色执行：

```sh
psql -X -v ON_ERROR_STOP=1 -d brocade \
  -f scripts/compat-xray-handoff-release.sql
```

脚本只增加发布策略、排空超时、强制退役证据和 `node_agent_state.xray_upgrade` 临时
观测列，并在事务内设置锁与语句超时；它故意拒绝重复执行。旧发布记录的策略保持
NULL，由新 Console 按既有的直接重启语义读取，不能回填成未经证明的平滑交接。
核对列、检查约束、现有发布记录数和目标记录数后，按目标 `0001_init.sql` 更新 SQLx
version 1 SHA-384。回滚旧 Console 前可保留这些可空列，但必须恢复旧 schema checksum。

数据库扩列不等于节点具备平滑交接。只有协议版本满足要求且 Agent 实时上报
`xray_handoff.state=ready` 的机器才能创建 handoff 发布；`blocked`、`busy`、缺失或
过期状态都必须阻止下发。`xray_upgrade` 只在一次发布尝试中显示，终态运行报告应将
其清为 NULL；流量计量仍读取原有单一累计账本，不能把两个 PID 的数值相加。

配置发布默认在 300 秒排空超时后强制退役上一代。已有库还需在停写和备份后、启动新
Console 前，以应用角色执行：

```sh
psql -X -v ON_ERROR_STOP=1 -d brocade \
  -f scripts/compat-config-xray-force-retirement.sql
```

该表保存系统管理员或后一张可执行配置单对一张旧配置单、一个节点和一次 claim generation
的精确授权；
配置发布的默认超时强退由 Agent 的持久运行态决定，不依赖 Console 在线。协议 v33 的 Agent
也会把 v32 遗留且已过期的配置 `handoff` 视为待强退，先收口旧代，下一轮再领取新配置。
Console 会拒绝向低于 v33 的 Agent 下发新配置，所以上线顺序是：兼容 SQL、Console、Agent，
确认节点协议为 v33 后再创建配置发布。核对表、约束和唯一部分索引后再更新 SQLx version 1
SHA-384；回滚旧 Console 时仍须恢复旧 checksum。

### 在线来源协议字段兼容

协议感知版本在 `user_online_sources` 增加可空 `protocols JSONB`，写入当时的
`0001_init.sql`。已有环境部署前先完成备份、记录原迁移 checksum，
在隔离库验证以下兼容 SQL，然后在受控窗口应用（新安装不需要）：

```sql
BEGIN;
SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '30s';
ALTER TABLE user_online_sources ADD COLUMN IF NOT EXISTS protocols JSONB;
DO $$ BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conrelid = 'user_online_sources'::regclass
           AND conname = 'user_online_sources_protocols_shape'
    ) THEN
        ALTER TABLE user_online_sources ADD CONSTRAINT user_online_sources_protocols_shape
        CHECK (
            protocols IS NULL OR CASE WHEN jsonb_typeof(protocols) = 'array' THEN
                jsonb_array_length(protocols) BETWEEN 1 AND 5
                AND protocols <@ '["vless", "anytls", "hysteria2", "mtproto", "unknown"]'::jsonb
            ELSE FALSE END
        ) NOT VALID;
    END IF;
END $$;
COMMIT;
```

随后单独以有限超时验证约束：
`ALTER TABLE user_online_sources VALIDATE CONSTRAINT user_online_sources_protocols_shape`。
检查列类型、约束定义与新安装 schema 一致，并审查本次 `0001` 是否还有其它差异。
只有所有差异已兼容后，才按目标二进制内嵌的 `0001` SHA-384 更新
`_sqlx_migrations` 对应记录；不能为绕过校验直接修改 checksum。
历史行保持 NULL，不根据现有配置回填。此步骤没有新增索引和连接明细表。

部署顺序为数据库兼容 → Console → 按发布流程升级 Agent/Xray，后两者任一仍旧时
显示「协议未知」，IP 来源统计正常。字段为向后兼容扩展，不提高最低协议版本。
回滚保留可空列；旧程序忽略它，恢复旧二进制前必须核对并恢复其迁移 checksum。
Agent 和 Xray 发布仍需要独立授权、灰度与观察，不能因部署 Console 自动执行。

### 在线来源会话指标兼容

接入链会话版在 `user_online_sources` 增加可空的 `online_since_at`、
`session_uplink_bytes` 和 `session_downlink_bytes`。已有环境必须先停止旧 Console 写入、
备份并记录原 `0001` checksum，再以应用角色运行：

```sh
psql -X -v ON_ERROR_STOP=1 -d brocade -v app_role=brocade \
  -f scripts/compat-user-online-session.sql
```

脚本只扩展现有表，不回填历史行，也不从月用量推算来源 IP 流量；三个字段必须同时为空或
同时有效。检查列类型和 `user_online_sources_session_shape` 与 `0001_init.sql` 一致，并确认
本次 `0001` 没有其它未兼容差异后，才按目标二进制内嵌值更新 SQLx version 1 checksum。

部署顺序为数据库兼容 → Console → 灰度 Agent/Xray。新字段在部署协议和 Xray protobuf
中均为可选扩展，不提高最低 Agent 协议版本；旧组件继续上报 IP、入口和协议，新 Console
将本次流量与在线时长显示为「—」。回滚时保留可空列，恢复旧 Console 前仍须恢复旧
checksum。来源会话计数只存在 Xray 内存中，Xray 重启后从下一次认证连接重新起算，不影响
计费用量账本。

### 用户自助转发兼容

用户转发增加独立的授权、完整期望状态、收敛状态和用量账本；不进入修订模型，也不复用
订阅流量账本。已有环境部署对应 Console 前先停止旧 Console 写入，完成备份并记录原
`0001` checksum，再以应用角色运行：

```sh
psql -X -v ON_ERROR_STOP=1 -d brocade -v app_role=brocade \
  -f scripts/compat-user-forwarding.sql
```

脚本故意不使用 `IF NOT EXISTS`，重复或部分执行会停止并要求人工核对。确认六张表、四个
业务索引、外键/约束、identity 序列所有者均与 `0001_init.sql` 一致，并审查本次 `0001`
没有其它未兼容差异后，才把 `_sqlx_migrations` version 1 更新为目标二进制内嵌的
SHA-384。不能只为消除 checksum 错误而修改迁移记录。

发布顺序为数据库兼容 → Console → Xray 配置发布（加入默认直连出站）→ Agent 26 灰度。
Agent 26 之前不会创建动态转发入站，Console 应持续显示待收敛；不可把 Console 部署当成
Agent/Xray 发布授权。回滚前先撤销授权并等待节点报告空期望，再回退 Agent；保留新增表和
历史用量，恢复旧 Console 前还须恢复与旧二进制匹配的 migration checksum。

若数据库已安装首版用户转发表，需要在部署支持双协议默认值的 Console 前停止旧 Console
写入，并额外运行：

```sh
psql -X -v ON_ERROR_STOP=1 -d brocade -v app_role=brocade \
  -f scripts/compat-forwarding-dual-protocol.sql
```

脚本只调整新授权的 UDP 默认值，并把监听端口唯一约束拆成 TCP、UDP 两个协议空间；不会
静默修改现有授权。确认旧索引 `forwarding_rules_active_port` 已消失、两个新唯一索引的谓词
与 `0001_init.sql` 一致，再更新 migration checksum。回滚旧 Console 前必须恢复旧索引；若
同号 TCP/UDP 规则已经创建，应先停用或改端口，否则旧索引无法重建。

若数据库已存在用户转发表，需要在部署采用 `30000–59999`、100 条规则并默认允许内部目标
的 Console 前停止旧 Console 写入，并额外运行：

```sh
psql -X -v ON_ERROR_STOP=1 -d brocade -v app_role=brocade \
  -f scripts/compat-forwarding-entitlement-defaults.sql
```

脚本只修改以后新插入授权所使用的列默认值，不回填或放宽任何现有授权。确认六个列默认值
与 `0001_init.sql` 一致、并审查本次 `0001` 的其它差异后，再更新 migration checksum。

### 机器事故通知中心兼容

通知中心为操作者增加持久已读游标，为离线事件保存最后联系时间，并将 Webhook
启停状态与机器事件本身分离。已有环境在部署对应 Console 前先备份数据库和旧二进制、
记录原迁移 checksum，并在隔离库验证以下兼容 SQL：

```sql
BEGIN;
SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '30s';

ALTER TABLE admin_operators
    ADD COLUMN IF NOT EXISTS notification_last_seen_event_id BIGINT DEFAULT 0 NOT NULL;
DO $$ BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conrelid = 'admin_operators'::regclass
           AND conname = 'admin_operators_notification_cursor_nonnegative'
    ) THEN
        ALTER TABLE admin_operators
            ADD CONSTRAINT admin_operators_notification_cursor_nonnegative
            CHECK (notification_last_seen_event_id >= 0) NOT VALID;
    END IF;
END $$;

ALTER TABLE machine_events ADD COLUMN IF NOT EXISTS last_contact_at TIMESTAMPTZ;
ALTER TABLE machine_events ADD COLUMN IF NOT EXISTS incident_started_at TIMESTAMPTZ;
ALTER TABLE machine_events ADD COLUMN IF NOT EXISTS metric_value REAL;
ALTER TABLE machine_events ADD COLUMN IF NOT EXISTS metric_peak_value REAL;
ALTER TABLE machine_events ADD COLUMN IF NOT EXISTS metric_threshold REAL;
-- 历史事件没有保存精确的最后轮询时间；按当时 90 秒离线判定窗回填近似值。
UPDATE machine_events
   SET last_contact_at = occurred_at - interval '90 seconds'
 WHERE event_kind = 'node_offline' AND last_contact_at IS NULL;
DO $$ BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conrelid = 'machine_events'::regclass
           AND conname = 'machine_events_last_contact_shape'
    ) THEN
        ALTER TABLE machine_events
            ADD CONSTRAINT machine_events_last_contact_shape CHECK (
                (event_kind = 'node_offline' AND last_contact_at IS NOT NULL
                    AND last_contact_at <= occurred_at)
                OR (event_kind <> 'node_offline' AND last_contact_at IS NULL)
            ) NOT VALID;
    END IF;
END $$;

ALTER TABLE machine_events DROP CONSTRAINT IF EXISTS machine_events_kind_known;
ALTER TABLE machine_events
    ADD CONSTRAINT machine_events_kind_known CHECK (event_kind IN (
        'node_online', 'node_offline', 'public_ip_changed',
        'cpu_steal_started', 'cpu_steal_recovered'
    )) NOT VALID;
ALTER TABLE machine_events DROP CONSTRAINT IF EXISTS machine_events_metric_shape;
ALTER TABLE machine_events
    ADD CONSTRAINT machine_events_metric_shape CHECK (
        (event_kind NOT IN ('cpu_steal_started', 'cpu_steal_recovered')
            AND incident_started_at IS NULL
            AND metric_value IS NULL
            AND metric_peak_value IS NULL
            AND metric_threshold IS NULL)
        OR (event_kind IN ('cpu_steal_started', 'cpu_steal_recovered')
            AND incident_started_at IS NOT NULL
            AND incident_started_at <= occurred_at
            AND metric_value IS NOT NULL
            AND metric_peak_value IS NOT NULL
            AND metric_threshold IS NOT NULL
            AND metric_value BETWEEN 0 AND 100
            AND metric_peak_value BETWEEN 0 AND 100
            AND metric_peak_value >= metric_value
            AND metric_threshold BETWEEN 0 AND 100)
    ) NOT VALID;
ALTER TABLE machine_events DROP CONSTRAINT IF EXISTS machine_events_shape;
ALTER TABLE machine_events
    ADD CONSTRAINT machine_events_shape CHECK (
        (event_kind = 'node_online'
            AND family IS NULL
            AND previous_value IN ('waiting', 'offline')
            AND current_value = 'online')
        OR (event_kind = 'node_offline'
            AND family IS NULL
            AND previous_value = 'online'
            AND current_value = 'offline')
        OR (event_kind = 'public_ip_changed'
            AND family IN (4, 6)
            AND previous_value IS NOT NULL
            AND current_value IS NOT NULL
            AND previous_value <> current_value)
        OR (event_kind = 'cpu_steal_started'
            AND family IS NULL
            AND previous_value = 'normal'
            AND current_value = 'active')
        OR (event_kind = 'cpu_steal_recovered'
            AND family IS NULL
            AND previous_value = 'active'
            AND current_value = 'normal')
    ) NOT VALID;

CREATE TABLE IF NOT EXISTS node_cpu_steal_state (
    node_id TEXT NOT NULL,
    status TEXT DEFAULT 'normal' NOT NULL,
    transition_started_at TIMESTAMPTZ,
    transition_observations INTEGER DEFAULT 0 NOT NULL,
    active_started_at TIMESTAMPTZ,
    last_window_end TIMESTAMPTZ NOT NULL,
    current_pct REAL NOT NULL,
    peak_pct REAL NOT NULL,
    updated_at TIMESTAMPTZ DEFAULT now() NOT NULL,
    CONSTRAINT node_cpu_steal_state_pkey PRIMARY KEY (node_id),
    CONSTRAINT node_cpu_steal_state_node_fkey
        FOREIGN KEY (node_id) REFERENCES nodes(id) ON DELETE CASCADE,
    CONSTRAINT node_cpu_steal_state_status_known
        CHECK (status IN ('normal', 'candidate', 'active', 'recovering')),
    CONSTRAINT node_cpu_steal_state_observations_nonnegative
        CHECK (transition_observations >= 0),
    CONSTRAINT node_cpu_steal_state_values CHECK (
        current_pct BETWEEN 0 AND 100
        AND peak_pct BETWEEN 0 AND 100
        AND peak_pct >= current_pct
    ),
    CONSTRAINT node_cpu_steal_state_shape CHECK (
        (status = 'normal'
            AND transition_started_at IS NULL
            AND transition_observations = 0
            AND active_started_at IS NULL)
        OR (status = 'candidate'
            AND transition_started_at IS NOT NULL
            AND transition_observations > 0
            AND active_started_at IS NULL)
        OR (status = 'active'
            AND transition_started_at IS NULL
            AND transition_observations = 0
            AND active_started_at IS NOT NULL)
        OR (status = 'recovering'
            AND transition_started_at IS NOT NULL
            AND transition_observations > 0
            AND active_started_at IS NOT NULL)
    ),
    CONSTRAINT node_cpu_steal_state_time_order CHECK (
        (transition_started_at IS NULL OR transition_started_at <= last_window_end)
        AND (active_started_at IS NULL OR active_started_at <= last_window_end)
    )
);

CREATE TABLE IF NOT EXISTS notification_channels (
    channel TEXT NOT NULL,
    enabled BOOLEAN DEFAULT FALSE NOT NULL,
    changed_at TIMESTAMPTZ DEFAULT now() NOT NULL,
    CONSTRAINT notification_channels_pkey PRIMARY KEY (channel),
    CONSTRAINT notification_channels_channel_known CHECK (channel = 'webhook')
);
INSERT INTO notification_channels (channel, enabled)
VALUES ('webhook', FALSE)
ON CONFLICT (channel) DO NOTHING;

ALTER TABLE notification_deliveries
    DROP CONSTRAINT IF EXISTS notification_deliveries_status_known;
ALTER TABLE notification_deliveries
    ADD CONSTRAINT notification_deliveries_status_known
    CHECK (status IN ('pending', 'delivering', 'delivered', 'suppressed'));
COMMIT;

ALTER TABLE admin_operators
    VALIDATE CONSTRAINT admin_operators_notification_cursor_nonnegative;
ALTER TABLE machine_events
    VALIDATE CONSTRAINT machine_events_last_contact_shape;
ALTER TABLE machine_events VALIDATE CONSTRAINT machine_events_kind_known;
ALTER TABLE machine_events VALIDATE CONSTRAINT machine_events_metric_shape;
ALTER TABLE machine_events VALIDATE CONSTRAINT machine_events_shape;
```

确认列、约束、`notification_channels` 以及历史离线事件回填正确后，才将
`_sqlx_migrations` 中 `0001` 的 checksum 更新为目标二进制所内嵌迁移的 SHA-384。
Console 启动时根据 `BROCADE_NOTIFICATION_WEBHOOK_URL` 收敛通道状态：未配置时把遗留的
`pending`/`delivering` 行标记为 `suppressed`，后续机器事件不再积压外部投递；已配置时只
投递启用后的新事件，不复活已抑制历史。持续 CPU steal 使用 `>=80%` 六十秒开启、
`<80%` 九十秒恢复；Webhook envelope 升为 schema v2，并为事件增加结构化指标字段。
回滚旧 Console 时保留兼容列、状态表和事件，并先恢复旧 checksum；旧版本会忽略
`suppressed` 行以及新的 steal 事件。

### 机器地区手动覆盖兼容

机器详情的手动地区只修正 Console 展示，不进入模型修订或节点产物。已有数据库部署对应
Console 前，先停止写入、备份并记录原 migration checksum，再以应用角色执行：

```sh
psql -X -v ON_ERROR_STOP=1 -d brocade -v app_role=brocade \
  -f scripts/compat-node-region-overrides.sql
```

确认 `node_region_overrides` 的主键、外键和两个输入约束与目标 `0001_init.sql` 一致，并审查
本次 `0001` 的其它差异后，才更新 version 1 的 SQLx SHA-384。旧 Console 不读取这张表，
回滚时可以保留手动选择记录，但必须恢复与旧二进制匹配的 migration checksum。

### 移除用户转发总额度（整合于 0002）

该版本取消转发总额度控件与写入接口，只保留用户 × 机器的独立额度。完整 `0002` 的退役段将旧总额度
配置改名归档为 `retired_forwarding_traffic_policies`，保留政策事件与全部转发用量；归档不再
参与执行。统一额度视图仅保留原接入分支，接入额度及其预算不调整。转发总额度预算和旧来源
分配解除；旧 Agent 从完整快照和兼容预算响应收敛，不需要清空节点状态。

迁移在一个事务内恢复受影响机器额度状态、推进期望代次，并为退役动作生成新的审计工单、
取代旧待执行意图。升级后核对机器额度、手动规则开关、用量与工单回执；线上未配置过
转发总额度时，不会生成这些补偿工单。迁移有 5 秒锁等待和 120 秒语句上限，失败回滚事务。

先完成备份和隔离升级验证，再统一替换 Console。旧总额度执行表和函数已退役，不能仅替换
为依赖它们的旧 Console 回滚；优先前向修复，需要恢复数据库时另行明确授权及数据损失边界。

## 上传与原子替换

生产 Console 应由 `brocade-console.socket` 持有监听端口，service 的 `[Unit]` 同时包含
`Requires=brocade-console.socket` 和 `After=brocade-console.socket`。socket 的
`ListenStream` 必须与 `BROCADE_ADMIN_BIND` 完全一致，默认单监听部署只传一个 fd；分离
Agent 入口时应再传一个与 `BROCADE_AGENT_BIND` 一致的 fd。二进制会拒绝数量、地址、类型
或监听状态不匹配的描述符，不会静默回退到另一个端口。

纯二进制替换期间只重启 `brocade-console.service`，不要停止、重启或 disable socket。这样旧进程
退出后到新进程完成数据库初始化期间，新 TCP 连接留在内核 backlog；它不能延长已经建立
连接的应用层超时，因此仍须保留 Agent 的有界 Load 补报作为较长维护窗口的第二层保护。
首次从旧的直接绑定 service 迁移到 socket activation 时，必须先停旧服务释放端口，再
启动 socket 和 service；记录这一次不可避免的短维护窗口并验证后续 restart 不再关闭端口。

数据库停写、耗时备份等长维护须单独验证入口和自动激活的处理。不要用让 service 反复跳过
启动的 `ConditionPathExists` 围栏来阻止 socket 自动激活：待处理连接可使 socket 触发
`trigger-limit-hit`，关闭监听并导致公网 502。若发生此情况，先按迁移历史确定应运行的
Console 字节，再清理本次临时围栏、重置相关 unit 的失败状态，启动 socket 和 service，
核对进程摘要及公网健康。记录监听曾关闭和实际错误采样，不能把恢复后的 active 状态当作
整个维护窗口内监听连续的证据。

先上传到临时路径并核对摘要，之后才在目标机安装。上传过程不应覆盖正在运行的二进制。

```sh
scp target/<target-triple>/release/brocade-console \
  deploy@console.example.net:/tmp/brocade-console.new

ssh deploy@console.example.net \
  'sha256sum /tmp/brocade-console.new'
```

远端摘要与发布记录一致后执行：

```sh
ssh deploy@console.example.net '
  set -eu
  sudo install -o root -g root -m 0755 \
    /tmp/brocade-console.new /opt/brocade/brocade-console.new
  sudo mv /opt/brocade/brocade-console.new /opt/brocade/brocade-console
  sudo systemctl restart brocade-console
  sudo systemctl --no-pager --full status brocade-console
'
```

不要通过删除数据库来处理启动或迁移失败。保留完整日志和当前二进制摘要，先判断是配置、数据库兼容、资源、探测执行器还是应用错误。

## 验证

```sh
sudo systemctl is-active brocade-console
sudo systemctl is-active brocade-console.socket
sudo journalctl -u brocade-console -n 100 --no-pager
curl --fail https://console.example.net/healthz
sha256sum /opt/brocade/brocade-console
```

随后验证：

- 管理端可以登录，初始化接口没有意外重新开放。
- Agent 轮询和运行状态保持新鲜，没有协议不兼容扩散。
- 用户授权验证与线路拨测可以调用受管 Xray。
- 本次改动涉及的页面/API 正常，错误率和资源指标没有异常。
- 部署 Console 没有被误当成 Agent 或 Xray 已发布。

## Console 二进制回滚

只有在确认旧二进制能够读取当前数据库和配置后，才能直接回滚二进制：

```sh
sudo install -o root -g root -m 0755 \
  /opt/brocade/releases/brocade-console.before-<release-id> \
  /opt/brocade/brocade-console.rollback
sudo mv /opt/brocade/brocade-console.rollback /opt/brocade/brocade-console
sudo systemctl restart brocade-console
sudo systemctl is-active brocade-console
curl --fail https://console.example.net/healthz
```

如果数据库已发生旧版本不兼容的变化，停止直接回滚。数据库恢复会覆盖部署后的写入，必须进入维护窗口，明确数据损失边界，并由操作者单独授权。恢复后重新运行完整验证。

## 记录

发布记录至少包含：目标环境、提交、构建目标、二进制摘要、开始/结束时间、操作者、备份位置、验证结果、异常、回滚判据以及是否执行回滚。不得记录秘密值。
