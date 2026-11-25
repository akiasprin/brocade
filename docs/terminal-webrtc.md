# 实例终端 WebRTC 直连

终端有两条已实现路径：浏览器与宿主 Agent 之间的 WebRTC DataChannel，以及原有的 Console WSS 转发。直连 WSS 暂缓，不需要给每台宿主配置 HTTPS 域名或证书。Shell 仍由实例的 CubeShim 持有，连接方式不改变实例权限和会话归属。

## 连接和恢复

首次打开使用 Console 转发。在终端“⋯”菜单点击“尝试直连”，浏览器才开始收集地址和连通性检查；浏览器要求本地网络授权时，由用户完成授权。不申请摄像头或麦克风权限。成功后仅在当前浏览器按账号和实例记住选择，后续连接自动尝试。没有成功或权限被拒绝时不保存选择。生产入口需要 HTTPS；普通局域网 HTTP 页面使用 Console 转发。

一次尝试只创建一个终端附件：先协商连接，再选用 RTC 或 Console。浏览器候选收集最多 2.2 秒，Agent STUN／SDP 准备最多 5 秒，随后连接检查最多 8 秒；准备或穿透失败则在同一次附件请求中使用 Console。候选交换仅经过已登录、同源的 Console WSS，并绑定一次性票据、操作者、实例运行身份和所属节点。浏览器不能指定另一台节点、runtime 或 STUN 服务。

直连成功后，终端输入、输出、窗口尺寸和字节 ACK 都走可靠、有序、加密的 DataChannel。Console WSS 保持连接，承担鉴权、续租和撤销；STUN 不转发终端数据。稳定连接不自动迁移。菜单显示实际连接路径，支持手动重新检测或切回 Console。

浏览器和 Agent 每秒分别通过 Console WSS 续租。Console 只为最近复核过权限和实例身份、且两秒内收到浏览器续租的附件发放 Agent 租约。Agent 租约为三秒，两段合计最长五秒；浏览器自身也以五秒为上限。两端均以探测发出时刻计算截止，迟到或重复的响应不能重启租期；到期停止接收新输入并分离，保留 Shell。浏览器后台冻结后恢复时，也先检查截止再发送输入。显式的撤销或连接关闭立即分离。

已选用 RTC 后发生故障，会关闭该附件，下一次恢复使用 Console、新票据和相同 Shell 身份，从 Shim 恢复画面；不会在旧传输上重发输入，也不把断线期间的按键排队。明确的权限／身份拒绝仍是失败，不会通过切换路径绕过。结束 Shell 仍需要显式“结束会话”，或原有生命周期及宿主策略撤销。

## 推荐部署：Cloudflare 公共 STUN

