# brocade

brocade 是面向自建多地域网络的声明式控制系统。它以统一模型描述节点、链路、代理组件、路由规则与用户授权，并将模型编译为各节点所需的配置产物。控制面不向节点发送一次性操作命令；节点上的 agent 持续获取期望状态、观测本机状态，并使二者收敛。

该设计主要关注三个性质：配置生成的确定性、发布过程的可审计性，以及控制面与节点失联时的数据面自治。brocade 适合需要集中管理多台网络节点，同时要求变更可预览、可分批发布并可追溯的场景。

> brocade 会生成网络服务配置，并由 agent 在节点上执行具有系统权限的收敛操作。将其用于生产环境之前，应当独立审查生成的配置、访问控制和部署边界。

## 设计概述

### 确定性编译

系统以一份完整的模型快照作为编译输入。编译过程不访问数据库、文件系统或外部服务；在版本与输入相同的条件下，输出产物按字节保持一致。核心数据流如下：

```text
ModelSnapshot → 中间表示（IR）→ 节点产物 → 期望状态 → Agent 收敛
```

基准产物作为测试夹具纳入版本控制。编译器发生修改时，测试会直接呈现产物差异，从而区分预期的语义变化与非预期回归。相同机制也用于发布前预览和历史修订的重新编译。

### 模型与拓扑

一个快照包含节点、用户、外部出口和应用。节点记录地址、密钥及运行参数；应用包含链、入口、转发步骤、规则与授权。链路路径不作为独立状态重复存储，而是由入口位置和逐跳转发规则推导得到。由此，拓扑成为规则的确定性结果，避免两套表示之间产生一致性偏差。

### 修订与发布

编辑操作首先写入草稿，提交后形成不可变修订。一次界面操作可以原子地修改多张关系表，而不必暴露中间状态。发布以特定修订为输入，并按影响范围划分为若干波次；包含破坏性操作的波次必须由操作者确认后才会继续。

配置发布与授权发布相互独立。前者可能改写配置并重启服务，后者只更新运行中的访问主体，因此具有不同的风险和执行代价。回滚不会修改既有修订，而是以历史修订为目标创建一次新的发布，以保留完整的因果记录。

### 节点自治与观测

agent 接收期望状态，而非待执行的命令序列。它将期望状态与本机实际状态比较，仅在存在偏差时执行操作，并将收敛结果回报控制面。最近一次期望状态会保存在节点本地，因此控制面暂时不可达时，节点仍能发现并修正本机漂移。

除配置收敛外，agent 还负责上报用量与运行指标、执行链路探测，并给出路径 MTU 建议。用量按固定窗口累计，配额状态的变化会触发相应的授权调整。机器观测以 30 秒窗口保存：磁盘面板针对 agent 状态目录所在文件系统，区分容量、inode、块设备吞吐、IOPS、完成延迟、队列与 I/O PSI；网络面板除连接与内核错误外，还按真实匿名端口范围估算最繁忙目标的出站端口压力。目标地址只在节点内参与聚合，不会上报控制面。

实时网卡速率是第三条独立通道：Agent 主动维持到控制面的 WebSocket，无浏览器查看时只保活、不采样；查看机器或机器总览时，控制面下发临时租约并按全局 1/2/5 秒配置采样，最后一个查看者离开 15 秒后停止。控制面只保留每台机器最近 120 秒、最多 600 点的内存环，进程重启即可丢失，不写数据库、不进入离线重放，也不改变诊断和用量的 30 秒口径。浏览器仅通过控制面的 SSE 读取数据，永远不连接 Agent，也不会收到节点地址或节点令牌。

节点日志默认有界：设置页配置全局上限（默认 100 MiB），机器可单独覆盖；清除覆盖后会继续继承全局值。Agent 每轮轮询直接取得最终值，不需要创建修订或发布线路。Agent 使用独立 journald namespace；Agent 拉起的 Xray 与每个 Phantun 实例分别写入 `$BROCADE_AGENT_STATE_DIR/logs`，每个日志项的当前段与前一段合计不超过生效上限。降低上限会在线截断已有分段，不重启 Xray/Phantun。查看 Agent 日志使用 `journalctl --namespace=brocade-agent -u brocade-agent`。不要删除仍被进程打开的日志来释放空间；有界 sink 会自行滚动，旧版 `/tmp/brocade-agent-*.log` 会在对应进程完成一次受控重启后移除。

## 代码结构

