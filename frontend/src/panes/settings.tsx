import { useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  fetchCerts,
  fetchBranding,
  saveCertDomain,
  saveBranding,
  scanCerts,
  fetchDistribution,
  fetchAgentLogPolicy,
  fetchLinkMtu,
  fetchSettings,
  saveDistribution,
  saveAgentLogDefault,
  saveNodeLogPolicy,
  saveSettings,
  createCertGroup,
  deleteCertGroup,
  requestSpareCertificate,
  serveCertificate,
  updateCertGroup,
  type CertsView,
  type BrandingSettings,
  type DistributionView,
  type AgentLogPolicyNode,
  type AgentLogPolicyView,
  type GroupCertificate,
  type NodeCertificateState,
  type LinkMtuItem,
  type ModelSettings,
} from '../api';
import { can, useSession } from '../session';
import { ErrorBox, Loading } from '../ui/bits';
import { BrandIcon } from '../ui/branding';
import { useNodeNames } from '../ui/node-name';
import { REALITY_FINGERPRINT_OPTIONS } from '../reality';

/* 空串表示不设置该项（服务端类型为 Option<T>），不应将空串作为 "" 提交 */
const text = (v: string | null) => v ?? '';
const orNull = (v: string) => (v.trim() === '' ? null : v.trim());

// 表单全部使用字符串：输入框的值本身即字符串，提前转换为数字或 null 会使
// 清空该字段与该字段取值为 0 无法区分。转换只在提交时执行一次。
type Form = {
  min: string;
  max: string;
  diff: string;
  dest: string;
  names: string;
  fp: string;
  flow: string;
  keepalive: string;
  mtu: string;
  ingressBase: string;
  hopBase: string;
  hy2Base: string;
  probeUrl: string;
  probeTimeout: string;
  probeInterval: string;
  geodataCron: string;
  geodataGeoip: string;
  geodataGeosite: string;
  connIdle: string;
  connUplink: string;
  connDownlink: string;
  connBuffer: string;
  connHandshake: string;
};

/* `Number(x) || 默认值` 在该组字段上不适用：0 是合法取值——上下行半关闭等待 0 秒表示
   对端关闭后立即关闭——而 `||` 会将其替换为默认值。只有空值和非数值才回退到默认值。 */
const numOr = (raw: string, fallback: number) => {
  const t = raw.trim();
  if (t === '') return fallback;
  const n = Number(t);
  return Number.isFinite(n) ? n : fallback;
};

const PROBE_URL_DEFAULT = 'http://cp.cloudflare.com/cdn-cgi/trace';
const GEODATA_CRON_DEFAULT = 'CRON_TZ=Asia/Shanghai 30 6 * * *';
const GEOIP_URL_DEFAULT = 'https://raw.githubusercontent.com/Loyalsoldier/v2ray-rules-dat/release/geoip.dat';
const GEOSITE_URL_DEFAULT = 'https://raw.githubusercontent.com/Loyalsoldier/v2ray-rules-dat/release/geosite.dat';

const EMPTY: Form = {
  min: '',
  max: '',
  diff: '',
  dest: '',
  names: '',
  fp: '',
  flow: '',
  keepalive: '25',
  mtu: '1420',
  ingressBase: '8443',
  hopBase: '20000',
  hy2Base: '18000',
  probeUrl: PROBE_URL_DEFAULT,
  probeTimeout: '10',
  probeInterval: '60',
  geodataCron: GEODATA_CRON_DEFAULT,
  geodataGeoip: GEOIP_URL_DEFAULT,
  geodataGeosite: GEOSITE_URL_DEFAULT,
  // 控制面的默认值：上行半关闭 2 秒，下行半关闭 5 秒。
  // 缓冲区留空是有意的：空值表示产物中不写入该键，由 xray 按 CPU 架构决定。
  connIdle: '300',
  connUplink: '2',
  connDownlink: '5',
  connBuffer: '',
  connHandshake: '60',
};

// 将已保存的设置转换为表单的形态。修改判定基于它：字符串与字符串比较，
// 不需要在 null、数字和空串之间做转换——这正是此前未修改却显示为已修改的原因。
function formOf(s: ModelSettings): Form {
  return {
    min: text(s.reality_client?.min_client_ver ?? null),
    max: text(s.reality_client?.max_client_ver ?? null),
    diff: s.reality_client?.max_time_diff_ms == null ? '' : String(s.reality_client.max_time_diff_ms),
    dest: text(s.reality_site?.dest ?? null),
    names: (s.reality_site?.server_names ?? []).join(', '),
    fp: text(s.reality_site?.fingerprint ?? null),
    flow: text(s.reality_site?.flow ?? null),
    keepalive: String(s.overlay?.keepalive_secs ?? 25),
    mtu: String(s.overlay?.mtu ?? 1420),
    ingressBase: String(s.ports?.ingress_base ?? 8443),
    hopBase: String(s.ports?.hop_base ?? 20000),
    hy2Base: String(s.ports?.hy2_base ?? 18000),
    probeUrl: s.probe?.endpoint_url ?? PROBE_URL_DEFAULT,
    probeTimeout: String(s.probe?.timeout_secs ?? 10),
    probeInterval: String(s.probe?.interval_secs ?? 60),
    geodataCron: s.geodata?.cron ?? GEODATA_CRON_DEFAULT,
    geodataGeoip: s.geodata?.geoip_url ?? GEOIP_URL_DEFAULT,
    geodataGeosite: s.geodata?.geosite_url ?? GEOSITE_URL_DEFAULT,
    connIdle: String(s.connection?.conn_idle_secs ?? 300),
    connUplink: String(s.connection?.uplink_only_secs ?? 2),
    connDownlink: String(s.connection?.downlink_only_secs ?? 5),
    // null 需转换为空串而非 '0'：该字段的空值表示不写入该键，而 0 表示不缓冲。
    connBuffer: s.connection?.buffer_size_kb == null ? '' : String(s.connection.buffer_size_kb),
    connHandshake: String(s.connection?.handshake_secs ?? 60),
  };
}

// 四个段各自保存、各自产生一个修订。段与 ModelSettings 的子对象一一对应
// （XRAY 段对应两个：站点和客户端版本限制都是 xray 的服务端参数，需要一起修改）。
type SectionKey = 'xray' | 'connection' | 'wireguard' | 'ports' | 'probe' | 'geodata';

const SECTION_FIELDS: Record<SectionKey, (keyof Form)[]> = {
  xray: ['dest', 'names', 'fp', 'flow', 'min', 'max', 'diff'],
  connection: ['connIdle', 'connUplink', 'connDownlink', 'connBuffer', 'connHandshake'],
  wireguard: ['keepalive', 'mtu'],
  ports: ['ingressBase', 'hopBase', 'hy2Base'],
  probe: ['probeUrl', 'probeTimeout', 'probeInterval'],
  geodata: ['geodataCron', 'geodataGeoip', 'geodataGeosite'],
};

/* 保存之后会发生什么，三档。段标题里只写结果，原因写在段自己的说明里。
 *
 * 这是九段之间最大的一处差别，此前它只出现在每段说明的末尾（「不盖修订，保存即生效」
 * 「修改本段需要发布一次」），与其余的说明同为一句灰色小字，需要读完整句才能得知。 */
type Apply = 'now' | 'publish' | 'cycle';

const APPLY: Record<Apply, string> = {
  now: '保存即生效',
  publish: '需要发布',
  cycle: '下一轮生效',
};

/* ── 段目录 ──
 *
 * 九段分两栏排列，目录平铺这九项，不按生效方式分组：那是段自身的属性，每段的标题栏里
 * 已经有一枚徽章写明，目录里再分一次只是把九行拆成三段间隔。
 *
 * 编号不是装饰：内容区按该顺序排列（左栏 01–04、右栏 05–09，从上到下、从左到右），
 * 可以直接引用第 5 段这样的位置。
 */
type NavItem = { id: string; no: string; label: string; apply: Apply; key?: SectionKey };

const NAV: NavItem[] = [
  { id: 'set-branding', no: '01', label: '站点外观', apply: 'now' },
  { id: 'set-dist', no: '02', label: '分发', apply: 'now' },
  { id: 'set-agent-logs', no: '03', label: '日志保留', apply: 'cycle' },
  { id: 'set-cert', no: '04', label: '证书', apply: 'now' },
  { id: 'set-xray', no: '05', label: 'XRAY', apply: 'publish', key: 'xray' },
  { id: 'set-conn', no: '06', label: '连接策略', apply: 'publish', key: 'connection' },
  { id: 'set-wg', no: '07', label: 'WireGuard', apply: 'publish', key: 'wireguard' },
  { id: 'set-ports', no: '08', label: '端口分配', apply: 'publish', key: 'ports' },
  // 探测配置不进产物：机器下一轮读到新值即生效，最长等一个原有周期。
  { id: 'set-probe', no: '09', label: '端到端探测', apply: 'cycle', key: 'probe' },
  { id: 'set-geodata', no: '10', label: '规则库更新', apply: 'publish', key: 'geodata' },
];

const APPLY_OF: Record<string, Apply> = Object.fromEntries(NAV.map(item => [item.id, item.apply]));

/** 段标题里的生效方式。只有「需要发布」着主色：九段里这一档占五段，
    且它是唯一一档「保存完还没完」，另外两档保存即到位。 */
