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
                jsonb_array_length(protocols) BETWEEN 1 AND 4
                AND protocols <@ '["vless", "anytls", "hysteria2", "unknown"]'::jsonb
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
