# Console 部署手册

## 适用范围

本手册用于首次准备 `brocade-console` 主机，以及之后替换 Console 二进制。它不发布模型配置、用户授权、Agent 或 Xray；这些流程见 `release-and-rollback.md`。

部署、重启、数据库备份/恢复和反向代理变更都会修改外部系统。Agent 可以完成只读预检和生成命令，但只有用户明确授权目标环境与操作后才能执行生产变更。

## 前置条件

- 明确目标环境、主机、CPU 架构、域名和维护窗口。
- 具备受控的 SSH、systemd、Nginx 和 PostgreSQL 管理权限。
- 按 `README.md` 的构建前置条件安装 Node.js、Go、Zig、Rust 工具链和目标架构。
- 准备与 `console.env.example` 同步的环境文件；真实秘密只存在于目标机器的受限文件或秘密管理系统。
- 确认数据库备份位置有足够空间，并验证恢复责任人和停机边界。

首次部署的系统账号、PostgreSQL、systemd、Nginx 与 TLS 示例见 `../../README.md` 的“部署 brocade-console”。本手册是操作检查表；配置清单的权威来源是 `../../console.env.example`。

## 发布前预检

1. 检查工作树和待发布提交，确认构建来自预期修订。
2. 按 `../standards/testing.md` 完成与改动相称的检查。
3. 构建目标架构的 release 产物，并记录产物 SHA-256、提交和构建环境。
4. 审查是否涉及环境变量、数据库、HTTP API、Console/Agent 协议、内嵌 Agent 或 Xray。
5. 确认旧二进制和数据库备份的保存位置，写下回滚判据。

示例构建和摘要：

```sh
cargo build --release --locked --target <target-triple> -p brocade-console
sha256sum target/<target-triple>/release/brocade-console
```

## 部署前备份

在目标机上备份数据库，并保存当前二进制。占位值必须替换为本次发布的唯一标识；不要把真实地址或凭据写回仓库。

```sh
sudo install -d -o postgres -g postgres -m 0700 /var/backups/brocade
sudo -u postgres pg_dump --format=custom \
  --file=/var/backups/brocade/before-<release-id>.dump brocade

sudo install -d -o root -g root -m 0755 /opt/brocade/releases
sudo install -o root -g root -m 0755 \
  /opt/brocade/brocade-console \
  /opt/brocade/releases/brocade-console.before-<release-id>
sha256sum /opt/brocade/brocade-console
```

确认备份命令成功、文件非空，并把旧二进制摘要写入发布记录。涉及不可逆数据库变化时，仅有备份文件不算完成预检；必须在隔离环境验证恢复。

## 数据库兼容

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

只修改 `0001_init.sql`，不增加 `0002`。新结构为
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

本次仍只维护 `0001_init.sql`。停止旧 Console 后才可转换旧 Xray 三表为
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

### 在线来源协议字段兼容

协议感知版本在 `user_online_sources` 增加可空 `protocols JSONB`；只维护
`0001_init.sql`，不新建 `0002`。已有环境部署前先完成备份、记录原迁移 checksum，
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

## 上传与原子替换

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
