# 接入与出口

接入保护、TCP 保活、外部隧道与出口拨测。VPN Gate 出口见 [VPN Gate](vpngate.md)。

## REALITY 回落限速

新建 VLESS REALITY 接入及启用 AnyTLS REALITY 时，fallback 限速默认使用「严格」：上传超过 256 KiB 后约 64 KiB/s，下载超过 1 MiB 后约 256 KiB/s；编译时按入口生成稳定的 ±10% 扰动。该限制只作用于未通过 REALITY 验证的回落连接，不限制已认证用户的代理吞吐。修改其他字段不会覆盖已有明确选择；存量入口需要通过草稿和配置发布切换档位，历史快照中缺失的策略仍按原来的关闭语义读取。

## TCP 保活

受管 TCP 入站（包括 VLESS、AnyTLS、MTProxy 和中继）在节点配置中显式开启 TCP keepalive：空闲 300 秒后探测，无响应时每 90 秒重试，连续 3 次失败后由内核关闭连接；探测次数通过 Linux `customSockopt` 固定，不依赖宿主机 sysctl。健康的空闲连接仍可复用，不因 5 分钟没有业务流量而被保活机制主动关闭。纯空闲连接失联的最坏检测时间约为 570 秒，之后还需 Agent 上报和页面刷新；已有未确认数据时由 TCP 重传或单独设置的 `TCP_USER_TIMEOUT` 决定，不承诺所有场景的固定离线时限。API、Hy2/QUIC、出站和客户端订阅不受此策略影响。新渲染的配置需经配置发布生效，已建立的旧连接不保证获得新参数；不会通过授权同步自动修改，也不为此重启全部节点。

保活优先于 MPTCP：旧内核 MPTCP（例如 Linux 5.15）无法应用保活配置时，Xray 在新建监听时回退普通 TCP，包括显式开启 `tcpMptcp` 的配置；普通 TCP 也无法应用时明确报错，不带着失效保活继续服务。Linux 启动与 FD 交接会回读校验实际参数；交接保留已回退的真实 TCP 协议、身份与配置摘要，不修改共享监听来掩盖不一致。因此本功能需要更新 Xray 并发布新配置，不能只更新渲染配置。经过 CDN 或反向代理的连接，TCP 保活也只能确认直接 TCP 对端的可达性，不能代替终端应用心跳。

## WARP

WARP 使用 Xray MASQUE 出站，默认 HTTP/3（UDP/443），也可在隧道参数中选择 HTTP/2（TCP/443）；两者都承载 TCP/UDP 业务，不自动互相回退。Endpoint 默认 `162.159.198.2:443`，TLS SNI 固定为 `consumer-masque.cloudflareclient.com`；设备使用 P-256 密钥认证并校验注册返回的服务端公钥。隧道内 DNS 默认 `1.1.1.1`、`1.0.0.1`，域名目标通过 WARP 查询。

申请流程仍是“创建并提交隧道 → 按机器同意条款并申请身份 → 发布”。注册适配器先创建临时设备，再登记 MASQUE 公钥；登记失败会注销临时设备。密钥和设备令牌加密保存，草稿预览不调用 Cloudflare。WARP 不再接受 WireGuard 的 MTU、Keepalive、Allowed IPs、TUN、Workers 和 reserved 参数；旧 WG 身份不能复用，需重新申请。此变更不兼容旧 WARP 模型和绑定数据，升级已有安装前须完成旧绑定清理与重新注册；运行时仅接受 MASQUE 格式，不转换旧快照。已有安装应先备份、停止 Console 写入，再使用 `scripts/migrate-warp-checkpoints.py` 审计并以 `--apply` 迁移未绑定、未引用的旧默认配置；脚本原子更新存储格式和订阅摘要，保留修订编号及运行引用。携带旧 WG 身份或引用旧隧道的数据会拒绝迁移，必须先显式重新注册并发布。先发布支持 MASQUE 的 Brocade Xray，再发布使用它的配置。

## 外部隧道标识

外部隧道的资源 ID 是不承载业务语义的随机标识：手工隧道使用 `custom-xxxx-xxxx`，WARP 使用 `warp-xxxx-xxxx`，VPN Gate 地区池或固定节点使用 `vpngate-xxxx-xxxx`，其中 `x` 是小写十六进制。地区、节点、租户和展示名称只保存在各自字段中，旧前缀或语义化 ID 不再接受。

## 外部隧道证书验证

TLS 隧道默认验证服务端证书。导入 AnyTLS、VLESS TLS 或 HTTPS 代理时，`insecure=1` / `allowInsecure=true` 会保存为该隧道的 `security.v.allow_insecure`；也可在手动编辑的 TLS 扩展中切换「跳过证书验证（insecure）」。显式开启后，节点配置与 Console 拨测均生成 `tlsSettings.allowInsecure: true`，前置隧道订阅生成 `skip-cert-verify: true`。XHTTP 独立下载的 TLS 设置可以单独开启。缺省、`0` 和 `false` 均保持证书验证。