| 组件                 | 职责                                                       |
| -------------------- | ---------------------------------------------------------- |
| `brocade-core`       | 纯函数编译器：模型快照 → IR → 节点产物                     |
| `brocade-store`      | PostgreSQL 持久化：修订、草稿、发布、凭据、配额与用量      |
| `brocade-console`    | 控制台 API、节点 API，以及内嵌的 Web 控制台与 agent 发行物 |
| `brocade-deployment` | 发布计划和控制面—节点协议类型                              |
| `brocade-agent`      | 节点侧收敛、观测、探测与用量采集                           |
| `brocade-probe`      | Agent 与控制面共用的临时 Xray 客户端和端到端拨测执行器     |
| `brocade-preview`    | 基于 Docker 的本地集群预览环境                             |
| `frontend`           | React + Vite 控制台                                        |
| `components/xray-core` | Brocade Xray fork 源码；当前钉在官方 `v26.4.25` 基线     |

生产构建会把前端资源以及 `x86_64`、`aarch64` 两种架构的静态 Agent 和 Brocade Xray 一并嵌入 `brocade-console`。因此，控制面部署只需分发一个二进制文件，前端、API 与节点发行物也不会因独立部署而发生版本漂移。

## 构建与验证

### 前置条件

仓库通过 [`rust-toolchain.toml`](rust-toolchain.toml) 固定 Rust 工具链和 agent 所需的 musl targets。完整构建还需要：

- Node.js 22 与 npm，用于构建前端；
- Go 1.26，用于从仓库内源码构建 Brocade Xray；
- Zig 0.16，用于交叉编译静态 agent；
- Docker，用于执行 PostgreSQL 集成测试；
- 目标平台的交叉链接器，仅在控制面本身需要交叉编译时使用。

构建脚本会在开始阶段检查必要工具，并在缺失时报告对应的安装方式。标准发行构建为：

```sh
cargo build --release --locked -p brocade-console
```

输出位于 `target/release/brocade-console`。构建脚本会执行 `npm ci` 与前端生产构建，并分别生成两种架构的 Agent 和 Brocade Xray。若前端已由其他流水线构建，可通过绝对路径指定待嵌入目录，从而跳过 npm：

```sh
BROCADE_CONSOLE_ASSETS_DIR=/absolute/path/to/frontend/dist \
  cargo build --release --locked -p brocade-console
```

### 测试

基础检查与默认测试集如下：

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked

cd frontend
npm ci
npm run lint
npm test
npm run build
```

PostgreSQL 集成测试由 testcontainers 启动临时数据库，必须显式执行被忽略的测试：

```sh
BROCADE_RUN_PG_TESTS=1 \
  cargo test -p brocade-store --test pg_integration --locked -- --ignored

BROCADE_RUN_PG_TESTS=1 \
  cargo test -p brocade-console --test http_integration --locked -- --ignored
```

## 部署 brocade-console

以下示例假定：构建主机为 `x86_64` Linux，目标服务器为 `aarch64` Ubuntu，并使用 PostgreSQL、systemd 与 Nginx。域名、账号和密码均为占位符，部署时必须替换。若目标架构与构建主机一致，可省略交叉编译相关设置。

### 1. 交叉编译控制面

在构建主机安装目标标准库与交叉链接器：

```sh
rustup target add aarch64-unknown-linux-gnu
sudo apt-get update
sudo apt-get install gcc-aarch64-linux-gnu
```

随后构建控制面：

```sh
CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
  cargo build --release --locked \
  --target aarch64-unknown-linux-gnu \
  -p brocade-console
```

产物位于：

```text
target/aarch64-unknown-linux-gnu/release/brocade-console
```

`brocade-console` 的构建脚本还会生成两种架构的静态 agent。对构建目录执行清理后，这些内嵌产物需要重新编译，因此完整构建所需时间通常明显长于普通 Rust crate。

### 2. 准备系统账号与 PostgreSQL

建议使用无登录权限的独立系统账号运行服务：

```sh
sudo useradd --system --no-create-home \
  --home-dir /opt/brocade \
  --shell /usr/sbin/nologin brocade