function ApplyBadge({ id }: { id: string }) {
  const kind = APPLY_OF[id];
  return <span className={kind === 'publish' ? 'applyb pub' : 'applyb'}>{APPLY[kind]}</span>;
}

// 这条路上的封装开销。接口不下发对端的传输方式，开销由 `路径 MTU − 建议值` 得出——
// agent 即按此相减，反向计算必然一致。
//
// 数字之外标注一项形态：同一栏内两条链路的建议值相差 12，仅给出数字会被理解为其中一条
// 测量有误，而该差值来自 phantun 以 TCP 头替换 UDP 头。开销的构成见
// `brocade-agent/src/icmp.rs` 的 `wireguard_overhead`，该处为准；此处只将差值映射为
// 对应的封装形态，无法匹配的组合只显示数字，不作推断。
const OVERHEAD_SHAPE: Record<number, string> = {
  60: '直连 v4',
  72: 'phantun v4',
  80: '双栈直连',
  92: 'phantun 双栈',
};

function Overhead({ link }: { link: LinkMtuItem }) {
  if (link.path_mtu == null || link.suggested_wg_mtu == null) return <span className="dim">—</span>;
  const bytes = link.path_mtu - link.suggested_wg_mtu;
  const shape = OVERHEAD_SHAPE[bytes];
  return (
    <>
      {bytes}
      {shape && (
        <span className="dim" style={{ fontFamily: 'var(--sans)' }}>
          {' '}
          {shape}
        </span>
      )}
    </>
  );
}

