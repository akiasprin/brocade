# 安装与升级

用发行包安装、初始化、升级和备份 Console。不使用安装器的部署方式见[手动部署](manual-deployment.md)。

## 下载

从 [Releases](https://github.com/akiasprin/brocade/releases/latest) 下载与服务器匹配的发行包和同名 `.sha256`，校验后解压。自 `v0.3.1` 起，GNU 包支持 glibc 2.35 及以上；Alpine 等 musl 系统选 `musl`。静态 musl 包也可安装到同架构 GNU 系统，launcher 会按宿主机的 libc 选择受管 PostgreSQL，不要求额外安装 musl 动态加载器。

`v0.3.0` 的 GNU 包要求 glibc 2.39，且该版本 musl 安装器不支持跨 libc 安装。兼容 Ubuntu 22.04、Debian 12 等较旧 GNU 系统时，请下载 `v0.3.1` 或更新版本：

```sh
v=0.3.3 p=linux-x86_64-gnu
url=https://github.com/akiasprin/brocade/releases/download/v$v/brocade-dist-$v-$p.tar.gz
curl -fLO "$url" && curl -fLO "$url.sha256"
sha256sum -c brocade-dist-$v-$p.tar.gz.sha256
tar xzf brocade-dist-$v-$p.tar.gz
cd brocade-dist-$v-$p
```

## 安装

CI 按版本、CPU 与 libc 生成 `brocade-dist-<version>-linux-<arch>-<libc>.tar.gz`。解压后的目录结构如下：

```text
brocade-dist-<version>-linux-x86_64-gnu/
├── brocade-launcher
├── brocade-console
├── install.sh
├── BROCADE_PLATFORM
└── SHA256SUMS
```

CI 同时生成同名 `.tar.gz.sha256`，用于在解压前校验整个归档。其中发行物内的组件使用完整名称 `brocade-launcher`，安装后仍由简洁的 `brocade` 命令作为用户入口。系统级安装执行：

```sh
sudo sh install.sh
```

安装器默认直接提供服务器 IP 的 HTTP 入口，监听 `0.0.0.0:8080`。交互安装先选择托管或外部 PostgreSQL，再确认自动识别的服务器 IP 和端口；端口提示为 `HTTP 端口 [8080]：`，直接回车使用 8080，也可输入 1 至 65535 之间的自定义端口。重启或升级不会重新分配入口地址。NAT 环境请填写外部机器能访问的 IP，并做好端口映射。非交互安装可指定：

```sh
sudo sh install.sh --public-ip 192.0.2.10 --managed-db
sudo sh install.sh --public-ip 192.0.2.10 --port 9000 \
  --database-url postgresql://brocade:password@database.example.net/brocade
```

省略 `--public-ip` 时，安装器使用默认路由的本机地址；省略 `--port` 时使用 8080。包含密码的数据库连接地址优先在交互菜单中填写，避免进入 shell 历史。安装器校验发行包与 CPU/libc 后补齐系统 CA、账号工具及 GNU 构建所需的 `liblzma`/`libgcc`，创建 `brocade` 服务账号并原子安装二进制和配置。有 systemd 时自动启用并启动 `brocade.service`，等待就绪后输出 `http://服务器IP:8080/#/initialize/...` 完整初始化链接；打开后设置管理员用户名和密码即可。无 systemd 时在当前终端前台运行。自动化仅安装文件可加 `--no-start`，随后手动启动：

```sh
sudo brocade up
```

## 数据库

未设置 `DATABASE_URL` 时，launcher 按 CPU 与 libc 下载并校验固定的 PostgreSQL 17.11.0，在持久目录维护数据库；设置了 `DATABASE_URL` 则只连接外部数据库，失败不会回退并新建空库。可用 `--managed-db` 或 `--external-db` 明确选择。系统安装器将访问地址保存到 `/etc/brocade/agent-public-url`，监听地址保存到 `/etc/brocade/listen-address`，以后启动继续使用。直接从发行包执行时可传 `brocade-launcher up --public-ip 192.0.2.10 --port 8080`；仅查看识别结果可执行 `brocade-launcher address`。

## 初始化

首次启动生成一次性初始化凭据。完整链接把凭据放在 `#/initialize/...` fragment 中，网页接收后清除地址栏 fragment，只在内存保留直到提交。完整链接仅向交互终端输出，重定向和 systemd 日志只输出普通地址；再次获取入口运行 `sudo brocade url`。初始化成功后凭据失效，私有文件被删除。初始化只创建系统管理员和根租户；登录后由入门向导纳管首台机器、创建链和首个用户、发布配置并完成真实节点拨测。登录、订阅链接和 Agent 安装命令均支持 HTTP IP 入口。

## 目录与账号

系统安装器把数据和下载缓存分别放在 `/var/lib/brocade` 与 `/var/cache/brocade`；`sudo brocade` 会先降权到专用账号，PostgreSQL 和 Console 都不会以 root 运行。直接从解压目录执行时，默认仍为 `$XDG_DATA_HOME/brocade`（或 `~/.local/share/brocade`）与 `$XDG_CACHE_HOME/brocade`（或 `~/.cache/brocade`）。完整参数见 `brocade up --help`。

## 升级

安装器管理的 systemd 部署，从 **v0.3.1 及以后版本**升级时，下载、校验并解压新发行包，在新包目录执行：

```sh
sudo sh install.sh --upgrade
```

升级复用 `/etc/brocade` 中原来的数据库、访问 IP 和端口，不重新选择，也不生成替代密钥。
不要同时传入 `--managed-db`、`--database-url`、`--public-ip`、`--port` 或 `--no-start`。
原服务须正常运行；外部数据库模式还要求本机已有兼容服务端版本的 `pg_dump`、`pg_restore`。

安装器先校验发行包和旧版本、只读检查数据库迁移历史，再停止服务，在
`/var/backups/brocade-upgrades/<时间>-<进程号>/` 保存压缩备份：`installation.tar.gz` 包含旧安装
和配置，`data.tar.gz` 包含密钥及持久数据。受管模式使用停服后的完整数据冷备份；外部数据库
另存内部启用 gzip 6 级压缩的定制格式 `database/database.dump`，可由 `pg_restore` 直接读取。
随后执行缺少的前向迁移，最后原子替换
Console/launcher 并检查入口就绪。升级存在停服窗口；数据较大时，备份时间也会增加。

迁移开始前失败会恢复旧服务；尚未修改 schema 的安装若在切换后失败，也会恢复旧安装。
一旦写入 `database/migration-started`，失败后会停止服务并保留现场，不自动运行可能不兼容的
旧程序，也不自动恢复数据库覆盖新写入。保留新发行包与备份目录，核对迁移历史后重试或按
[部署恢复流程](../.agents/runbooks/console-deployment.md#console-二进制回滚)处理。
备份目录包含秘密，应按数据库凭据保管；tar/逻辑备份的目录校验不等于已在目标环境完成恢复演练。

升级 Console 不会自动升级任何机器上的 Agent 或 Xray；它们通过「发布」页的发布单更新，见[模型、修订与发布](concepts.md#agent-与-xray-发布单)。

v0.3.3 的强制升级验收基线为官方 v0.3.1，覆盖外置 PostgreSQL 和受管 PostgreSQL、旧账户登录、
配置/密钥/额度/用量保留、压缩备份恢复、重复升级及真实 systemd 重启。安装器不会自动发布节点
Agent/Xray。真实验收与证据要求见[升级验收](../scripts/tests/upgrade/README.md)。

Console 内嵌全部数据库迁移，兼容库无需逐个启动中间版本。官方 v0.3.1 使用
`0001` 基线；预检逐条核对既有迁移的版本、成功状态和 SHA-384，拒绝未知校验值、失败历史、
空库误升级和数据库版本高于程序的降级。不支持 v0.3.1 之前的历史转换、自行改库或
PostgreSQL 大版本转换，不会改写 SQLx 校验值。

手动部署可使用新 Console 的维护命令进行只读预检；它们沿用 `DATABASE_URL`，不创建库，
不启动 HTTP 或节点发布：

```sh
brocade-console database-check
# 停止 Console 写入、备份并核验后，才执行：
brocade-console database-migrate
```

只读预检核对迁移回执，不承诺探测所有人工修改的结构；不兼容 DDL 仍会使正常迁移失败。
手动部署的进程管理、数据库与密钥备份由操作者按[手动部署](manual-deployment.md)完成。

## 备份

使用托管 PostgreSQL 时，`/var/lib/brocade` 保存数据库数据和密封密钥 `secret-key`。停止服务后完整复制该目录：

```sh
sudo systemctl stop brocade
sudo tar -C /var/lib -czf brocade-backup.tar.gz brocade
sudo systemctl start brocade
```

备份包含密钥，应按凭据保管。使用已有 PostgreSQL 时，用 `pg_dump --format=custom --compress=6` 保存压缩数据库备份；若为 Console 配置了 `BROCADE_SECRET_KEY`，与数据库备份一同保管。丢失密钥后，已密封的 DNS 凭据和证书私钥无法恢复。