sudo install -d -o root -g root -m 0755 /opt/brocade
```

在 PostgreSQL 中创建专用角色和数据库。`createuser --pwprompt` 会交互式读取密码，避免将密码写入 shell 历史：

```sh
sudo -u postgres createuser --pwprompt brocade
sudo -u postgres createdb --owner=brocade brocade
```

数据库迁移会在控制面启动时自动执行。若数据库不存在且角色具有 `CREATEDB` 权限，控制面也可以自动创建数据库；生产环境通常更适合预先创建数据库，并遵循最小权限原则。

### 3. 安装控制面拨测所需的 Xray

用户页的「授权验证」由控制面使用当前 Serving 中的真实用户授权发起，因此控制面主机也必须安装与机队版本一致的 Brocade Xray。它只作为短生命周期的客户端执行，不运行常驻 Xray 服务。源码固定在仓库的 `components/xray-core/`，当前基线为 `v26.4.25`；部署流程不再下载社区 Xray。以下命令在构建机生成 `aarch64` 产物：

```sh
mkdir -p target/brocade-xray
BROCADE_COMMIT=$(git rev-parse --short=7 HEAD)
git status --porcelain | grep -q . && BROCADE_COMMIT="${BROCADE_COMMIT}-dirty"
cd components/xray-core
CGO_ENABLED=0 GOOS=linux GOARCH=arm64 GOTOOLCHAIN=local \
  go build -mod=readonly -trimpath -buildvcs=false -gcflags=all=-l=4 \
  -ldflags="-X github.com/xtls/xray-core/core.build=${BROCADE_COMMIT} -s -w -buildid=" \
  -o ../../target/brocade-xray/xray-aarch64 ./main
cd ../..
target/brocade-xray/xray-aarch64 version | head -n 1
```

`x86_64` 构建将 `GOARCH` 改为 `amd64`，输出名改为 `xray-x86_64`。`brocade-console/build.rs` 会自动构建并内嵌这两个架构；上面的独立产物用于安装控制面自己的拨测 Xray，也可通过 `BROCADE_XRAY_BIN_X86_64` / `BROCADE_XRAY_BIN_AARCH64` 复用给 Console 构建。

### 4. 安装二进制与环境文件

先将构建产物上传至目标服务器的临时目录，再安装到 `/opt/brocade`：

```sh
scp target/aarch64-unknown-linux-gnu/release/brocade-console \
  deploy@console.example.net:/tmp/brocade-console

ssh deploy@console.example.net \
  'sudo install -o root -g root -m 0755 /tmp/brocade-console /opt/brocade/brocade-console'
```

以 [`console.env.example`](console.env.example) 为依据创建 `/opt/brocade/console.env`。以下配置采用单一监听端口同时承载控制台 API 与节点 API，因此有意不设置 `BROCADE_AGENT_BIND`：

```dotenv
DATABASE_URL=postgres://brocade:CHANGE_ME@127.0.0.1:5432/brocade
BROCADE_ADMIN_BIND=127.0.0.1:8080
BROCADE_AGENT_PUBLIC_URL=https://console.example.net
BROCADE_XRAY_VERSION=v26.4.25
BROCADE_PROBE_XRAY_BIN=/opt/brocade/libexec/xray
BROCADE_PROBE_RUNTIME_DIR=/run/brocade/probes
```

环境文件包含数据库密码及密封密钥，应在写入任何内容之前将权限限制为 root 可读。随后在目标服务器上直接生成并追加 `BROCADE_SECRET_KEY`，避免密钥经过构建主机、终端输出或复制过程：

```sh
sudo install -o root -g root -m 0600 /dev/null /opt/brocade/console.env
sudoedit /opt/brocade/console.env
sudo sh -c 'printf "BROCADE_SECRET_KEY=%s\n" "$(openssl rand -base64 32)" >> /opt/brocade/console.env'
sudo chown root:root /opt/brocade/console.env
sudo chmod 0600 /opt/brocade/console.env
```

该密钥用于密封 DNS 凭据和证书私钥；丢失后，数据库中的既有密文无法恢复，因此应与数据库备份一同保管。对已有部署重新执行生成命令会改变密钥，使既有密文失效；该步骤只应在首次部署或明确执行密钥轮换时运行。

若数据库密码包含 `@`、`:`、`/` 等 URI 保留字符，必须先对用户名或密码部分进行百分号编码，再写入 `DATABASE_URL`。

若需要分别限制控制台流量与节点流量，可以设置 `BROCADE_AGENT_BIND`，并为第二个监听地址配置独立的反向代理入口。其余环境变量及留空时的行为见 [`console.env.example`](console.env.example)。

### 5. 配置 systemd

创建 `/etc/systemd/system/brocade-console.service`：

```ini
[Unit]
Description=Brocade Console
After=network-online.target postgresql.service
Wants=network-online.target

[Service]
Type=simple
User=brocade
Group=brocade
WorkingDirectory=/opt/brocade
EnvironmentFile=/opt/brocade/console.env
Environment=BROCADE_CACHE_DIR=/var/cache/brocade
ExecStart=/opt/brocade/brocade-console
Restart=on-failure
RestartSec=5s
TimeoutStopSec=90s
UMask=0077
CacheDirectory=brocade
RuntimeDirectory=brocade
RuntimeDirectoryMode=0700
NoNewPrivileges=true
PrivateTmp=true
ProtectHome=true
ProtectSystem=strict