// wg MTU 探测结果，只读。
// 探测由每台 agent 执行：控制面没有到 underlay 的路径，机器之间的链路只有两端可观测。
// 测量按链路对（路径 MTU 是路径的属性），建议值按机器（一个 wg0 对应一个 MTU）。
//
// 采纳建议值的按钮在节点页，不在此处：MTU 是节点属性，需要逐台查看逐台修改。且修改
// MTU 会重新生成该机器的 wg 配置并中断一次链路，中断时机由运营者决定。
function MtuProbe() {
  const nameOf = useNodeNames();
  const probe = useQuery({ queryKey: ['link-mtu'], queryFn: () => fetchLinkMtu() });
  const [open, setOpen] = useState(false);

  if (probe.isLoading) return null;
  if (probe.error) return <div className="note dim">探测结果读不到</div>;
  const data = probe.data;
  if (!data || data.links.length === 0) return <div className="note dim">还没有探测结果</div>;

  // 生效值跟探测建议不相等不是问题，两个方向的意思完全相反：偏大是包大过路径能过的
  // 尺寸，大包被中间某一跳悄悄打掉，这才要报；偏小只是没跑满、链路照常通，而全局默认
  // 本来就是个保守兜底，没有义务等于任何一台探出来的数。从前这里拿 `!==` 一把抓，
  // 于是「还有余量」也顶着一句金色的「对不上」，逼人去改一个本来就对的值。
  const tooBig = data.nodes.filter(n => n.suggested_mtu != null && n.current_mtu > n.suggested_mtu);
  const headroom = data.nodes.filter(n => n.suggested_mtu != null && n.current_mtu < n.suggested_mtu);
  /* 建议值人人相同时就把数写进那句话里，省得为一个数点开表格。各台不同时只报台数。 */
  const oneOf = (list: typeof data.nodes) => {
    const set = new Set(list.map(n => n.suggested_mtu));
    return set.size === 1 ? [...set][0] : null;
  };
  const tooBigTo = oneOf(tooBig);
  const headroomTo = oneOf(headroom);

  return (
    <>
      {tooBig.length > 0 && (
        <span className="cost hot">
          <b>{tooBig.length} 台</b>
          {tooBigTo != null ? `的生效值大过探测建议 ${tooBigTo}，大包会被打掉` : '的生效值大过探测建议，大包会被打掉'}
        </span>
      )}
      {tooBig.length === 0 && headroom.length > 0 && (
        <span className="hint">
          {headroom.length} 台还有余量
          {headroomTo != null && `，最大能到 ${headroomTo}`}
          ——不调也通，调了快一点
        </span>
      )}
      <button className="btn sm" onClick={() => setOpen(!open)}>
        {open ? '收起探测结果' : '看探测结果'}
      </button>
      {open && (
        <div className="panel" style={{ marginTop: 8, width: '100%' }}>
          <table className="t">
            <thead>
              <tr>
                <th>机器</th>
                <th>当前</th>
                <th>探测建议</th>
                <th>最窄的一条通向</th>
              </tr>
            </thead>
            <tbody>
              {data.nodes.map(n => (
                <tr key={n.node_id}>
                  <td title={n.node_id}>{nameOf(n.node_id)}</td>
                  <td className="mono">
                    {n.current_mtu}
                    {!n.overridden && <span className="dim"> 默认</span>}
                  </td>
                  <td className="mono">
                    {n.suggested_mtu ?? <span className="dim">没探出</span>}
                    {n.inconclusive > 0 && <span className="dim"> ·{n.inconclusive} 条没结果</span>}
                  </td>
                  <td className="d2" title={n.tightest_peer ?? ''}>
                    {n.tightest_peer ? nameOf(n.tightest_peer) : '—'}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
          <p className="note" style={{ marginTop: 8 }}>
            建议值 = 最小路径 MTU 减去 wg 封装开销，采纳要去节点面逐台改。开销不是一个常数， 随<b>对端</b>
            的入口形态变：直连 v4 是 60（IP 20 + UDP 8 + wg 32）；对端走 phantun 假 TCP 就是 72——TCP 头顶掉 UDP 头，多
            12 字节；落点有 AAAA 记录的各再加 20 （wg 可能走 v6，宁可把建议值算小）。所以两台的建议值差 12 或 20
            是正常的， 不是哪一条探歪了。
          </p>
          <details style={{ marginTop: 6 }}>
            <summary className="note">按对的原始探测结果（{data.links.length} 条路径）</summary>
            <table className="t" style={{ marginTop: 6 }}>
              <thead>
                <tr>
                  <th>路径</th>
                  <th>探的落点</th>
                  <th>路径 MTU</th>
                  <th title="路径 MTU 减去建议值，也就是这条路上 wg 的封装开销">封装开销</th>
                  <th>探测时间</th>
                </tr>
              </thead>
              <tbody>
                {data.links.map(l => (
                  <tr key={`${l.node_id}|${l.peer_node_id}`}>
                    <td className="mono">
                      {l.node_id} → {l.peer_node_id}
                    </td>
                    <td className="mono d2">{l.endpoint_host}</td>
                    <td className="mono">
                      {l.status === 'ok' ? (
                        l.path_mtu
                      ) : (
                        <span className="dim">
                          {
                            {
                              unreachable: 'ping 不通',
                              blocked: 'ICMP 被挡',
                              unsupported: '这台机器测不了（权限）',
                            }[l.status]
                          }
                        </span>
                      )}
                    </td>
                    <td className="mono">
                      <Overhead link={l} />
                    </td>
                    <td className="d2">{l.probed_at.slice(0, 19)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </details>
        </div>
      )}
    </>
  );
}

/* 段编号取自目录，段上不另写一份：两处各写一份时，插入一段就会出现目录说 05、段自己
   说 04 的情况，而这个号的用处正是让「目录里点的那一项」和「滚到的这一段」能对上。 */
const NO_OF: Record<string, string> = Object.fromEntries(NAV.map(item => [item.id, item.no]));

function Section({
  id,
  name,
  sub,
  dirty,
  saving,
  savedRev,
  editable,
  onSave,
  children,
}: {
  id: string;
  name: string;
  sub: string;
  dirty: boolean;
  saving: boolean;
  savedRev: number | null;
  editable: boolean;
  onSave: () => void;
  children: React.ReactNode;
}) {
  return (
    <section className="panel titled" id={id}>
      {/* 说明不进标题栏：一栏宽 470px，「编号 + 段名 + 徽章 + 状态 + 保存」已经占满，
          再塞一句说明会把色带顶成两行。它落在色带下方，与段内的字段同起一条竖线。 */}
      <header>
        <span className="no">{NO_OF[id]}</span>
        <h4>{name}</h4>
        <ApplyBadge id={id} />
        <span className="sp" />
        {dirty && <span className="dirty">有未保存的改动</span>}
        {!dirty && savedRev !== null && <span className="dirty done">已保存，盖出修订 {savedRev}</span>}
        <button
          className={dirty ? 'btn primary save' : 'btn save idle'}
          disabled={!editable || !dirty || saving}
          onClick={onSave}
        >
          {saving ? '保存中…' : '保存这一段'}
        </button>
      </header>
      <p className="cardsub">{sub}</p>
      {children}
    </section>
  );
}

/* 分发设置：节点访问控制面的地址，以及安装哪个版本的 xray。
 *
 * 独立成段而非并入上面的 Section，因为它与其他段有一项根本差异：**不产生修订、不需要发布**。
 * 上面各段保存后显示「已保存，产生修订 N」，而这两个字段不进入任何产物，保存后立即生效。
 * 沿用该标题结构需要在显示修订号的位置写明本次没有修订号，会先建立预期再否定它。
 *
 * 它同样不进入草稿：`saveDistribution` 直接 PUT，与 `saveSettings` 写入草稿是两条路径。
 *
 * 置于最前：地址配置错误时，下面所有设置都无法生效——安装步骤即无法完成。 */
/* 证书：机队中每台节点的 TLS 证书，由控制面签发、入库，并随下发包送达节点。
 *
 * 与分发同类——不产生修订、不需要发布、保存即生效——但它后台有一个 worker，
 * 因此增加一个立即执行一轮的按钮。该按钮不是另一条代码路径，而是同一轮扫描的提前触发。
 *
 * 该段最易出错的是失败状态的显示方式。续期失败**不等同于**没有证书：已有的证书在到期前
 * 仍可使用。显示为红色的「没有」会被理解为入口已不可用；显示为灰色提示又会导致忽略真正的
 * 问题——证书过期后入口整台停止服务，而机器仍上报为已收敛。因此按剩余天数分档：
 * 剩余较多时为提醒，剩余较少时才是告警。 */
const DAY = 86_400_000;

/** 距到期的天数。没有到期时间（尚未签发）时返回 null。 */
function daysLeft(expiresAt: string | null): number | null {
  if (!expiresAt) return null;
  const at = Date.parse(expiresAt);
  return Number.isFinite(at) ? Math.floor((at - Date.now()) / DAY) : null;
}

/** 机器上是否已有该证书。与签发状态是两项内容，必须分开显示——控制面已签发
    不等同于机器已获取。 */
/* 机器上是旧证书与控制面尚未收到上报是两种状态，但显示形式相同。
 *
 * 节点上报当前持有的证书与签发不在同一条路径：签发在控制面执行，上报由节点按轮次执行。
 * 因此签发后的数分钟内，上报内容仍是上一张证书——不是机器未更新，而是上报数据晚于签发。
 *
 * 判定依据是时间先后而非经过的分钟数：上报时间早于签发时间即表示尚未上报新状态。 */
function notYetHeard(issuedAt: string | null, observedAt: string | null): boolean {
  if (!issuedAt || !observedAt) return false;
  const at = (text: string) => Date.parse(text.replace(' ', 'T'));
  const issued = at(issuedAt);
  const observed = at(observedAt);
  return !Number.isNaN(issued) && !Number.isNaN(observed) && observed < issued;
}

/** 一台机器持有的是不是本组正在出示的那张。
    换证书后最长一小时才生效：agent 十分钟一轮取到新字节，xray 再按自己的周期热重载。
    这段时间里「旧的」是预期状态，不是故障。 */
function diskState(
  row: NodeCertificateState,
  serving: GroupCertificate | undefined,
): { tone: string; text: string } | null {
  switch (row.on_disk) {
    case 'current':
      return null; // 一切如常的那一档不占版面
    case 'stale':
      return notYetHeard(serving?.issued_at ?? null, row.observed_at)
        ? { tone: 'idle', text: '刚换过，这台还没报上来' }
        : { tone: 'warn', text: '机器上是旧的' };
    case 'absent':
      return { tone: 'err', text: '机器上没有' };
    default:
      // 最需要关注的一档：agent 从未上报，说明其版本过旧、不管理证书。在没有该列时，
      // 该状态与正常状态的显示完全相同。
      return { tone: 'idle', text: 'agent 没报' };
  }
}

/** 一张证书的状态。颜色只表示严重程度，文字负责说明它现在是什么角色。 */
function certState(cert: GroupCertificate): { tone: string; text: string } {
  const left = daysLeft(cert.expires_at);
  switch (cert.status) {
    case 'serving':
      // 到期在即才提醒：续期是自动的，但自动也会失败，而失败只有靠剩余天数才看得出来。
      if (left !== null && left <= 7) return { tone: 'err', text: `在用 · 只剩 ${left} 天` };
      if (left !== null && left <= 20) return { tone: 'warn', text: `在用 · 还剩 ${left} 天` };
      return { tone: 'ok', text: '在用' };
    case 'ready':
      return { tone: 'idle', text: '待命' };
    case 'pending':
      return { tone: 'idle', text: '排队签发中' };
    case 'failed':
      return { tone: 'err', text: `签发失败 · 试了 ${cert.attempts} 次` };
    default:
      return { tone: 'idle', text: '已换下' };
  }
}

function CertSection({ editable, view }: { editable: boolean; view: CertsView }) {
  const qc = useQueryClient();
  const [form, setForm] = useState<{
    domain: string;
    credential: string;
    directory: string;
    contact: string;
    renew: string;
  } | null>(null);
  const [saved, setSaved] = useState(false);

  const [syncedFrom, setSyncedFrom] = useState<CertsView | undefined>(undefined);
  if (view !== syncedFrom) {
    setSyncedFrom(view);
    const d = view.domain;
    setForm({
      domain: d?.domain ?? '',
      // 凭据不回读，因此表单中始终为空；已存储时通过下方的 placeholder 说明。
      credential: '',
      directory: d?.acme_directory ?? view.letsencrypt,
      contact: d?.acme_contact ?? '',
      renew: String(d?.renew_before_days ?? 30),
    });
  }

  const save = useMutation({
    mutationFn: () =>
      saveCertDomain({
        domain: form!.domain.trim(),
        dns_credential: form!.credential.trim() ? form!.credential.trim() : null,
        acme_directory: form!.directory,
        acme_contact: form!.contact.trim() ? form!.contact.trim() : null,
        renew_before_days: Number(form!.renew) || 30,
      }),
    onSuccess: () => {
      setSaved(true);
      setForm(prev => (prev ? { ...prev, credential: '' } : prev));
      qc.invalidateQueries({ queryKey: ['certs'] });
    },
  });

  const scan = useMutation({
    mutationFn: () => scanCerts(),
    onSuccess: () => qc.invalidateQueries({ queryKey: ['certs'] }),
  });

  const d = view.domain;
  const f = form ?? { domain: '', credential: '', directory: view.letsencrypt, contact: '', renew: '30' };
  const dirty =
    f.domain.trim() !== (d?.domain ?? '') ||
    f.credential.trim() !== '' ||
    f.directory !== (d?.acme_directory ?? view.letsencrypt) ||
    f.contact.trim() !== (d?.acme_contact ?? '') ||
    Number(f.renew) !== (d?.renew_before_days ?? 30);

  const staging = f.directory === view.letsencrypt_staging;
  // 折叠标题上的计数：看的是签发失败的证书张数，不是机器台数——一张证书失败会连累整组机器，
  // 数机器会把同一个问题报成好几个。
  const bad = view.groups.flatMap(group => group.certificates.filter(cert => cert.status === 'failed'));

  return (
    <section className="panel titled" id="set-cert">
      <header>
        <span className="no">{NO_OF['set-cert']}</span>
        <h4>证书</h4>
        <ApplyBadge id="set-cert" />
        <span className="sp" />
        {dirty && <span className="dirty">有未保存的改动</span>}
        {!dirty && saved && <span className="dirty done">已保存，立刻生效</span>}
        <button
          className={dirty ? 'btn primary save' : 'btn save idle'}
          disabled={!editable || !dirty || save.isPending || !view.sealing_available}
          title={view.sealing_available ? '' : '这台控制面没配 BROCADE_SECRET_KEY，存不了凭据'}
          onClick={() => save.mutate()}
        >
          {save.isPending ? '保存中…' : '保存这一段'}
        </button>
      </header>
      <p className="cardsub">每台一张独立的通配证书，由控制面签发，随下发包送达节点</p>
      {save.error && <ErrorBox error={save.error} />}

      <div className="setgrp">
        <p className="eyebrow">签发</p>

        <div className="setfld">
          <label>证书域名</label>
          <div className="v">
            <input
              className={dirty && f.domain.trim() !== (d?.domain ?? '') ? 'f chg' : 'f'}
              style={{ width: 280 }}
              placeholder="example.net"
              value={f.domain}
              onChange={e => setForm({ ...f, domain: e.target.value })}
            />
          </div>
        </div>

        <div className="setfld">
          <label>Cloudflare Token</label>
          <div className="v">
            <input
              className={f.credential ? 'f chg' : 'f'}
              style={{ width: 430 }}
              type="password"
              placeholder={d?.has_credential ? '已配置（重填才会覆盖）' : 'Zone:Read + DNS:Edit'}
              value={f.credential}
              onChange={e => setForm({ ...f, credential: e.target.value })}
            />
            {d?.has_credential && !f.credential && <span className="hint">已配置，不可读出</span>}
          </div>
        </div>

        <div className="setfld">
          <label>ACME 目录</label>
          <div className="v">
            <span
              className={dirty && f.directory !== (d?.acme_directory ?? view.letsencrypt) ? 'segsw chg' : 'segsw'}
              role="group"
            >
              <button
                type="button"
                aria-pressed={!staging}
                onClick={() => setForm({ ...f, directory: view.letsencrypt })}
              >
                正式
              </button>
              <button
                type="button"
                aria-pressed={staging}
                onClick={() => setForm({ ...f, directory: view.letsencrypt_staging })}
              >
                staging
              </button>
            </span>
          </div>
        </div>

        <div className="setfld">
          <label>联系邮箱</label>
          <div className="v">
            <input
              className={dirty && f.contact.trim() !== (d?.acme_contact ?? '') ? 'f chg' : 'f'}
              style={{ width: 280 }}
              placeholder="选填"
              value={f.contact}
              onChange={e => setForm({ ...f, contact: e.target.value })}
            />
          </div>
        </div>

        <div className="setfld">
          <label>提前续期</label>
          <div className="v">
            <input
              className={dirty && Number(f.renew) !== (d?.renew_before_days ?? 30) ? 'f chg' : 'f'}
              style={{ width: 70 }}
              value={f.renew}
              onChange={e => setForm({ ...f, renew: e.target.value })}
            />
            <span className="unit">天</span>
          </div>
        </div>

        <div className="guard">
          使用<b>单独的域名</b>。Cloudflare token 按 zone 授权，无法限制到子域，域名分开可防止凭据泄露波及控制面。
        </div>

        <div className="guard">
          每台一张<b>独立</b>证书。Let&apos;s Encrypt <b>同一组名字每 7 天最多签发 5 张</b>
          ，随机标签使每台名字唯一，不受此限制。
        </div>

        <div className="guard">
          证书会进入 CT 公开日志，随机标签防猜测但不防枚举。DNS-01 <b>不需要 A 记录</b>，名字与 IP 的对应关系不公开。
        </div>
      </div>

      {d && (
        <div className="setgrp">
          <p className="eyebrow">
            机队 · {view.nodes.length} 台
            {bad.length > 0 && <b style={{ color: 'var(--err)' }}> · {bad.length} 张要处理</b>}
          </p>
          <div className="setfld">
            <label />
            <div className="v">
              <button className="btn" disabled={!editable || scan.isPending} onClick={() => scan.mutate()}>
                {scan.isPending ? '已排上…' : '现在检查一轮'}
              </button>
              <span className="hint">每台约半分钟，串行执行。并发会触发 CA 的速率限制</span>
            </div>
          </div>

          <CertGroups view={view} editable={editable} />

          <div className="guard">
            续期失败<b>不等于没有证书</b>，在用的到期前仍可用。风险是长期未处理导致过期后整组停服。
          </div>
          <div className="guard">
            组内换证书<b>不改 SNI、不需要发布</b>，约一小时生效。<b>换组才会改 SNI</b>，已发出的订阅会断连。
          </div>
        </div>
      )}
    </section>
  );
}

/** 证书组的列表：一个组、组里的证书、用这个组的机器。
 *
 * 三层而不是一张平表：证书属于组，机器也属于组，但证书和机器之间没有直接关系——同组十台机器
 * 共用一张证书，各自独立地报告自己拿到没有。平表会把那一张证书重复十遍，也就看不出轮换过程中
 * 「已经换过的」和「还没换到的」是同一张证书的两侧。 */
function CertGroups({ view, editable }: { view: CertsView; editable: boolean }) {
  const qc = useQueryClient();
  const nameOf = useNodeNames();
  const reload = () => qc.invalidateQueries({ queryKey: ['certs'] });
  const [creating, setCreating] = useState<{ name: string; note: string } | null>(null);
  const [editing, setEditing] = useState<{ id: string; name: string; note: string } | null>(null);
  const [failed, setFailed] = useState<string | null>(null);

  const run = (what: Promise<unknown>) => {
    setFailed(null);
    what.then(reload).catch((error: unknown) => setFailed(String(error)));
  };

  if (view.groups.length === 0) {
    return (
      <>
        {creating ? (
          <GroupForm
            value={creating}
            onChange={setCreating}
            onCancel={() => setCreating(null)}
            onSave={() => {
              run(createCertGroup({ name: creating.name, note: creating.note || null }));
              setCreating(null);
            }}
          />
        ) : (
          <div className="guard">
            还没有证书组。同组机器共享 SNI 和证书，签发额度按组计算。同组机器会被识别为同一批。
            <div style={{ marginTop: 8 }}>
              <button className="btn" disabled={!editable} onClick={() => setCreating({ name: '', note: '' })}>
                新建证书组
              </button>
            </div>
          </div>
        )}
        {failed && <div className="note bad">{failed}</div>}
      </>
    );
  }

  return (
    <>
      <div className="setfld">
        <label />
        <div className="v">
          <button
            className="btn"
            disabled={!editable || !!creating}
            onClick={() => setCreating({ name: '', note: '' })}
          >
            新建证书组
          </button>
          <span className="hint">每个组一个 SNI，一张证书供组内所有机器使用</span>
        </div>
      </div>
      {creating && (
        <GroupForm
          value={creating}
          onChange={setCreating}
          onCancel={() => setCreating(null)}
          onSave={() => {
            run(createCertGroup({ name: creating.name, note: creating.note || null }));
            setCreating(null);
          }}
        />
      )}
      {failed && <div className="note bad">{failed}</div>}

      {view.groups.map(group => {
        const serving = group.certificates.find(c => c.status === 'serving');
        const members = view.nodes.filter(row => row.label_id === group.id);
        return (
          <div className="certgrp" key={group.id}>
            <div className="certgrp-hd">
              <b>{group.name}</b>
              <span className="cnames mono">{group.names[1] ?? group.names[0]}</span>
              {group.note && <span className="hint">{group.note}</span>}
              <span className="hint">{members.length} 台机器</span>
              <span className="ctl">
                <button
                  className="btn sm"
                  disabled={!editable}
                  onClick={() => setEditing({ id: group.id, name: group.name, note: group.note ?? '' })}
                >
                  改名
                </button>
                <button
                  className="btn sm"
                  disabled={!editable}
                  title="多签一张备用。它待命，由你决定何时启用；自动续期那张不经过这里"
                  onClick={() => run(requestSpareCertificate(group.id))}
                >
                  加一张备用
                </button>
                <button
                  className="btn sm danger"
                  disabled={!editable || members.length > 0}
                  title={members.length > 0 ? '还有机器在用这个组' : '删除这个组'}
                  onClick={() => {
                    if (window.confirm(`删除证书组「${group.name}」？它的证书会一并删除。`)) {
                      run(deleteCertGroup(group.id));
                    }
                  }}
                >
                  删除
                </button>
              </span>
            </div>

            {editing?.id === group.id && (
              <GroupForm
                value={editing}
                onChange={next => setEditing({ ...next, id: group.id })}
                onCancel={() => setEditing(null)}
                onSave={() => {
                  run(updateCertGroup(group.id, { name: editing.name, note: editing.note || null }));
                  setEditing(null);
                }}
              />
            )}

            <div className="certtbl">
              {group.certificates.length === 0 ? (
                <div className="hint">还没有证书。签发每小时一轮，也可以点上方「现在检查一轮」。</div>
              ) : (
                group.certificates.map(cert => {
                  const state = certState(cert);
                  const left = daysLeft(cert.expires_at);
                  return (
                    <div className="certrow cert" key={cert.id}>
                      <span className={`cstate ${state.tone}`}>{state.text}</span>
                      <span className="hint">{cert.origin === 'spare' ? '手动加的备用' : '自动续期'}</span>
                      <span className="cissuer">
                        {cert.issuer ? (
                          <span className={/STAGING/i.test(cert.issuer) ? 'cstate warn' : 'hint'}>
                            {/STAGING/i.test(cert.issuer) ? `${cert.issuer}（不被信任）` : cert.issuer}
                          </span>
                        ) : (
                          <span className="hint">—</span>
                        )}
                      </span>
                      <span className="cwhen">
                        {cert.expires_at ? (
                          <>
                            {cert.expires_at.slice(0, 10)}
                            {left !== null && <span className="hint"> · 还有 {left} 天</span>}
                          </>
                        ) : (
                          <span className="hint">—</span>
                        )}
                      </span>
                      {cert.status === 'ready' && (
                        <button
                          className="btn sm"
                          disabled={!editable}
                          title="让这个组的机器改用这张。SNI 不变，不需要发布"
                          onClick={() => run(serveCertificate(cert.id))}
                        >
                          启用
                        </button>
                      )}
                      {cert.last_error && <span className="cerr">{cert.last_error}</span>}
                    </div>
                  );
                })
              )}
            </div>

            {members.length > 0 && (
              <div className="certtbl">
                {members.map(row => {
                  const disk = diskState(row, serving);
                  return (
                    <div className="certrow member" key={row.node_id}>
                      <span className="cname" title={row.node_id}>
                        {nameOf(row.node_id)}
                      </span>
                      {disk ? (
                        <span className={`cdisk cstate ${disk.tone}`}>{disk.text}</span>
                      ) : (
                        <span className="hint">证书已就位</span>
                      )}
                    </div>
                  );
                })}
              </div>
            )}
          </div>
        );
      })}
    </>
  );
}

/** 建组和改组用同一个表单：两者要填的东西相同，分开写会让它们慢慢长得不一样。 */
function GroupForm({
  value,
  onChange,
  onCancel,
  onSave,
}: {
  value: { name: string; note: string };
  onChange: (next: { name: string; note: string }) => void;
  onCancel: () => void;
  onSave: () => void;
}) {
  return (
    <div className="setfld">
      <label>组名</label>
      <div className="v">
        <input
          className="f"
          style={{ width: 160 }}
          placeholder="香港前置"
          value={value.name}
          onChange={e => onChange({ ...value, name: e.target.value })}
        />
        <input
          className="f"
          style={{ width: 280 }}
          placeholder="备注（可选）：这组是干什么的"
          value={value.note}
          onChange={e => onChange({ ...value, note: e.target.value })}
        />
        <button className="btn" disabled={!value.name.trim()} onClick={onSave}>
          保存
        </button>
        <button className="btn" onClick={onCancel}>
          取消
        </button>
      </div>
    </div>
  );
}

const BRAND_ICON_TYPES = ['image/png', 'image/jpeg', 'image/webp'];
const BRAND_ICON_MAX_BYTES = 256 * 1024;

function BrandingSection({ editable, data }: { editable: boolean; data: BrandingSettings }) {
  const qc = useQueryClient();
  const [form, setForm] = useState<BrandingSettings | null>(null);
  const [savedAt, setSavedAt] = useState(false);
  const [fileError, setFileError] = useState<string | null>(null);
  const [syncedFrom, setSyncedFrom] = useState<BrandingSettings | undefined>(undefined);
  if (data !== syncedFrom) {
    setSyncedFrom(data);
    setForm(data);
  }

  const f = form ?? data;
  const dirty = f.site_name !== data.site_name || f.icon_data_url !== data.icon_data_url;
  const save = useMutation({
    mutationFn: () => saveBranding(f),
    onSuccess: saved => {
      setSavedAt(true);
      setFileError(null);
      setForm(saved);
      qc.setQueryData(['branding'], saved);
    },
  });

  const chooseIcon = (file: File | undefined) => {
    setFileError(null);
    if (!file) return;
    if (!BRAND_ICON_TYPES.includes(file.type)) {
      setFileError('只支持 PNG、JPEG 或 WebP');
      return;
    }
    if (file.size > BRAND_ICON_MAX_BYTES) {
      setFileError('图片不能超过 256 KiB');
      return;
    }
    const reader = new FileReader();
    reader.onerror = () => setFileError('图片读取失败');
    reader.onload = () => {
      if (typeof reader.result !== 'string') {
        setFileError('图片读取失败');
        return;
      }
      setForm(current => ({ ...(current ?? data), icon_data_url: reader.result as string }));
    };
    reader.readAsDataURL(file);
  };

  return (
    <section className="panel titled" id="set-branding">
      <header>
        <span className="no">{NO_OF['set-branding']}</span>
        <h4>站点外观</h4>
        <ApplyBadge id="set-branding" />
        <span className="sp" />
        {dirty && <span className="dirty">有未保存的改动</span>}
        {!dirty && savedAt && <span className="dirty done">已保存，立刻生效</span>}
        <button
          className={dirty ? 'btn primary save' : 'btn save idle'}
          disabled={!editable || !dirty || save.isPending}
          onClick={() => save.mutate()}
        >
          {save.isPending ? '保存中…' : '保存这一段'}
        </button>
      </header>
      <p className="cardsub">控制台左上角使用这里的名称和图标；名称也同步到登录页和浏览器标题</p>
      {save.error && <ErrorBox error={save.error} />}
      {fileError && <div className="callout err">{fileError}</div>}
      <Group>
        <Fld label="站点名称">
          <input
            className={dirty && f.site_name !== data.site_name ? 'f chg' : 'f'}
            style={{ width: 260 }}
            maxLength={64}
            placeholder="Brocade"
            value={f.site_name}
            onChange={event => setForm({ ...f, site_name: event.target.value })}
          />
        </Fld>
        <Fld label="站点图标">
          <span className="branding-preview" title="左上角预览">
            <BrandIcon branding={f} className="branding-preview-icon" />
          </span>
          <label className="btn sm branding-file">
            选择图片
            <input
              type="file"
              accept={BRAND_ICON_TYPES.join(',')}
              disabled={!editable}
              onChange={event => {
                chooseIcon(event.currentTarget.files?.[0]);
                event.currentTarget.value = '';
              }}
            />
          </label>
          {f.icon_data_url && (
            <button
              className="btn sm"
              type="button"
              disabled={!editable}
              onClick={() => setForm({ ...f, icon_data_url: null })}
            >
              恢复默认图标
            </button>
          )}
          <span className="hint">PNG / JPEG / WebP，最大 256 KiB；建议使用正方形图片</span>
        </Fld>
      </Group>
    </section>
  );
}

function DistributionSection({ editable, data }: { editable: boolean; data: DistributionView }) {
  const qc = useQueryClient();
  const [form, setForm] = useState<{ url: string; version: string } | null>(null);
  const [savedAt, setSavedAt] = useState(false);

  // 与上面的表使用同一做法：数据就绪后在渲染期填充，不放在 effect 中（会多出一帧空表单）。
  const [syncedFrom, setSyncedFrom] = useState<DistributionView | undefined>(undefined);
  if (data !== syncedFrom) {
    setSyncedFrom(data);
    setForm({ url: data.stored.agent_public_url ?? '', version: data.stored.xray_version ?? '' });
  }

  const save = useMutation({
    mutationFn: () =>
      saveDistribution({
        agent_public_url: form?.url.trim() ? form.url.trim() : null,
        xray_version: form?.version.trim() ? form.version.trim() : null,
      }),
    onSuccess: () => {
      setSavedAt(true);
      qc.invalidateQueries({ queryKey: ['distribution'] });
    },
  });

  const stored = data.stored;
  const effective = data.effective;
  const f = form ?? { url: '', version: '' };
  const dirty = f.url !== (stored.agent_public_url ?? '') || f.version !== (stored.xray_version ?? '');
  // 留空不等于没有取值：会回退到进程启动时的环境变量，再回退到内置默认值。因此两个字段都
  // 显示当前实际使用的值，否则空输入框会被理解为未配置，而安装命令中实际有地址在使用。
  const fallbackNote = (own: string | null, live: string | null) =>
    !own && live ? <span className="hint">当前生效：{live}（来自环境变量）</span> : null;

  return (
    <section className="panel titled" id="set-dist">
      <header>
        <span className="no">{NO_OF['set-dist']}</span>
        <h4>分发</h4>
        <ApplyBadge id="set-dist" />
        <span className="sp" />
        {dirty && <span className="dirty">有未保存的改动</span>}
        {!dirty && savedAt && <span className="dirty done">已保存，立刻生效</span>}
        <button
          className={dirty ? 'btn primary save' : 'btn save idle'}
          disabled={!editable || !dirty || save.isPending}
          onClick={() => save.mutate()}
        >
          {save.isPending ? '保存中…' : '保存这一段'}
        </button>
      </header>
      <p className="cardsub">节点从哪里访问这台控制面、安装哪个 XRAY</p>
      {save.error && <ErrorBox error={save.error} />}
      <Group>
        <Fld label="Agent 请求地址">
          <input
            className={dirty && f.url !== (stored.agent_public_url ?? '') ? 'f chg' : 'f'}
            style={{ width: 430 }}
            placeholder={effective.agent_public_url ?? 'https://…'}
            value={f.url}
            onChange={e => setForm({ ...f, url: e.target.value })}
          />
          {fallbackNote(stored.agent_public_url, effective.agent_public_url)}
        </Fld>
        <Fld label="XRAY 版本">
          <input
            className={dirty && f.version !== (stored.xray_version ?? '') ? 'f chg' : 'f'}
            style={{ width: 160 }}
            placeholder={effective.xray_version ?? '未钉'}
            value={f.version}
            onChange={e => setForm({ ...f, version: e.target.value })}
          />
          {effective.xray_version && fallbackNote(stored.xray_version, effective.xray_version)}
        </Fld>
        <div className="guard">
          XRAY 版本不要留空。控制面依赖的两个上游特性没有一个版本以上的重叠区间：geodata 热加载需要 ≥ 26.4，而 VLESS
          反向隧道自 26.5 起只出不回。留空会让每台新机器安装到最新版本，而最新版本正好在该区间之外。
        </div>
      </Group>
    </section>
  );
}

const LOG_MIN_MIB = 16;
const LOG_MAX_MIB = 4096;

const validLogMib = (raw: string) => {
  if (!/^\d+$/.test(raw.trim())) return null;
  const value = Number(raw);
  return Number.isSafeInteger(value) && value >= LOG_MIN_MIB && value <= LOG_MAX_MIB ? value : null;
};

export function NodeLogPolicyRow({ editable, node }: { editable: boolean; node: AgentLogPolicyNode }) {
  const qc = useQueryClient();
  const [form, setForm] = useState(String(node.override_max_mib ?? node.effective_max_mib));
  const [customizing, setCustomizing] = useState(false);
  const [syncedFrom, setSyncedFrom] = useState(node);
  if (node !== syncedFrom) {
    setSyncedFrom(node);
    setForm(String(node.override_max_mib ?? node.effective_max_mib));
    setCustomizing(false);
  }
  const mutation = useMutation({
    mutationFn: (maxMib: number | null) => saveNodeLogPolicy(node.node_id, maxMib),
    onSuccess: view => {
      qc.setQueryData(['agent-log-policy'], view);
    },
  });
  const inherited = node.override_max_mib == null && !customizing;
  const value = validLogMib(form);
  const dirty = !inherited && (customizing || (value !== null && value !== node.override_max_mib));

  return (
    <div className="agent-log-node">
      <div className="agent-log-node-name">
        <b>{node.name}</b>
        <span>{node.tenant_id}</span>
      </div>
      <span className={inherited ? 'agent-log-source' : 'agent-log-source overridden'}>
        {inherited ? '继承全局' : '机器覆盖'}
      </span>
      <label className="agent-log-value">
        <input
          className={dirty ? 'f chg' : 'f'}
          type="number"
          min={LOG_MIN_MIB}
          max={LOG_MAX_MIB}
          step={1}
          aria-label={`${node.name} 日志上限`}
          disabled={!editable || inherited || mutation.isPending}
          value={inherited ? node.effective_max_mib : form}
          onChange={event => setForm(event.target.value)}
        />
        <span>MiB</span>
      </label>
      {inherited ? (
        <button
          className="btn sm"
          type="button"
          disabled={!editable}
          onClick={() => {
            setForm(String(node.effective_max_mib));
            setCustomizing(true);
          }}
        >
          设置覆盖
        </button>
      ) : (
        <>
          <button
            className={dirty ? 'btn sm primary' : 'btn sm'}
            type="button"
            disabled={!editable || value === null || !dirty || mutation.isPending}
            onClick={() => value !== null && mutation.mutate(value)}
          >
            {mutation.isPending && mutation.variables !== null ? '保存中…' : '保存'}
          </button>
          <button
            className="btn sm"
            type="button"
            disabled={!editable || mutation.isPending}
            onClick={() => {
              if (node.override_max_mib == null) {
                setCustomizing(false);
                setForm(String(node.effective_max_mib));
              } else {
                mutation.mutate(null);
              }
            }}
          >
            {mutation.isPending && mutation.variables === null
              ? '取消中…'
              : node.override_max_mib == null
                ? '取消'
                : '取消覆盖'}
          </button>
        </>
      )}
      {value === null && !inherited && (
        <span className="agent-log-invalid">
          {LOG_MIN_MIB}–{LOG_MAX_MIB}
        </span>
      )}
      {mutation.error && <ErrorBox error={mutation.error} />}
    </div>
  );
}

export function AgentLogPolicySection({ editable, data }: { editable: boolean; data: AgentLogPolicyView }) {
  const qc = useQueryClient();
  const [form, setForm] = useState(String(data.global_max_mib));
  const [syncedFrom, setSyncedFrom] = useState(data.global_max_mib);
  if (data.global_max_mib !== syncedFrom) {
    setSyncedFrom(data.global_max_mib);
    setForm(String(data.global_max_mib));
  }
  const value = validLogMib(form);
  const dirty = value !== null && value !== data.global_max_mib;
  const save = useMutation({
    mutationFn: () => saveAgentLogDefault(value!),
    onSuccess: view => qc.setQueryData(['agent-log-policy'], view),
  });

  return (
    <section className="panel titled agent-log-policy" id="set-agent-logs">
      <header>
        <span className="no">{NO_OF['set-agent-logs']}</span>
        <h4>日志保留</h4>
        <ApplyBadge id="set-agent-logs" />
        <span className="sp" />
        {dirty && <span className="dirty">有未保存的改动</span>}
        <button
          className={dirty ? 'btn primary save' : 'btn save idle'}
          disabled={!editable || !dirty || save.isPending}
          onClick={() => save.mutate()}
        >
          {save.isPending ? '保存中…' : '保存全局值'}
        </button>
      </header>
      <p className="cardsub">Agent、XRAY 与每个 Phantun 日志项的磁盘上限；机器覆盖优先于全局</p>
      {save.error && <ErrorBox error={save.error} />}
      <Group label="全局默认">
        <Fld label="每个日志项最多">
          <label className="agent-log-value global">
            <input
              className={dirty ? 'f chg' : 'f'}
              type="number"
              min={LOG_MIN_MIB}
              max={LOG_MAX_MIB}
              step={1}
              aria-label="全局日志上限"
              disabled={!editable || save.isPending}
              value={form}
              onChange={event => setForm(event.target.value)}
            />
            <span>MiB</span>
          </label>
          <span className="hint">
            范围 {LOG_MIN_MIB}–{LOG_MAX_MIB}。修改后，所有未覆盖的机器随下一轮 Agent 轮询更新
          </span>
          {value === null && (
            <span className="agent-log-invalid">
              请输入 {LOG_MIN_MIB}–{LOG_MAX_MIB} 的整数
            </span>
          )}
        </Fld>
      </Group>
      <Group label="机器覆盖">
        <div className="agent-log-nodes">
          {data.nodes.length === 0 ? (
            <span className="hint">还没有机器</span>
          ) : (
            data.nodes.map(node => <NodeLogPolicyRow key={node.node_id} editable={editable} node={node} />)
          )}
        </div>
        <div className="guard">
          不产生修订、不需要发布线路。降低上限会立即截断已有日志；XRAY 与 Phantun 不会因此重启。
        </div>
      </Group>
    </section>
  );
}

const Group = ({ label, children }: { label?: string; children: React.ReactNode }) => (
  <div className="setgrp">
    {label && <p className="eyebrow">{label}</p>}
    {children}
  </div>
);

/* 半关闭的两项：从预设值中选择，不手动输入。
 *
 * 它们与该段的其他项不同——空闲回收的范围是 10–86400、缓冲区是 0–65536，只能输入；
 * 而这两项的实际取值是个位数秒，提供一个 0–3600 的输入框相当于在数千个取值中定位一个。
 * 机器详情页的对应卡片使用同一控件：同一字段在两个页面上是同一项配置。
 *
 * 已修改未保存时与相邻输入框一致使用金色（`.chg`），而非改为实心：金色表示该字段已修改，
 * 主色表示该档位为当前选中，两者使用不同颜色。实心样式用于机器详情页——在该页它表示
 * 该机器覆盖了全局配置，而设置页没有可覆盖的上级配置。
 *
 * 使用 .segsw 而非 .seg：它与相邻的输入框等高，同一段内两种控件上下排列时才能对齐。
 *
 * 显示的档位是 1–5，而接口接受 0–3600。已存储的值落在该范围之外时需要将其加入为一档——
 * 否则所有档位均未选中，任意点击都会修改该值。0 同样按此处理：不主动提供（半关闭等待
 * 0 秒会中断正在传输的数据，不应是易于点击的选项），但已设置时正常显示并保留。 */
const SECS_PICKS = [1, 2, 3, 4, 5];

function Secs({ value, changed, onPick }: { value: string; changed: boolean; onPick: (v: string) => void }) {
  const now = Number(value.trim());
  const picks = [...SECS_PICKS];
  if (Number.isFinite(now) && value.trim() !== '' && !picks.includes(now)) picks.push(now);
  picks.sort((a, b) => a - b);
  return (
    <span className={changed ? 'segsw chg' : 'segsw'} role="group">
      {picks.map(v => (
        <button key={v} type="button" aria-pressed={String(v) === value.trim()} onClick={() => onPick(String(v))}>
          {v}
        </button>
      ))}
    </span>
  );
}

/* 一个字段一行：标签宽 132px 右对齐，值及其单位、提示、标签排在同一行内。 */
const Fld = ({ label, children }: { label?: string; children: React.ReactNode }) => (
  <div className="setfld">
    <label>{label ?? ''}</label>
    <div className="v">{children}</div>
  </div>
);

export function SettingsPane() {
  const { who } = useSession();
  const qc = useQueryClient();
  const settings = useQuery({ queryKey: ['settings'], queryFn: () => fetchSettings() });
  // 证书、分发与站点外观的查询统一放在页面层：各段各自结束时会先亮一次整页骨架，
  // 再在刚渲染出的段落里亮第二次——加载态被看到两次。查询提到页面层、一次等齐，
  // 加载期只呈现一个骨架。
  const branding = useQuery({ queryKey: ['branding'], queryFn: () => fetchBranding() });
  const certs = useQuery({
    queryKey: ['certs'],
    queryFn: () => fetchCerts(),
    // 有证书正在签发时页面需要自动更新：一轮约半分钟，要求手动刷新才能看到进展不可接受。
    refetchInterval: query =>
      (query.state.data?.groups ?? []).some(group => group.certificates.some(cert => cert.status === 'pending'))
        ? 10_000
        : false,
  });
  const dist = useQuery({ queryKey: ['distribution'], queryFn: () => fetchDistribution() });
  const logPolicy = useQuery({ queryKey: ['agent-log-policy'], queryFn: fetchAgentLogPolicy });
  const [form, setForm] = useState<Form>(EMPTY);
  const [saved, setSaved] = useState<Partial<Record<SectionKey, number>>>({});

  const pristine = settings.data ? formOf(settings.data) : null;

  // 服务端数据就绪或变化时重新填充表单。在渲染期执行而非在 effect 中：在 effect 中调用
  // setState 会额外触发一轮渲染，中间一帧会显示空表单。React 对渲染期调用组件自身的
  // setState 有特殊处理——它会丢弃本轮输出并重新渲染，不提交该帧。
  // 使用引用比较：TanStack Query 有结构共享，数据未变化时 data 是同一个对象。
  const [syncedFrom, setSyncedFrom] = useState<typeof settings.data>(undefined);
  if (settings.data && settings.data !== syncedFrom) {
    setSyncedFrom(settings.data);
    setForm(formOf(settings.data));
  }

  const save = useMutation({
    mutationFn: (key: SectionKey) => {
      // 当前段取表单中的值，其他段取已保存的值——而非表单中已修改但未保存的值。
      // 分段保存的作用即在于此：修改 XRAY 后又修改探测配置，点击 XRAY 的保存按钮时，
      // 探测部分的改动必须保持未提交状态，等待其自身的保存操作。
      const own = SECTION_FIELDS[key];
      const base = pristine ?? EMPTY;
      const v = (f: keyof Form) => (own.includes(f) ? form[f] : base[f]);
      const body: ModelSettings = {
        reality_client: {
          min_client_ver: orNull(v('min')),
          max_client_ver: orNull(v('max')),
          max_time_diff_ms: v('diff').trim() === '' ? null : Number(v('diff')),
        },
        reality_site: {
          dest: orNull(v('dest')),
          server_names: v('names')
            .split(',')
            .map(x => x.trim())
            .filter(Boolean),
          fingerprint: orNull(v('fp')),
          flow: orNull(v('flow')),
        },
        overlay: {
          keepalive_secs: Number(v('keepalive')) || 25,
          mtu: Number(v('mtu')) || 1420,
        },
        ports: {
          ingress_base: Number(v('ingressBase')) || 8443,
          hop_base: Number(v('hopBase')) || 20000,
          hy2_base: Number(v('hy2Base')) || 18000,
        },
        probe: {
          endpoint_url: v('probeUrl').trim() || PROBE_URL_DEFAULT,
          timeout_secs: Number(v('probeTimeout')) || 10,
          interval_secs: Number(v('probeInterval')) || 60,
        },
        geodata: {
          cron: v('geodataCron').trim() || GEODATA_CRON_DEFAULT,
          geoip_url: v('geodataGeoip').trim() || GEOIP_URL_DEFAULT,
          geosite_url: v('geodataGeosite').trim() || GEOSITE_URL_DEFAULT,
        },
        connection: {
          conn_idle_secs: numOr(v('connIdle'), 300),
          uplink_only_secs: numOr(v('connUplink'), 2),
          downlink_only_secs: numOr(v('connDownlink'), 5),
          buffer_size_kb: v('connBuffer').trim() === '' ? null : numOr(v('connBuffer'), 0),
          handshake_secs: numOr(v('connHandshake'), 60),
        },
        // 该项没有对应的表单字段——统计得出的在线数尚无展示位置，提供一个无法看到效果的
        // 开关不如不提供。此处原样传递，避免保存其他段时将其重置为 false。
        stats_user_online: settings.data?.stats_user_online ?? false,
      };
      return saveSettings(body).then(r => ({ key, revision_id: r.revision_id }));
    },
    onSuccess: r => {
      setSaved(s => ({ ...s, [r.key]: r.revision_id }));
      qc.invalidateQueries({ queryKey: ['settings'] });
      qc.invalidateQueries({ queryKey: ['revisions'] });
    },
  });

  if (settings.isPending || branding.isPending || certs.isPending || dist.isPending || logPolicy.isPending)
    return <Loading />;
  if (settings.error) return <ErrorBox error={settings.error} />;

  const editable = can(who.role, 'system');
  const dirtyOf = (key: SectionKey) => pristine !== null && SECTION_FIELDS[key].some(f => form[f] !== pristine[f]);
  // 已修改的字段自行标记。与段标题中的「有未保存的改动」是同一信息的两个粒度：
  // 标题表示该段是否有改动，字段表示具体修改了哪几项。
  const chg = (f: keyof Form) => (pristine !== null && form[f] !== pristine[f] ? 'f chg' : 'f');
  const secProps = (key: SectionKey) => ({
    dirty: dirtyOf(key),
    saving: save.isPending && save.variables === key,
    savedRev: saved[key] ?? null,
    editable,
    onSave: () => save.mutate(key),
  });

  return (
    <div className="cardpage">
      <div className="duo">
        {/* 左栏四段、右栏五段：两栏各自成流，不对齐底部。分段位置按高度定——证书段带着
            机队列表，单它一段就抵得上右栏的两段，与它同栏的只能是最短的那两段。
            编号仍从上到下、从左到右连续。 */}
        <div className="col">
          {save.error && <ErrorBox error={save.error} />}

          {branding.error ? (
            <ErrorBox error={branding.error} />
          ) : (
            <BrandingSection editable={editable} data={branding.data!} />
          )}
          {dist.error ? <ErrorBox error={dist.error} /> : <DistributionSection editable={editable} data={dist.data!} />}
          {logPolicy.error ? (
            <ErrorBox error={logPolicy.error} />
          ) : (
            <AgentLogPolicySection editable={editable} data={logPolicy.data!} />
          )}
          {certs.error ? <ErrorBox error={certs.error} /> : <CertSection editable={editable} view={certs.data!} />}

          <Section id="set-xray" name="XRAY" sub="所有接入面共用的服务端参数" {...secProps('xray')}>
            <Group label="REALITY · 目标站点">
              <Fld label="dest">
                <input
                  className={chg('dest')}
                  style={{ width: 230 }}
                  placeholder="example.com:443"
                  value={form.dest}
                  onChange={e => setForm({ ...form, dest: e.target.value })}
                />
                <span className="unit">站点:端口</span>
              </Fld>
              <Fld label="server_names">
                <input
                  className={chg('names')}
                  style={{ width: 280 }}
                  placeholder="example.com"
                  value={form.names}
                  onChange={e => setForm({ ...form, names: e.target.value })}
                />
                <span className="hint">SNI，多个用逗号分隔</span>
              </Fld>
              <Fld label="fingerprint">
                <select
                  className={chg('fp')}
                  style={{ width: 130 }}
                  value={form.fp}
                  onChange={e => setForm({ ...form, fp: e.target.value })}
                >
                  <option value="">未设置</option>
                  {REALITY_FINGERPRINT_OPTIONS.map(([value, label]) => (
                    <option value={value} key={value}>
                      {label}
                    </option>
                  ))}
                </select>
              </Fld>
              <div className="guard">
                SNI 与 dest 必须指向同一个真实站点，不一致会导致握手失败。接入面未填写站点时使用这里的值。
              </div>
            </Group>

            <Group label="XTLS · 流控">
              <Fld label="flow">
                <select
                  className={chg('flow')}
                  style={{ width: 250 }}
                  value={form.flow}
                  onChange={e => setForm({ ...form, flow: e.target.value })}
                >
                  {/* 显示为大写、value 仍为小写：写入 xray.json 和 grants 的必须是
                  `xtls-rprx-vision` 原值，大写只是该字段的显示形式。 */}
                  <option value="xtls-rprx-vision">XTLS-RPRX-VISION（默认）</option>
                  <option value="">关闭（普通 VLESS over TLS）</option>
                </select>
              </Fld>
              <details className="more">
                <summary>为什么默认开着 Vision，以及它为什么不用重启</summary>
                <div className="body">
                  <p>
                    Vision 使内层不再叠加第二层 TLS，长连接下的 CPU 占用和延迟都更低，因此默认开启。关闭后即为存在
                    TLS-in-TLS 特征的普通代理，仅在客户端版本过旧时才选择。注意服务端开启而客户端不带 flow 时，XRAY
                    会直接拒绝连接（<code>rejected since the client flow is empty</code>），不会回退到普通 TLS。
                  </p>
                  <p>
                    它与本节其他参数不同：flow 写在每个用户账号上，不进入 <code>xray.json</code>，因此修改它走 grants
                    热同步，<b>不重启 XRAY、不断开现有连接</b>。本段末尾「会重启 XRAY」指的是目标站点和客户端限制，
                    不包括这一项。
                  </p>
                </div>
              </details>
            </Group>

            <Group label="REALITY · 客户端闸">
              <Fld label="min_client_ver">
                <input
                  className={chg('min')}
                  style={{ width: 260 }}
                  placeholder="留空 = 不限"
                  value={form.min}
                  onChange={e => setForm({ ...form, min: e.target.value })}
                />
              </Fld>
              <Fld label="max_client_ver">
                <input
                  className={chg('max')}
                  style={{ width: 260 }}
                  placeholder="留空 = 不限"
                  value={form.max}
                  onChange={e => setForm({ ...form, max: e.target.value })}
                />
              </Fld>
              <Fld label="max_time_diff_ms">
                <input
                  className={chg('diff')}
                  style={{ width: 260 }}
                  placeholder="留空 = 用 XRAY 默认"
                  value={form.diff}
                  onChange={e => setForm({ ...form, diff: e.target.value })}
                />
              </Fld>
              <div className="guard">
                修改目标站点或客户端限制会使<b>所有含 REALITY 入口的节点</b>重启 XRAY，按金丝雀波次逐台确认。
                上方的流控不在此列。
              </div>
            </Group>
          </Section>
        </div>

        <div className="col">
          {/* 位于 XRAY 之后、WIREGUARD 之前：上一段是接入面的服务端参数，本段是同一个
          xray 进程的另一部分——连接的存活时长和内存占用。两者都属于 xray，
          先说明对外配置再说明内部配置。 */}
          <Section
            id="set-conn"
            name="连接策略"
            sub="连接保持多久、每条占用多少内存。每台机器可单独覆盖"
            {...secProps('connection')}
          >
            <Group>
              <Fld label="空闲多久回收（秒）">
                <input
                  className={chg('connIdle')}
                  style={{ width: 90 }}
                  value={form.connIdle}
                  onChange={e => setForm({ ...form, connIdle: e.target.value })}
                />
                <span className="hint">
                  多久没有数据往返就回收该连接。默认 300，范围 10–86400。
                  <b>中转节点的内存主要消耗在这里</b>：空闲连接会持续占用下面的两个缓冲区
                </span>
              </Fld>
              <Fld label="转发缓冲（KB）">
                <input
                  className={chg('connBuffer')}
                  style={{ width: 90 }}
                  placeholder="跟 CPU 架构"
                  value={form.connBuffer}
                  onChange={e => setForm({ ...form, connBuffer: e.target.value })}
                />
                <span className="hint">
                  收发之间的队列，<b>每条连接每个方向一个</b>。<b>推荐留空</b>：不写该项时 XRAY 按 CPU 架构自行决定。填
                  0 表示不缓冲，与留空不同
                </span>
              </Fld>
              <Fld label="UplinkOnly 等待（秒）">
                <Secs
                  value={form.connUplink}
                  changed={chg('connUplink') !== 'f'}
                  onPick={v => setForm({ ...form, connUplink: v })}
                />
                <span className="hint">对端服务器先关闭下行、连接只剩上行时，再等待这么久后整条断开。默认 2</span>
              </Fld>
              <Fld label="DownlinkOnly 等待（秒）">
                <Secs
                  value={form.connDownlink}
                  changed={chg('connDownlink') !== 'f'}
                  onPick={v => setForm({ ...form, connDownlink: v })}
                />
                <span className="hint">相反方向：客户端先关闭上行、只剩下行。默认 5</span>
              </Fld>
              <Fld label="握手超时（秒）">
                <input
                  className={chg('connHandshake')}
                  style={{ width: 90 }}
                  value={form.connHandshake}
                  onChange={e => setForm({ ...form, connHandshake: e.target.value })}
                />
                <span className="hint">
                  <b>无明确理由不要修改</b>，也不支持按机器单独设置。60 是 XRAY 为对齐 nginx 的{' '}
                  <code>client_header_timeout</code> 选定的，目的是让这个值不暴露后端是什么。
                  改成其他值即产生一处可测量的差异；每台各设一个值，则形成一组可分别识别的机器
                </span>
              </Fld>
              <div className="guard">
                TCP 半关闭等待时间。过短会截断回传数据，过长会多占内存。<b>默认 UplinkOnly 2 秒、DownlinkOnly 5 秒</b>。
                机器详情页可逐台覆盖。
              </div>
              <div className="guard">
                转发缓冲留空时 XRAY 按架构取值：x86_64 512 KB，arm64 4 KB。填入数值会统一所有架构。
              </div>
              <div className="guard">
                上面四项<b>均可按机器单独覆盖</b>（机器详情页），握手超时除外。
              </div>
              <div className="guard">
                保存后需发布，<b>agent 应用时会重启 XRAY</b>，现有连接断开。
              </div>
            </Group>
          </Section>

          <Section id="set-wg" name="WIREGUARD" sub="overlay 链路，全互联算出来的" {...secProps('wireguard')}>
            <Group>
              <Fld label="keepalive_secs">
                <input
                  className={chg('keepalive')}
                  style={{ width: 90 }}
                  value={form.keepalive}
                  onChange={e => setForm({ ...form, keepalive: e.target.value })}
                />
                <span className="hint">默认 25。NAT 表项老化较快的环境应调小</span>
              </Fld>
              <Fld label="mtu 默认值">
                <input
                  className={chg('mtu')}
                  style={{ width: 90 }}
                  value={form.mtu}
                  onChange={e => setForm({ ...form, mtu: e.target.value })}
                />
                <span className="hint">
                  范围 1000–9000。仅影响未单独设置的机器。修改会重新生成 wg 配置并断开一次链路
                </span>
              </Fld>
              <Fld>
                <MtuProbe />
              </Fld>
              <div className="guard">keepalive 仅在单向拨号时生效，即公网地址全部留空、由对端发起连接的那一侧。</div>
            </Group>
          </Section>

          <Section
            id="set-ports"
            name="端口分配"
            sub="自动分配端口时的起始值，仅影响新建，现有端口不变"
            {...secProps('ports')}
          >
            <Group>
              <Fld label="接入面">
                <input
                  className={chg('ingressBase')}
                  style={{ width: 90 }}
                  value={form.ingressBase}
                  onChange={e => setForm({ ...form, ingressBase: e.target.value })}
                />
                <span className="hint">建链时从该端口向上查找空闲端口</span>
              </Fld>
              <Fld label="Hysteria 2">
                <input
                  className={chg('hy2Base')}
                  style={{ width: 90 }}
                  value={form.hy2Base}
                  onChange={e => setForm({ ...form, hy2Base: e.target.value })}
                />
                <span className="hint">
                  走 UDP，与上一项使用各自的端口段。同一个端口号在 TCP 与 UDP 上互不冲突。开启端口跳转时，还会从分配到的
                  端口向上连续占用一段
                </span>
              </Fld>
              <Fld label="中转口">
                <input
                  className={chg('hopBase')}
                  style={{ width: 90 }}
                  value={form.hopBase}
                  onChange={e => setForm({ ...form, hopBase: e.target.value })}
                />
                <span className="hint">默认 20000。挑高位段，不跟接入面和系统服务混在一起</span>
              </Fld>
              <details className="more">
                <summary>为什么默认 8443 而不是 443</summary>
                <div className="body">
                  <p>443 上通常已有其他服务，冲突时两个进程争用同一端口，症状要到 XRAY 启动失败才会显现。</p>
                  <p>
                    抬头那句「只影响新建」是有代价撑着的：端口一变就是 XRAY 配置变、进程重启、
                    那台机器上所有连接断掉，所以
                    <b>已经配好的端口一个都不动</b>。
                  </p>
                </div>
              </details>
            </Group>
          </Section>

          <Section
            id="set-probe"
            name="端到端探测"
            sub="由链头为每条链发起一次探测。改完最长等待一个原有周期"
            {...secProps('probe')}
          >
            <Group>
              <Fld label="请求哪个地址">
                <input
                  className={chg('probeUrl')}
                  style={{ width: 330 }}
                  placeholder={PROBE_URL_DEFAULT}
                  value={form.probeUrl}
                  onChange={e => setForm({ ...form, probeUrl: e.target.value })}
                />
              </Fld>
              <Fld label="超时（秒）">
                <input
                  className={chg('probeTimeout')}
                  style={{ width: 90 }}
                  value={form.probeTimeout}
                  onChange={e => setForm({ ...form, probeTimeout: e.target.value })}
                />
                <span className="hint">超过该时间仍未收到首字节即判定为断。范围 1–120</span>
              </Fld>
              <Fld label="多久探一轮">
                <input
                  className={chg('probeInterval')}
                  style={{ width: 90 }}
                  value={form.probeInterval}
                  onChange={e => setForm({ ...form, probeInterval: e.target.value })}
                />
                <span className="hint">秒。默认 60，范围 15–86400</span>
              </Fld>
              <div className="guard">
                落点需为<b>明文 HTTP</b>，返回纯文本且包含 <code>ip=</code>。探测使用隐藏凭据，不占名额。
              </div>
            </Group>
          </Section>

          {/* 不提供开关是有意的：规则表中的 geosite: / geoip: 依赖这两个文件，文件过期不会报错，
          而是导致规则匹配失败、流量走兜底规则且无提示。因此此处只能配置更新时间和
          更新来源，不能配置是否更新。 */}
          <Section
            id="set-geodata"
            name="规则库更新"
            sub="每台机器按计划自行拉取 geoip.dat / geosite.dat，热加载、不重启、不断开连接"
            {...secProps('geodata')}
          >
            <Group>
              <Fld label="什么时候更新">
                <input
                  className={chg('geodataCron')}
                  style={{ width: 260 }}
                  placeholder={GEODATA_CRON_DEFAULT}
                  value={form.geodataCron}
                  onChange={e => setForm({ ...form, geodataCron: e.target.value })}
                />
                <span className="hint">
                  五段 cron：分 时 日 月 周。
                  <b>
                    不要去掉前缀 <code>CRON_TZ=</code>
                  </b>
                  ：不带前缀时按每台机器自身的时区解释，同一个表达式在不同机器上会相差数小时。默认 6:30（UTC+8）＝ 22:30
                  UTC，比上游发布晚半小时
                </span>
              </Fld>
              <Fld label="geoip.dat">
                <input
                  className={chg('geodataGeoip')}
                  style={{ width: 430 }}
                  placeholder={GEOIP_URL_DEFAULT}
                  value={form.geodataGeoip}
                  onChange={e => setForm({ ...form, geodataGeoip: e.target.value })}
                />
              </Fld>
              <Fld label="geosite.dat">
                <input
                  className={chg('geodataGeosite')}
                  style={{ width: 430 }}
                  placeholder={GEOSITE_URL_DEFAULT}
                  value={form.geodataGeosite}
                  onChange={e => setForm({ ...form, geodataGeosite: e.target.value })}
                />
              </Fld>
              <div className="guard">
                下载<b>不验签不校验和</b>，推荐改为自建镜像。更新走 <code>out:internal</code>，不经过用户链。
              </div>
              <div className="guard">
                落地文件名固定，不可配置：<code>geosite:</code> 在 XRAY 内部会被改写成 <code>ext:geosite.dat:</code>
                ，改名后将无法读取。
              </div>
            </Group>
          </Section>
        </div>
      </div>
    </div>
  );
}
