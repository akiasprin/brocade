import { useServerForm } from '../ui/server-form';
import { ReverseHealthSettings, reversePoliciesError } from '../reverse-health-settings';
import { useRef, useState, useSyncExternalStore } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  DEFAULT_HOP_MUX,
  DEFAULT_REVERSE_HEALTH,
  MIN_AGENT_PROTOCOL_VERSION,
  type ReverseHealthPolicy,
  type ReverseHealthOverride,
  fetchCerts,
  fetchAuthState,
  fetchBranding,
  saveCertDomain,
  saveBranding,
  scanCerts,
  fetchDistribution,
  fetchAgentLogPolicy,
  fetchLinkMtu,
  fetchNodes,
  fetchSettings,
  hopMuxError,
  fetchPingProbeSettings,
  fetchTunnelProbeCapability,
  fetchTunnelProbes,
  fetchVpngateOverview,
  startVpngateIntelligenceRefresh,
  saveDistribution,
  saveAgentLogDefault,
  saveNodeLogPolicy,
  savePortSettings,
  saveProbeSettings,
  saveSettings,
  savePingProbeSettings,
  setVisitorAccess,
  updateVpngateIntelligenceNode,
  updateVpngateAdmissionPolicy,
  updateVpngateIntelligenceCredentials,
  updateVpngateIntelligencePolicy,
  createCertGroup,
  deleteCertificate,
  deleteCertGroup,
  requestSpareCertificate,
  serveCertificate,
  updateCertGroup,
  type CertsView,
  type CertificateTrack,
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
  type NodeAgentStateItem,
  PING_PROBE_FAMILIES,
  type PingProbeFamily,
  type PingProbeKind,
  type PingProbeSettings,
  type VpngateAdmissionPolicy,
  type VpngateIntelligencePolicy,
  type VpngateIpProvider,
  type VpngateOverview,
} from '../api';
import { draft } from '../draft';
import { can, useSession } from '../session';
import { ErrorBox, Loading, SegmentedControl } from '../ui/bits';
import { BrandIcon } from '../ui/branding';
import { PanelTitle, type IconName } from '../ui/icons';
import { useNodeNames } from '../ui/node-name';
import { LOG_MAX_MIB, LOG_MIN_MIB, validLogMib } from '../ui/log-policy';
import { PING_FAMILY_LABEL } from '../ui/ping-probe';
import { SettingsParameterSummary } from '../ui/settings-parameter-summary';
import { confirmDiscardChanges, useUnsavedChanges } from '../ui/navigation-guard';
import { REALITY_FINGERPRINT_OPTIONS } from '../reality';
import { TunnelProbeSettingsSection } from '../tunnel-probe';

// Kept as re-exports for callers that used the settings module before the values moved into a
// lightweight shared module.
export { LOG_MAX_MIB, LOG_MIN_MIB, validLogMib } from '../ui/log-policy';

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
  statsOnline: string;
  muxConcurrency: string;
  muxPrewarmWorkers: string;
  muxReuseThreshold: string;
  muxMaxProbing: string;
  muxProbeInterval: string;
  muxProbeTimeout: string;
  muxIdleTtl: string;
  muxMaxRequests: string;
  anyTlsPadding: string;
  reverseHealth: string;
  reverseOverrides: string;
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
    prewarm_workers: numberField(form.muxPrewarmWorkers),
    reuse_threshold: numberField(form.muxReuseThreshold),
    max_probing_workers: numberField(form.muxMaxProbing),
    probe_interval_ms: numberField(form.muxProbeInterval),
    probe_timeout_ms: numberField(form.muxProbeTimeout),
    idle_ttl_ms: numberField(form.muxIdleTtl),
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
  mtu: '1280',
  ingressBase: '13443',
  anytlsBase: '14443',
  vlessEncryptionBase: '13800',
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
  statsOnline: 'false',
  muxConcurrency: String(DEFAULT_HOP_MUX.concurrency),
  muxPrewarmWorkers: String(DEFAULT_HOP_MUX.prewarm_workers),
  muxReuseThreshold: String(DEFAULT_HOP_MUX.reuse_threshold),
  muxMaxProbing: String(DEFAULT_HOP_MUX.max_probing_workers),
  muxProbeInterval: String(DEFAULT_HOP_MUX.probe_interval_ms),
  muxProbeTimeout: String(DEFAULT_HOP_MUX.probe_timeout_ms),
  muxIdleTtl: String(DEFAULT_HOP_MUX.idle_ttl_ms),
  muxMaxRequests: String(DEFAULT_HOP_MUX.max_requests_per_worker),
  anyTlsPadding: ANYTLS_PADDING_DEFAULT,
  reverseHealth: JSON.stringify(DEFAULT_REVERSE_HEALTH),
  reverseOverrides: '[]',
};