[Install]
WantedBy=multi-user.target
```

控制面会在 `BROCADE_CACHE_DIR` 中保存 GeoIP 数据库缓存。`CacheDirectory=brocade` 由 systemd 创建并授予服务账号写权限，因此无需放宽 `/opt/brocade` 的文件权限。授权拨测的临时配置包含真实用户凭据，只会以 `0600` 写入 `RuntimeDirectory` 下的 `probes` 子目录，任务结束后删除；该目录本身由控制面收紧为 `0700`。

加载并启动服务：

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now brocade-console
sudo systemctl status brocade-console
sudo journalctl -u brocade-console -n 100 --no-pager
```

首次启动时应核对日志中实际连接或自动创建的数据库名称，避免因 `DATABASE_URL` 拼写错误而得到一个新的空数据库。

### 6. 配置 Nginx 与 TLS

控制面应只监听回环地址，由 Nginx 负责公网 TLS 终止。将以下配置写入 `/etc/nginx/sites-available/brocade`。示例假定证书已安装于 `/etc/letsencrypt/live/console.example.net/`：

```nginx
map $http_upgrade $connection_upgrade {
    default upgrade;
    ''      close;
}

server {
    listen 80;
    server_name console.example.net;
    return 301 https://$host$request_uri;
}

server {
    listen 443 ssl;
    server_name console.example.net;

    ssl_certificate /etc/letsencrypt/live/console.example.net/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/console.example.net/privkey.pem;

    location / {
        proxy_pass http://127.0.0.1:8080;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
        # Agent 实时遥测使用出站 WebSocket；浏览器实时视图使用 SSE。两者都经过控制面，
        # 浏览器不会连接或获知节点地址。
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection $connection_upgrade;
        proxy_read_timeout 1h;
    }
}
```

启用配置前必须先验证语法：

```sh
sudo ln -s /etc/nginx/sites-available/brocade /etc/nginx/sites-enabled/brocade
sudo nginx -t
sudo systemctl reload nginx
```

若域名经过 Cloudflare 等代理，应使用可验证的源站证书，并启用严格的端到端 TLS 验证（Cloudflare 对应 `Full (strict)`），而不应依赖不校验证书的兼容模式。

### 7. 更新与回滚准备

升级前应备份数据库，并记录当前二进制的 SHA-256：

```sh
sudo install -d -o postgres -g postgres -m 0700 /var/backups/brocade
sudo -u postgres pg_dump --format=custom \
  --file=/var/backups/brocade/before-upgrade.dump brocade
sha256sum /opt/brocade/brocade-console
/opt/brocade/libexec/xray version | head -n 1
```

新二进制应先上传到临时路径，再原子替换并重启服务。上传阶段不会中断现有进程：

```sh
scp target/aarch64-unknown-linux-gnu/release/brocade-console \
  deploy@console.example.net:/tmp/brocade-console.new

ssh deploy@console.example.net '
  set -eu
  sudo install -o root -g root -m 0755 \
    /tmp/brocade-console.new /opt/brocade/brocade-console.new
  sudo mv /opt/brocade/brocade-console.new /opt/brocade/brocade-console
  sudo systemctl restart brocade-console
  sudo systemctl --no-pager --full status brocade-console
'
```

迁移只会在启动时向前执行。常规升级不应通过删除数据库来“重建”状态；若新版本涉及不可逆迁移，应在部署前验证备份可恢复性，并准备与数据库版本相匹配的旧二进制。

升级后除服务状态外，还应确认本地拨测执行器可用：

```sh
sudo systemctl is-active brocade-console
/opt/brocade/libexec/xray version | head -n 1
sudo journalctl -u brocade-console -n 100 --no-pager
curl --fail https://console.example.net/healthz
```

登录控制台后，用户页「授权验证」不应显示“Console 未安装或无法执行拨测 Xray”。拨测使用真实用户凭据和真实完整链路，产生的少量流量会正常计入该用户用量；浏览器只提交 Serving 条目的不透明 ID，不能指定目标地址或凭据。

常用诊断命令如下：

```sh
sudo systemctl status brocade-console
sudo journalctl -u brocade-console -f
sudo -u postgres psql brocade
```

## 本地前端开发

前端开发服务器与生产构建分别使用：

```sh
cd frontend
npm ci
npm run dev
```

```sh
cd frontend
npm run build
```

生产控制面默认使用编译时内嵌的前端。仅在本地迭代或紧急替换静态资源时，才应通过 `BROCADE_CONSOLE_DIST=/absolute/path/to/frontend/dist` 改为运行时读取目录；这种模式无法保证页面与 API 来自同一次构建。

## 许可证

本项目采用 Apache License 2.0，详见 [LICENSE](LICENSE)。
