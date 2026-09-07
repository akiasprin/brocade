import { useServerForm } from '../ui/server-form';
import { ReverseHealthSettings } from '../reverse-health-settings';
import { useRef, useState, useSyncExternalStore } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  DEFAULT_HOP_MUX,
  fetchCerts,
  fetchAuthState,
  fetchBranding,
  saveCertDomain,
  saveBranding,
  scanCerts,
  fetchDistribution,
  fetchAgentLogPolicy,
  fetchLinkMtu,
  fetchSettings,
  hopMuxError,
  fetchPingProbeSettings,
  saveDistribution,
  saveAgentLogDefault,
  saveNodeLogPolicy,
  saveSettings,
  savePingProbeSettings,
  setVisitorAccess,
  createCertGroup,
  deleteCertificate,
  deleteCertGroup,
  requestSpareCertificate,
  serveCertificate,
  updateCertGroup,
  type CertsView,
  type BrandingSettings,
  type DistributionView,
  type AgentLogLimits,
  type AgentLogLimitOverrides,
  type AgentLogPolicyNode,
  type AgentLogPolicyView,
  type GroupCertificate,
  type LinkMtuItem,
  type HopMux,
  type ModelSettings,
  type PingProbeSettings,
} from '../api';
import { draft } from '../draft';
import { can, useSession } from '../session';
import { ErrorBox, Loading } from '../ui/bits';
import { BrandIcon } from '../ui/branding';
import { PanelTitle, type IconName } from '../ui/icons';
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
  anytlsBase: string;
  vlessEncryptionBase: string;
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
  muxConcurrency: string;
  muxMinIdle: string;
  muxMaxIdle: string;
  muxMaxProbing: string;
  muxProbeInterval: string;
  muxProbeTimeout: string;
  muxIdleTtl: string;
  muxMaxRequests: string;
  anyTlsPadding: string;
};

/* `Number(x) || 默认值` 在该组字段上不适用：0 是合法取值——上下行半关闭等待 0 秒表示
   对端关闭后立即关闭——而 `||` 会将其替换为默认值。只有空值和非数值才回退到默认值。 */
const numOr = (raw: string, fallback: number) => {
  const t = raw.trim();
  if (t === '') return fallback;
  const n = Number(t);
  return Number.isFinite(n) ? n : fallback;
};

const numberField = (raw: string): number => (raw.trim() === '' ? Number.NaN : Number(raw));

function relayMuxOfForm(form: Form): HopMux {
  return {
    concurrency: numberField(form.muxConcurrency),
    min_idle_workers: numberField(form.muxMinIdle),
    max_idle_workers: numberField(form.muxMaxIdle),
    max_probing_workers: numberField(form.muxMaxProbing),
    probe_interval_secs: numberField(form.muxProbeInterval),
    probe_timeout_ms: numberField(form.muxProbeTimeout),
    idle_ttl_secs: numberField(form.muxIdleTtl),
    max_requests_per_worker: numberField(form.muxMaxRequests),
  };
}

const PROBE_URL_DEFAULT = 'http://cp.cloudflare.com/cdn-cgi/trace';
const GEODATA_CRON_DEFAULT = 'CRON_TZ=Asia/Shanghai 30 6 * * *';
const GEOIP_URL_DEFAULT = 'https://raw.githubusercontent.com/Loyalsoldier/v2ray-rules-dat/release/geoip.dat';
const GEOSITE_URL_DEFAULT = 'https://raw.githubusercontent.com/Loyalsoldier/v2ray-rules-dat/release/geosite.dat';
const ANYTLS_PADDING_DEFAULT = ['stop=4', '0=20-30', '1=64-100', '2=90-130,c,180-250', '3=220-480'].join('\n');

/* 四阶段、只在第 2 阶段切一次。各阶段的最大记录预算不超过 AnyTLS 原生默认对应阶段的
   最小预算，且第 4 个包起完全停止 padding。端点取自 Web Crypto；点击只修改表单，保存并
   发布后才会影响节点。 */
function randomAnyTlsPadding(): string {
  const random = crypto.getRandomValues(new Uint8Array(10));
  const pick = (byte: number, from: number, to: number) => from + (byte % (to - from + 1));
  const zero = [pick(random[0], 20, 25), pick(random[1], 26, 30)];
  const one = [pick(random[2], 48, 72), pick(random[3], 80, 100)];
  const twoA = [pick(random[4], 80, 100), pick(random[5], 110, 140)];
  const twoB = [pick(random[6], 160, 200), pick(random[7], 220, 260)];
  const three = [pick(random[8], 160, 260), pick(random[9], 320, 500)];
  return [
    'stop=4',
    `0=${zero[0]}-${zero[1]}`,
    `1=${one[0]}-${one[1]}`,
    `2=${twoA[0]}-${twoA[1]},c,${twoB[0]}-${twoB[1]}`,
    `3=${three[0]}-${three[1]}`,
  ].join('\n');
}

const EMPTY: Form = {
  min: '',
  max: '',
  diff: '',
  dest: '',
  names: '',
  fp: '',
  flow: '',
  keepalive: '10',
  mtu: '1420',
  ingressBase: '13443',
  anytlsBase: '14443',
  vlessEncryptionBase: '48000',
  hopBase: '20000',
  hy2Base: '30000',
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
  muxConcurrency: String(DEFAULT_HOP_MUX.concurrency),
  muxMinIdle: String(DEFAULT_HOP_MUX.min_idle_workers),
  muxMaxIdle: String(DEFAULT_HOP_MUX.max_idle_workers),
  muxMaxProbing: String(DEFAULT_HOP_MUX.max_probing_workers),
  muxProbeInterval: String(DEFAULT_HOP_MUX.probe_interval_secs),
  muxProbeTimeout: String(DEFAULT_HOP_MUX.probe_timeout_ms),
  muxIdleTtl: String(DEFAULT_HOP_MUX.idle_ttl_secs),
  muxMaxRequests: String(DEFAULT_HOP_MUX.max_requests_per_worker),
  anyTlsPadding: ANYTLS_PADDING_DEFAULT,
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
    keepalive: String(s.overlay?.keepalive_secs ?? 10),
    mtu: String(s.overlay?.mtu ?? 1420),
    ingressBase: String(s.ports?.ingress_base ?? 13443),
    anytlsBase: String(s.ports?.anytls_base ?? 14443),
    vlessEncryptionBase: String(s.ports?.vless_encryption_base ?? 48000),
    hopBase: String(s.ports?.hop_base ?? 20000),
    hy2Base: String(s.ports?.hy2_base ?? 30000),
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
    muxConcurrency: String(s.relay_mux?.concurrency ?? 1),
    muxMinIdle: String(s.relay_mux?.min_idle_workers ?? 0),
    muxMaxIdle: String(s.relay_mux?.max_idle_workers ?? 2),
    muxMaxProbing: String(s.relay_mux?.max_probing_workers ?? 1),
    muxProbeInterval: String(s.relay_mux?.probe_interval_secs ?? 5),
    muxProbeTimeout: String(s.relay_mux?.probe_timeout_ms ?? 2000),
    muxIdleTtl: String(s.relay_mux?.idle_ttl_secs ?? 24),
    muxMaxRequests: String(s.relay_mux?.max_requests_per_worker ?? 128),
    anyTlsPadding: (s.anytls_padding_scheme ?? ANYTLS_PADDING_DEFAULT.split('\n')).join('\n'),
  };
}

// 四个段各自保存、各自产生一个修订。段与 ModelSettings 的子对象一一对应
// （XRAY 段对应两个：站点和客户端版本限制都是 xray 的服务端参数，需要一起修改）。
type SectionKey = 'xray' | 'connection' | 'wireguard' | 'ports' | 'probe' | 'geodata';

const SECTION_FIELDS: Record<SectionKey, (keyof Form)[]> = {
  xray: ['dest', 'names', 'fp', 'flow', 'min', 'max', 'diff', 'anyTlsPadding'],
  connection: [
    'connIdle',
    'connUplink',
    'connDownlink',
    'connBuffer',
    'connHandshake',
    'muxConcurrency',
    'muxMinIdle',
    'muxMaxIdle',
    'muxMaxProbing',
    'muxProbeInterval',
    'muxProbeTimeout',
    'muxIdleTtl',
    'muxMaxRequests',
  ],
  wireguard: ['keepalive', 'mtu'],
  ports: ['ingressBase', 'anytlsBase', 'vlessEncryptionBase', 'hopBase', 'hy2Base'],
  probe: ['probeUrl', 'probeTimeout', 'probeInterval'],
  geodata: ['geodataCron', 'geodataGeoip', 'geodataGeosite'],
};

/* 保存之后会发生什么，四类。段标题里只写结果，原因写在段自己的说明里。
 *
 * 这是九段之间最大的一处差别，此前它只出现在每段说明的末尾（「不盖修订，保存即生效」
 * 「修改本段需要发布一次」），与其余的说明同为一句灰色小字，需要读完整句才能得知。 */
type Apply = 'now' | 'publish' | 'cycle' | 'future';

const APPLY: Record<Apply, string> = {
  now: '保存即生效',
  publish: '需要发布',
  cycle: '下一轮生效',
  future: '仅影响新建',
};

/* 标题图标与机器配置、链路设置共用同一套线稿图标。设置项仍按两栏顺序排列；
 * 生效方式是段自身的属性，由标题右侧的徽章说明。 */
type NavItem = { id: string; label: string; icon: IconName; apply: Apply; key?: SectionKey };

const NAV: NavItem[] = [
  { id: 'set-branding', label: '站点外观', icon: 'settings', apply: 'now' },
  { id: 'set-visitor', label: '访客模式', icon: 'access', apply: 'now' },
  { id: 'set-dist', label: '分发', icon: 'deploy', apply: 'now' },
  { id: 'set-agent-logs', label: '日志保留', icon: 'artifacts', apply: 'cycle' },
  { id: 'set-cert', label: '证书', icon: 'certificate', apply: 'now' },
  { id: 'set-xray', label: 'XRAY', icon: 'protocol', apply: 'publish', key: 'xray' },
  { id: 'set-conn', label: '连接策略', icon: 'config', apply: 'publish', key: 'connection' },
  { id: 'set-wg', label: 'WireGuard', icon: 'tunnels', apply: 'publish', key: 'wireguard' },
  { id: 'set-ports', label: '端口分配', icon: 'ingress', apply: 'future', key: 'ports' },
  // 探测配置不进产物：机器下一轮读到新值即生效，最长等一个原有周期。
  { id: 'set-probe', label: '端到端探测', icon: 'observe', apply: 'cycle', key: 'probe' },
  { id: 'set-ping-probe', label: 'Ping 链路探测', icon: 'diag', apply: 'cycle' },
  { id: 'set-geodata', label: '规则库更新', icon: 'dns', apply: 'publish', key: 'geodata' },
];