Cloudflare 的公共地址为 `stun:stun.cloudflare.com:3478`，[官方 FAQ](https://developers.cloudflare.com/realtime/turn/faq/)声明 STUN 免费。它与普通橙云 CDN 是独立服务。Console 的 HTTPS／WSS 可以继续走 CF CDN，无需公开 Console 源站地址、增加 STUN 子域名或自建 UDP 3478 服务。

在 Console 已有的受限环境文件中设置：

```dotenv
BROCADE_TERMINAL_STUN_SERVERS=stun:stun.cloudflare.com:3478
```

该变量仅在容器版启用，最多两个 `stun:主机名:端口`，也接受带方括号的 IPv6 地址。空值表示只尝试本地候选，不内置其他公共服务；不接受 `turn:`、凭据、路径或查询参数。配置无效时拒绝启动，避免静默使用另一服务。

在支持容器终端的宿主 Agent 服务中，配置一个实际属于该宿主、浏览器可能访问的本地 IP。例如下面地址仅为示例，部署时必须替换：

```ini
# /etc/systemd/system/brocade-agent.service.d/40-terminal-webrtc.conf
[Service]
Environment="BROCADE_AGENT_TERMINAL_WEBRTC_BIND=192.168.10.20:30000"
```

Agent 使用该地址的 UDP 30000–30005，每个附件占用一个端口。显式地址限制了向浏览器公布的本地接口；不接受 `0.0.0.0` 或 `::`。未设置则不宣告 RTC 能力，浏览器保留原有转发。测试允许端口 0，生产建议使用固定端口池。首版每台节点选择一个接口／地址族；IPv6 可使用 `[实际IPv6]:30000`，暂不同时公布多个接口。

部署顺序为 Console、宿主 Agent，然后打开终端测试。将 drop-in 安装到选定节点后，按节点维护流程执行 `systemctl daemon-reload` 和 Agent 重启；Console 按已有安装／升级流程读取环境配置。升级前后都保留宿主原有私有配置，不覆盖整个服务文件。本页配置示例不会自动修改主机或启动服务。

网络需要满足：

- 浏览器与 Agent 都能访问 `stun.cloudflare.com` 的 UDP 3478；使用 DNS 名称，不固定 Cloudflare IP。
- 同网直连时，宿主防火墙允许浏览器来源访问所选地址的 UDP 端口池。
- 跨网时，ICE 尝试 NAT 映射与穿透。STUN 不能保证穿透对称 NAT、严格防火墙或禁止 UDP 的网络；失败仍使用 Console WSS，无需为首版强行开放公网端口转发。
- Console 的 WSS 路由、同源校验和 Cookie 配置按[实例终端](cvm.md#实例终端webshell)保持可用。多 Console 进程仍需把同一节点相关连接路由到同一进程。

Cloudflare 会看到参与地址发现的客户端公网地址。STUN 不带终端内容，不等于 TURN 中继；当前实现没有配置或调用 TURN。Cloudflare 公共服务的实际可达性取决于浏览器与宿主所在网络；一个开发节点的成功不代表生产网络已验收。

RTC 依赖仅进入容器版 Agent，使用 str0m 的 RustCrypto 提供者并关闭示例 HTTP 服务和 OpenSSL。当前上游的证书生成链仍通过 dimpl／rcgen 引入 AWS-LC，因而不是完全没有原生编译依赖；双架构 musl 构建需要原有 Zig 工具链。发布时应记录实际二进制增量、首轮构建耗时及运行开销，不把免费 STUN 等同于功能零成本。

## 自建或代理的区别

如果以后需要自行控制 STUN，可部署独立 STUN 服务并改上述 Console 变量。与 Console 同机也可以，但 STUN 的 UDP 入口须直接可达。普通橙云 CDN 不代理此 UDP；Free、Pro、Business 的 Spectrum 也不支持通用 UDP，需 Enterprise 的付费附加服务，见[套餐矩阵](https://developers.cloudflare.com/spectrum/protocols-per-plan/)。自建入口的 DNS-only 记录会公布它指向的公网 IP，且 CNAME 链不能再指向普通橙云代理地址。使用 Cloudflare 公共 STUN 无需这一自建步骤。

## 验收与回退

先在选定的浏览器与宿主同网测试：默认 Console，首次点击才尝试 RTC；确认路径显示“WebRTC 直连”，输入、中文、窗口调整与持续输出正常。再测试跨网，确认实际成功率与连接时间。仅看 SDP、STUN 查询成功或同一网段地址不足以判定直连成功，应以 DataChannel 建立及终端交互为准。

故障验收包含禁止 RTC UDP 后初始回退、建立后断开 RTC 的原 Shell 恢复、浏览器到 Console 与 Agent 到 Console 分别断开后最多 5 秒停止输入、权限撤销及重启／重装身份变化。未确认送达的输入不重放；新页面接管后旧页面也不能继续输入。实际 Guest Shell 的 PID、工作目录和变量保留须在部署目标验收。

本地可执行：

```sh
cargo test -p brocade-deployment --features cvm cvm::terminal
cargo test -p brocade-agent --features cvm cvm::terminal
cd frontend
npx vitest run tests/cvm/terminal-transport.test.tsx --config vitest.config.ts
```

真实 Chromium 与 Rust RTC 互通测试显式执行，不依赖 Guest 或公网；需先安装前端依赖和 Playwright Chromium。`BROCADE_TEST_CHROMIUM` 可指定本机测试浏览器路径：

```sh
cargo test -p brocade-agent --features cvm chromium_terminal_adapter_round_trip -- --ignored --nocapture
# 单独验证当前执行机器到公共 STUN 的 UDP 可达性，不代表另一台宿主的结果。
cargo test -p brocade-agent --features cvm cloudflare_stun_binding -- --ignored --nocapture
```

紧急停用 RTC 时，移除选定 Agent 的绑定配置并按维护流程重启；它重新注册后不再宣告 RTC 能力，后续全部使用 Console。清空 STUN 列表只去掉公网地址发现，同网 RTC 仍可能成功。数据库没有新增迁移；旧版终端 v2 在未协商独立 RTC 能力时继续使用原有消息，不接收 RTC／续租扩展。
