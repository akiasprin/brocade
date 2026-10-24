# 手动部署 Console

不使用安装器、以 systemd 与 Nginx 部署 Console 的完整示例。构建前置条件见[从源码构建](build.md)。

以下示例假定：构建主机为 `x86_64` Linux，目标服务器为 `aarch64` Ubuntu，并使用 PostgreSQL、systemd 与 Nginx。域名、账号和密码均为占位符，部署时必须替换。若目标架构与构建主机一致，可省略交叉编译相关设置。

## 1. 交叉编译控制面

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

## 2. 准备系统账号与 PostgreSQL

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

## 3. 安装二进制与环境文件

先将构建产物上传至目标服务器的临时目录，再安装到 `/opt/brocade`：

```sh
scp target/aarch64-unknown-linux-gnu/release/brocade-console \
  deploy@console.example.net:/tmp/brocade-console

ssh deploy@console.example.net \
  'sudo install -o root -g root -m 0755 /tmp/brocade-console /opt/brocade/brocade-console'
```

以 [`console.env.example`](../console.env.example) 为依据创建 `/opt/brocade/console.env`。以下配置采用单一监听端口同时承载控制台 API 与节点 API，因此有意不设置 `BROCADE_AGENT_BIND`：

```dotenv
DATABASE_URL=postgres://brocade:CHANGE_ME@127.0.0.1:5432/brocade
BROCADE_BOOTSTRAP_TOKEN=CHANGE_ME
BROCADE_ADMIN_BIND=127.0.0.1:8080
BROCADE_AGENT_PUBLIC_URL=https://console.example.net
BROCADE_CACHE_DIR=/var/cache/brocade
BROCADE_PROBE_RUNTIME_DIR=/run/brocade/probes
```

环境文件包含数据库密码及密封密钥，应在写入任何内容之前将权限限制为 root 可读。随后在目标服务器上直接生成并追加 `BROCADE_SECRET_KEY`，避免密钥经过构建主机、终端输出或复制过程：

```sh
sudo install -o root -g root -m 0600 /dev/null /opt/brocade/console.env
sudoedit /opt/brocade/console.env
sudo sh -c 'printf "BROCADE_SECRET_KEY=%s\n" "$(openssl rand -base64 32)" >> /opt/brocade/console.env'
sudo sh -c 'printf "BROCADE_BOOTSTRAP_TOKEN=%s\n" "$(openssl rand -base64 32)" >> /opt/brocade/console.env'
sudo chown root:root /opt/brocade/console.env
sudo chmod 0600 /opt/brocade/console.env
```

`BROCADE_SECRET_KEY` 用于密封 DNS 凭据和证书私钥；丢失后，数据库中的既有密文无法恢复，因此应与数据库备份一同保管。对已有部署重新执行生成命令会改变密钥，使既有密文失效；该步骤只应在首次部署或明确执行密钥轮换时运行。

`BROCADE_BOOTSTRAP_TOKEN` 只授权一次 `/auth/init`。首次打开页面时从这个 root 可读的环境文件复制该值；初始化成功后删除这一行并重启服务。直接运行 `brocade-console` 而不是 launcher 时，若未设置该变量，控制面仍提供健康检查和初始化页面，但初始化请求会以 503 失败。

若数据库密码包含 `@`、`:`、`/` 等 URI 保留字符，必须先对用户名或密码部分进行百分号编码，再写入 `DATABASE_URL`。

若需要分别限制控制台流量与节点流量，可以设置 `BROCADE_AGENT_BIND`，并为第二个监听地址配置独立的反向代理入口。其余环境变量及留空时的行为见 [`console.env.example`](../console.env.example)。

## 4. 配置 systemd

先创建 `/etc/systemd/system/brocade-console.socket`。这里的地址必须与 `BROCADE_ADMIN_BIND` 完全一致；默认的单监听模式同时承载 Console 与 Agent API：

```ini
[Unit]
Description=Brocade Console listener

[Socket]
ListenStream=127.0.0.1:8080
Accept=no
Backlog=4096
NoDelay=true
FileDescriptorName=admin
Service=brocade-console.service

[Install]
WantedBy=sockets.target
```

再创建 `/etc/systemd/system/brocade-console.service`：

```ini
[Unit]
Description=Brocade Console
Requires=brocade-console.socket
After=brocade-console.socket network-online.target postgresql.service
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

控制面会在 `BROCADE_CACHE_DIR` 中保存 GeoIP 数据库缓存。`CacheDirectory=brocade` 由 systemd 创建并授予服务账号写权限，因此无需放宽 `/opt/brocade` 的文件权限。授权与隧道拨测的临时配置包含真实凭据，只会以 `0600` 写入 `RuntimeDirectory` 下的 `probes` 子目录，任务结束后删除；该目录本身由控制面收紧为 `0700`。数据库只保存脱敏后的配置指纹与归类结果，不保存 Xray 原始日志。

全新安装时加载并启动 socket 和服务：

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now brocade-console.socket
sudo systemctl enable --now brocade-console.service
sudo systemctl status brocade-console
sudo journalctl -u brocade-console -n 100 --no-pager
```

systemd 持有监听 fd；正常 `restart brocade-console.service` 时，新连接在内核监听队列中等待新进程接管，旧进程仍按关闭期限完成手上的请求。不要在普通发布中重启或停止 `brocade-console.socket`。从旧的进程自绑定部署首次切换时，必须先停止旧服务释放端口，再启动 socket 和服务；这一次转换本身仍有短维护窗口。若设置了 `BROCADE_AGENT_BIND`，需要让 systemd 同时传入两个与配置完全相同的监听地址；Console 按本地地址识别它们，与 fd 顺序无关。

首次启动时应核对日志中实际连接或自动创建的数据库名称，避免因 `DATABASE_URL` 拼写错误而得到一个新的空数据库。

## 5. 配置 Nginx 与 TLS

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

    # Selected Agents gzip VPN Gate snapshots before upload. The application decompresses with a
    # separate 16 MiB ceiling and accepts this larger body only on the catalogue-report route.
    client_max_body_size 4m;

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

## 6. 更新与回滚准备

升级前应备份数据库，并记录当前二进制的 SHA-256：

```sh
sudo install -d -o postgres -g postgres -m 0700 /var/backups/brocade
sudo -u postgres pg_dump --format=custom --compress=6 \
  --file=/var/backups/brocade/before-upgrade.dump brocade
sha256sum /opt/brocade/brocade-console
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
sudo journalctl -u brocade-console -n 100 --no-pager
curl --fail https://console.example.net/healthz
```

登录控制台后，用户页「授权验证」与自定义隧道详情页「拨测」均不应显示“Console 未安装或无法执行拨测 Xray”。授权拨测使用真实用户凭据和真实完整链路，产生的少量流量会正常计入该用户用量；隧道拨测固定从 Console 发出。拨测 Serving 时浏览器只提交条目的不透明 ID；拨测草稿时浏览器提交现有草稿操作，服务端在授权事务中生成并冻结出口，浏览器仍不能直接指定探测落点或后台执行配置。

常用诊断命令如下：

```sh
sudo systemctl status brocade-console
sudo journalctl -u brocade-console -f
sudo -u postgres psql brocade
```