const APPLY_OF: Record<string, Apply> = Object.fromEntries(NAV.map(item => [item.id, item.apply]));
const ICON_OF: Record<string, IconName> = Object.fromEntries(NAV.map(item => [item.id, item.icon]));

function SettingsTitle({ id, children }: { id: string; children: React.ReactNode }) {
  return <PanelTitle of={ICON_OF[id]}>{children}</PanelTitle>;
}

/** 段标题里的生效方式。复用全站状态签：即时生效是完成态，需要发布是待处理态，
    下一轮生效保持中性，避免再造一套外观相近但语义不同的徽章。 */
function ApplyBadge({ id }: { id: string }) {
  const kind = APPLY_OF[id];
  const tone = kind === 'now' ? 'st-ok' : kind === 'publish' ? 'st-gold' : kind === 'cycle' ? 'st-pending' : '';
  return <span className={`st settings-apply-status ${tone}`}>{APPLY[kind]}</span>;
}

function SettingsSaveBar({
  dirty,
  saving,
  savedText,
  editable,
  disabled = false,
  title,
  label = '保存这一段',
  onSave,
}: {
  dirty: boolean;
  saving: boolean;
  savedText?: string | null;
  editable: boolean;
  disabled?: boolean;
  title?: string;
  label?: string;
  onSave: () => void;
}) {
  if (!dirty) return null;

  return (
    <footer className="settings-savebar">
      <span
        className={dirty ? 'settings-save-state dirty' : savedText ? 'settings-save-state done' : 'settings-save-state'}
      >
        {dirty ? '有未保存的改动' : (savedText ?? '当前设置已保存')}
      </span>
      <button
        type="button"
        className={dirty ? 'btn primary save' : 'btn save'}
        disabled={!editable || !dirty || saving || disabled}
        title={title}
        onClick={onSave}
      >
        {saving ? '保存中…' : label}
      </button>
    </footer>
  );
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
  const probe = useQuery({
    queryKey: ['link-mtu'],
    queryFn: () => fetchLinkMtu(),
    refetchInterval: 60_000,
  });
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
      {/* SettingsPane 的只读态由 fieldset 保证；查看探测结果不修改配置，仍应可展开。
          使用非表单控件避免被 fieldset 一并禁用，并补齐键盘语义。 */}
      <span
        className="btn sm"
        role="button"
        tabIndex={0}
        onClick={() => setOpen(!open)}
        onKeyDown={event => {
          if (event.key === 'Enter' || event.key === ' ') {
            event.preventDefault();
            setOpen(!open);
          }
        }}
      >
        {open ? '收起探测结果' : '看探测结果'}
      </span>
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

function Section({
  id,
  name,
  sub,
  dirty,
  saving,
  savedRev,
  editable,
  validationError = null,
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
  validationError?: string | null;
  onSave: () => void;
  children: React.ReactNode;
}) {
  return (
    <section className="panel config-panel" id={id}>
      <header>
        <SettingsTitle id={id}>{name}</SettingsTitle>
        <ApplyBadge id={id} />
      </header>
      <p className="cardsub">{sub}</p>
      {children}
      <SettingsSaveBar
        dirty={dirty}
        saving={saving}
        savedText={savedRev !== null ? `已保存，盖出修订 ${savedRev}` : null}
        editable={editable}
        disabled={validationError !== null}
        title={validationError ?? undefined}
        onSave={onSave}
      />
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

/** 一张证书的状态。颜色只表示严重程度，文字负责说明它现在是什么角色。 */
function certState(cert: GroupCertificate): { tone: string; text: string } {
  const left = daysLeft(cert.expires_at);
  switch (cert.status) {
    case 'serving':
      // 到期在即才提醒：续期是自动的，但自动也会失败，而失败只有靠剩余天数才看得出来。
      if (left !== null && left < 0) return { tone: 'err', text: '在用 · 已过期' };
      if (left !== null && left <= 7) return { tone: 'err', text: `在用 · 只剩 ${left} 天` };
      if (left !== null && left <= 20) return { tone: 'warn', text: `在用 · 还剩 ${left} 天` };
      return { tone: 'ok', text: '在用' };
    case 'ready':
      return {
        tone: 'idle',
        text: '备用待启用',
      };
    case 'compatible':
      return { tone: 'idle', text: '保留' };
    case 'pending':
      return { tone: 'idle', text: '待签发' };
    case 'failed':
      return { tone: 'err', text: `签发失败 · 试了 ${cert.attempts} 次` };
    default:
      return { tone: 'idle', text: '已换下' };
  }
}

function retainedCertificate(cert: GroupCertificate): boolean {
  return cert.sha256 !== null && ['ready', 'serving', 'compatible'].includes(cert.status);
}

const certificateTime = (value: string | null) => (value ? value.slice(0, 19).replace('T', ' ') : '—');

function issuanceResultText(result: { issued: number; failed: number }): string {
  if (result.issued === 0 && result.failed === 0) return '处理完成，暂无需要签发或续期的证书。';
  return `处理完成：成功 ${result.issued} 张，失败 ${result.failed} 张。${result.failed ? '请查看下方失败原因，修正后可立即重试。' : ''}`;
}

function CertSection({ editable, view }: { editable: boolean; view: CertsView }) {
  const qc = useQueryClient();
  const certForm = (source: CertsView) => ({
    domain: source.domain?.domain ?? '',
    signingMethod: source.domain?.signing_method ?? ('public-ca' as const),
    credential: '',
    directory: source.domain?.signing_method === 'public-ca' ? source.domain.acme_directory : source.letsencrypt,
    contact: source.domain?.acme_contact ?? '',
    renew: String(source.domain?.renew_before_days ?? 30),
  });
  const { form: f, setForm, accept } = useServerForm(certForm(view));
  const [saved, setSaved] = useState(false);
  const save = useMutation({
    onMutate: () => qc.cancelQueries({ queryKey: ['certs'] }),
    mutationFn: (submitted: typeof f) =>
      saveCertDomain({
        domain: submitted.domain.trim(),
        signing_method: submitted.signingMethod,
        dns_credential: submitted.credential.trim() || null,
        acme_directory: submitted.directory,
        acme_contact: submitted.contact.trim() || null,
        renew_before_days: Number(submitted.renew) || 30,
      }),
    onSuccess: async (next, submitted) => {
      await qc.cancelQueries({ queryKey: ['certs'] });
      setSaved(true);
      accept(certForm(next), submitted);
      qc.setQueryData(['certs'], next);
    },
  });

  const scan = useMutation({
    mutationFn: () => scanCerts(),
    onSuccess: () => qc.invalidateQueries({ queryKey: ['certs'] }),
  });

  const d = view.domain;
  const selfSigned = f.signingMethod === 'self-signed';
  const storedDirectory = d?.signing_method === 'public-ca' ? d.acme_directory : view.letsencrypt;
  const dirty =
    (!selfSigned && f.domain.trim() !== (d?.domain ?? '')) ||
    f.signingMethod !== (d?.signing_method ?? 'public-ca') ||
    f.credential.trim() !== '' ||
    (!selfSigned && f.directory !== storedDirectory) ||
    (!selfSigned && f.contact.trim() !== (d?.acme_contact ?? '')) ||
    (!selfSigned && Number(f.renew) !== (d?.renew_before_days ?? 30));

  const staging = f.directory === view.letsencrypt_staging;
  // 折叠标题上的计数：看的是签发失败的证书张数，不是机器台数——一张证书失败会连累整组机器，
  // 数机器会把同一个问题报成好几个。
  const bad = view.groups.flatMap(group => group.certificates.filter(cert => cert.status === 'failed'));

  return (
    <section className="panel config-panel" id="set-cert">
      <header>
        <SettingsTitle id="set-cert">证书</SettingsTitle>
        <span className="sub">申领设置与已签发证书分开管理</span>
      </header>
      <p className="cardsub">新建证书组会立即申领证书。机器切换证书组需保存草稿、提交并发布，应用时重启 Xray。</p>
      {save.error && <ErrorBox error={save.error} />}
      {scan.error && <ErrorBox error={scan.error} />}

      <details className="setgrp settings-block cert-issuance-settings" open={!d}>
        <summary>
          <span className="eyebrow">新证书申领设置</span>
          <span className="hint">
            {d ? (d.signing_method === 'self-signed' ? '当前使用自签证书' : "当前使用 Let's Encrypt") : '请先配置'}
            {dirty ? ' · 有未保存的修改' : ''}
          </span>
        </summary>
        <p className="hint">用于后续新建、备用及续期申领；修改设置不会改写已签发的证书。新申领使用已保存的设置。</p>

        <div className="setfld">
          <label>签发方式</label>
          <div className="v">
            <span
              className={dirty && f.signingMethod !== (d?.signing_method ?? 'public-ca') ? 'segsw chg' : 'segsw'}
              role="group"
              aria-label="证书签发方式"
            >
              <button
                type="button"
                aria-pressed={!selfSigned}
                onClick={() =>
                  setForm({
                    ...f,
                    signingMethod: 'public-ca',
                    directory: f.directory === 'self-signed' ? view.letsencrypt : f.directory,
                  })
                }
              >
                Let&apos;s Encrypt
              </button>
              <button
                type="button"
                aria-pressed={selfSigned}
                onClick={() => setForm({ ...f, signingMethod: 'self-signed', credential: '' })}
              >
                自签证书
              </button>
            </span>
            <span className="hint">
              {selfSigned
                ? '自动生成随机但逼真的专用 SNI，无需填写或持有域名。'
                : "由 Let's Encrypt 验证域名并签发，客户端默认信任。"}
            </span>
          </div>
        </div>

        {!selfSigned && (
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
        )}

        {!selfSigned && (
          <>
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
              <label>签发环境</label>
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
          </>
        )}

        {!selfSigned && (
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
        )}

        {selfSigned ? (
          <div className="guard">
            默认自签证书组首次初始化一对主备证书，单张有效期 100 年。自动名称使用随机生成的 <b>.com</b>
            域名，不再拼接二级域名或通配符；私钥加密保存。
          </div>
        ) : (
          <>
            <div className="guard">
              使用<b>单独的域名</b>。Cloudflare token 按 zone 授权，无法限制到子域，域名分开可防止凭据泄露波及控制面。
            </div>
            <div className="guard">
              每组运行时只安装一张当前证书，数据库最多保留一张待启用备用。Let&apos;s Encrypt
              <b> 同一组名字每 7 天最多签发 5 张</b>，避免无意义消耗额度。
            </div>
            <div className="guard">
              证书会进入 CT 公开日志，随机标签防猜测但不防枚举。DNS-01 <b>不需要 A 记录</b>，名字与 IP
              的对应关系不公开。
            </div>
          </>
        )}
        <SettingsSaveBar
          dirty={dirty}
          saving={save.isPending}
          savedText={saved ? '已保存，后续申领使用此设置' : null}
          editable={editable}
          disabled={!view.sealing_available}
          title={view.sealing_available ? '' : '这台控制面没配 BROCADE_SECRET_KEY，存不了凭据'}
          label="保存申领设置"
          onSave={() => save.mutate(f)}
        />
      </details>

      {d && (
        <div className="setgrp settings-block">
          <p className="eyebrow">
            证书状态
            {bad.length > 0 && <b style={{ color: 'var(--err)' }}> · {bad.length} 张要处理</b>}
          </p>
          <div className="setfld">
            <label />
            <div className="v">
              <button className="btn" disabled={!editable || scan.isPending} onClick={() => scan.mutate()}>
                {scan.isPending ? '正在签发与续期…' : '立即签发与续期'}
              </button>
              <span className="hint">立即处理待签发、失败和需要续期的证书；正常证书不会重新签发。</span>
            </div>
          </div>

          {scan.data?.processing && !scan.isPending && (
            <p role="status" className={scan.data.processing.failed ? 'note bad' : 'hint'}>
              {issuanceResultText(scan.data.processing)}
            </p>
          )}
          <CertGroups view={view} editable={editable && !scan.isPending} />

          <div className="guard">
            续期失败<b>不等于没有证书</b>，在用的到期前仍可用。风险是长期未处理导致过期后整组停服。
          </div>
          <div className="guard">
            公有证书组内续期不改 SNI；自签证书会先进入备用槽，预装确认后才允许启用新的 SNI 与校验值。
          </div>
        </div>
      )}
    </section>
  );
}

/** 证书组及其完整证书记录。机器与证书组的对应关系暂不在全局设置页展示；这里专注于
 * 签发材料、运行位置、有效期和失败信息，避免把证书状态与机器收敛状态混在一起。 */
function CertGroups({ view, editable }: { view: CertsView; editable: boolean }) {
  const qc = useQueryClient();
  const reload = () => qc.invalidateQueries({ queryKey: ['certs'] });
  const selfSigned = view.domain?.signing_method === 'self-signed';
  const [creating, setCreating] = useState<{ name: string; note: string; certificateName: string } | null>(null);
  const [editing, setEditing] = useState<{
    id: string;
    name: string;
    note: string;
    certificateName: string;
  } | null>(null);
  const [failed, setFailed] = useState<string | null>(null);
  const [pending, setPending] = useState<string | null>(null);
  const [resultText, setResultText] = useState<string | null>(null);
  const [expandedGroups, setExpandedGroups] = useState<Set<string>>(() => new Set());
  // Unlike the section saves, these older actions are plain promises rather than useMutation.
  // Keep a synchronous lock as well as disabled buttons: two click events can be delivered before
  // React commits the pending render, and asking for one spare must never create two rows.
  const pendingRef = useRef(false);

  const openGroup = (id: string) =>
    setExpandedGroups(current => {
      if (current.has(id)) return current;
      const next = new Set(current);
      next.add(id);
      return next;
    });

  const toggleGroup = (id: string) =>
    setExpandedGroups(current => {
      const next = new Set(current);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });

  const run = (key: string, what: () => Promise<unknown>, onSuccess?: () => void) => {
    if (pendingRef.current) return;
    pendingRef.current = true;
    setPending(key);
    setFailed(null);
    setResultText(null);
    Promise.resolve()
      .then(what)
      .then(result => {
        if (result && typeof result === 'object' && 'processing' in result) {
          const processing = result.processing as { issued: number; failed: number };
          setResultText(issuanceResultText(processing));
          if ('id' in result && typeof result.id === 'string') {
            openGroup(key.startsWith('spare:') ? key.slice('spare:'.length) : result.id);
          }
        }
        onSuccess?.();
        return reload();
      })
      .catch((error: unknown) => {
        setFailed(String(error));
        return reload();
      })
      .finally(() => {
        pendingRef.current = false;
        setPending(null);
      });
  };

  if (view.groups.length === 0) {
    return (
      <>
        {creating ? (
          <GroupForm
            value={creating}
            creating
            showCertificateName={selfSigned}
            busy={pending !== null}
            onChange={setCreating}
            onCancel={() => setCreating(null)}
            onSave={() => {
              run(
                'create-group',
                () =>
                  createCertGroup({
                    name: creating.name,
                    note: creating.note || null,
                    certificate_name: creating.certificateName.trim() || null,
                  }),
                () => setCreating(null),
              );
            }}
          />
        ) : (
          <div className="guard">
            还没有证书组。同组机器共享 SNI 和证书，签发额度按组计算。同组机器会被识别为同一批。
            <div style={{ marginTop: 8 }}>
              <button
                className="btn"
                disabled={!editable || pending !== null}
                onClick={() => setCreating({ name: '', note: '', certificateName: '' })}
              >
                新建证书组
              </button>
            </div>
          </div>
        )}
        {failed && <div className="note bad">{failed}</div>}
        {resultText && (
          <p role="status" className="hint">
            {resultText}
          </p>
        )}
      </>
    );
  }

  return (
    <>
      <div className="certgroups-toolbar">
        <div>
          <b>证书组</b>
          <span className="hint">按组查看完整的 SNI、签发记录和运行槽</span>
        </div>
        <div className="v">
          <button
            type="button"
            className="btn"
            aria-label="新建证书组"
            disabled={!editable || !!creating || pending !== null}
            onClick={() => setCreating({ name: '', note: '', certificateName: '' })}
          >
            ＋ 新建证书组
          </button>
        </div>
      </div>
      {creating && (
        <GroupForm
          value={creating}
          creating
          showCertificateName={selfSigned}
          busy={pending !== null}
          onChange={setCreating}
          onCancel={() => setCreating(null)}
          onSave={() => {
            run(
              'create-group',
              () =>
                createCertGroup({
                  name: creating.name,
                  note: creating.note || null,
                  certificate_name: creating.certificateName.trim() || null,
                }),
              () => setCreating(null),
            );
          }}
        />
      )}
      {failed && <div className="note bad">{failed}</div>}
      {resultText && (
        <p role="status" className="hint">
          {resultText}
        </p>
      )}

      {view.groups.map(group => {
        const expanded = expandedGroups.has(group.id);
        const members = view.nodes.filter(row => row.label_id === group.id);
        const groupSelfSigned = group.signing_method === 'self-signed';
        const failedCount = group.certificates.filter(cert => cert.status === 'failed').length;
        const pendingCount = group.certificates.filter(cert => cert.status === 'pending').length;
        const xrayPinsActive = group.certificates.some(
          cert => retainedCertificate(cert) && cert.signing_method === 'self-signed',
        );
        const configuredMethod = group.signing_method;
        const standbyBusy = group.certificates.some(
          cert =>
            cert.signing_method === configuredMethod &&
            ['pending', 'ready', 'compatible', 'failed'].includes(cert.status),
        );
        const preloadPending = members.some(row => row.on_disk !== 'current');
        const runtimeSlots = groupSelfSigned
          ? group.certificates.filter(cert => cert.signing_method === configuredMethod && cert.runtime_slot !== null)
              .length
          : 0;
        return (
          <article className={`certgrp${expanded ? ' expanded' : ''}`} key={group.id}>
            <header className="certgrp-hd">
              <button
                type="button"
                className="certgrp-identity certgrp-toggle"
                aria-expanded={expanded}
                aria-controls={`certgrp-body-${group.id}`}
                onClick={() => toggleGroup(group.id)}
              >
                <span className="certgrp-mark" aria-hidden="true">
                  {group.is_default ? '默' : group.name.slice(0, 1).toUpperCase()}
                </span>
                <div>
                  <div className="certgrp-title">
                    <b>{group.name}</b>
                    {group.is_default && <span className="certgrp-default">默认</span>}
                    <span className="cert-origin">{groupSelfSigned ? '自签证书' : "Let's Encrypt"}</span>
                    {failedCount > 0 && <span className="cstate err">{failedCount} 张签发失败</span>}
                    {pendingCount > 0 && <span className="cstate idle">{pendingCount} 张待签发</span>}
                  </div>
                  {group.note && <span className="certgrp-note">{group.note}</span>}
                </div>
                <span className="certgrp-disclosure" aria-hidden="true" />
              </button>
              <span className="ctl">
                <button
                  type="button"
                  className="btn sm"
                  disabled={!editable || pending !== null || group.is_default}
                  title={group.is_default ? '默认自签证书组名称固定' : '修改证书组名称'}
                  onClick={() => {
                    openGroup(group.id);
                    setEditing({
                      id: group.id,
                      name: group.name,
                      note: group.note ?? '',
                      certificateName: '',
                    });
                  }}
                >
                  改名
                </button>
                {!groupSelfSigned && (
                  <button
                    type="button"
                    className="btn sm"
                    disabled={!editable || pending !== null || standbyBusy}
                    title={
                      standbyBusy
                        ? groupSelfSigned
                          ? '主备槽已经占满，请先停止并清理旧保留证书'
                          : '已有待处理的备用证书，请先启用或清理'
                        : '多签一张待命证书，由你决定何时启用'
                    }
                    onClick={() => run(`spare:${group.id}`, () => requestSpareCertificate(group.id))}
                  >
                    {pending === `spare:${group.id}` ? '正在申领…' : '立即申领备用证书'}
                  </button>
                )}
                <button
                  type="button"
                  className="btn sm danger"
                  disabled={!editable || members.length > 0 || pending !== null || group.is_default}
                  title={
                    group.is_default
                      ? '默认自签证书组不能删除'
                      : members.length > 0
                        ? '还有机器在用这个组'
                        : '删除这个组'
                  }
                  onClick={() => {
                    if (window.confirm(`删除证书组「${group.name}」？它的证书会一并删除。`)) {
                      run(`delete-group:${group.id}`, () => deleteCertGroup(group.id));
                    }
                  }}
                >
                  删除
                </button>
              </span>
            </header>

            <div id={`certgrp-body-${group.id}`} className="certgrp-body" hidden={!expanded}>
              <dl className="certgrp-facts" aria-label="证书组完整信息">
                <div>
                  <dt>组状态</dt>
                  <dd>{group.status === 'active' ? '使用中' : group.status === 'draining' ? '排空中' : '已停用'}</dd>
                </div>
                {groupSelfSigned ? (
                  <div>
                    <dt>运行槽</dt>
                    <dd>{runtimeSlots}/2</dd>
                  </div>
                ) : (
                  <div>
                    <dt>运行方式</dt>
                    <dd>单一当前证书</dd>
                  </div>
                )}
                <div>
                  <dt>标签</dt>
                  <dd className="mono">{group.label}</dd>
                </div>
                <div>
                  <dt>域名</dt>
                  <dd className="mono">{group.domain || '—'}</dd>
                </div>
                <div className="wide">
                  <dt>证书名称</dt>
                  <dd className="mono">{group.names.length > 0 ? group.names.join(' · ') : '—'}</dd>
                </div>
                <div className="wide">
                  <dt>证书组 ID</dt>
                  <dd className="mono">{group.id}</dd>
                </div>
              </dl>

              {editing?.id === group.id && (
                <GroupForm
                  value={editing}
                  busy={pending !== null}
                  onChange={next => setEditing({ ...next, id: group.id })}
                  onCancel={() => setEditing(null)}
                  onSave={() => {
                    run(
                      `update-group:${group.id}`,
                      () => updateCertGroup(group.id, { name: editing.name, note: editing.note || null }),
                      () => setEditing(null),
                    );
                  }}
                />
              )}

              {xrayPinsActive && (
                <div className="certtrust-note">
                  两张自签证书的 SNI 始终同时有效。切换只更新新订阅；已保存的配置仍可使用原 SNI，拨测也会继续信任它。
                </div>
              )}

              <section className="certgrp-section">
                <header>
                  <b>已签发证书与申领记录</b>
                  <span>{group.certificates.length} 张</span>
                </header>
                <div className="certtbl">
                  {group.certificates.length === 0 ? (
                    <div className="hint">还没有证书。点击上方「立即签发与续期」开始申领。</div>
                  ) : (
                    group.certificates.map(cert => {
                      const state = certState(cert);
                      const left = daysLeft(cert.expires_at);
                      const trustedByXray =
                        cert.signing_method === 'self-signed' &&
                        ['ready', 'serving', 'compatible'].includes(cert.status);
                      const origin =
                        cert.origin === 'bootstrap' ? '初始化' : cert.origin === 'spare' ? '手动添加' : '自动续期';
                      return (
                        <article className={`cert-record ${state.tone}`} key={cert.id}>
                          <header className="cert-record-head">
                            <div className="cert-record-state">
                              <span className={`cstate ${state.tone}`}>{state.text}</span>
                              <span className="cert-origin">{origin}</span>
                              {trustedByXray && (
                                <span className={left !== null && left < 0 ? 'ctrust warn' : 'ctrust'}>
                                  {left !== null && left < 0
                                    ? '已过期 · 仍在运行槽'
                                    : cert.status === 'ready'
                                      ? '备用槽已就绪'
                                      : 'XRAY 正在提供'}
                                </span>
                              )}
                            </div>
                            <div className="cert-record-actions">
                              {(cert.status === 'ready' || cert.status === 'compatible') && (
                                <button
                                  type="button"
                                  className="btn sm"
                                  disabled={
                                    !editable ||
                                    pending !== null ||
                                    (cert.signing_method === 'self-signed' && preloadPending)
                                  }
                                  title={
                                    cert.status === 'compatible'
                                      ? '切换到这张保留证书；当前证书将转为保留'
                                      : cert.signing_method === 'self-signed'
                                        ? preloadPending
                                          ? '等待主备槽预装确认后才能启用'
                                          : '启用这张证书，更新新订阅的 SNI 与校验值'
                                        : '让这个组改用这张同名公有证书'
                                  }
                                  onClick={() => run(`serve:${cert.id}`, () => serveCertificate(cert.id))}
                                >
                                  {pending === `serve:${cert.id}`
                                    ? '切换中…'
                                    : cert.status === 'compatible'
                                      ? '切换'
                                      : '启用'}
                                </button>
                              )}
                              {cert.status !== 'serving' && (
                                <button
                                  type="button"
                                  className="btn sm danger"
                                  disabled={!editable || pending !== null}
                                  title={
                                    cert.status === 'compatible'
                                      ? '停止保留；仍使用旧配置的客户端将无法重新连接'
                                      : '删除这条证书记录'
                                  }
                                  onClick={() => {
                                    const impact =
                                      cert.status === 'compatible'
                                        ? '停止保留后，仍使用这份旧 SNI 和证书校验的客户端将无法重新连接。继续？'
                                        : '删除这条证书记录？';
                                    if (window.confirm(impact)) {
                                      run(`delete-certificate:${cert.id}`, () => deleteCertificate(cert.id));
                                    }
                                  }}
                                >
                                  删除
                                </button>
                              )}
                            </div>
                          </header>
                          <dl className="cert-record-facts">
                            <div className="wide primary">
                              <dt>SNI / 证书名称</dt>
                              <dd className="mono">{cert.certificate_name ?? '等待签发'}</dd>
                            </div>
                            {cert.signing_method === 'self-signed' && (
                              <div>
                                <dt>运行槽</dt>
                                <dd>{cert.runtime_slot ? cert.runtime_slot.toUpperCase() : '未分配'}</dd>
                              </div>
                            )}
                            <div>
                              <dt>签发方式</dt>
                              <dd>{cert.signing_method === 'self-signed' ? '自签证书' : "Let's Encrypt"}</dd>
                            </div>
                            <div className="wide">
                              <dt>签发者</dt>
                              <dd className={cert.issuer && /STAGING/i.test(cert.issuer) ? 'warn' : undefined}>
                                {cert.issuer
                                  ? /STAGING/i.test(cert.issuer)
                                    ? `${cert.issuer}（不被信任）`
                                    : cert.issuer
                                  : '—'}
                              </dd>
                            </div>
                            <div>
                              <dt>签发时间</dt>
                              <dd className="mono">{certificateTime(cert.issued_at)}</dd>
                            </div>
                            <div>
                              <dt>到期时间</dt>
                              <dd className="mono">{certificateTime(cert.expires_at)}</dd>
                            </div>
                            <div>
                              <dt>剩余有效期</dt>
                              <dd>{left === null ? '—' : left < 0 ? `已过期 ${Math.abs(left)} 天` : `${left} 天`}</dd>
                            </div>
                            <div>
                              <dt>签发尝试</dt>
                              <dd>{cert.attempts} 次</dd>
                            </div>
                            <div>
                              <dt>最后尝试</dt>
                              <dd className="mono">{certificateTime(cert.last_attempt_at)}</dd>
                            </div>
                            <div className="wide">
                              <dt>SHA-256</dt>
                              <dd className="mono break">{cert.sha256 ?? '—'}</dd>
                            </div>
                            <div className="wide">
                              <dt>证书 ID</dt>
                              <dd className="mono break">{cert.id}</dd>
                            </div>
                          </dl>
                          {cert.last_error && (
                            <div className="cert-record-error">
                              <b>最后错误</b>
                              <span>{cert.last_error}</span>
                            </div>
                          )}
                        </article>
                      );
                    })
                  )}
                </div>
              </section>
            </div>
          </article>
        );
      })}
    </>
  );
}

/** 建组和改组用同一个表单：两者要填的东西相同，分开写会让它们慢慢长得不一样。 */
function GroupForm({
  creating = false,
  value,
  busy,
  showCertificateName = false,
  onChange,
  onCancel,
  onSave,
}: {
  creating?: boolean;
  value: { name: string; note: string; certificateName: string };
  busy: boolean;
  showCertificateName?: boolean;
  onChange: (next: { name: string; note: string; certificateName: string }) => void;
  onCancel: () => void;
  onSave: () => void;
}) {
  return (
    <div className="cert-group-form">
      <div className="cert-group-form-fields">
        <label>
          <span>组名</span>
          <input
            className="f"
            placeholder="香港前置"
            value={value.name}
            disabled={busy}
            onChange={e => onChange({ ...value, name: e.target.value })}
          />
        </label>
        <label>
          <span>备注</span>
          <input
            className="f"
            placeholder="选填，这组是做什么的"
            value={value.note}
            disabled={busy}
            onChange={e => onChange({ ...value, note: e.target.value })}
          />
        </label>
        {showCertificateName && (
          <label>
            <span>证书名称</span>
            <input
              className="f mono"
              placeholder="选填；留空自动生成"
              value={value.certificateName ?? ''}
              disabled={busy}
              onChange={e => onChange({ ...value, certificateName: e.target.value })}
              title="仅手动新建时可指定；留空会生成随机的 .com 名称"
            />
          </label>
        )}
      </div>
      <div className="cert-group-form-actions">
        <span className="hint">
          {creating ? '使用已保存的申领设置，创建后立即签发并显示结果。' : '保存后立即更新组信息'}
        </span>
        <button type="button" className="btn" disabled={busy} onClick={onCancel}>
          取消
        </button>
        <button type="button" className="btn primary" disabled={busy || !value.name.trim()} onClick={onSave}>
          {busy ? (creating ? '正在创建并申领…' : '保存中…') : creating ? '创建并立即申领' : '保存'}
        </button>
      </div>
    </div>
  );
}

const BRAND_ICON_TYPES = ['image/png', 'image/jpeg', 'image/webp'];
const BRAND_ICON_MAX_BYTES = 256 * 1024;

function BrandingSection({ editable, data }: { editable: boolean; data: BrandingSettings }) {
  const qc = useQueryClient();
  const { form: f, setForm, accept } = useServerForm(data);
  const [savedAt, setSavedAt] = useState(false);
  const [fileError, setFileError] = useState<string | null>(null);
  const dirty = f.site_name !== data.site_name || f.icon_data_url !== data.icon_data_url;
  const save = useMutation({
    onMutate: () => qc.cancelQueries({ queryKey: ['branding'] }),
    mutationFn: (submitted: BrandingSettings) => saveBranding(submitted),
    onSuccess: async (saved, submitted) => {
      await qc.cancelQueries({ queryKey: ['branding'] });
      setSavedAt(true);
      setFileError(null);
      accept(saved, submitted);
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
    <section className="panel config-panel" id="set-branding">
      <header>
        <SettingsTitle id="set-branding">站点外观</SettingsTitle>
        <ApplyBadge id="set-branding" />
      </header>
      <p className="cardsub">控制台左上角使用这里的名称和图标；名称也同步到登录页和浏览器标题</p>
      {save.error && <ErrorBox error={save.error} />}
      {fileError && <div className="callout err">{fileError}</div>}
      <Group label="品牌标识">
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
      <SettingsSaveBar
        dirty={dirty}
        saving={save.isPending}
        savedText={savedAt ? '已保存，立刻生效' : null}
        editable={editable}
        onSave={() => save.mutate(f)}
      />
    </section>
  );
}

function VisitorAccessSection({ editable, enabled }: { editable: boolean; enabled: boolean }) {
  const qc = useQueryClient();
  const update = useMutation({
    mutationFn: (next: boolean) => setVisitorAccess(next),
    onSuccess: state => qc.setQueryData(['auth-state'], state),
  });

  return (
    <section className="panel config-panel" id="set-visitor">
      <header>
        <SettingsTitle id="set-visitor">访客模式</SettingsTitle>
        <ApplyBadge id="set-visitor" />
        <span className="sp" />
        <b>{enabled ? '已开启' : '已关闭'}</b>
      </header>
      <p className="cardsub">开启后无需账号即可进入脱敏后的只读页面；关闭会立即退出现有访客</p>
      {update.error && <ErrorBox error={update.error} />}
      <Group label="公开访问">
        <Fld label="访问状态">
          <span className="segsw" role="group">
            <button
              type="button"
              aria-pressed={!enabled}
              disabled={!editable || update.isPending}
              onClick={() => update.mutate(false)}
            >
              关闭
            </button>
            <button
              type="button"
              aria-pressed={enabled}
              disabled={!editable || update.isPending}
              onClick={() => update.mutate(true)}
            >
              开启
            </button>
          </span>
          <span className="hint">管理员和用户登录不受影响</span>
        </Fld>
      </Group>
    </section>
  );
}

function DistributionSection({ editable, data }: { editable: boolean; data: DistributionView }) {
  const qc = useQueryClient();
  const { form: f, setForm, accept } = useServerForm({ url: data.stored.agent_public_url ?? '' });
  const [savedAt, setSavedAt] = useState(false);
  const save = useMutation({
    onMutate: () => qc.cancelQueries({ queryKey: ['distribution'] }),
    mutationFn: (submitted: { url: string }) =>
      saveDistribution({
        agent_public_url: submitted.url.trim() || null,
        xray_version: data.stored.xray_version,
      }),
    onSuccess: async (next, submitted) => {
      await qc.cancelQueries({ queryKey: ['distribution'] });
      setSavedAt(true);
      accept({ url: next.stored.agent_public_url ?? '' }, submitted);
      qc.setQueryData(['distribution'], next);
    },
  });
  const stored = data.stored;
  const effective = data.effective;
  const dirty = f.url !== (stored.agent_public_url ?? '');
  // 地址留空会回退到进程启动时的环境变量，因此仍显示实际值，避免把空输入框误解为未配置。
  const fallbackNote = (own: string | null, live: string | null) =>
    !own && live ? <span className="hint">当前生效：{live}（来自环境变量）</span> : null;

  return (
    <section className="panel config-panel" id="set-dist">
      <header>
        <SettingsTitle id="set-dist">分发</SettingsTitle>
        <ApplyBadge id="set-dist" />
      </header>
      <p className="cardsub">节点从哪里访问这台控制面，以及当前控制台内置的 XRAY 构建</p>
      {save.error && <ErrorBox error={save.error} />}
      <Group label="节点分发">
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
        <Fld label="内置 XRAY">
          <span className={effective.xray_version ? 'st st-ok' : 'st st-warn'}>
            {effective.xray_version ? '内置可用' : '版本未知'}
          </span>
          {effective.xray_version && <code>{effective.xray_version}</code>}
          <span className="hint">随当前控制台构建提供，不支持在设置中覆盖</span>
        </Fld>
        <div className="guard">
          版本与二进制由控制台一同内置，节点安装时会校验文件摘要。升级 XRAY 需要部署包含目标构建的新控制台。
        </div>
      </Group>
      <SettingsSaveBar
        dirty={dirty}
        saving={save.isPending}
        savedText={savedAt ? '已保存，立刻生效' : null}
        editable={editable}
        onSave={() => save.mutate(f)}
      />
    </section>
  );
}

/* 机器详情页的「本机覆盖」卡也编辑这一项，取值范围由此处导出而非各自写一份：
   两处写同一个数字时，改动只会落在其中一处，另一处把服务端会拒绝的值显示为合法。 */
export const LOG_MIN_MIB = 16;
export const LOG_MAX_MIB = 4096;

export const validLogMib = (raw: string) => {
  if (!/^\d+$/.test(raw.trim())) return null;
  const value = Number(raw);
  return Number.isSafeInteger(value) && value >= LOG_MIN_MIB && value <= LOG_MAX_MIB ? value : null;
};

type AgentLogKey = keyof AgentLogLimits;
type AgentLogForm = Record<AgentLogKey, string>;

const AGENT_LOG_CLASSES: ReadonlyArray<{
  key: AgentLogKey;
  kind: string;
  name: string;
  file: string;
}> = [
  {
    key: 'agent_journal_mib',
    kind: 'Agent',
    name: '系统日志',
    file: '独立 journal 命名空间',
  },
  { key: 'xray_mib', kind: 'XRAY', name: '运行日志', file: 'xray.log + xray.log.1' },
  {
    key: 'phantun_mib',
    kind: 'Phantun',
    name: '每个运行实例',
    file: '每个实例的 .log + .log.1',
  },
];

const logLimitForm = (limits: AgentLogLimits): AgentLogForm => ({
  agent_journal_mib: String(limits.agent_journal_mib),
  xray_mib: String(limits.xray_mib),
  phantun_mib: String(limits.phantun_mib),
});

const logOverrideForm = (overrides: AgentLogLimitOverrides): AgentLogForm => ({
  agent_journal_mib: overrides.agent_journal_mib == null ? '' : String(overrides.agent_journal_mib),
  xray_mib: overrides.xray_mib == null ? '' : String(overrides.xray_mib),
  phantun_mib: overrides.phantun_mib == null ? '' : String(overrides.phantun_mib),
});

const parsedLogLimits = (form: AgentLogForm): AgentLogLimits | null => {
  const agent = validLogMib(form.agent_journal_mib);
  const xray = validLogMib(form.xray_mib);
  const phantun = validLogMib(form.phantun_mib);
  return agent === null || xray === null || phantun === null
    ? null
    : { agent_journal_mib: agent, xray_mib: xray, phantun_mib: phantun };
};

const parsedLogOverrides = (form: AgentLogForm): AgentLogLimitOverrides | null => {
  const parse = (raw: string) => (raw.trim() === '' ? null : validLogMib(raw));
  const agent = parse(form.agent_journal_mib);
  const xray = parse(form.xray_mib);
  const phantun = parse(form.phantun_mib);
  if (
    (form.agent_journal_mib.trim() !== '' && agent === null) ||
    (form.xray_mib.trim() !== '' && xray === null) ||
    (form.phantun_mib.trim() !== '' && phantun === null)
  ) {
    return null;
  }
  return { agent_journal_mib: agent, xray_mib: xray, phantun_mib: phantun };
};

export function NodeLogPolicyRow({
  editable,
  node,
  global,
}: {
  editable: boolean;
  node: AgentLogPolicyNode;
  global: AgentLogLimits;
}) {
  const qc = useQueryClient();
  const { form, setForm, accept } = useServerForm(logOverrideForm(node.overrides));
  const mutation = useMutation({
    mutationFn: (overrides: AgentLogLimitOverrides) => saveNodeLogPolicy(node.node_id, overrides),
    onMutate: () => qc.cancelQueries({ queryKey: ['agent-log-policy'] }),
    onSuccess: async (view, submitted) => {
      await qc.cancelQueries({ queryKey: ['agent-log-policy'] });
      const saved = view.nodes.find(item => item.node_id === node.node_id);
      if (saved) accept(logOverrideForm(saved.overrides), logOverrideForm(submitted));
      qc.setQueryData(['agent-log-policy'], view);
    },
  });
  const baseline = logOverrideForm(node.overrides);
  const next = parsedLogOverrides(form);
  const dirty = AGENT_LOG_CLASSES.some(item => form[item.key].trim() !== baseline[item.key]);
  const overrideCount = AGENT_LOG_CLASSES.filter(item => node.overrides[item.key] !== null).length;
  const clear: AgentLogLimitOverrides = { agent_journal_mib: null, xray_mib: null, phantun_mib: null };

  return (
    <div className="agent-log-node">
      <div className="agent-log-node-head">
        <div className="agent-log-node-name">
          <b>{node.name}</b>
        </div>
        <span className={overrideCount === 0 ? 'st' : 'st st-warn'}>
          {overrideCount === 0 ? '全部继承' : `${overrideCount} 项覆盖`}
        </span>
      </div>
      <div className="agent-log-node-limits">
        {AGENT_LOG_CLASSES.map(item => (
          <label key={item.key}>
            <span>{item.kind}</span>
            <input
              className={form[item.key].trim() !== baseline[item.key] ? 'f chg' : 'f'}
              type="number"
              min={LOG_MIN_MIB}
              max={LOG_MAX_MIB}
              step={1}
              aria-label={`${node.name} ${item.kind} 日志上限`}
              placeholder={String(global[item.key])}
              disabled={!editable || mutation.isPending}
              value={form[item.key]}
              onChange={event => setForm({ ...form, [item.key]: event.target.value })}
            />
            <small>{form[item.key].trim() === '' ? `继承 ${global[item.key]}` : 'MiB'}</small>
          </label>
        ))}
      </div>
      <div className="agent-log-node-actions">
        {dirty && (
          <button
            className="btn sm primary"
            type="button"
            disabled={!editable || next === null || mutation.isPending}
            onClick={() => next && mutation.mutate(next)}
          >
            {mutation.isPending ? '保存中…' : '保存覆盖'}
          </button>
        )}
        {dirty && (
          <button className="btn sm" type="button" disabled={mutation.isPending} onClick={() => setForm(baseline)}>
            还原
          </button>
        )}
        {overrideCount > 0 && !dirty && (
          <button
            className="btn sm"
            type="button"
            disabled={!editable || mutation.isPending}
            onClick={() => mutation.mutate(clear)}
          >
            {mutation.isPending ? '清除中…' : '全部继承'}
          </button>
        )}
      </div>
      {next === null && (
        <span className="agent-log-invalid">
          三项均需留空或填写 {LOG_MIN_MIB}–{LOG_MAX_MIB} 的整数
        </span>
      )}
      {mutation.error && <ErrorBox error={mutation.error} />}
    </div>
  );
}

export function AgentLogPolicySection({ editable, data }: { editable: boolean; data: AgentLogPolicyView }) {
  const qc = useQueryClient();
  const { form, setForm, accept } = useServerForm(logLimitForm(data.global));
  const limits = parsedLogLimits(form);
  const dirty = limits !== null && AGENT_LOG_CLASSES.some(item => limits[item.key] !== data.global[item.key]);
  const save = useMutation({
    onMutate: () => qc.cancelQueries({ queryKey: ['agent-log-policy'] }),
    mutationFn: (submitted: AgentLogForm) => saveAgentLogDefault(parsedLogLimits(submitted)!),
    onSuccess: async (view, submitted) => {
      await qc.cancelQueries({ queryKey: ['agent-log-policy'] });
      accept(logLimitForm(view.global), submitted);
      qc.setQueryData(['agent-log-policy'], view);
    },
  });

  return (
    <section className="panel config-panel agent-log-policy" id="set-agent-logs">
      <header>
        <SettingsTitle id="set-agent-logs">日志保留</SettingsTitle>
        <ApplyBadge id="set-agent-logs" />
      </header>
      <p className="cardsub">只设置全局默认值；单台机器的覆盖项在对应机器配置中管理</p>
      {save.error && <ErrorBox error={save.error} />}
      <Group label="全局日志上限 · 每类独立">
        <div className="agent-log-scope" aria-label="日志额度作用范围">
          {AGENT_LOG_CLASSES.map(item => (
            <div className="agent-log-scope-row" key={item.key}>
              <span className="agent-log-scope-kind">{item.kind}</span>
              <span className="agent-log-scope-name">
                <b>{item.name}</b>
                <code>{item.file}</code>
              </span>
              <label className="agent-log-scope-limit">
                <input
                  className={limits && limits[item.key] !== data.global[item.key] ? 'f chg' : 'f'}
                  type="number"
                  min={LOG_MIN_MIB}
                  max={LOG_MAX_MIB}
                  step={1}
                  aria-label={`全局 ${item.kind} 日志上限`}
                  disabled={!editable || save.isPending}
                  value={form[item.key]}
                  onChange={event => setForm({ ...form, [item.key]: event.target.value })}
                />
                <span>MiB</span>
              </label>
            </div>
          ))}
        </div>
        {limits === null && (
          <span className="agent-log-invalid">
            三项均需填写 {LOG_MIN_MIB}–{LOG_MAX_MIB} 的整数
          </span>
        )}
        <p className="agent-log-scope-note">保存后由机器下一轮读取，不产生修订，也不需要发布线路。</p>
      </Group>
      <SettingsSaveBar
        dirty={dirty}
        saving={save.isPending}
        savedText={save.isSuccess ? '已保存，下一轮生效' : null}
        editable={editable}
        label="保存全局值"
        onSave={() => save.mutate(form)}
      />
    </section>
  );
}

const validPingProbeNumber = (value: number, min: number, max: number) =>
  Number.isInteger(value) && value >= min && value <= max;

function validIpv6Literal(host: string): boolean {
  try {
    new URL(`http://[${host}]/`);
    return true;
  } catch {
    return false;
  }
}

export function pingProbeAddressError(address: string): string | null {
  if (address.length > 512) return '探测地址不能超过 512 个字符';
  const tcp = address.match(/^tcp:\/\/(.+)$/);
  if (tcp) {
    const authority = tcp[1];
    if (/\s|[/?#]/.test(authority)) return 'TCP 地址格式应为 tcp://host:port';
    const bracketedMatch = authority.match(/^\[([^\]]+)]:(\d+)$/);
    const bracketed = bracketedMatch && validIpv6Literal(bracketedMatch[1]) ? bracketedMatch : null;
    const plainMatch = authority.match(/^(.+):(\d+)$/);
    const plain =
      plainMatch && !plainMatch[1].includes(':') && !plainMatch[1].includes('[') && !plainMatch[1].includes(']')
        ? plainMatch
        : null;
    const match = bracketed ?? plain;
    if (!match) return 'TCP 地址格式应为 tcp://host:port；IPv6 地址需放在方括号内';
    const port = Number(match[2]);
    return Number.isInteger(port) && port >= 1 && port <= 65_535 ? null : 'TCP 端口必须为 1–65535';
  }
  const icmp = address.match(/^icmp:\/\/(.+)$/);
  if (icmp) {
    const authority = icmp[1];
    if (/\s|[/?#]/.test(authority)) return 'ICMP 地址格式应为 icmp://host，不接受路径或端口';
    const bracketed = authority.match(/^\[([^\]]+)]$/);
    if (bracketed) return validIpv6Literal(bracketed[1]) ? null : 'ICMP 方括号内必须是 IPv6 地址';
    if (!authority || authority.includes(':') || authority.includes('[') || authority.includes(']'))
      return 'ICMP 不接受端口；IPv6 地址需放在方括号内';
    return null;
  }
  return '探测地址必须使用 tcp://host:port 或 icmp://host';
}

export function pingProbeFormError(form: PingProbeSettings): string | null {
  if (!validPingProbeNumber(form.interval_secs, 5, 86_400)) return '探测间隔必须为 5–86400 秒的整数';
  if (!validPingProbeNumber(form.timeout_ms, 1, 120_000)) return '探测超时必须为 1–120000 毫秒的整数';
  if (form.targets.length > 32) return '最多配置 32 个目标';
  const addresses = new Set<string>();
  for (const target of form.targets) {
    if (!target.name.trim()) return '每个目标都要填写名称';
    if (Array.from(target.name.trim()).length > 64) return '目标名称不能超过 64 个字符';
    const addressError = pingProbeAddressError(target.address.trim());
    if (addressError) return addressError;
    if (addresses.has(target.address.trim())) return `地址不能重复：${target.address.trim()}`;
    addresses.add(target.address.trim());
  }
  return null;
}

function PingProbeSettingsSection({ editable, data }: { editable: boolean; data: PingProbeSettings }) {
  const qc = useQueryClient();
  const { form, setForm, accept } = useServerForm(data);
  const [saved, setSaved] = useState(false);
  const normalized = {
    ...form,
    targets: form.targets.map(target => ({ name: target.name.trim(), address: target.address.trim() })),
  };
  const dirty = JSON.stringify(normalized) !== JSON.stringify(data);
  const invalid = pingProbeFormError(normalized);
  const save = useMutation({
    onMutate: () => qc.cancelQueries({ queryKey: ['ping-probe-settings'] }),
    mutationFn: (submitted: PingProbeSettings) =>
      savePingProbeSettings({
        ...submitted,
        targets: submitted.targets.map(target => ({ name: target.name.trim(), address: target.address.trim() })),
      }),
    onSuccess: async (next, submitted) => {
      await qc.cancelQueries({ queryKey: ['ping-probe-settings'] });
      setSaved(true);
      accept(next, submitted);
      qc.setQueryData(['ping-probe-settings'], next);
      qc.invalidateQueries({ queryKey: ['ping-probe-nodes'] });
    },
  });

  const updateTarget = (index: number, field: 'name' | 'address', value: string) =>
    setForm(current => ({
      ...current,
      targets: current.targets.map((target, targetIndex) =>
        targetIndex === index ? { ...target, [field]: value } : target,
      ),
    }));

  return (
    <section className="panel config-panel ping-probe-settings" id="set-ping-probe">
      <header>
        <SettingsTitle id="set-ping-probe">Ping 链路探测</SettingsTitle>
        <ApplyBadge id="set-ping-probe" />
      </header>
      <p className="cardsub">统一配置巡检节奏与目标；TCP Connect 和 ICMP Echo 共用一张目标清单</p>
      {save.error && <ErrorBox error={save.error} />}
      <div className="ping-probe-schedule" aria-label="Ping 探测调度">
        <label className="ping-probe-timing">
          <span>巡检周期</span>
          <div>
            <input
              className="f"
              aria-label="探测间隔"
              type="number"
              min={5}
              max={86_400}
              value={form.interval_secs}
              onChange={event => setForm({ ...form, interval_secs: Number(event.target.value) })}
            />
            <b>秒</b>
          </div>
          <small>默认 60；每轮每个目标各探测一次</small>
        </label>
        <label className="ping-probe-timing">
          <span>单次超时</span>
          <div>
            <input
              className="f"
              aria-label="探测超时"
              type="number"
              min={1}
              max={120_000}
              value={form.timeout_ms}
              onChange={event => setForm({ ...form, timeout_ms: Number(event.target.value) })}
            />
            <b>ms</b>
          </div>
          <small>默认 420；超过后记为无响应</small>
        </label>
      </div>
      <section className="ping-probe-target-section">
        <header className="ping-probe-target-head">
          <div>
            <b>探测目标</b>
            <span>{form.targets.length}/32</span>
          </div>
          <div className="ping-probe-add">
            {(['tcp', 'icmp'] as const).map(protocol => (
              <button
                className="btn sm"
                type="button"
                key={protocol}
                disabled={!editable || form.targets.length >= 32}
                onClick={() =>
                  setForm(current => ({
                    ...current,
                    targets: [...current.targets, { name: '', address: `${protocol}://` }],
                  }))
                }
              >
                ＋ {protocol.toUpperCase()}
              </button>
            ))}
          </div>
        </header>
        <div className="ping-probe-targets">
          {form.targets.map((target, index) => (
            <div className="ping-probe-target" key={index}>
              <span className={`ping-probe-kind ${target.address.startsWith('icmp://') ? 'icmp' : 'tcp'}`}>
                {target.address.startsWith('icmp://') ? 'ICMP' : target.address.startsWith('tcp://') ? 'TCP' : '—'}
              </span>
              <label>
                <span>名称</span>
                <input
                  className="f"
                  aria-label={`目标 ${index + 1} 名称`}
                  placeholder="Cloudflare"
                  value={target.name}
                  onChange={event => updateTarget(index, 'name', event.target.value)}
                />
              </label>
              <label>
                <span>地址</span>
                <input
                  className="f mono"
                  aria-label={`目标 ${index + 1} 地址`}
                  placeholder="tcp://1.1.1.1:443 或 icmp://1.1.1.1"
                  value={target.address}
                  onChange={event => updateTarget(index, 'address', event.target.value)}
                />
              </label>
              <button
                className="btn sm ping-probe-remove"
                type="button"
                disabled={!editable}
                onClick={() =>
                  setForm(current => ({
                    ...current,
                    targets: current.targets.filter((_, targetIndex) => targetIndex !== index),
                  }))
                }
              >
                移除
              </button>
            </div>
          ))}
          {form.targets.length === 0 && (
            <div className="ping-probe-empty-settings">
              <b>还没有探测目标</b>
              <span>从右上角添加 TCP 或 ICMP 目标后，机器会在下一轮开始采样。</span>
            </div>
          )}
        </div>
        {invalid && <span className="agent-log-invalid">{invalid}</span>}
        <p className="ping-probe-note">
          两类计时都从域名解析完成后开始。TCP 只计建连，ICMP 只计 Echo 往返；不会采集 DNS 耗时、内核 RTT、RTO、SYN
          重传或连接错误分类。没有可用 IPv6 路由或 ICMP Socket 权限时记为未探测，不计作丢包。
        </p>
      </section>
      <SettingsSaveBar
        dirty={dirty}
        saving={save.isPending}
        savedText={saved ? '已保存，下一轮生效' : null}
        editable={editable}
        disabled={invalid !== null}
        title={invalid ?? undefined}
        onSave={() => save.mutate(form)}
      />
    </section>
  );
}

const Group = ({ label, children, className }: { label?: string; children: React.ReactNode; className?: string }) => (
  <div className={className ? `setgrp settings-block ${className}` : 'setgrp settings-block'}>
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
  // 证书、分发与站点外观的查询统一放在页面层，一次等齐后再呈现完整页面，
  // 避免各段在不同时间出现而造成布局连续跳动。
  const branding = useQuery({ queryKey: ['branding'], queryFn: () => fetchBranding() });
  const visitor = useQuery({ queryKey: ['auth-state'], queryFn: () => fetchAuthState() });
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
  const pingProbe = useQuery({ queryKey: ['ping-probe-settings'], queryFn: fetchPingProbeSettings });
  const [form, setForm] = useState<Form>(EMPTY);
  const [saved, setSaved] = useState<Partial<Record<SectionKey, number>>>({});
  const [muxExpanded, setMuxExpanded] = useState(false);

  /* 分段保存写的是草稿（`saveSettings` → `update_settings`），因此「已保存」的基准是草稿
     生效后的值，而不是 `GET /settings`——后者是直连接口，草稿提交前不会变。以它为基准有
     两个后果，都实测复现过（tests/settings-draft-baseline.test.tsx）：
       一、保存完那一段，标题栏仍显示「有未保存的改动」，保存按钮一直亮着；
       二、更严重的是保存另一段时，本段未覆盖的字段会从已提交值重新取一遍
           （见下面 `save` 里的 `v`），把前一段刚写进草稿的改动覆盖回旧值。
     订阅草稿版本以便其变化时重算基准。 */
  useSyncExternalStore(draft.subscribe, draft.version);
  const pendingOp = draft.ops().find(op => op.op === 'update_settings');
  const pendingSettings = pendingOp?.op === 'update_settings' ? pendingOp.settings : null;

  const pristine = pendingSettings ? formOf(pendingSettings) : settings.data ? formOf(settings.data) : null;

  // Rebase untouched fields when the draft changes or is discarded. Preserve only genuine
  // local edits, so saving one section never clears another section's unfinished input.
  const [syncedFrom, setSyncedFrom] = useState<Form | null>(null);
  if (pristine && JSON.stringify(pristine) !== JSON.stringify(syncedFrom)) {
    const next = { ...pristine };
    if (syncedFrom) {
      for (const key of Object.keys(next) as (keyof Form)[]) {
        if (form[key] !== syncedFrom[key]) next[key] = form[key];
      }
    }
    setSyncedFrom(pristine);
    setForm(next);
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
        anytls_padding_scheme: v('anyTlsPadding')
          .split(/\r?\n/)
          .map(line => line.trim())
          .filter(Boolean),
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
          keepalive_secs: Number(v('keepalive')) || 10,
          mtu: Number(v('mtu')) || 1420,
          // 链路禁用由机器页的独立草稿操作维护；保存全局 WG 数值时必须原样带回。
          disabled_links: settings.data?.overlay.disabled_links ?? [],
        },
        ports: {
          ingress_base: Number(v('ingressBase')) || 13443,
          anytls_base: Number(v('anytlsBase')) || 14443,
          vless_encryption_base: Number(v('vlessEncryptionBase')),
          hop_base: Number(v('hopBase')) || 20000,
          hy2_base: Number(v('hy2Base')) || 30000,
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
        reverse_health: settings.data?.reverse_health,
        reverse_health_overrides: settings.data?.reverse_health_overrides,
        relay_mux: {
          concurrency: numOr(v('muxConcurrency'), 1),
          min_idle_workers: numOr(v('muxMinIdle'), 0),
          max_idle_workers: numOr(v('muxMaxIdle'), 2),
          max_probing_workers: numOr(v('muxMaxProbing'), 1),
          probe_interval_secs: numOr(v('muxProbeInterval'), 5),
          probe_timeout_ms: numOr(v('muxProbeTimeout'), 2000),
          idle_ttl_secs: numOr(v('muxIdleTtl'), 24),
          max_requests_per_worker: numOr(v('muxMaxRequests'), 128),
        },
        // 该项没有对应的表单字段——统计得出的在线数尚无展示位置，提供一个无法看到效果的
        // 开关不如不提供。此处原样传递，避免保存其他段时将其重置为 false。
        stats_user_online: settings.data?.stats_user_online ?? false,
      };
      return saveSettings(body).then(r => ({
        key,
        revision_id: r.revision_id,
        submitted: form,
        normalized: formOf(body),
      }));
    },
    onSuccess: r => {
      setSaved(s => ({ ...s, [r.key]: r.revision_id }));
      setForm(current => {
        const next = { ...current };
        for (const key of SECTION_FIELDS[r.key]) {
          if (current[key] === r.submitted[key]) next[key] = r.normalized[key];
        }
        return next;
      });
      qc.invalidateQueries({ queryKey: ['settings'] });
      qc.invalidateQueries({ queryKey: ['revisions'] });
    },
  });

  if (
    settings.isPending ||
    branding.isPending ||
    visitor.isPending ||
    certs.isPending ||
    dist.isPending ||
    logPolicy.isPending ||
    pingProbe.isPending
  )
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
  const relayMuxError = hopMuxError(relayMuxOfForm(form));

  return (
    <div className="cardpage">
      {/* 非 system-admin 仍可查看实际配置，但整页必须是真正的只读控件。此前只禁用了
          保存按钮，输入框和分段开关仍能改出一份永远无法保存的“脏”表单。 */}
      <fieldset disabled={!editable} style={{ border: 0, margin: 0, padding: 0, minWidth: 0 }}>
        <div className="duo">
          {/* 两栏各自成流，不对齐底部。分段位置按高度定——证书段展示完整
            签发记录，单它一段就抵得上右栏的两段，与它同栏的只能是最短的那两段。
            编号仍从上到下、从左到右连续。 */}
          <div className="col">
            {save.error && <ErrorBox error={save.error} />}

            {branding.error ? (
              <ErrorBox error={branding.error} />
            ) : (
              <BrandingSection editable={editable} data={branding.data!} />
            )}
            {visitor.error ? (
              <ErrorBox error={visitor.error} />
            ) : (
              <VisitorAccessSection editable={editable} enabled={visitor.data!.public_open} />
            )}
            {dist.error ? (
              <ErrorBox error={dist.error} />
            ) : (
              <DistributionSection editable={editable} data={dist.data!} />
            )}
            {logPolicy.error ? (
              <ErrorBox error={logPolicy.error} />
            ) : (
              <AgentLogPolicySection editable={editable} data={logPolicy.data!} />
            )}
            {certs.error ? <ErrorBox error={certs.error} /> : <CertSection editable={editable} view={certs.data!} />}

            <Section id="set-xray" name="XRAY" sub="伪装站点、接入兼容与 AnyTLS 全局填充" {...secProps('xray')}>
              <div className="xray-settings-grid">
                <Group className="xray-setting-block" label="REALITY · 伪装站点">
                  <Fld label="目标地址">
                    <input
                      className={`${chg('dest')} xray-control`}
                      placeholder="example.com:443"
                      value={form.dest}
                      onChange={e => setForm({ ...form, dest: e.target.value })}
                    />
                    <span className="unit">站点:端口</span>
                  </Fld>
                  <Fld label="允许的 SNI">
                    <input
                      className={`${chg('names')} xray-control`}
                      placeholder="example.com"
                      value={form.names}
                      onChange={e => setForm({ ...form, names: e.target.value })}
                    />
                    <span className="hint">多个名称用逗号分隔</span>
                  </Fld>
                  <Fld label="TLS 指纹">
                    <select
                      className={`${chg('fp')} xray-control`}
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
                </Group>

                <Group className="xray-setting-block" label="REALITY · 接入兼容">
                  <Fld label="XTLS 流控">
                    <select
                      className={`${chg('flow')} xray-control`}
                      value={form.flow}
                      onChange={e => setForm({ ...form, flow: e.target.value })}
                    >
                      {/* 显示为大写、value 仍为小写：写入 xray.json 和 grants 的必须是
                      `xtls-rprx-vision` 原值，大写只是该字段的显示形式。 */}
                      <option value="xtls-rprx-vision">XTLS-RPRX-VISION（默认）</option>
                      <option value="">关闭（普通 VLESS over TLS）</option>
                    </select>
                  </Fld>
                  <div className="xray-version-grid">
                    <Fld label="最低客户端版本">
                      <input
                        className={`${chg('min')} xray-control`}
                        placeholder="留空 = 不限"
                        value={form.min}
                        onChange={e => setForm({ ...form, min: e.target.value })}
                      />
                    </Fld>
                    <Fld label="最高客户端版本">
                      <input
                        className={`${chg('max')} xray-control`}
                        placeholder="留空 = 不限"
                        value={form.max}
                        onChange={e => setForm({ ...form, max: e.target.value })}
                      />
                    </Fld>
                  </div>
                  <Fld label="最大时钟偏差">
                    <input
                      className={`${chg('diff')} xray-control`}
                      placeholder="留空 = 使用默认值"
                      value={form.diff}
                      onChange={e => setForm({ ...form, diff: e.target.value })}
                    />
                    <span className="unit">ms</span>
                  </Fld>
                </Group>

                {who.role !== 'readonly' && (
                  <Group className="xray-setting-block wide" label="AnyTLS · Padding">
                    <Fld label="全局方案">
                      <textarea
                        className={`${chg('anyTlsPadding')} xray-padding`}
                        rows={5}
                        value={form.anyTlsPadding}
                        readOnly
                        aria-label="AnyTLS 全局 Padding Scheme"
                      />
                      <button
                        type="button"
                        className="btn sm"
                        disabled={!editable}
                        onClick={() => setForm({ ...form, anyTlsPadding: randomAnyTlsPadding() })}
                      >
                        重新生成
                      </button>
                      <span className="hint">所有留空的 AnyTLS 入口跟随这套四阶段方案。</span>
                    </Fld>
                  </Group>
                )}
              </div>
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
              validationError={relayMuxError}
            >
              <Group label="连接资源">
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
                <Fld label="转发缓冲（KiB）">
                  <input
                    className={chg('connBuffer')}
                    style={{ width: 90 }}
                    placeholder="跟 CPU 架构"
                    value={form.connBuffer}
                    onChange={e => setForm({ ...form, connBuffer: e.target.value })}
                  />
                  <span className="hint">
                    收发之间的队列，<b>每条连接每个方向一个</b>。<b>推荐留空</b>：不写该项时 XRAY 按 CPU
                    架构自行决定。填 0 表示不缓冲，与留空不同
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
                  TCP 半关闭等待时间。过短会截断回传数据，过长会多占内存。<b>默认 UplinkOnly 2 秒、DownlinkOnly 5 秒</b>
                  。 机器详情页可逐台覆盖。
                </div>
                <div className="guard">
                  转发缓冲留空时 XRAY 按架构取值：x86_64 512 KiB，arm64 4 KiB。填入数值会统一所有架构。
                </div>
                <div className="guard">
                  上面四项<b>均可按机器单独覆盖</b>（机器详情页），握手超时除外。
                </div>
                <div className="guard">
                  保存后需发布，<b>agent 应用时会重启 XRAY</b>，现有连接断开。
                </div>
              </Group>
              <Group label="中继 Mux" className="relay-mux-settings">
                <div className="relay-mux-summary">
                  <span>
                    复用流 {form.muxConcurrency} · 空闲 {form.muxMinIdle}–{form.muxMaxIdle} · 探测{' '}
                    {form.muxProbeInterval}s/{form.muxProbeTimeout}ms · 寿命 {form.muxIdleTtl}s
                  </span>
                  {editable ? (
                    <button type="button" className="btn sm" onClick={() => setMuxExpanded(open => !open)}>
                      {muxExpanded ? '收起' : '配置'}
                    </button>
                  ) : (
                    <span
                      className="btn sm"
                      role="button"
                      tabIndex={0}
                      onClick={() => setMuxExpanded(open => !open)}
                      onKeyDown={event => {
                        if (event.key === 'Enter' || event.key === ' ') {
                          event.preventDefault();
                          setMuxExpanded(open => !open);
                        }
                      }}
                    >
                      {muxExpanded ? '收起' : '查看'}
                    </span>
                  )}
                </div>
                {relayMuxError && <p className="note settings-validation-error">{relayMuxError}</p>}
                {muxExpanded && (
                  <div className="relay-mux-fields">
                    <Fld label="复用流数量">
                      <input
                        className={chg('muxConcurrency')}
                        type="number"
                        min={1}
                        max={128}
                        value={form.muxConcurrency}
                        onChange={e => setForm({ ...form, muxConcurrency: e.target.value })}
                      />
                    </Fld>
                    <Fld label="空闲连接">
                      <span className="field-pair">
                        <span className="unit">最少</span>
                        <input
                          className={chg('muxMinIdle')}
                          type="number"
                          min={0}
                          value={form.muxMinIdle}
                          onChange={e => setForm({ ...form, muxMinIdle: e.target.value })}
                        />
                        <span className="unit">最多</span>
                        <input
                          className={chg('muxMaxIdle')}
                          type="number"
                          min={1}
                          value={form.muxMaxIdle}
                          onChange={e => setForm({ ...form, muxMaxIdle: e.target.value })}
                        />
                      </span>
                    </Fld>
                    <Fld label="同时探测">
                      <input
                        className={chg('muxMaxProbing')}
                        type="number"
                        min={1}
                        max={Number(form.muxMaxIdle) || undefined}
                        value={form.muxMaxProbing}
                        onChange={e => setForm({ ...form, muxMaxProbing: e.target.value })}
                      />
                    </Fld>
                    <Fld label="探测周期">
                      <input
                        className={chg('muxProbeInterval')}
                        type="number"
                        min={2}
                        max={60}
                        value={form.muxProbeInterval}
                        onChange={e => setForm({ ...form, muxProbeInterval: e.target.value })}
                      />
                      <span className="unit">秒</span>
                    </Fld>
                    <Fld label="单次超时">
                      <input
                        className={chg('muxProbeTimeout')}
                        type="number"
                        min={200}
                        max={10000}
                        value={form.muxProbeTimeout}
                        onChange={e => setForm({ ...form, muxProbeTimeout: e.target.value })}
                      />
                      <span className="unit">毫秒</span>
                    </Fld>
                    <Fld label="空闲寿命">
                      <input
                        className={chg('muxIdleTtl')}
                        type="number"
                        min={1}
                        value={form.muxIdleTtl}
                        onChange={e => setForm({ ...form, muxIdleTtl: e.target.value })}
                      />
                      <span className="unit">秒</span>
                    </Fld>
                    <Fld label="累计子连接">
                      <input
                        className={chg('muxMaxRequests')}
                        type="number"
                        min={1}
                        max={65535}
                        value={form.muxMaxRequests}
                        onChange={e => setForm({ ...form, muxMaxRequests: e.target.value })}
                      />
                    </Fld>
                    <div className="guard">探测中的连接不会承接新流；没有可用连接时会立即新建，不等待探测超时。</div>
                  </div>
                )}
              </Group>
            </Section>

            <ReverseHealthSettings />

            <Section id="set-wg" name="WIREGUARD" sub="overlay 链路，全互联算出来的" {...secProps('wireguard')}>
              <Group label="Overlay 默认值">
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
              <Group label="新建资源端口基线">
                <Fld label="VLESS · TLS / REALITY">
                  <input
                    className={chg('ingressBase')}
                    style={{ width: 90 }}
                    value={form.ingressBase}
                    onChange={e => setForm({ ...form, ingressBase: e.target.value })}
                  />
                  <span className="hint">建链时从该端口向上查找空闲端口</span>
                </Fld>
                <Fld label="VLESS · Encryption 起始端口">
                  <input
                    className={chg('vlessEncryptionBase')}
                    style={{ width: 90 }}
                    type="number"
                    min={1}
                    max={65535}
                    aria-label="VLESS · Encryption 起始端口"
                    value={form.vlessEncryptionBase}
                    onChange={e => setForm({ ...form, vlessEncryptionBase: e.target.value })}
                  />
                  <span className="hint">默认 48000；新开启入站时向上查找空闲 TCP 端口，现有端口不变</span>
                </Fld>
                <Fld label="AnyTLS">
                  <input
                    className={chg('anytlsBase')}
                    style={{ width: 90 }}
                    value={form.anytlsBase}
                    onChange={e => setForm({ ...form, anytlsBase: e.target.value })}
                  />
                  <span className="hint">走 TCP；新开启 AnyTLS 时从该端口向上查找空闲端口</span>
                </Fld>
                <Fld label="Hysteria 2">
                  <input
                    className={chg('hy2Base')}
                    style={{ width: 90 }}
                    value={form.hy2Base}
                    onChange={e => setForm({ ...form, hy2Base: e.target.value })}
                  />
                  <span className="hint">
                    走 UDP，与上一项使用各自的端口段。同一个端口号在 TCP 与 UDP
                    上互不冲突。开启端口跳跃时，还会从分配到的 端口向上连续占用一段
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
              </Group>
            </Section>

            <Section
              id="set-probe"
              name="端到端探测"
              sub="由入口节点为每条链发起一次探测。改完最长等待一个原有周期"
              {...secProps('probe')}
            >
              <Group label="探测计划">
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

            {pingProbe.error ? (
              <ErrorBox error={pingProbe.error} />
            ) : (
              <PingProbeSettingsSection editable={editable} data={pingProbe.data!} />
            )}

            {/* 不提供开关是有意的：规则表中的 geosite: / geoip: 依赖这两个文件，文件过期不会报错，
          而是导致规则匹配失败、流量走兜底规则且无提示。因此此处只能配置更新时间和
          更新来源，不能配置是否更新。 */}
            <Section
              id="set-geodata"
              name="规则库更新"
              sub="每台机器按计划自行拉取 geoip.dat / geosite.dat，热加载、不重启、不断开连接"
              {...secProps('geodata')}
            >
              <Group label="更新计划与来源">
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
                    ：不带前缀时按每台机器自身的时区解释，同一个表达式在不同机器上会相差数小时。默认 6:30（UTC+8）＝
                    22:30 UTC，比上游发布晚半小时
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
      </fieldset>
    </div>
  );
}