AnyTLS 链接支持 `peer` 作为 `sni` 的别名、`fingerprint` 作为 `fp` 的别名，并接受 `udp=1` / `udp=true`；AnyTLS 原生承载 UDP over TCP，不额外生成 UDP 开关。别名冲突或布尔值无效时会明确拒绝导入。

本功能需要配套的 Brocade Xray，内置 fork 保留显式 `allowInsecure`，不受上游的日期停用逻辑影响。升级时先发布新版 Xray，再发布开启 insecure 的隧道配置；只更新 Console 不会升级节点上的 Xray。开启 insecure 会跳过服务端证书身份验证；现有证书钉扎与名称约束仍按 Xray 的独立规则生效。

## 隧道目标域名解析

手动隧道的编辑设置支持「目标域名解析」，导入分享链接时也可选择。默认「远端解析（AsIs）」把目标域名原样交给代理服务器；旧隧道缺失此字段时保持这一行为。选择本地策略后，Brocade 节点先解析目标域名，再把 IP 交给隧道。这是出站顶层 `targetStrategy`，不控制 AnyTLS 等隧道服务器自身地址的解析，也不能要求远端优先选择 IPv4。

WireGuard 的 `AsIs` 表示由其自身的域名策略处理目标，不表示远端 DNS；原有 WireGuard 协议内域名策略继续独立配置。

`UseIPv4v6` 先查询 IPv4，解析报错或无结果时再查询 IPv6；已经取得 IPv4 但连接失败时不会回退 IPv6。`UseIPv6v4` 顺序相反。`UseIP` 查询双栈，`UseIPv4` / `UseIPv6` 仅查询相应地址族；这些 `Use` 策略在解析失败时可回退到系统解析，并非强制限制地址族。

修改随隧道一起保存到草稿，经配置发布后在引用它的节点生效；历史修订和回滚保留该选项，Console 隧道拨测使用相同配置。前置隧道的客户端订阅仍使用客户端自身的解析设置，不继承此节点侧选项。

此策略控制发送给隧道的目标 IP，不保证远端最终出口使用同一地址族。远端仍可能根据 HTTP Host、TLS SNI 或自身代理链重新选择目标和出口。Console 公共落点会提前解析、验证并固定公网 IP，拨测显示的出口 IP 是远端响应中的观测值，不能单独据此判断本地解析策略是否执行。

`targetStrategy` 只处理目标域名。客户端已经提供 IP 字面量时不会重新解析或跨地址族转换；例如 `UseIPv4` 不会把一个已经指定的 IPv6 目标改为 IPv4。Console 拨测在固定 IP 之前，单独按该策略选择已验证的公网地址：IPv4 档优先选 A 结果，IPv6 档优先选 AAAA 结果，所选地址族无结果时允许 `Use` 策略回退。IP 字面量保持原地址族；缺省、AsIs 和 UseIP 保持原有默认选择。固定后的 IP 不会再次经过 Xray 的域名解析分支。

## 出口拨测

用户页的「授权验证」会使用当前 Serving 中的真实用户授权发起短生命周期拨测；隧道详情页的手动拨测可选择当前 Serving 或点击时的完整草稿，设置页的定时监测则始终只列出并监测 Console 能执行的 Serving 出口。它们都由 Console 机器发起，并强制流量只经过所选外部出口。Serving 任务引用不可变发布快照；草稿任务只把所选出口加密冻结到任务中，并在任务结束时清除密文，不会创建模型修订。公共落点和超时同样在入队时冻结，结果与定时策略持久化到 PostgreSQL；Console 重启或多实例抢占不会重复执行同一任务。草稿结果会进入最近记录，但不参与 Serving 健康、趋势或告警。定时任务采用稳定抖动且不补跑错过的周期，记录保留 7 天。WARP 因身份按节点分配，在 Console 拥有独立身份前不会出现在定时监测列表；VPN Gate 由引用它的节点 Agent 实拨，同样只在自己的观测页展示。

公共落点只支持明文 HTTP。Console 会先解析并拒绝回环、私网、链路本地、文档和其它非公网地址，再把已批准的 IP 固定写入 SOCKS 请求，避免 DNS 重绑定；原始主机名仅保留在 HTTP `Host` 头。Console 会把本次构建内嵌的对应架构 Brocade Xray 校验并原子写入 `BROCADE_CACHE_DIR`，无需再单独安装。`BROCADE_PROBE_XRAY_BIN` 只保留为显式运维覆盖；一旦设置，路径或版本错误会直接报告，不会静默回退。