// 将已保存的设置转换为表单的形态。修改判定基于它：字符串与字符串比较，
// 不需要在 null、数字和空串之间做转换——这正是此前未修改却显示为已修改的原因。
function formOf(s: ModelSettings): Form {
  return {
    reverseHealth: JSON.stringify(s.reverse_health ?? DEFAULT_REVERSE_HEALTH),
    reverseOverrides: JSON.stringify(s.reverse_health_overrides ?? []),
    min: text(s.reality_client?.min_client_ver ?? null),
    max: text(s.reality_client?.max_client_ver ?? null),
    diff: s.reality_client?.max_time_diff_ms == null ? '' : String(s.reality_client.max_time_diff_ms),
    dest: text(s.reality_site?.dest ?? null),
    names: (s.reality_site?.server_names ?? []).join(', '),
    fp: text(s.reality_site?.fingerprint ?? null),
    flow: text(s.reality_site?.flow ?? null),
    keepalive: String(s.overlay?.keepalive_secs ?? 10),
    mtu: String(s.overlay?.mtu ?? 1280),
    ingressBase: String(s.ports?.ingress_base ?? 13443),
    anytlsBase: String(s.ports?.anytls_base ?? 14443),
    vlessEncryptionBase: String(s.ports?.vless_encryption_base ?? 13800),
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
    statsOnline: String(s.stats_user_online ?? false),
    muxConcurrency: String(s.relay_mux?.concurrency ?? 1),
    muxPrewarmWorkers: String(s.relay_mux?.prewarm_workers ?? 0),
    muxReuseThreshold: String(s.relay_mux?.reuse_threshold ?? 2),
    muxMaxProbing: String(s.relay_mux?.max_probing_workers ?? 1),
    muxProbeInterval: String(s.relay_mux?.probe_interval_ms ?? 5000),
    muxProbeTimeout: String(s.relay_mux?.probe_timeout_ms ?? 2000),
    muxIdleTtl: String(s.relay_mux?.idle_ttl_ms ?? 24000),
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
    'reverseHealth',
    'reverseOverrides',
    'connIdle',
    'connUplink',
    'connDownlink',
    'connBuffer',
    'connHandshake',
    'statsOnline',
    'muxConcurrency',
    'muxPrewarmWorkers',
    'muxReuseThreshold',
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

/* 标题图标与机器配置、链路设置共用同一套线稿图标。设置项仍按两栏顺序排列。 */
type NavItem = { id: string; label: string; icon: IconName; key?: SectionKey };

const NAV: NavItem[] = [
  { id: 'set-branding', label: '站点外观', icon: 'settings' },
  { id: 'set-visitor', label: '访客模式', icon: 'access' },
  { id: 'set-dist', label: '分发', icon: 'deploy' },
  { id: 'set-agent-logs', label: '日志保留', icon: 'artifacts' },
  { id: 'set-cert', label: '证书', icon: 'certificate' },
  { id: 'set-xray', label: 'XRAY', icon: 'protocol', key: 'xray' },
  { id: 'set-conn', label: '连接策略', icon: 'config', key: 'connection' },
  { id: 'set-wg', label: 'WireGuard', icon: 'tunnels', key: 'wireguard' },
  { id: 'set-ports', label: '端口分配', icon: 'ingress', key: 'ports' },
  // 探测配置不进产物：机器下一轮读到新值即生效，最长等一个原有周期。
  { id: 'set-probe', label: '端到端探测', icon: 'observe', key: 'probe' },
  { id: 'set-ping-probe', label: 'Ping 链路探测', icon: 'diag' },
  { id: 'set-geodata', label: '规则库更新', icon: 'dns', key: 'geodata' },
  { id: 'set-tunnel-probes', label: '隧道监测', icon: 'tunnels' },
  { id: 'set-vpngate-intelligence', label: '情报任务', icon: 'observe' },
];

const ICON_OF: Record<string, IconName> = Object.fromEntries(NAV.map(item => [item.id, item.icon]));

function SettingsTitle({ id, children }: { id: string; children: React.ReactNode }) {
  return <PanelTitle of={ICON_OF[id]}>{children}</PanelTitle>;
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

  return (
    <>
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
            建议值 = 最小路径 MTU 减去 wg 封装开销，采纳要去机器面逐台改。开销不是一个常数， 随<b>对端</b>
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
      </header>
      <p className="cardsub">{sub}</p>
      {children}
      <SettingsSaveBar
        dirty={dirty}
        saving={saving}
        savedText={
          savedRev !== null ? (savedRev === 0 ? '已加入变更集，尚未提交' : `已保存，盖出修订 ${savedRev}`) : null
        }
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
    credential: '',
    directory: source.domain?.acme_directory ?? source.letsencrypt,
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
        signing_method: 'public-ca',
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
  const storedDirectory = d?.acme_directory ?? view.letsencrypt;
  const dirty =
    f.domain.trim() !== (d?.domain ?? '') ||
    f.credential.trim() !== '' ||
    f.directory !== storedDirectory ||
    f.contact.trim() !== (d?.acme_contact ?? '') ||
    Number(f.renew) !== (d?.renew_before_days ?? 30);
  useUnsavedChanges(dirty, '证书签发配置');

  const publicCaConfigured = d?.signing_method === 'public-ca';
  return (
    <section className="panel config-panel" id="set-cert">
      <header>
        <SettingsTitle id="set-cert">证书</SettingsTitle>
      </header>
      <p className="cardsub">
        签发配置与证书组分开管理。新建证书组时选择证书类型；类型建成后固定。机器切换证书组需保存草稿、提交并发布，应用时重启
        Xray。
      </p>
      {save.error && <ErrorBox error={save.error} />}
      {scan.error && <ErrorBox error={scan.error} />}

      <div className="setgrp settings-block cert-method-settings">
        <p className="eyebrow">签发配置</p>

        <details className="cert-method-config">
          <summary className="cert-method-config-head">
            <div>
              <span className="cert-method-config-name">自签证书</span>
              <span className="cert-origin">系统内置</span>
            </div>
            <span className="hint">已配置 · 固定 A/B 主备</span>
          </summary>
          <dl className="cert-config-facts" aria-label="自签证书配置概览">
            <div>
              <dt>运行槽</dt>
              <dd>固定 A / B 两份</dd>
            </div>
            <div>
              <dt>域名与验证</dt>
              <dd>无需域名与 DNS 验证</dd>
            </div>
            <div>
              <dt>公开信息</dt>
              <dd>不进入 CT 公开日志</dd>
            </div>
          </dl>
        </details>

        <details className="cert-method-config">
          <summary className="cert-method-config-head">
            <div>
              <span className="cert-method-config-name">Let&apos;s Encrypt</span>
              <span className="cert-origin">Cloudflare DNS-01</span>
            </div>
            <span className={publicCaConfigured ? 'hint' : 'hint bad'}>
              {publicCaConfigured ? '已配置' : '未配置'}
              {dirty ? ' · 有未保存的修改' : ''}
            </span>
          </summary>

          <dl className="cert-config-notes" aria-label="Let's Encrypt 配置说明">
            <div>
              <dt>运行槽</dt>
              <dd>数量不限，权威证书无需预先固定</dd>
            </div>
            <div>
              <dt>域名与验证</dt>
              <dd>独立域名，通过 Cloudflare DNS-01 验证</dd>
            </div>
            <div>
              <dt>公开信息</dt>
              <dd>证书名称会进入 CT 公开日志</dd>
            </div>
          </dl>

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
            <label>签发环境</label>
            <div className="v">
              <SegmentedControl
                value={f.directory}
                options={[
                  { value: view.letsencrypt, label: '正式' },
                  { value: view.letsencrypt_staging, label: 'staging' },
                ]}
                className={dirty && f.directory !== storedDirectory ? 'chg' : undefined}
                ariaLabel="签发环境"
                onChange={directory => setForm({ ...f, directory })}
              />
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

          <SettingsSaveBar
            dirty={dirty}
            saving={save.isPending}
            savedText={saved ? '已保存，此配置可供新证书组使用' : null}
            editable={editable}
            disabled={!view.sealing_available}
            title={view.sealing_available ? '' : '这台控制面没配 BROCADE_SECRET_KEY，存不了凭据'}
            label="保存配置"
            onSave={() => save.mutate(f)}
          />
        </details>
      </div>

      <div className="setgrp settings-block">
        <p className="eyebrow">证书状态</p>
        <div className="setfld">
          <label />
          <div className="v">
            <button className="btn" disabled={!editable || scan.isPending} onClick={() => scan.mutate()}>
              {scan.isPending ? '正在签发与续期…' : '立即签发与续期'}
            </button>
            <span className="hint">只处理待签发、失败和即将到期的证书</span>
          </div>
        </div>

        {scan.data?.processing && !scan.isPending && (
          <p role="status" className={scan.data.processing.failed ? 'note bad' : 'hint'}>
            {issuanceResultText(scan.data.processing)}
          </p>
        )}
        <CertGroups view={view} editable={editable && !scan.isPending} />
      </div>
    </section>
  );
}

/** 证书组及其完整证书记录。机器与证书组的对应关系暂不在全局设置页展示；这里专注于
 * 签发材料、运行位置、有效期和失败信息，避免把证书状态与机器收敛状态混在一起。 */
function CertGroups({ view, editable }: { view: CertsView; editable: boolean }) {
  const qc = useQueryClient();
  const reload = () => qc.invalidateQueries({ queryKey: ['certs'] });
  const publicCaConfigured = view.domain?.signing_method === 'public-ca';
  const emptyGroup = (): CertificateGroupFormValue => ({
    name: '',
    note: '',
    certificateName: '',
    signingMethod: null,
  });
  const [creating, setCreating] = useState<CertificateGroupFormValue | null>(null);
  const [editing, setEditing] = useState<(CertificateGroupFormValue & { id: string }) | null>(null);
  const [failed, setFailed] = useState<string | null>(null);
  const [pending, setPending] = useState<string | null>(null);
  const [resultText, setResultText] = useState<string | null>(null);
  const [expandedGroups, setExpandedGroups] = useState<Set<string>>(() => new Set());
  const createGuardScope = 'certificate-group:create';
  const editGuardScope = editing ? `certificate-group:edit:${editing.id}` : 'certificate-group:edit';
  const creatingDirty =
    creating !== null &&
    (creating.name !== '' ||
      creating.note !== '' ||
      creating.certificateName !== '' ||
      creating.signingMethod !== null);
  const editedGroup = editing ? view.groups.find(group => group.id === editing.id) : undefined;
  const editingDirty =
    editing !== null &&
    editedGroup !== undefined &&
    (editing.name !== editedGroup.name || editing.note !== (editedGroup.note ?? ''));
  useUnsavedChanges(creatingDirty, '新证书组', createGuardScope);
  useUnsavedChanges(editingDirty, `${editedGroup?.name ?? '证书组'}的信息`, editGuardScope);

  const cancelCreating = () => {
    if (confirmDiscardChanges(createGuardScope)) setCreating(null);
  };
  const cancelEditing = () => {
    if (confirmDiscardChanges(editGuardScope)) setEditing(null);
  };
  // Unlike the section saves, these actions are plain promises rather than useMutation.
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
            publicCaConfigured={publicCaConfigured}
            busy={pending !== null}
            onChange={setCreating}
            onCancel={cancelCreating}
            onSave={() => {
              run(
                'create-group',
                () =>
                  createCertGroup({
                    name: creating.name,
                    signing_method: creating.signingMethod!,
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
                onClick={() => setCreating(emptyGroup())}
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
            onClick={() => setCreating(emptyGroup())}
          >
            ＋ 新建证书组
          </button>
        </div>
      </div>
      {creating && (
        <GroupForm
          value={creating}
          creating
          publicCaConfigured={publicCaConfigured}
          busy={pending !== null}
          onChange={setCreating}
          onCancel={cancelCreating}
          onSave={() => {
            run(
              'create-group',
              () =>
                createCertGroup({
                  name: creating.name,
                  signing_method: creating.signingMethod!,
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
        const servingMethod = group.certificates.find(cert => cert.status === 'serving')?.signing_method;
        const displayedMethod = servingMethod ?? group.signing_method;
        const groupSelfSigned = displayedMethod === 'self-signed';
        const methodMismatch = servingMethod !== undefined && servingMethod !== group.signing_method;
        const failedCount = group.certificates.filter(cert => cert.status === 'failed').length;
        const pendingCount = group.certificates.filter(cert => cert.status === 'pending').length;
        const xrayPinsActive = group.certificates.some(
          cert => retainedCertificate(cert) && cert.signing_method === 'self-signed',
        );
        const configuredMethod = displayedMethod;
        const preloadPending = members.some(row => row.on_disk !== 'current');
        const runtimeSlots = groupSelfSigned
          ? group.certificates.filter(cert => cert.signing_method === configuredMethod && cert.runtime_slot !== null)
              .length
          : 0;
        return (
          <article className={`certgrp${expanded ? ' expanded' : ''}`} key={group.id}>
            <header
              className="certgrp-hd"
              onClick={event => {
                if ((event.target as HTMLElement).closest('button')) return;
                toggleGroup(group.id);
              }}
            >
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
                    {methodMismatch && <span className="cstate err">组配置类型异常</span>}
                    {failedCount > 0 && <span className="cstate err">{failedCount} 张签发失败</span>}
                    {pendingCount > 0 && <span className="cstate idle">{pendingCount} 张待签发</span>}
                  </div>
                  {group.note && <span className="certgrp-note">{group.note}</span>}
                </div>
              </button>
              <span className="ctl">
                <button
                  type="button"
                  className="btn sm"
                  disabled={!editable || pending !== null || group.is_default}
                  title={group.is_default ? '默认自签证书组名称固定' : '修改证书组名称'}
                  onClick={() => {
                    if (!confirmDiscardChanges(editGuardScope)) return;
                    openGroup(group.id);
                    setEditing({
                      id: group.id,
                      name: group.name,
                      note: group.note ?? '',
                      certificateName: '',
                      signingMethod: displayedMethod,
                    });
                  }}
                >
                  改名
                </button>
                {!groupSelfSigned && (
                  <button
                    type="button"
                    className="btn sm"
                    disabled={!editable || pending !== null}
                    title="再签一张同名证书；可保留多张，由你决定启用或删除"
                    onClick={() => run(`spare:${group.id}`, () => requestSpareCertificate(group.id))}
                  >
                    {pending === `spare:${group.id}` ? '正在申领…' : '＋ 增加证书'}
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
                    <dd>{runtimeSlots}/2 · 固定主备</dd>
                  </div>
                ) : (
                  <div>
                    <dt>证书数量</dt>
                    <dd>{group.certificates.length} 张 · 任选一张在用</dd>
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
                  onCancel={cancelEditing}
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
                              {cert.status !== 'serving' && !groupSelfSigned && (
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
                              <dd className="mono">
                                {cert.certificate_name ??
                                  (group.names.length > 0 ? group.names.join(' · ') : '等待签发')}
                              </dd>
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

/** 建组和改组共用名称与备注；申领类型只在建组时选择，建成后不跨信任轨修改。 */
type CertificateGroupFormValue = {
  name: string;
  note: string;
  certificateName: string;
  signingMethod: CertificateTrack | null;
};

function GroupForm({
  creating = false,
  value,
  busy,
  publicCaConfigured = false,
  onChange,
  onCancel,
  onSave,
}: {
  creating?: boolean;
  value: CertificateGroupFormValue;
  busy: boolean;
  publicCaConfigured?: boolean;
  onChange: (next: CertificateGroupFormValue) => void;
  onCancel: () => void;
  onSave: () => void;
}) {
  const methodMissing = creating && value.signingMethod === null;
  const publicCaUnavailable = creating && value.signingMethod === 'public-ca' && !publicCaConfigured;
  return (
    <div className="cert-group-form">
      {creating && (
        <div className="cert-group-method">
          <span>证书类型</span>
          <div className="v">
            <SegmentedControl<CertificateTrack | ''>
              value={value.signingMethod ?? ''}
              options={[
                { value: 'self-signed', label: '自签证书' },
                { value: 'public-ca', label: "Let's Encrypt + Cloudflare DNS" },
              ]}
              disabled={busy}
              ariaLabel="证书组类型"
              onChange={signingMethod => signingMethod && onChange({ ...value, signingMethod })}
            />
            <span className={publicCaUnavailable ? 'hint bad' : 'hint'}>
              {value.signingMethod === null
                ? '请选择这个证书组的类型。'
                : value.signingMethod === 'self-signed'
                  ? '系统立即生成固定 A/B 主备两份；不能增加第三份。'
                  : publicCaConfigured
                    ? '使用上方已保存的 Let’s Encrypt + Cloudflare DNS 配置；组内可以继续增加多份证书。'
                    : '这项配置尚未完成，请先保存上方的证书域名与 Cloudflare Token。'}
            </span>
          </div>
        </div>
      )}
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
      </div>
      <div className="cert-group-form-actions">
        <span className="hint">
          {creating
            ? value.signingMethod === null
              ? '选择证书类型后才能创建。'
              : value.signingMethod === 'self-signed'
                ? '创建后立即生成并签发固定主备两份。'
                : '创建后立即申领第一份证书，之后可按需继续增加。'
            : '保存后立即更新组信息'}
        </span>
        <button type="button" className="btn" disabled={busy} onClick={onCancel}>
          取消
        </button>
        <button
          type="button"
          className="btn primary"
          disabled={busy || !value.name.trim() || methodMissing || publicCaUnavailable}
          onClick={onSave}
        >
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
  useUnsavedChanges(dirty, '站点外观');
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
      </header>
      <p className="cardsub">控制台左上角使用这里的名称和图标；它们也同步到登录页和浏览器标签页</p>
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
        <span className="sp" />
      </header>
      <p className="cardsub">开启后无需账号即可进入脱敏后的只读页面；关闭会立即退出现有访客</p>
      {update.error && <ErrorBox error={update.error} />}
      <Group label="公开访问">
        <Fld label="访问状态">
          <SegmentedControl
            value={enabled}
            options={[
              { value: false, label: '关闭' },
              { value: true, label: '开启' },
            ]}
            disabled={!editable || update.isPending}
            ariaLabel="访客模式"
            onChange={next => update.mutate(next)}
          />
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
  useUnsavedChanges(dirty, 'Agent 分发地址');
  // 地址留空会回退到进程启动时的环境变量，因此仍显示实际值，避免把空输入框误解为未配置。
  const fallbackNote = (own: string | null, live: string | null) =>
    !own && live ? <span className="hint">当前生效：{live}（来自环境变量）</span> : null;

  return (
    <section className="panel config-panel" id="set-dist">
      <header>
        <SettingsTitle id="set-dist">分发</SettingsTitle>
      </header>
      <p className="cardsub">机器从哪里访问这台控制面，以及当前控制台内置的 XRAY 构建</p>
      {save.error && <ErrorBox error={save.error} />}
      <Group label="机器分发">
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
          版本与二进制由控制台一同内置，机器安装时会校验文件摘要。升级 XRAY
          时先部署包含目标构建的新控制台，再到发布页创建灰度发布。
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
  useUnsavedChanges(dirty, `${node.name} 的日志保留`);
  const overrideCount = AGENT_LOG_CLASSES.filter(item => node.overrides[item.key] !== null).length;
  const clear: AgentLogLimitOverrides = { agent_journal_mib: null, xray_mib: null, phantun_mib: null };

  return (
    <div className="agent-log-node">
      <div className="agent-log-node-head">
        <div className="agent-log-node-name">
          <b>{node.name}</b>
        </div>
        <span className={overrideCount === 0 ? 'st' : 'st st-warn'}>
          {overrideCount === 0 ? '与全局一致' : `${overrideCount} 项覆盖`}
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
            {mutation.isPending ? '清除中…' : '与全局一致'}
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
  useUnsavedChanges(dirty, '全局日志保留');
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
      </header>
      <p className="cardsub">只设置全局默认值；单台机器的覆盖项在对应机器配置中管理</p>
      {save.error && <ErrorBox error={save.error} />}
      <Group label="全局日志上限 · 每类独立">
        <div className="agent-log-scope" aria-label="日志额度作用范围">
          {AGENT_LOG_CLASSES.map(item => (
            <div className="agent-log-scope-row" key={item.key}>
              <span className="agent-log-scope-kind">{item.kind}</span>
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

/** The family an IP literal fixes; null for a domain. Leading zeros make a domain, as on the server. */
function literalFamily(host: string): PingProbeFamily | null {
  const octets = host.split('.');
  if (octets.length === 4 && octets.every(octet => /^(0|[1-9]\d{0,2})$/.test(octet) && Number(octet) <= 255))
    return 'ipv4';
  if (host.includes(':') && validIpv6Literal(host)) return 'ipv6';
  return null;
}

const PING_ENDPOINT_EXAMPLE: Record<PingProbeKind, Record<PingProbeFamily, string>> = {
  icmp: { ipv4: '1.1.1.1', ipv6: '2606:4700:4700::1111' },
  tcp: { ipv4: '1.1.1.1:443', ipv6: '[2606:4700:4700::1111]:443' },
};

/** Trimmed endpoint, or null when the field is empty. ICMP has no port, so an IPv6 address needs no
 * brackets and is stored without them. */
export function normalizePingEndpoint(kind: PingProbeKind, value: string | null): string | null {
  const trimmed = (value ?? '').trim();
  if (!trimmed) return null;
  if (kind === 'icmp') {
    const bracketed = trimmed.match(/^\[([^\]]+)]$/);
    if (bracketed && validIpv6Literal(bracketed[1])) return bracketed[1];
  }
  return trimmed;
}

/** One address field: ICMP takes a host; TCP takes host:port, an IPv6 address in brackets. The host
 * is an IP literal of the field's family or a domain. An empty field is valid. */
export function pingProbeEndpointError(kind: PingProbeKind, family: PingProbeFamily, raw: string): string | null {
  const label = PING_FAMILY_LABEL[family];
  const value = raw.trim();
  if (!value) return null;
  if (Array.from(value).length > 512) return `${label} 地址不能超过 512 个字符`;
  if (/[\s/?#@]/.test(value)) return `${label} 地址只写${kind === 'tcp' ? '主机和端口' : '主机'}，不带协议或路径`;
  let host = value;
  if (kind === 'tcp') {
    const bracketed = value.match(/^\[([^\]]+)]:(\d+)$/);
    const plain = value.match(/^([^:[\]]+):(\d+)$/);
    const match = bracketed ?? plain;
    if (!match)
      return validIpv6Literal(value.replace(/^\[|]$/g, ''))
        ? 'TCP 的 IPv6 地址写作 [地址]:端口'
        : 'TCP 地址格式应为 主机:端口';
    if (bracketed && !validIpv6Literal(bracketed[1])) return '方括号内必须是 IPv6 地址';
    const port = Number(match[2]);
    if (!Number.isInteger(port) || port < 1 || port > 65_535) return 'TCP 端口必须为 1–65535';
    host = match[1];
  } else {
    const bracketed = value.match(/^\[([^\]]+)]$/);
    if (bracketed) {
      if (!validIpv6Literal(bracketed[1])) return '方括号内必须是 IPv6 地址';
      host = bracketed[1];
    } else if (/[[\]]/.test(value)) {
      return '方括号内必须是 IPv6 地址';
    } else if (value.includes(':') && !validIpv6Literal(value)) {
      return 'ICMP 不接受端口';
    }
  }
  const literal = literalFamily(host);
  if (literal && literal !== family) return `${label} 栏不能填 ${PING_FAMILY_LABEL[literal]} 地址`;
  return null;
}

export interface PingProbeFormError {
  text: string;
  /** The target row at fault; schedule and count errors have none. */
  row?: number;
  /** The address fields of that row to mark. */
  families?: readonly PingProbeFamily[];
}

/** The first error of the form, in reading order. */
export function pingProbeFormError(form: PingProbeSettings): PingProbeFormError | null {
  if (!validPingProbeNumber(form.interval_secs, 5, 86_400)) return { text: '探测间隔必须为 5–86400 秒的整数' };
  if (!validPingProbeNumber(form.timeout_ms, 1, 120_000)) return { text: '探测超时必须为 1–120000 毫秒的整数' };
  if (form.targets.length > 32) return { text: '最多配置 32 个目标' };
  const seen = new Set<string>();
  for (const [row, target] of form.targets.entries()) {
    const name = target.name.trim();
    if (!name) return { text: '每个目标都要填写名称', row };
    if (Array.from(name).length > 64) return { text: '目标名称不能超过 64 个字符', row };
    const filled = PING_PROBE_FAMILIES.filter(family => normalizePingEndpoint(target.kind, target[family]) !== null);
    if (filled.length === 0) return { text: `${name}：至少填写一个地址`, row, families: PING_PROBE_FAMILIES };
    for (const family of filled) {
      const error = pingProbeEndpointError(target.kind, family, target[family] ?? '');
      if (error) return { text: `${name}：${error}`, row, families: [family] };
    }
    for (const family of filled) {
      const value = normalizePingEndpoint(target.kind, target[family])!;
      const key = `${target.kind}|${family}|${value.toLowerCase()}`;
      if (seen.has(key))
        return {
          text: `${target.kind.toUpperCase()} 的 ${PING_FAMILY_LABEL[family]} 地址重复：${value}`,
          row,
          families: [family],
        };
      seen.add(key);
    }
  }
  return null;
}

/** What the server stores: trimmed names, normalized endpoints, and null for an empty field. */
const normalizePingProbeSettings = (settings: PingProbeSettings): PingProbeSettings => ({
  ...settings,
  targets: settings.targets.map(target => ({
    name: target.name.trim(),
    kind: target.kind,
    ipv4: normalizePingEndpoint(target.kind, target.ipv4),
    ipv6: normalizePingEndpoint(target.kind, target.ipv6),
  })),
});

function PingProbeSettingsSection({ editable, data }: { editable: boolean; data: PingProbeSettings }) {
  const qc = useQueryClient();
  const { form, setForm, accept } = useServerForm(data);
  const [saved, setSaved] = useState(false);
  const normalized = normalizePingProbeSettings(form);
  const dirty = JSON.stringify(normalized) !== JSON.stringify(data);
  useUnsavedChanges(dirty, 'Ping 链路探测');
  const invalid = pingProbeFormError(normalized);
  const save = useMutation({
    onMutate: () => qc.cancelQueries({ queryKey: ['ping-probe-settings'] }),
    mutationFn: (submitted: PingProbeSettings) => savePingProbeSettings(normalizePingProbeSettings(submitted)),
    onSuccess: async (next, submitted) => {
      await qc.cancelQueries({ queryKey: ['ping-probe-settings'] });
      setSaved(true);
      accept(next, submitted);
      qc.setQueryData(['ping-probe-settings'], next);
      qc.invalidateQueries({ queryKey: ['ping-probe-nodes'] });
    },
  });

  const updateTarget = (index: number, field: 'name' | PingProbeFamily, value: string) =>
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
      </header>
      <p className="cardsub">机器到指定目标的周期探测</p>
      {save.error && <ErrorBox error={save.error} />}
      <div className="setgrp settings-block ping-probe-settings-block">
        <div className="setgrp ping-probe-schedule" aria-label="Ping 探测调度">
          <p className="eyebrow">探测节奏</p>
          <div className="ping-probe-schedule-grid">
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
            </label>
          </div>
        </div>
        <section className="setgrp ping-probe-target-section">
          <div className="ping-probe-target-toolbar">
            <p className="eyebrow">
              探测目标 <span>{form.targets.length}/32</span>
            </p>
            <div className="ping-probe-add">
              {(['tcp', 'icmp'] as const).map(kind => (
                <button
                  className="btn sm"
                  type="button"
                  key={kind}
                  disabled={!editable || form.targets.length >= 32}
                  onClick={() =>
                    setForm(current => ({
                      ...current,
                      targets: [...current.targets, { name: '', kind, ipv4: '', ipv6: '' }],
                    }))
                  }
                >
                  ＋ {kind.toUpperCase()}
                </button>
              ))}
            </div>
          </div>
          {form.targets.length > 0 && (
            <div className="ping-probe-target-head" aria-hidden="true">
              <span>类型</span>
              <span>名称</span>
              <span>IPv4</span>
              <span>IPv6</span>
              <span />
            </div>
          )}
          <div className="ping-probe-targets">
            {form.targets.map((target, index) => (
              <div className="ping-probe-target" key={index}>
                <span className="ping-probe-kind">{target.kind.toUpperCase()}</span>
                <input
                  className="f"
                  aria-label={`目标 ${index + 1} 名称`}
                  placeholder="Cloudflare"
                  value={target.name}
                  onChange={event => updateTarget(index, 'name', event.target.value)}
                />
                {PING_PROBE_FAMILIES.map(family => {
                  const label = PING_FAMILY_LABEL[family];
                  const other = family === 'ipv4' ? 'ipv6' : 'ipv4';
                  const value = target[family] ?? '';
                  const wrong =
                    pingProbeEndpointError(target.kind, family, value) !== null ||
                    (invalid?.row === index && invalid.families?.includes(family) === true);
                  return (
                    <label className="ping-probe-address" key={family}>
                      {/* 宽屏由表头标明两列；窄屏表头隐藏，地址栏前显示地址族 */}
                      <span className="ping-probe-family-tag" aria-hidden="true">
                        {label}
                      </span>
                      <input
                        className="f mono"
                        aria-label={`目标 ${index + 1} ${label} 地址`}
                        aria-invalid={wrong || undefined}
                        placeholder={
                          normalizePingEndpoint(target.kind, target[other]) !== null
                            ? '不探测'
                            : PING_ENDPOINT_EXAMPLE[target.kind][family]
                        }
                        value={value}
                        onChange={event => updateTarget(index, family, event.target.value)}
                        onBlur={event => {
                          const next = normalizePingEndpoint(target.kind, event.target.value) ?? '';
                          if (next !== event.target.value) updateTarget(index, family, next);
                        }}
                      />
                    </label>
                  );
                })}
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
                <span>添加 TCP 或 ICMP 目标</span>
              </div>
            )}
          </div>
          {invalid && <span className="agent-log-invalid">{invalid.text}</span>}
        </section>
      </div>
      <SettingsSaveBar
        dirty={dirty}
        saving={save.isPending}
        savedText={saved ? '已保存，下一轮生效' : null}
        editable={editable}
        disabled={invalid !== null}
        title={invalid?.text}
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

function Secs({
  value,
  changed,
  ariaLabel,
  onPick,
}: {
  value: string;
  changed: boolean;
  ariaLabel: string;
  onPick: (v: string) => void;
}) {
  const now = Number(value.trim());
  const picks = [...SECS_PICKS];
  if (Number.isFinite(now) && value.trim() !== '' && !picks.includes(now)) picks.push(now);
  picks.sort((a, b) => a - b);
  return (
    <SegmentedControl
      value={value.trim()}
      options={picks.map(v => ({ value: String(v), label: v }))}
      className={changed ? 'chg' : undefined}
      ariaLabel={ariaLabel}
      onChange={onPick}
    />
  );
}

/* 一个字段一行：标签宽 132px 右对齐，值及其单位、提示、标签排在同一行内。 */
const Fld = ({ label, children }: { label?: string; children: React.ReactNode }) => (
  <div className="setfld">
    <label>{label ?? ''}</label>
    <div className="v">{children}</div>
  </div>
);

function intelligenceEligibility(node: NodeAgentStateItem): { eligible: boolean; reason: string } {
  if (node.lifecycle_phase !== 'active') return { eligible: false, reason: '机器不在运行生命周期' };
  if (node.operationally_isolated) return { eligible: false, reason: '机器已隔离' };
  if (node.agent_protocol_version == null || node.agent_protocol_version < MIN_AGENT_PROTOCOL_VERSION) {
    return { eligible: false, reason: `需要 Agent 协议 v${MIN_AGENT_PROTOCOL_VERSION} 或更高版本` };
  }
  if (!node.runtime_report_fresh) return { eligible: false, reason: '运行状态已过期' };
  return { eligible: true, reason: '可分发' };
}

const VPNGATE_PROVIDER_LABEL: Record<VpngateIpProvider, string> = {
  proxycheck: 'ProxyCheck v3',
  ffraud: 'FFraud',
  iplogs: 'IPLogs',
};

const DEFAULT_VPNGATE_INTELLIGENCE_POLICY: VpngateIntelligencePolicy = {
  refresh_mode: 'on_change',
  refresh_interval_hours: 168,
  active_window_hours: 72,
  stale_policy: 'retain',
  stale_after_hours: 168,
};

function VpngateIntelligenceSection({
  nodes,
  admissionPolicy,
  intelligencePolicy,
  proxycheckApiKeyConfigured,
  editable,
}: {
  nodes: NodeAgentStateItem[];
  admissionPolicy: VpngateAdmissionPolicy;
  intelligencePolicy: VpngateIntelligencePolicy;
  proxycheckApiKeyConfigured: boolean;
  editable: boolean;
}) {
  const queryClient = useQueryClient();
  const [proxycheckApiKeyInputs, setProxycheckApiKeyInputs] = useState(['']);
  const { form: admissionForm, setForm: setAdmissionForm, accept: acceptAdmission } = useServerForm(admissionPolicy);
  const {
    form: intelligenceForm,
    setForm: setIntelligenceForm,
    accept: acceptIntelligence,
  } = useServerForm(intelligencePolicy);
  const admissionDirty = JSON.stringify(admissionForm) !== JSON.stringify(admissionPolicy);
  const intelligenceDirty = JSON.stringify(intelligenceForm) !== JSON.stringify(intelligencePolicy);
  useUnsavedChanges(admissionDirty, 'VPN Gate 准入规则', 'vpngate-admission-policy');
  useUnsavedChanges(intelligenceDirty, 'VPN Gate 情报更新规则', 'vpngate-intelligence-policy');
  const selection = useMutation({
    mutationFn: ({ nodeId, enabled }: { nodeId: string; enabled: boolean }) =>
      updateVpngateIntelligenceNode(nodeId, enabled),
    onSuccess: result => {
      queryClient.setQueryData<{ nodes: NodeAgentStateItem[] }>(['nodes'], current =>
        current
          ? {
              nodes: current.nodes.map(node =>
                node.node_id === result.node_id ? { ...node, vpngate_intelligence_enabled: result.enabled } : node,
              ),
            }
          : current,
      );
    },
  });
  const savePolicy = useMutation({
    mutationFn: (submitted: VpngateAdmissionPolicy) => updateVpngateAdmissionPolicy(submitted),
    onSuccess: (saved, submitted) => {
      acceptAdmission(saved, submitted);
      queryClient.setQueryData<VpngateOverview>(['vpngate'], current =>
        current ? { ...current, admission_policy: saved } : current,
      );
    },
  });
  const saveIntelligencePolicy = useMutation({
    mutationFn: (submitted: VpngateIntelligencePolicy) => updateVpngateIntelligencePolicy(submitted),
    onSuccess: (saved, submitted) => {
      acceptIntelligence(saved, submitted);
      queryClient.setQueryData<VpngateOverview>(['vpngate'], current =>
        current ? { ...current, intelligence_policy: saved } : current,
      );
    },
  });
  const saveIntelligenceCredentials = useMutation({
    mutationFn: updateVpngateIntelligenceCredentials,
    onSuccess: credentials => {
      setProxycheckApiKeyInputs(['']);
      queryClient.setQueryData<VpngateOverview>(['vpngate'], current =>
        current ? { ...current, intelligence_credentials: credentials } : current,
      );
    },
  });
  const refreshIntelligence = useMutation({
    mutationFn: startVpngateIntelligenceRefresh,
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ['vpngate'] }),
  });
  const providerScore = (provider: VpngateIpProvider) =>
    admissionForm.provider_rules.find(rule => rule.provider === provider)?.maximum_score ?? 80;
  const setProviderScore = (provider: VpngateIpProvider, maximumScore: number) =>
    setAdmissionForm({
      ...admissionForm,
      provider_rules: admissionForm.provider_rules.map(rule =>
        rule.provider === provider ? { ...rule, maximum_score: maximumScore } : rule,
      ),
    });
  const policyValid =
    Number.isInteger(admissionForm.minimum_successful_sources) &&
    admissionForm.minimum_successful_sources >= 1 &&
    admissionForm.minimum_successful_sources <= 3 &&
    admissionForm.provider_rules.length === 3 &&
    admissionForm.provider_rules.every(
      rule => Number.isInteger(rule.maximum_score) && rule.maximum_score >= 0 && rule.maximum_score <= 100,
    );
  const intelligencePolicyValid = [
    intelligenceForm.active_window_hours,
    intelligenceForm.refresh_interval_hours,
    intelligenceForm.stale_after_hours,
  ].every(hours => Number.isInteger(hours) && hours >= 1 && hours <= 87_600);
  const proxycheckApiKeys = proxycheckApiKeyInputs.map(key => key.trim());
  const proxycheckApiKeysValid =
    proxycheckApiKeys.length >= 1 &&
    proxycheckApiKeys.length <= 32 &&
    proxycheckApiKeys.every(key => /^[A-Za-z0-9]{6}(?:-[A-Za-z0-9]{6}){3}$/.test(key)) &&
    new Set(proxycheckApiKeys).size === proxycheckApiKeys.length;
  const rows = nodes
    .map(node => ({ node, eligibility: intelligenceEligibility(node) }))
    .sort(
      (left, right) =>
        Number(right.node.vpngate_intelligence_enabled) - Number(left.node.vpngate_intelligence_enabled) ||
        Number(right.eligibility.eligible) - Number(left.eligibility.eligible) ||
        left.node.name.localeCompare(right.node.name),
    );
  const selected = rows.filter(row => row.node.vpngate_intelligence_enabled).length;
  return (
    <section className="panel config-panel vpngate-intelligence-settings" id="set-vpngate-intelligence">
      <header>
        <SettingsTitle id="set-vpngate-intelligence">情报任务</SettingsTitle>
        <span className="vpngate-selected-count">{selected} 台 Agent</span>
      </header>
      <p className="cardsub">集中管理 VPN Gate 目录情报与出口 IP 情报</p>
      <div className="vpngate-intelligence-layout">
        <section className="setgrp settings-block vpngate-settings-section vpngate-intelligence-policy-card">
          <div className="vpngate-settings-card-head">
            <p className="eyebrow">出口 IP 情报</p>
            <button
              type="button"
              className="btn sm"
              disabled={!editable || refreshIntelligence.isPending}
              onClick={() => refreshIntelligence.mutate()}
            >
              {refreshIntelligence.isPending ? '排队中…' : '立即刷新'}
            </button>
          </div>
          <Fld label="ProxyCheck API">
            <div className="vpngate-api-key-list" role="group" aria-label="ProxyCheck API 密钥">
              {proxycheckApiKeyInputs.map((key, index) => (
                <div className="vpngate-api-key-row" key={index}>
                  <input
                    className="f vpngate-api-key"
                    type="password"
                    autoComplete="new-password"
                    aria-label={`ProxyCheck API 密钥 ${index + 1}`}
                    placeholder={
                      index === 0 && proxycheckApiKeyConfigured ? '输入要追加的新密钥' : 'xxxxxx-xxxxxx-xxxxxx-xxxxxx'
                    }
                    value={key}
                    onChange={event =>
                      setProxycheckApiKeyInputs(current =>
                        current.map((item, itemIndex) => (itemIndex === index ? event.target.value : item)),
                      )
                    }
                  />
                  {proxycheckApiKeyInputs.length > 1 && (
                    <button
                      type="button"
                      className="btn sm vpngate-api-key-remove"
                      aria-label={`移除 ProxyCheck API 密钥 ${index + 1}`}
                      disabled={!editable || saveIntelligenceCredentials.isPending}
                      onClick={() =>
                        setProxycheckApiKeyInputs(current => current.filter((_, itemIndex) => itemIndex !== index))
                      }
                    >
                      ×
                    </button>
                  )}
                </div>
              ))}
              <div className="vpngate-api-key-actions">
                <button
                  type="button"
                  className="btn sm"
                  disabled={!editable || proxycheckApiKeyInputs.length >= 32 || saveIntelligenceCredentials.isPending}
                  onClick={() => setProxycheckApiKeyInputs(current => [...current, ''])}
                >
                  ＋ 添加密钥
                </button>
                <button
                  type="button"
                  className="btn sm primary"
                  disabled={!editable || !proxycheckApiKeysValid || saveIntelligenceCredentials.isPending}
                  onClick={() => saveIntelligenceCredentials.mutate(proxycheckApiKeys)}
                >
                  {saveIntelligenceCredentials.isPending ? '追加中…' : '追加到 Key 池'}
                </button>
              </div>
            </div>
            <span className="hint">
              {proxycheckApiKeyConfigured ? '已配置；旧 Key 不回显，追加不会覆盖' : '未配置'} · 最多 32 个，随机起点轮换
            </span>
          </Fld>
          {saveIntelligenceCredentials.error && <ErrorBox error={saveIntelligenceCredentials.error} />}
          <Fld label="查询策略">
            <SegmentedControl
              value={intelligenceForm.refresh_mode}
              options={[
                { value: 'on_change', label: '仅出口变化时' },
                { value: 'periodic', label: '定期刷新' },
              ]}
              ariaLabel="IP 情报查询策略"
              onChange={refresh_mode => setIntelligenceForm({ ...intelligenceForm, refresh_mode })}
            />
          </Fld>
          {intelligenceForm.refresh_mode === 'periodic' && (
            <Fld label="刷新周期">
              <input
                className="f vpngate-hours-input"
                aria-label="IP 情报刷新周期间隔"
                type="number"
                min={1}
                max={87_600}
                value={intelligenceForm.refresh_interval_hours}
                onChange={event =>
                  setIntelligenceForm({ ...intelligenceForm, refresh_interval_hours: Number(event.target.value) })
                }
              />
              <span>小时</span>
            </Fld>
          )}
          <Fld label="可拨窗口">
            <input
              className="f vpngate-hours-input"
              aria-label="纳入 IP 情报更新的最后成功拨通小时数"
              type="number"
              min={1}
              max={87_600}
              value={intelligenceForm.active_window_hours}
              onChange={event =>
                setIntelligenceForm({ ...intelligenceForm, active_window_hours: Number(event.target.value) })
              }
            />
            <span className="unit">小时</span>
          </Fld>
          <Fld label="旧情报">
            <select
              className="f"
              value={intelligenceForm.stale_policy}
              onChange={event =>
                setIntelligenceForm({
                  ...intelligenceForm,
                  stale_policy: event.target.value as VpngateIntelligencePolicy['stale_policy'],
                })
              }
            >
              <option value="retain">继续使用最近一次成功结果</option>
              <option value="mark">标记陈旧但继续使用</option>
              <option value="reject">超过期限后禁止准入</option>
            </select>
          </Fld>
          {intelligenceForm.stale_policy !== 'retain' && (
            <Fld label="陈旧期限">
              <input
                className="f vpngate-hours-input"
                aria-label="IP 情报陈旧期限"
                type="number"
                min={1}
                max={87_600}
                value={intelligenceForm.stale_after_hours}
                onChange={event =>
                  setIntelligenceForm({ ...intelligenceForm, stale_after_hours: Number(event.target.value) })
                }
              />
              <span className="unit">小时</span>
            </Fld>
          )}
          <SettingsSaveBar
            dirty={intelligenceDirty}
            saving={saveIntelligencePolicy.isPending}
            savedText={saveIntelligencePolicy.isSuccess ? '更新规则已保存' : null}
            editable={editable}
            disabled={!intelligencePolicyValid}
            title={intelligencePolicyValid ? undefined : '小时数必须是 1–87600 的整数'}
            label="保存更新规则"
            onSave={() => saveIntelligencePolicy.mutate(intelligenceForm)}
          />
          {saveIntelligencePolicy.error && <ErrorBox error={saveIntelligencePolicy.error} />}
          {refreshIntelligence.data && (
            <p className="hint">已将 {refreshIntelligence.data.queued} 个当前可拨出口加入刷新队列。</p>
          )}
          {refreshIntelligence.error && <ErrorBox error={refreshIntelligence.error} />}
        </section>

        <section className="setgrp settings-block vpngate-settings-section vpngate-admission-settings">
          <p className="eyebrow">准入规则</p>
          <div className="vpngate-admission-grid">
            <Fld label="成功来源">
              <select
                className="f"
                value={admissionForm.minimum_successful_sources}
                onChange={event =>
                  setAdmissionForm({ ...admissionForm, minimum_successful_sources: Number(event.target.value) })
                }
              >
                <option value={1}>1 家（任一来源即可）</option>
                <option value={2}>2 家</option>
                <option value={3}>3 家</option>
              </select>
            </Fld>
            <Fld label="地区判断">
              <select
                className="f"
                value={admissionForm.country_policy}
                onChange={event =>
                  setAdmissionForm({
                    ...admissionForm,
                    country_policy: event.target.value as VpngateAdmissionPolicy['country_policy'],
                  })
                }
              >
                <option value="any_match">任一来源匹配即可</option>
                <option value="all_match">所有已返回来源都要匹配</option>
                <option value="ignore">不检查地区</option>
              </select>
            </Fld>
            <Fld label="结论组合">
              <select
                className="f"
                value={admissionForm.risk_decision_policy}
                onChange={event =>
                  setAdmissionForm({
                    ...admissionForm,
                    risk_decision_policy: event.target.value as VpngateAdmissionPolicy['risk_decision_policy'],
                  })
                }
              >
                <option value="all_available_pass">所有已返回来源分别通过</option>
                <option value="any_available_pass">任一已返回来源通过</option>
              </select>
            </Fld>
            {(['proxycheck', 'ffraud', 'iplogs'] as const).map(provider => (
              <Fld label={VPNGATE_PROVIDER_LABEL[provider]} key={provider}>
                <input
                  className="f"
                  type="number"
                  min={0}
                  max={100}
                  value={providerScore(provider)}
                  onChange={event => setProviderScore(provider, Number(event.target.value))}
                />
                <span className="unit">分</span>
              </Fld>
            ))}
          </div>
          <SettingsSaveBar
            dirty={admissionDirty}
            saving={savePolicy.isPending}
            savedText={savePolicy.isSuccess ? '准入规则已保存' : null}
            editable={editable}
            disabled={!policyValid}
            title={policyValid ? undefined : '来源数量和风险分数超出范围'}
            label="保存准入规则"
            onSave={() => savePolicy.mutate(admissionForm)}
          />
          {savePolicy.error && <ErrorBox error={savePolicy.error} />}
        </section>

        <section className="setgrp settings-block vpngate-settings-section vpngate-intelligence-agents">
          <div className="vpngate-settings-card-head">
            <p className="eyebrow">情报执行 Agent</p>
            <span className="vpngate-agent-count">
              {selected}/{rows.length}
            </span>
          </div>
          <p className="vpngate-intelligence-agent-note">
            选中的 Agent 同时执行 VPN Gate 上游目录采集和出口 IP 情报查询；无需安装 OpenVPN。
          </p>
          <div className="vpngate-intelligence-grid">
            {rows.map(({ node, eligibility }) => {
              const busy = selection.isPending && selection.variables?.nodeId === node.node_id;
              return (
                <label className="vpngate-intelligence-node" key={node.node_id}>
                  <input
                    type="checkbox"
                    checked={Boolean(node.vpngate_intelligence_enabled)}
                    disabled={!editable || busy || (!eligibility.eligible && !node.vpngate_intelligence_enabled)}
                    onChange={event => selection.mutate({ nodeId: node.node_id, enabled: event.currentTarget.checked })}
                  />
                  <span>
                    <b>{node.name}</b>
                    <small className="mono">{node.node_id}</small>
                  </span>
                  {!eligibility.eligible && <span className="vpngate-agent-warning">{eligibility.reason}</span>}
                </label>
              );
            })}
          </div>
          {rows.length === 0 && <p className="empty">机队中还没有 Agent。</p>}
          {selection.error && <ErrorBox error={selection.error} />}
        </section>
      </div>
    </section>
  );
}

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
  const tunnelProbes = useQuery({
    queryKey: ['tunnel-probes'],
    queryFn: fetchTunnelProbes,
    staleTime: 30_000,
    refetchInterval: 30_000,
  });
  const tunnelProbeCapability = useQuery({
    queryKey: ['tunnel-probe-capability'],
    queryFn: fetchTunnelProbeCapability,
    staleTime: 60_000,
  });
  const nodes = useQuery({ queryKey: ['nodes'], queryFn: () => fetchNodes() });
  const vpngate = useQuery({ queryKey: ['vpngate'], queryFn: fetchVpngateOverview });
  const [form, setForm] = useState<Form>(EMPTY);
  const [saved, setSaved] = useState<Partial<Record<SectionKey, number>>>({});
  const [muxExpanded, setMuxExpanded] = useState(false);

  /* 会改变机器产物的分段保存写入草稿（`saveSettings` → `update_settings`），因此这些段
     「已保存」的基准是草稿生效后的值，而不是 `GET /settings`。以它为基准有
     两个后果，都实测复现过（tests/settings-draft-baseline.test.tsx）：
       一、保存完那一段，标题栏仍显示「有未保存的改动」，保存按钮一直亮着；
       二、更严重的是保存另一段时，本段未覆盖的字段会从已提交值重新取一遍
           （见下面 `save` 里的 `v`），把前一段刚写进草稿的改动覆盖回旧值。
     订阅草稿版本以便其变化时重算基准。 */
  useSyncExternalStore(draft.subscribe, draft.version);
  const pendingOp = draft.ops().find(op => op.op === 'update_settings');
  const pendingSettings = pendingOp?.op === 'update_settings' ? pendingOp.settings : null;
  // 端口默认值和端到端探测设置已改为分段即时提交。机器设置草稿仍会携带保存当时的
  // ports/probe 副本；显示和后续保存必须用当前已提交值覆盖，服务端回放时也做同样保护。
  const pendingBaseline =
    pendingSettings && settings.data
      ? { ...pendingSettings, ports: settings.data.ports, probe: settings.data.probe }
      : pendingSettings;
  const pristine = pendingBaseline ? formOf(pendingBaseline) : settings.data ? formOf(settings.data) : null;
  const modelFormDirty =
    pristine !== null && Object.values(SECTION_FIELDS).some(fields => fields.some(key => form[key] !== pristine[key]));
  useUnsavedChanges(modelFormDirty, '全局模型设置');

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
          mtu: Number(v('mtu')) || 1280,
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
        reverse_health: JSON.parse(v('reverseHealth')) as ReverseHealthPolicy,
        reverse_health_overrides: JSON.parse(v('reverseOverrides')) as ReverseHealthOverride[],
        relay_mux: {
          concurrency: numOr(v('muxConcurrency'), 1),
          prewarm_workers: numOr(v('muxPrewarmWorkers'), 0),
          reuse_threshold: numOr(v('muxReuseThreshold'), 2),
          max_probing_workers: numOr(v('muxMaxProbing'), 1),
          probe_interval_ms: numOr(v('muxProbeInterval'), 5000),
          probe_timeout_ms: numOr(v('muxProbeTimeout'), 2000),
          idle_ttl_ms: numOr(v('muxIdleTtl'), 24000),
          max_requests_per_worker: numOr(v('muxMaxRequests'), 128),
        },
        stats_user_online: v('statsOnline') === 'true',
      };
      const request: Promise<{ revision_id: number; settings?: ModelSettings }> =
        key === 'ports'
          ? savePortSettings(body.ports)
          : key === 'probe'
            ? saveProbeSettings(body.probe)
            : saveSettings(body);
      return request.then(r => {
        const committed = r.settings ?? null;
        return {
          key,
          revision_id: r.revision_id,
          submitted: form,
          normalized: formOf(committed ?? body),
          committed,
        };
      });
    },
    onSuccess: async r => {
      if (r.committed) {
        await qc.cancelQueries({ queryKey: ['settings'] });
        qc.setQueryData(['settings'], r.committed);
      }
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
    pingProbe.isPending ||
    tunnelProbes.isPending ||
    tunnelProbeCapability.isPending ||
    vpngate.isPending ||
    nodes.isPending
  )
    return <Loading variant="settings" />;
  const initialError =
    settings.error ??
    branding.error ??
    visitor.error ??
    certs.error ??
    dist.error ??
    logPolicy.error ??
    pingProbe.error ??
    tunnelProbes.error ??
    tunnelProbeCapability.error ??
    vpngate.error ??
    nodes.error;
  if (initialError) return <ErrorBox error={initialError} />;

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
  const reversePolicy = JSON.parse(form.reverseHealth) as ReverseHealthPolicy;
  const reverseOverrides = JSON.parse(form.reverseOverrides) as ReverseHealthOverride[];
  const reversePolicyError = reversePoliciesError(reversePolicy, reverseOverrides);

  return (
    <div className="cardpage">
      {/* 非 system-admin 仍可查看实际配置，但整页必须是真正的只读控件。此前只禁用了
          保存按钮，输入框和分段开关仍能改出一份永远无法保存的“脏”表单。 */}
      <div className="duo settings-layout">
        {/* 两栏各自成流，不对齐底部。代理池情报补在较短的左栏，隧道监测补在右栏；
            两张运行时设置卡不再脱离双栏成为通栏。 */}
        <div className="col">
          <fieldset className="settings-column-fields" disabled={!editable}>
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
            <VpngateIntelligenceSection
              nodes={nodes.data!.nodes}
              admissionPolicy={vpngate.data!.admission_policy}
              intelligencePolicy={vpngate.data!.intelligence_policy ?? DEFAULT_VPNGATE_INTELLIGENCE_POLICY}
              proxycheckApiKeyConfigured={Boolean(
                vpngate.data!.intelligence_credentials?.proxycheck_api_key_configured,
              )}
              editable={editable}
            />
          </fieldset>
        </div>

        <div className="col">
          <fieldset className="settings-column-fields" disabled={!editable}>
            {/* 位于 XRAY 之后、WIREGUARD 之前：上一段是接入面的服务端参数，本段是同一个
          xray 进程的另一部分——连接的存活时长和内存占用。两者都属于 xray，
          先说明对外配置再说明内部配置。 */}
            <Section
              id="set-conn"
              name="连接策略"
              sub="连接资源、中继 Mux 与反向隧道的探测、恢复策略"
              {...secProps('connection')}
              validationError={relayMuxError ?? reversePolicyError}
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
                    ariaLabel="UplinkOnly 等待秒数"
                    onPick={v => setForm({ ...form, connUplink: v })}
                  />
                  <span className="hint">对端服务器先关闭下行、连接只剩上行时，再等待这么久后整条断开。默认 2</span>
                </Fld>
                <Fld label="DownlinkOnly 等待（秒）">
                  <Secs
                    value={form.connDownlink}
                    changed={chg('connDownlink') !== 'f'}
                    ariaLabel="DownlinkOnly 等待秒数"
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
                <Fld label="在线来源">
                  <SegmentedControl
                    ariaLabel="在线来源统计"
                    value={form.statsOnline}
                    onChange={value => setForm({ ...form, statsOnline: value })}
                    options={[
                      { value: 'false', label: '关闭' },
                      { value: 'true', label: '记录公网 IP' },
                    ]}
                  />
                  <span className="hint">
                    按用户统计当前连接的不同公网 IP；同一 IP 的多个连接只算一个。原始 IP
                    保存在控制面，用于定位共享账号和连接问题。
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
              <Group label="中继 Mux" className="relay-mux-settings settings-field-grid-container">
                <SettingsParameterSummary
                  label="中继 Mux"
                  metrics={[
                    { label: '复用流', value: form.muxConcurrency },
                    { label: '预热目标', value: form.muxPrewarmWorkers },
                    { label: '复用阈值', value: form.muxReuseThreshold },
                    { label: '探活', value: `${form.muxProbeInterval} / ${form.muxProbeTimeout} ms` },
                    { label: '寿命', value: `${form.muxIdleTtl} ms` },
                  ]}
                  expanded={muxExpanded}
                  editable={editable}
                  controls="relay-mux-parameters"
                  onToggle={() => setMuxExpanded(open => !open)}
                />
                {relayMuxError && <p className="note settings-validation-error">{relayMuxError}</p>}
                {muxExpanded && (
                  <div className="settings-parameter-content" id="relay-mux-parameters">
                    <div className="settings-parameter-group">
                      <p className="eyebrow">复用容量</p>
                      <div className="settings-parameter-grid">
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
                      </div>
                    </div>
                    <div className="settings-parameter-group">
                      <p className="eyebrow">连接池</p>
                      <div className="settings-parameter-grid">
                        <Fld label="预热目标">
                          <input
                            className={chg('muxPrewarmWorkers')}
                            type="number"
                            min={0}
                            value={form.muxPrewarmWorkers}
                            onChange={e => setForm({ ...form, muxPrewarmWorkers: e.target.value })}
                          />
                        </Fld>
                        <Fld label="复用阈值">
                          <input
                            className={chg('muxReuseThreshold')}
                            type="number"
                            min={1}
                            value={form.muxReuseThreshold}
                            onChange={e => setForm({ ...form, muxReuseThreshold: e.target.value })}
                          />
                        </Fld>
                        <Fld label="超额空闲寿命">
                          <input
                            className={chg('muxIdleTtl')}
                            type="number"
                            min={1000}
                            value={form.muxIdleTtl}
                            onChange={e => setForm({ ...form, muxIdleTtl: e.target.value })}
                          />
                          <span className="unit">ms</span>
                        </Fld>
                      </div>
                    </div>
                    <div className="settings-parameter-group">
                      <p className="eyebrow">探活</p>
                      <div className="settings-parameter-grid">
                        <Fld label="探活并发">
                          <input
                            className={chg('muxMaxProbing')}
                            type="number"
                            min={1}
                            max={Number(form.muxReuseThreshold) || undefined}
                            value={form.muxMaxProbing}
                            onChange={e => setForm({ ...form, muxMaxProbing: e.target.value })}
                          />
                        </Fld>
                        <Fld label="探活周期">
                          <input
                            className={chg('muxProbeInterval')}
                            type="number"
                            min={2000}
                            max={60000}
                            value={form.muxProbeInterval}
                            onChange={e => setForm({ ...form, muxProbeInterval: e.target.value })}
                          />
                          <span className="unit">ms</span>
                        </Fld>
                        <Fld label="探活超时">
                          <input
                            className={chg('muxProbeTimeout')}
                            type="number"
                            min={200}
                            max={10000}
                            value={form.muxProbeTimeout}
                            onChange={e => setForm({ ...form, muxProbeTimeout: e.target.value })}
                          />
                          <span className="unit">ms</span>
                        </Fld>
                      </div>
                    </div>
                    <p className="hint settings-parameter-note">
                      优先使用已验证的空闲 Worker；没有空闲时先建到复用阈值，再复用活跃 Worker
                      的槽位。可用槽位用尽后允许突发扩容，超额 Worker 空闲后回收。
                    </p>
                    <p className="hint settings-parameter-note">
                      预热会在复用阈值内尽力补足空闲连接，不保证繁忙时仍有空闲。健康探测会保留预热目标内的空闲
                      Worker；寿命只回收超出预热目标的空闲容量。
                    </p>
                    <p className="hint settings-parameter-note">
                      探活或收尾中的 Worker 不承接新流；End 写入使用独立 10 秒宽限，超时后只转为排空，
                      不会因此关闭同载的其他业务流。
                    </p>
                  </div>
                )}
              </Group>
              <Group label="反向隧道" className="reverse-health-settings">
                <ReverseHealthSettings
                  policy={reversePolicy}
                  rows={reverseOverrides}
                  editable={editable}
                  validationError={reversePolicyError}
                  onPolicyChange={policy => setForm({ ...form, reverseHealth: JSON.stringify(policy) })}
                  onOverridesChange={rows => setForm({ ...form, reverseOverrides: JSON.stringify(rows) })}
                />
              </Group>
            </Section>

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
                <Fld label="全局 MTU">
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
              <Group label="新建资源端口基线" className="settings-field-grid-container">
                <div className="settings-field-grid port-allocation-grid">
                  <Fld label="VLESS · TLS / REALITY">
                    <input
                      className={chg('ingressBase')}
                      type="number"
                      min={1}
                      max={65535}
                      value={form.ingressBase}
                      onChange={e => setForm({ ...form, ingressBase: e.target.value })}
                    />
                  </Fld>
                  <Fld label="VLESS · Encryption">
                    <input
                      className={chg('vlessEncryptionBase')}
                      type="number"
                      min={1}
                      max={65535}
                      aria-label="VLESS · Encryption 起始端口"
                      value={form.vlessEncryptionBase}
                      onChange={e => setForm({ ...form, vlessEncryptionBase: e.target.value })}
                    />
                  </Fld>
                  <Fld label="AnyTLS · TCP">
                    <input
                      className={chg('anytlsBase')}
                      type="number"
                      min={1}
                      max={65535}
                      value={form.anytlsBase}
                      onChange={e => setForm({ ...form, anytlsBase: e.target.value })}
                    />
                  </Fld>
                  <Fld label="Hysteria 2 · UDP">
                    <input
                      className={chg('hy2Base')}
                      type="number"
                      min={1}
                      max={65535}
                      value={form.hy2Base}
                      onChange={e => setForm({ ...form, hy2Base: e.target.value })}
                    />
                  </Fld>
                  <Fld label="中转端口">
                    <input
                      className={chg('hopBase')}
                      type="number"
                      min={1}
                      max={65535}
                      value={form.hopBase}
                      onChange={e => setForm({ ...form, hopBase: e.target.value })}
                    />
                  </Fld>
                </div>
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
          </fieldset>
          <TunnelProbeSettingsSection editable={can(who.role, 'edit')} />
        </div>
      </div>
    </div>
  );
}
