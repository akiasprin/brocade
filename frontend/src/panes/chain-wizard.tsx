import { useId, useMemo, useState, type ReactNode } from 'react';
import type { HopWireKind } from '../ui/format';
import { keepPreviousData, useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  createApp,
  egressDnsEndpointText,
  egressDnsIssue,
  createChain,
  createIngress,
  createUser,
  setNodeEgressDns,
  upsertExternalOutbound,
  fetchCompileView,
  fetchNodes,
  fetchRevisions,
  fetchSettings,
  fetchSnapshot,
  fetchTenants,
  fetchUsers,
  putStep,
  stageGrant,
  type HopDial,
  type DestMatch,
  type EgressDnsResolution,
  type ExternalOutbound,
  type HopInRequest,
  type NodeAgentStateItem,
  type SnapshotApp,
  type RealityFallbackMode,
  type UserListItem,
  type Wires,
} from '../api';
import { nodeCertificateLabel } from '../certificate';
import { can, useSession } from '../session';
import {
  defaultHopWire,
  dialKindOf,
  egressDnsSelectorKey,
  hostOf,
  matchValues,
  reusableListeners,
  under,
} from './rules';
import { ErrorBox, Loading } from '../ui/bits';
import { freePortAcross, freeSpanAcross, hopListener, isValidSlug, occupiedPorts, portClash, spanClash } from './ports';
import { appId as randomAppId, modelIdPair } from '../model-id';
import {
  DEFAULT_TLS_FINGERPRINT,
  REALITY_FINGERPRINT_OPTIONS,
  realityFingerprintIsValid,
  realityServerNameIsValid,
} from '../reality';
import { DEFAULT_VLESS_ENCRYPTION } from '../vless-encryption';
import { newRealityFallbackLimits } from '../reality-fallback';
import { WizardCard, WizardField, WizardFooter, WizardPaper, WizardPaperHeader } from '../ui/wizard-paper';
import { Icon, type IconName } from '../ui/icons';
import { GeneratedUserAvatar } from '../ui/user-avatar';
import { useInitialReady } from '../ui/use-initial-ready';
import { vpngateNodeEligibility } from '../vpngate-capability';
import { mtprotoNodeEligibility } from '../mtproto-capability';
import { INGRESS_PROTOCOL_DESCRIPTIONS } from '../ingress-protocols';
import { useUnsavedChanges } from '../ui/navigation-guard';
import {
  wizardDefaultListenerWire,
  wizardEgressRule,
  wizardForwardEdges,
  wizardMembers,
  wizardRuleIssue,
  wizardRulesWithListenerPorts,
  wizardSpine,
  type WizardRuleTables,
} from './chain-wizard-graph';
import { WizardPathEditor, type WizardDnsChange } from './chain-wizard-path';

// 使一台机器运行 xray 的方式：为其创建一条链和一个接入面。
// 是否运行 xray 由编译器计算得出（physical/node.rs 的 xray_plan），节点上没有也不应有
// 「启用 xray」这类开关——那会使同一状态存在两个数据源。
//
// # 版面：以链的结构为主线
//
// 此处原为九行的两列表单，其中真正需要决策的只有三项：入口位于哪台机器、是否需要中继、
// 使用哪个端口。其余六行是可自动计算的 id 和名称，却各占一行，且每一行都可能覆盖已有的链
// （`chains.id` 是全局主键，写入接口是 upsert）。
//
// 主体与链详情共用 Rule 的语义：任意规则形成主干，例外转发形成支路；每台机器只有
// 一张规则表和一个中转监听。创建时看到的规则就是写入草稿的规则，不另外维护线性路径。
//
// 分组、链和入口的内部 id 都自动生成并隐藏；用户只维护可读名称。
//
// # 两个入口，同一套界面
//
// 从线路页和机器页进入的是同一套界面，差异只在哪一项被预填：从线路页进入时线路一项
// 是固定文本，从机器页进入时是下拉框。位置和样式不变——此前线路页的该项位于向导之外
// 且使用另一套排版，同一功能存在两种形式。

// 校验规则来自 ir/validate.rs：链必须有接入面（chain.no-ingress）。链头即接入面所在的
// 机器，路径由规则表表达——向导自动满足该要求：入口挂在本机，每台写入自己的
// 规则表；默认 `any → 下一台`（末位为出网），也可在此修改并增加例外。

// 「＋ 新建分组…」在下拉框中的取值。前后空格与冒号不会出现在 app-xxxx 中。
const NEW_APP = ' :new-app:';
const VLESS_PORT_BASE = 13443;
const ANYTLS_PORT_BASE = 14443;
const HY2_PORT_BASE = 30000;
const HY2_HOP_SPAN = 100;
const MTPROTO_PORT_BASE = 28800;

export const NEW_CHAIN_PROTOCOL_DEFAULTS = {
  vless: true,
  vlessEncryption: false,
  anytls: true,
  hysteria2: true,
  mtproto: false,
} as const;

interface TcpListenerChoice {
  label: string;
  port: number;
}

/** Return one readable error for TCP listeners created together on the entry node. */
export function entryTcpPortCollision(listeners: TcpListenerChoice[]): string | null {
  const byPort = new Map<number, string[]>();
  for (const listener of listeners) {
    const labels = byPort.get(listener.port) ?? [];
    labels.push(listener.label);
    byPort.set(listener.port, labels);
  }
  for (const [port, labels] of byPort) {
    if (labels.length > 1) return `${labels.join(' 与 ')} 不能共用 TCP ${port}`;
  }
  return null;
}

/** A custom hop field is a host only; the listener port is managed in the adjacent field. */
export function customHopHostError(value: string): string | null {
  const host = value.trim();
  if (!host) return '填写自定义主机地址';
  if (/[\s/?#]/.test(host)) return '这里只填写主机地址，不要带端口、路径或空格';
  if (host.startsWith('[') !== host.endsWith(']')) return 'IPv6 方括号不完整';
  if (host.includes(':')) {
    const literal = host.startsWith('[') ? host.slice(1, -1) : host;
    try {
      new URL(`http://[${literal}]/`);
    } catch {
      return 'IPv6 地址无效；这里只填写地址，端口在右侧设置';
    }
  }
  return null;
}

export function newChainWires({
  vlessEncryption = false,
  vlessEncryptionPort = 13800,
  vlessEncryptionProfile = 'default',
  vless,
  anytls,
  hysteria2,
  mtproto = false,
  anytlsPort,
  mtprotoPort = MTPROTO_PORT_BASE,
  anytlsPaddingScheme = [],
  hy2Start,
  hy2End,
  hy2Up = '',
  hy2Down = '',
}: {
  vlessEncryption?: boolean;
  vlessEncryptionPort?: number;
  vlessEncryptionProfile?: 'default' | 'native';
  vless: boolean;
  anytls: boolean;
  hysteria2: boolean;
  mtproto?: boolean;
  anytlsPort: number;
  mtprotoPort?: number;
  anytlsPaddingScheme?: string[];
  hy2Start: number;
  hy2End: number;
  hy2Up?: string;
  hy2Down?: string;
}): Wires {
  return {
    ...(vlessEncryption
      ? {
          vless_encryption: {
            port: vlessEncryptionPort,
            ...(vlessEncryptionProfile === 'native'
              ? { options: { ...DEFAULT_VLESS_ENCRYPTION, appearance: 'native' as const } }
              : {}),
          },
        }
      : {}),
    vless: vless ? { kind: 'vless-reality' } : null,
    anytls: anytls
      ? {
          port: anytlsPort,
          security: 'tls',
          padding_scheme: anytlsPaddingScheme,
          idle_session_check_interval_secs: 30,
          idle_session_timeout_secs: 30,
          min_idle_session: 1,
          masquerade: { kind: 'not-found' },
        }
      : null,
    hysteria2: hysteria2
      ? {
          port: hy2Start,
          hop: { start: hy2Start, end: hy2End },
          bandwidth: {
            ...(hy2Up.trim() ? { up: hy2Up.trim() } : {}),
            ...(hy2Down.trim() ? { down: hy2Down.trim() } : {}),
          },
          congestion: 'brutal',
          obfs: { kind: 'salamander', password: 'quick-brown-fox' },
          masquerade: { kind: 'not-found' },
        }
      : null,
    mtproto: mtproto ? { port: mtprotoPort } : null,
  };
}

type HopSec = HopWireKind;

// 中转端口按**监听的机器**存储而非按跳存储——模型中 `hop_in` 关联在 `(chain, node)` 上，
// 一台机器在一条链上只有一个端口。常规档位由下游监听，反向两档由下游连接上游、
// 端口开在上游，两种跳可能位于同一台机器上（前一跳常规进入、后一跳反向发出），
// 此时它们本应是同一个端口。按跳存储会导致两份状态写入同一条记录，后写入的覆盖先写入的。
type PortEdit = { port?: string; sec?: HopSec };

/** Default the wire of one listener from every chain edge that uses it. */
export function defaultListenerHopWire(spine: string[], host: string, dialAt: (index: number) => HopDial): HopSec {
  let usedOverOverlay = false;
  for (let i = 1; i < spine.length; i += 1) {
    const dial = dialAt(i);
    if (hopListener(spine, i, dial.t === 'reverse') !== host) continue;
    if (defaultHopWire(dial) === 'encryption') return 'encryption';
    usedOverOverlay = true;
  }
  return usedOverOverlay ? 'none' : 'encryption';
}

function WizardPortInput({
  label,
  value,
  disabled,
  invalid,
  onChange,
}: {
  label: string;
  value: string;
  disabled: boolean;
  invalid: boolean;
  onChange: (value: string) => void;
}) {
  return (
    <input
      className="f mono"
      inputMode="numeric"
      aria-label={label}
      value={value}
      disabled={disabled}
      aria-invalid={invalid || undefined}
      onChange={event => onChange(event.target.value)}
    />
  );
}

/** 手填过的端口才出现：回到按占用自动分配。端口范围共用一个，两端一起恢复。 */
function WizardPortAuto({ label, onReset }: { label: string; onReset: () => void }) {
  return (
    <button type="button" className="btn sm" aria-label={`${label}恢复自动分配`} onClick={onReset}>
      自动
    </button>
  );
}

function WizardProtocolTile({
  name,
  note,
  icon,
  enabled,
  onToggle,
  port,
  params,
  expanded,
  onExpand,
  portRange = false,
  issues = [],
}: {
  name: string;
  note: string;
  icon: IconName;
  enabled: boolean;
  onToggle: (enabled: boolean) => void;
  port: ReactNode;
  params?: ReactNode;
  expanded?: boolean;
  onExpand?: () => void;
  portRange?: boolean;
  issues?: string[];
}) {
  const checkboxId = useId();
  return (
    <div className={`protocol-choice wzp-card${enabled ? ' on' : ''}${portRange ? ' range' : ''}`}>
      <input
        id={checkboxId}
        type="checkbox"
        aria-label={name}
        checked={enabled}
        onChange={event => onToggle(event.target.checked)}
      />
      <label className="protocol-choice-copy" htmlFor={checkboxId}>
        <b>
          <Icon of={icon} size={14} className="protocol-choice-icon" />
          {name}
        </b>
        <span className="note">{note}</span>
      </label>
      <span className="wzp-port">{port}</span>
      {params && (
        <button type="button" className="wzp-more" aria-expanded={expanded} onClick={onExpand}>
          {expanded ? '收起参数' : '参数'}
        </button>
      )}
      {issues.map(issue => (
        <p className="note bad wzp-issue" role="alert" key={issue}>
          {issue}
        </p>
      ))}
      {expanded && params && <div className="wzp-body">{params}</div>}
    </div>
  );
}

export function ChainWizard({
  node,
  defaultApp,
  onDone,
}: {
  // 预填的链头。机器页传入（入口即该机器），线路页不传入——由路径的第一行选择。
  // 不传入不表示使用另一套界面：该行本身存在，只是从固定文本变为
  // 用于选择入口机器的下拉框。
  node?: NodeAgentStateItem;
  // 从分组等入口进入时默认选中该分组，仍可切换到其他分组或新建。
  defaultApp?: { id: string; label: string };
  onDone: (created?: { appId: string; chainId: string; ingressId: string }) => void;
}) {
  const { who } = useSession();
  const qc = useQueryClient();
  const system = can(who.role, 'system');
  const snapshot = useQuery({ queryKey: ['snapshot'], queryFn: () => fetchSnapshot() });
  const settings = useQuery({ queryKey: ['settings'], queryFn: () => fetchSettings() });
  const nodes = useQuery({ queryKey: ['nodes'], queryFn: () => fetchNodes() });
  const ingressBase = settings.data?.ports?.ingress_base || VLESS_PORT_BASE;
  const hopBase = settings.data?.ports?.hop_base || 20000;
  // 中转端口选择 REALITY 时请求中需要填写的站点。它没有接入面的本机 TLS 证书模式，必须
  // 使用已经明确配置的全局站点；没有站点时该选择会被拦截，而不是静默塞入工厂域名。
  const realitySite = {
    dest: settings.data?.reality_site?.dest ?? '',
    names: settings.data?.reality_site?.server_names ?? [],
  };
  const globalRealityReady = realitySite.dest.trim() !== '' && realitySite.names.length > 0;

  const apps = snapshot.data?.snapshot.apps ?? [];

  // 分组的选择：优先默认分组，否则使用第一个已有分组，没有任何分组时才使用新建。
  // 默认选择不改变菜单顺序。与 id、端口遵循同一规则——
  // state 存储的是是否手动选择过（null 表示未选择），显示值实时计算。不能使用 useState 的
  // 初始值：快照尚未返回时 `apps` 为空，此时计算的默认值始终是新建，且不会再更新。
  // 创建分组需要 system-admin 而创建链只需 editor，两种权限都不具备时该项无法给出取值：
  // 「＋ 新建分组…」照常列出但禁用，`targetApp` 为空使 `ready` 拦截提交。
  const [appModeRaw, setAppMode] = useState<'new' | 'existing' | null>(null);
  const [appLabelRaw, setAppLabel] = useState<string | null>(null);
  const [pickedAppRaw, setPickedApp] = useState<string | null>(null);
  const appMode: 'new' | 'existing' = appModeRaw ?? (apps.length > 0 ? 'existing' : 'new');
  const pickedApp = pickedAppRaw ?? apps.find(app => app.id === defaultApp?.id)?.id ?? apps[0]?.id ?? '';
  const [chainName, setChainName] = useState('');
  // 与链详情共用 Rule 的语义：主干和支路均从显式转发规则推导，不保存第二份路径顺序。
  const [headId, setHeadId] = useState<string | null>(node?.node_id ?? null);
  const [pathRules, setPathRules] = useState<WizardRuleTables>(node ? { [node.node_id]: [wizardEgressRule()] } : {});
  const [dnsChanges, setDnsChanges] = useState<WizardDnsChange[]>([]);
  const [importedOutbounds, setImportedOutbounds] = useState<ExternalOutbound[]>([]);
  const availableOutbounds = [
    ...(snapshot.data?.snapshot.external_outbounds ?? []),
    ...importedOutbounds.filter(
      item => !(snapshot.data?.snapshot.external_outbounds ?? []).some(existing => existing.id === item.id),
    ),
  ];
  const spine = wizardSpine(headId, pathRules);
  const members = wizardMembers(headId, pathRules);
  const exceptionCount = members.reduce(
    (count, id) => count + (pathRules[id] ?? []).filter(rule => rule.m.t !== 'any').length,
    0,
  );
  const bind = '0.0.0.0';
  const [paddingMode, setPaddingMode] = useState<'default' | 'custom'>('default');
  const [customPadding, setCustomPadding] = useState('');
  const [hy2Up, setHy2Up] = useState('');
  const [hy2Down, setHy2Down] = useState('');
  /* 键是监听的机器而非跳。见 PortEdit。 */
  const [portEdits, setPortEdits] = useState<Record<string, PortEdit>>({});
  const [showOps, setShowOps] = useState(false);
  const [openProtocol, setOpenProtocol] = useState<string | null>(null);
  const [realityTargetRaw, setRealityTarget] = useState<RealityFallbackMode | '' | null>(null);
  const [encryptionEnabled, setEncryptionEnabled] = useState<boolean>(NEW_CHAIN_PROTOCOL_DEFAULTS.vlessEncryption);
  const [encryptionProfile, setEncryptionProfile] = useState<'default' | 'native'>('default');
  const [vlessEnabled, setVlessEnabled] = useState<boolean>(NEW_CHAIN_PROTOCOL_DEFAULTS.vless);
  const [anyTlsEnabled, setAnyTlsEnabled] = useState<boolean>(NEW_CHAIN_PROTOCOL_DEFAULTS.anytls);
  const [hy2Enabled, setHy2Enabled] = useState<boolean>(NEW_CHAIN_PROTOCOL_DEFAULTS.hysteria2);
  const [mtprotoEnabled, setMtProtoEnabled] = useState<boolean>(NEW_CHAIN_PROTOCOL_DEFAULTS.mtproto);
  const [customRealityDest, setCustomRealityDest] = useState('');
  const [customRealityNames, setCustomRealityNames] = useState('');
  const [customRealityFingerprint, setCustomRealityFingerprint] = useState<string>(DEFAULT_TLS_FINGERPRINT);
  const [error, setError] = useState<unknown>(null);
  const [attempted, setAttempted] = useState(false);
  const [submitting, setSubmitting] = useState(false);

  const nameMap = new Map((nodes.data?.nodes ?? []).map(n => [n.node_id, n.name]));
  const nameOf = (id: string) => nameMap.get(id) || id;
  // 链头即接入面所在的机器，也是主干的第 0 位。整条链的租户和默认端口取自它，
  // 因此在未选择之前本页无法给出任何默认值——`ready` 会拦截提交。
  const head = (nodes.data?.nodes ?? []).find(n => n.node_id === headId) ?? node ?? null;
  const headLabel = head ? head.name || head.node_id : '';
  const headCertificateNode = snapshot.data?.snapshot.nodes?.find(candidate => candidate.id === head?.node_id);
  const headCertificate = headCertificateNode?.certificate_name ?? null;
  const headCertificateLabel = nodeCertificateLabel(headCertificateNode?.certificate_track);
  const mtprotoUnavailable = head ? mtprotoNodeEligibility(head).reason : null;
  // The ordinary case requires no extra choice: a managed node already belongs to the default
  // certificate group, so REALITY borrows that exact local identity. The selector remains visible
  // for an operator who wants the global/custom target instead.
  const realityTarget: RealityFallbackMode | '' =
    realityTargetRaw ?? (headCertificate ? 'node-certificate' : globalRealityReady ? 'global-site' : '');
  const customRealityServerNames = customRealityNames
    .split(/[\s,]+/)
    .map(value => value.trim())
    .filter(Boolean);
  const realityTargetReady =
    !vlessEnabled ||
    (realityTarget === 'node-certificate' && !!headCertificate) ||
    (realityTarget === 'global-site' && globalRealityReady) ||
    (realityTarget === 'custom-site' &&
      /^\S+:[1-9]\d*$/.test(customRealityDest.trim()) &&
      Number(customRealityDest.trim().split(':').at(-1)) <= 65535 &&
      customRealityServerNames.length > 0 &&
      customRealityServerNames.every(realityServerNameIsValid) &&
      realityFingerprintIsValid(customRealityFingerprint));
  const [appId] = useState(() => randomAppId());
  const appLabel = appLabelRaw ?? headLabel;
  /* 选择连接方式需要读取对端的公网地址，因此此处需要完整的节点数据而非只有名称。 */
  const nodeOf = (id: string) => (nodes.data?.nodes ?? []).find(n => n.node_id === id) ?? null;
  // 订阅地区只描述主链最终从哪里出网，因此跟随 Any 转发主干的末尾节点；例外支路不改变
  // 这条链的默认出口标识。节点地区已经合并了 GeoIP 探测和人工纠正，不再让建链者重复选择。
  const tail = nodeOf(spine.at(-1) ?? '');
  const exitCountry = tail?.public_ipv4_country?.trim().toUpperCase() || null;

  // 系统层的端口（WireGuard）只存在于编译产生的 IR 中。查询键与顶栏角标、检视窗相同，
  // 因此通常命中缓存。
  const revisions = useQuery({ queryKey: ['revisions'], queryFn: () => fetchRevisions() });
  const current = revisions.data?.current_revision;
  const compile = useQuery({
    queryKey: ['compile', current],
    queryFn: () => fetchCompileView(current!),
    enabled: !!current,
    placeholderData: keepPreviousData,
  });

  // 技术 ID 在向导的整个生命周期中保持不变：后续的链、接入面、授权与规则操作都引用
  // 同一组值。端口则随已占用端口计算，只有用户明确修改后才固定。
  const [{ chainId, ingressId }] = useState(() =>
    modelIdPair(
      new Set(
        apps.flatMap(app => [
          ...(app.chains ?? []).map(chain => chain.id),
          ...(app.ingresses ?? []).map(ingress => ingress.id),
        ]),
      ),
    ),
  );
  const [portRaw, setPort] = useState<string | null>(null);
  const [encryptionPortRaw, setEncryptionPort] = useState<string | null>(null);
  const [anyTlsPortRaw, setAnyTlsPort] = useState<string | null>(null);
  const [mtprotoPortRaw, setMtProtoPort] = useState<string | null>(null);
  const [hy2StartRaw, setHy2Start] = useState<string | null>(null);
  const [hy2EndRaw, setHy2End] = useState<string | null>(null);

  const targetApp = appMode === 'new' ? appId.trim() : pickedApp;

  // 端口需要避开所有线路中已有的监听，而非只避开目标线路。技术 ID 的极小概率冲突
  // 由同一草稿中的 create-only 操作在服务端原子拒绝，不会再把已有对象当成更新目标。
  const taken = useMemo(
    () => occupiedPorts(apps, nodes.data?.nodes ?? [], compile.data?.system),
    /* snapshot 尚未返回时 apps 每次都是新建的空数组，以它作为依赖会每帧重新计算；
   因此依赖 snapshot.data */
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [snapshot.data, nodes.data, compile.data],
  );

  // VLESS 起始值取自全局设置（settings.ports.ingress_base）。常量只用于设置尚未加载时的
  // 短暂回退；始终硬编码会让运营者修改基线后，建链向导仍填入旧值。
  const portText = portRaw ?? String(freePortAcross(taken, [head?.node_id ?? ''], ingressBase));
  const port = Number(portText);

  // The wizard exposes which protocols are created but keeps protocol tuning out of the first
  // decision. Each enabled protocol receives a conflict-free factory port; detailed transport,
  // hopping and masquerade controls remain on the chain detail page.
  let autoAnyTlsPort = freePortAcross(
    taken,
    [head?.node_id ?? ''],
    settings.data?.ports?.anytls_base || ANYTLS_PORT_BASE,
  );
  while (autoAnyTlsPort === port && autoAnyTlsPort < 65536) autoAnyTlsPort += 1;
  const anyTlsPortText = anyTlsPortRaw ?? String(autoAnyTlsPort);
  const anyTlsPort = Number(anyTlsPortText);
  const udpTaken = useMemo(
    () => occupiedPorts(apps, nodes.data?.nodes ?? [], compile.data?.system, undefined, 'udp'),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [snapshot.data, nodes.data, compile.data],
  );
  const autoHy2Start = freeSpanAcross(
    udpTaken,
    [head?.node_id ?? ''],
    settings.data?.ports?.hy2_base || HY2_PORT_BASE,
    HY2_HOP_SPAN,
  );
  const hy2StartText = hy2StartRaw ?? String(autoHy2Start);
  const hy2Start = Number(hy2StartText);
  const hy2EndText = hy2EndRaw ?? String(hy2Start + HY2_HOP_SPAN - 1);
  const hy2End = Number(hy2EndText);
  let autoEncryptionPort = freePortAcross(
    taken,
    [head?.node_id ?? ''],
    settings.data?.ports?.vless_encryption_base || 13800,
  );
  while (
    autoEncryptionPort < 65536 &&
    ((vlessEnabled && autoEncryptionPort === port) ||
      (anyTlsEnabled && autoEncryptionPort === anyTlsPort) ||
      taken.get(head?.node_id ?? '')?.has(autoEncryptionPort))
  )
    autoEncryptionPort += 1;
  const encryptionPortText = encryptionPortRaw ?? String(autoEncryptionPort);
  const encryptionPort = Number(encryptionPortText);
  let autoMtProtoPort = freePortAcross(
    taken,
    [head?.node_id ?? ''],
    settings.data?.ports?.mtproto_base || MTPROTO_PORT_BASE,
  );
  while (
    autoMtProtoPort < 65536 &&
    ((vlessEnabled && autoMtProtoPort === port) ||
      (encryptionEnabled && autoMtProtoPort === encryptionPort) ||
      (anyTlsEnabled && autoMtProtoPort === anyTlsPort) ||
      taken.get(head?.node_id ?? '')?.has(autoMtProtoPort))
  )
    autoMtProtoPort += 1;
  const mtprotoPortText = mtprotoPortRaw ?? String(autoMtProtoPort);
  const mtprotoPort = Number(mtprotoPortText);
  const enabledProtocolCount =
    Number(vlessEnabled) +
    Number(anyTlsEnabled) +
    Number(hy2Enabled) +
    Number(encryptionEnabled) +
    Number(mtprotoEnabled);
  const wires = newChainWires({
    vlessEncryption: encryptionEnabled,
    vlessEncryptionPort: encryptionPort,
    vlessEncryptionProfile: encryptionProfile,
    vless: vlessEnabled,
    anytls: anyTlsEnabled,
    hysteria2: hy2Enabled,
    mtproto: mtprotoEnabled,
    anytlsPort: anyTlsPort,
    mtprotoPort,
    anytlsPaddingScheme:
      paddingMode === 'custom'
        ? customPadding
            .split(/\r?\n/)
            .map(line => line.trim())
            .filter(Boolean)
        : [],
    hy2Start,
    hy2End,
    hy2Up,
    hy2Down,
  });
  const entryTcpListeners: TcpListenerChoice[] = [];
  if (vlessEnabled) entryTcpListeners.push({ label: 'VLESS · TLS/REALITY', port });
  if (encryptionEnabled) entryTcpListeners.push({ label: 'VLESS · Encryption', port: encryptionPort });
  if (anyTlsEnabled) entryTcpListeners.push({ label: 'AnyTLS', port: anyTlsPort });
  if (mtprotoEnabled) entryTcpListeners.push({ label: 'MTProxy', port: mtprotoPort });
  const entryPortCollision = entryTcpPortCollision(entryTcpListeners);
  const entryPortIssues = entryTcpListeners.flatMap(listener => {
    if (!Number.isInteger(listener.port) || listener.port < 1 || listener.port > 65_535) {
      return [`${listener.label} 端口必须是 1–65535`];
    }
    const clash = portClash(taken, [head?.node_id ?? ''], listener.port);
    return clash ? [`${listener.label}：${clash}`] : [];
  });
  if (hy2Enabled) {
    if (
      !Number.isInteger(hy2Start) ||
      !Number.isInteger(hy2End) ||
      hy2Start < 1 ||
      hy2End > 65_535 ||
      hy2End < hy2Start
    )
      entryPortIssues.push('Hysteria 2 端口范围必须在 1–65535，且结束端口不小于起始端口');
    else {
      const clash = spanClash(udpTaken, [head?.node_id ?? ''], hy2Start, hy2End);
      if (clash) entryPortIssues.push(`Hysteria 2：${clash}`);
    }
  }
  const tcpIssuesFor = (label: string) => {
    const listener = entryTcpListeners.find(choice => choice.label === label);
    return [
      ...entryPortIssues.filter(issue => issue.startsWith(`${label}：`) || issue.startsWith(`${label} 端口`)),
      ...(entryPortCollision &&
      listener &&
      entryTcpListeners.some(choice => choice.label !== label && choice.port === listener.port)
        ? [entryPortCollision]
        : []),
    ];
  };
  const certificateIssue = head && !headCertificate ? `入口尚未分配${headCertificateLabel}` : null;
  const protocolIssues = {
    vless: vlessEnabled
      ? [...tcpIssuesFor('VLESS · TLS/REALITY'), ...(!realityTargetReady ? ['补全 REALITY 伪装目标'] : [])]
      : [],
    encryption: encryptionEnabled ? tcpIssuesFor('VLESS · Encryption') : [],
    anytls: anyTlsEnabled
      ? [
          ...tcpIssuesFor('AnyTLS'),
          ...(certificateIssue ? [certificateIssue] : []),
          ...(paddingMode === 'custom' && !customPadding.trim() ? ['填写自定义 Padding 规则'] : []),
        ]
      : [],
    hy2: hy2Enabled
      ? [
          ...entryPortIssues.filter(issue => issue.startsWith('Hysteria 2')),
          ...(certificateIssue ? [certificateIssue] : []),
          ...(Boolean(hy2Up.trim()) !== Boolean(hy2Down.trim()) ? ['上下行带宽需同时填写'] : []),
        ]
      : [],
    mtproto: mtprotoEnabled ? [...tcpIssuesFor('MTProxy'), ...(mtprotoUnavailable ? [mtprotoUnavailable] : [])] : [],
  };
  // AnyTLS 与 Hysteria 2 的 TLS 用入口的证书。没有证书时这一行不出现，原因由协议卡的问题行说明。
  const certificateField = headCertificate && (
    <WizardField label="证书">
      <span className="mono">{headCertificate}</span>
    </WizardField>
  );

  const edges = wizardForwardEdges(headId, pathRules);
  const listeners = [...new Set(edges.map(edge => edge.listener))];

  // 为每台监听的机器选择一个未占用的端口。选择时不需要考虑该链上的其他机器——不同机器上的
  // 端口互不影响；同一台机器不会重复选择，由上面的去重保证。
  // 不使用 useMemo：主干只有少量机器，`freePortAcross` 的开销为一次 Map 查找起步的循环，
  // 而添加依赖数组需要传入每帧新建的数组或将其序列化为字符串再解析——两种方式都是为了
  // 适配记忆化而增加复杂度，React Compiler 会自行处理。
  const autoHostPorts = new Map<string, number>();
  for (const host of listeners) autoHostPorts.set(host, freePortAcross(taken, [host], hopBase));

  const hostPortOf = (host: string) => portEdits[host]?.port ?? String(autoHostPorts.get(host) ?? hopBase);
  const hostSecOf = (host: string): HopSec => portEdits[host]?.sec ?? wizardDefaultListenerWire(edges, host);
  const patchPort = (host: string, next: PortEdit) =>
    setPortEdits(prev => ({ ...prev, [host]: { ...prev[host], ...next } }));
  const stagedRules = wizardRulesWithListenerPorts(headId, pathRules, host => Number(hostPortOf(host)) || hopBase);
  const dnsPolicies = snapshot.data?.node_egress_dns ?? [];
  const patchDns = (nodeId: string, selector: DestMatch, resolution: EgressDnsResolution | null) => {
    const key = egressDnsSelectorKey(selector);
    const baseline =
      dnsPolicies.find(policy => policy.node === nodeId && egressDnsSelectorKey(policy.selector) === key)?.resolution ??
      null;
    setDnsChanges(current => {
      const remaining = current.filter(
        change => change.node !== nodeId || egressDnsSelectorKey(change.selector) !== key,
      );
      return JSON.stringify(resolution) === JSON.stringify(baseline)
        ? remaining
        : [...remaining, { node: nodeId, selector, resolution }];
    });
  };
  const activeDnsChanges = dnsChanges.filter(change =>
    (stagedRules[change.node] ?? []).some(
      rule => rule.a.t === 'egress' && egressDnsSelectorKey(rule.m) === egressDnsSelectorKey(change.selector),
    ),
  );
  const usedImportedOutbounds = importedOutbounds.filter(outbound =>
    members.some(id => (stagedRules[id] ?? []).some(rule => rule.a.t === 'proxy' && rule.a.outbound === outbound.id)),
  );

  // 冲突时拦截。upsert 的语义是存在即覆盖，放行会在无提示的情况下覆盖已有配置。
  // 字符集在此一并校验：链 id 会拼入接受凭据的 label（形如 {chain}@{node}），违反该约束
  // 要到发布前才报 label.charset，而此前配置在界面上没有异常表现。
  const badSlug = (v: string) => (isValidSlug(v) ? null : `id 只能用 [a-z0-9._-]，最长 32。`);
  const chainOwner = apps.find(a => (a.chains ?? []).some(c => c.id === chainId.trim()));
  const ingressOwner = apps.find(a => (a.ingresses ?? []).some(i => i.id === ingressId.trim()));
  const chainClash =
    badSlug(chainId.trim()) ??
    (chainOwner ? `链 ID「${chainId.trim()}」已经被线路「${chainOwner.label || chainOwner.id}」用了` : null);
  const ingressClash =
    badSlug(ingressId.trim()) ??
    (ingressOwner
      ? `接入面 ID「${ingressId.trim()}」已经被线路「${ingressOwner.label || ingressOwner.id}」用了`
      : null);
  const hopAddressIssues = edges.flatMap(edge => {
    if (dialKindOf(edge.dial, nodeOf(edge.target)) !== 'custom') return [];
    const message = customHopHostError(hostOf(edge.dial));
    return message ? [{ host: edge.target, msg: message }] : [];
  });
  /* 各监听机器分别校验：端口冲突会导致 xray 启动失败，编译时报 node.port-clash。 */
  const hopPortIssues = listeners.flatMap(host => {
    const raw = hostPortOf(host);
    const p = Number(raw);
    if (!/^\d+$/.test(raw) || p <= 0 || p > 65535) {
      return [{ host, msg: '端口必须是 1-65535' }];
    }
    const clash = portClash(taken, [host], p);
    if (clash) return [{ host, msg: clash }];
    // 链头作为反向上游时，其上同时开启接入协议和该反向端口。它们都是本次新建的，
    // `taken` 中尚不包含，上面的校验无法覆盖；需要检查所有 TCP 接入协议，而不只是 VLESS。
    const entryListener = host === head?.node_id ? entryTcpListeners.find(listener => listener.port === p) : null;
    if (entryListener) {
      return [{ host, msg: `与 ${entryListener.label} 接入口 TCP ${p} 冲突（同在 ${nameOf(host)} 上）` }];
    }
    return [];
  });
  const hopIssueOf = (host: string) => hopPortIssues.find(x => x.host === host)?.msg ?? null;
  const existingApp = apps.find(candidate => candidate.id === targetApp) ?? null;
  const sourceApp: SnapshotApp = {
    ...(existingApp ?? { id: targetApp, label: appLabel.trim(), ingresses: [], fronts: [], grants: [] }),
    chains: [
      ...(existingApp?.chains ?? []),
      { id: chainId.trim(), tenant: head?.tenant_id ?? '', name: chainName.trim() },
    ],
    steps: [
      ...(existingApp?.steps ?? []),
      ...members.map(id => ({
        chain: chainId.trim(),
        node: id,
        accept: null,
        hop_in: null,
        rules: stagedRules[id] ?? [],
      })),
    ],
  };
  const listenerApps = [...apps.filter(candidate => candidate.id !== sourceApp.id), sourceApp];
  const pathTargetIssue =
    members.flatMap(source =>
      (stagedRules[source] ?? []).flatMap(rule => {
        if (rule.a.t === 'forward') {
          const target = nodeOf(rule.a.to);
          if (!target) return [`${nameOf(source)} 的转发目标不存在`];
          if (target.retired_at) return [`${nameOf(target.node_id)} 已退役，不能作为转发目标`];
          if (!under(head?.tenant_id ?? '', target.tenant_id))
            return [`${nameOf(target.node_id)} 不在当前线路的可用范围内`];
        }
        if (rule.a.t === 'proxy') {
          const outboundId = rule.a.outbound;
          const outbound = availableOutbounds.find(candidate => candidate.id === outboundId);
          if (!outbound) return [`${nameOf(source)} 的代理出站不存在`];
          if (
            outbound.tenant !== head?.tenant_id &&
            (outbound.protocol.t === 'warp' || !under(head?.tenant_id ?? '', outbound.tenant))
          )
            return [`${outbound.name || outbound.id} 不在当前线路的可用范围内`];
          if (outbound.protocol.t === 'vpngate') {
            const eligibility = vpngateNodeEligibility(nodeOf(source) ?? undefined);
            if (!eligibility.eligible) return [`${nameOf(source)} 不能使用 VPN Gate：${eligibility.reason}`];
          }
        }
        if (rule.a.t === 'reuse_listener') {
          const listener = rule.a.listener;
          const candidate = reusableListeners({
            apps: listenerApps,
            sourceApp: sourceApp.id,
            sourceChain: chainId.trim(),
            sourceNode: source,
            sourceRules: stagedRules[source] ?? [],
            sourceDrafts: stagedRules,
            nodes: nodes.data?.nodes ?? [],
          }).find(item => item.ref.chain === listener.chain && item.ref.node === listener.node);
          if (!candidate) return [`${nameOf(source)} 引用的监听不存在`];
          if (candidate.blocked) return [`${nameOf(source)} 不能引用 ${candidate.nodeName}：${candidate.blocked}`];
          if (rule.a.dial.t === 'addr') {
            const issue = customHopHostError(rule.a.dial.v);
            if (issue) return [`${nameOf(source)} 引用 ${candidate.nodeName}：${issue}`];
          }
        }
        return [];
      }),
    )[0] ?? null;

  // ── 该链的授权对象 ──
  // 链创建后仍不可用：接入面已开启但没有任何 grant，无法建立连接。此时需要离开向导、
  // 切换到用户页逐个授权——而该过程中没有新的决策，创建链时已确定授权对象。
  // 选中的用户各生成一条 `upsert_grant`，与链和接入面进入同一批草稿。
  const users = useQuery({ queryKey: ['users'], queryFn: () => fetchUsers(true) });
  const tenants = useQuery({ queryKey: ['tenants'], queryFn: () => fetchTenants() });
  /* 键使用 `租户/用户`：用户 id 只在租户内唯一（user.dup 只在单个租户内查重）。 */
  const [pickedUsers, setPickedUsers] = useState<Set<string>>(new Set());
  const [newUser, setNewUser] = useState<{ id: string } | null>(null);
  const [userQuery, setUserQuery] = useState('');
  const guardScope = node ? `chain-wizard:node:${node.node_id}` : `chain-wizard:app:${defaultApp?.id ?? 'new'}`;
  const initialPathRules: WizardRuleTables = node ? { [node.node_id]: [wizardEgressRule()] } : {};
  const dirty =
    appModeRaw !== null ||
    appLabelRaw !== null ||
    pickedAppRaw !== null ||
    chainName !== '' ||
    headId !== (node?.node_id ?? null) ||
    JSON.stringify(pathRules) !== JSON.stringify(initialPathRules) ||
    activeDnsChanges.length > 0 ||
    paddingMode !== 'default' ||
    customPadding !== '' ||
    hy2Up !== '' ||
    hy2Down !== '' ||
    Object.keys(portEdits).length > 0 ||
    realityTargetRaw !== null ||
    encryptionEnabled !== NEW_CHAIN_PROTOCOL_DEFAULTS.vlessEncryption ||
    encryptionProfile !== 'default' ||
    vlessEnabled !== NEW_CHAIN_PROTOCOL_DEFAULTS.vless ||
    anyTlsEnabled !== NEW_CHAIN_PROTOCOL_DEFAULTS.anytls ||
    hy2Enabled !== NEW_CHAIN_PROTOCOL_DEFAULTS.hysteria2 ||
    mtprotoEnabled !== NEW_CHAIN_PROTOCOL_DEFAULTS.mtproto ||
    customRealityDest !== '' ||
    customRealityNames !== '' ||
    customRealityFingerprint !== DEFAULT_TLS_FINGERPRINT ||
    portRaw !== null ||
    encryptionPortRaw !== null ||
    anyTlsPortRaw !== null ||
    mtprotoPortRaw !== null ||
    hy2StartRaw !== null ||
    hy2EndRaw !== null ||
    pickedUsers.size > 0 ||
    Boolean(newUser?.id.trim());
  const clearUnsavedChanges = useUnsavedChanges(dirty, '新链向导', guardScope);

  // 用户可以先于入口选择；入口确定后再依据接入面租户校验授权范围
  // （validate.rs 的 tenant.scope：`under(grant.tenant, ingress.tenant)`）。
  const ingressTenant = head?.tenant_id ?? '';
  const userRows = [...(users.data?.users ?? [])]
    .sort((a, b) => a.id.localeCompare(b.id) || a.tenant_id.localeCompare(b.tenant_id))
    .map(u => ({
      ...u,
      key: `${u.tenant_id}/${u.id}`,
      // 不可选的保留在列表中并说明原因，判定和处理方式与规则编辑器的下拉框一致：
      // 直接隐藏会导致该用户从列表中消失，需要到其他位置查找。停用用户也不能在新链上
      // 获得一条看似可用的授权，否则向导完成后仍无法连接，原因却要去用户页寻找。
      blocked:
        u.status !== 'active'
          ? '该用户已停用'
          : !head || under(u.tenant_id, ingressTenant)
            ? null
            : '该用户不在当前入口的可授权范围内',
    }));

  const selectedUsers = userRows.filter(u => pickedUsers.has(u.key));
  // 实际会写入的授权。更换入口后重新计算，不清空 `pickedUsers`：更换机器可能使某个用户
  // 超出租户范围，此时不应写入；但若删除其勾选状态，切换回原机器时该选择会丢失——
  // 而反复切换是建链时的常见操作。因此保留失效的键，合法性每次实时计算。
  const grantedUsers = selectedUsers.filter(u => !u.blocked);
  /* 有用户因当前入口而不可授权时给出提示。不提示时页脚的操作条数会少于预期且无法解释。 */
  const droppedUsers = selectedUsers.filter(u => u.blocked);
  const toggleUser = (key: string) =>
    setPickedUsers(prev => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  const tenantOptions = [...(tenants.data?.tenants ?? [])].sort((a, b) => a.id.localeCompare(b.id));
  const normalizedUserQuery = userQuery.trim().toLowerCase();
  const visibleUsers = userRows.filter(
    user => !normalizedUserQuery || user.id.toLowerCase().includes(normalizedUserQuery),
  );
  const visibleSelectableUsers = visibleUsers.filter(user => !user.blocked);
  const defaultNewUserTenant =
    (head && tenantOptions.some(tenant => tenant.id === head.tenant_id) ? head.tenant_id : '') ||
    (tenantOptions.length === 1 ? tenantOptions[0].id : '');
  const newUserTenant = defaultNewUserTenant;
  const newUserId = newUser?.id.trim() ?? '';
  const newUserBadSlug = newUserId && !isValidSlug(newUserId) ? 'ID 只能用 a-z 0-9 . _ -，最长 32' : null;
  const newUserDuplicate = userRows.some(user => user.tenant_id === newUserTenant && user.id === newUserId)
    ? `用户 ID ${newUserId} 已存在`
    : null;
  const newUserReady = Boolean(newUserId && newUserTenant && !newUserBadSlug && !newUserDuplicate);
  const addUser = useMutation({
    mutationFn: (user: { tenant_id: string; id: string }) => createUser(user),
    onSuccess: (result, user) => {
      qc.setQueryData<{ users: UserListItem[] }>(['users'], current => ({
        users: [
          ...(current?.users ?? []).filter(item => !(item.tenant_id === user.tenant_id && item.id === user.id)),
          result.user,
        ],
      }));
      setPickedUsers(previous => new Set(previous).add(`${user.tenant_id}/${user.id}`));
      setNewUser(null);
      setUserQuery('');
      void qc.invalidateQueries({ queryKey: ['revisions'] });
      void qc.invalidateQueries({ queryKey: ['snapshot'] });
    },
  });

  const targeted = new Set(edges.map(edge => edge.target));
  const stepBodies: Array<{ id: string; body: Parameters<typeof putStep>[3] }> = members.map(id => {
    const sec = hostSecOf(id);
    const hopIn: HopInRequest | undefined = listeners.includes(id)
      ? {
          port: Number(hostPortOf(id)) || hopBase,
          security:
            sec === 'reality'
              ? { t: 'reality', v: { dest: realitySite.dest, server_names: realitySite.names } }
              : { t: sec },
        }
      : undefined;
    return {
      id,
      body: {
        rules: stagedRules[id] ?? [],
        ...(targeted.has(id) ? { accept: {} } : {}),
        ...(hopIn ? { hop_in: hopIn } : {}),
      },
    };
  });

  // 将写入草稿的操作列表。该列表既用于页脚展示，也是提交时实际执行的内容——
  // 分两处实现会导致预览显示三条而实际写入四条，且该偏差没有任何提示。
  const ops = (() => {
    const list: { op: string; arg: string }[] = [];
    if (appMode === 'new') list.push({ op: 'upsert_app', arg: `${targetApp}「${appLabel.trim() || targetApp}」` });
    list.push({
      op: 'upsert_chain',
      arg: `${chainId.trim()}「${chainName.trim() || '未命名'}」`,
    });
    const protocolNames = [
      vlessEnabled && 'VLESS · TLS/REALITY',
      encryptionEnabled && 'VLESS · Encryption',
      anyTlsEnabled && 'AnyTLS',
      hy2Enabled && 'Hysteria 2',
      mtprotoEnabled && 'MTProxy',
    ].filter(Boolean);
    list.push({
      op: 'upsert_ingress',
      arg: `${ingressId.trim()} → ${headLabel} · ${protocolNames.join(' + ')}${
        vlessEnabled
          ? ` · REALITY（${
              realityTarget === 'node-certificate'
                ? headCertificateLabel
                : realityTarget === 'global-site'
                  ? '全局站点'
                  : realityTarget === 'custom-site'
                    ? '自定义站点'
                    : '未选择目标'
            }）`
          : ''
      }`,
    });
    for (const outbound of usedImportedOutbounds)
      list.push({ op: 'upsert_external_outbound', arg: `${outbound.name}（${outbound.id}）` });
    for (const step of stepBodies) {
      const summary = step.body.rules.map(rule => {
        const match =
          rule.m.t === 'any' ? '任意' : `${rule.m.t}${matchValues(rule.m) ? `=${matchValues(rule.m)}` : ''}`;
        const action =
          rule.a.t === 'forward'
            ? `转发 ${nameOf(rule.a.to)}`
            : rule.a.t === 'proxy'
              ? `代理出站 ${rule.a.outbound}`
              : rule.a.t === 'egress'
                ? '从本机出网'
                : '拒绝';
        return `${match} → ${action}`;
      });
      list.push({ op: 'put_step', arg: `${nameOf(step.id)}：${summary.join('；')}` });
    }
    for (const change of activeDnsChanges) {
      list.push({
        op: 'set_node_egress_dns',
        arg: `${nameOf(change.node)}：${change.selector.t}${matchValues(change.selector) ? `=${matchValues(change.selector)}` : ''} → ${change.resolution ? `自定义 DNS ${egressDnsEndpointText(change.resolution)}` : '默认 DNS'}`,
      });
    }
    /* 授权排在最后：grant 引用接入面，接入面需要先创建。草稿按顺序回放。 */
    for (const u of grantedUsers) {
      list.push({
        op: 'upsert_grant',
        arg: `${u.id} → ${ingressId.trim()}`,
      });
    }
    return list;
  })();

  const submit = async () => {
    setError(null);
    setSubmitting(true);
    try {
      const app = targetApp;
      if (appMode === 'new') await createApp({ id: app, label: appLabel.trim() || app });
      await createChain(app, {
        id: chainId.trim(),
        tenant_id: head?.tenant_id ?? '',
        name: chainName.trim(),
        subscription_country: exitCountry,
      });
      await createIngress(app, {
        id: ingressId.trim(),
        chain_id: chainId.trim(),
        node_id: head?.node_id ?? '',
        bind: bind.trim(),
        port,
        reality:
          realityTarget === 'custom-site'
            ? {
                fallback_mode: realityTarget,
                dest: customRealityDest.trim(),
                server_names: customRealityServerNames,
                fingerprint: customRealityFingerprint,
                fallback_limits: newRealityFallbackLimits(),
                fallback_guard: true,
              }
            : {
                fallback_mode: (realityTarget || 'global-site') as Exclude<RealityFallbackMode, 'custom-site'>,
                fallback_limits: newRealityFallbackLimits(),
                fallback_guard: true,
              },
        wires,
        projection: {
          ...(wires.vless_encryption ? { vless_encryption: {} } : {}),
          ...(wires.anytls ? { anytls: {} } : {}),
          ...(wires.hysteria2 ? { hysteria2: {} } : {}),
          ...(wires.mtproto ? { mtproto: {} } : {}),
        },
        guard: {
          no_private: true,
          no_bittorrent: true,
          no_mail: true,
          no_udp_amplification: true,
          tcp_and_quic_only: false,
          cn_source_block: 'off',
        },
      });

      for (const outbound of usedImportedOutbounds)
        await upsertExternalOutbound({
          id: outbound.id,
          tenant_id: outbound.tenant,
          name: outbound.name,
          address: outbound.address,
          port: outbound.port,
          protocol: outbound.protocol,
          security: outbound.security,
        });

      // 目标凭据、反向监听和规则全部来自同一份 stepBodies；与页脚预览逐项对应。
      for (const step of stepBodies) await putStep(app, chainId.trim(), step.id, step.body);
      // DNS 是机器级策略，与链规则同批草稿，但不归新链所有。
      for (const change of activeDnsChanges) await setNodeEgressDns(change.node, change.selector, change.resolution);

      // 授权最后写入：它引用接入面，前面的 upsert_ingress 需要先进入草稿。
      // 顺序与页脚列出的一致——两处不一致会使预览内容与实际执行不符。
      for (const u of grantedUsers) {
        stageGrant({
          app_id: app,
          tenant_id: u.tenant_id,
          user_id: u.id,
          ingress_id: ingressId.trim(),
          enabled: true,
        });
      }

      qc.invalidateQueries({ queryKey: ['revisions'] });
      qc.invalidateQueries({ queryKey: ['snapshot'] });
      qc.invalidateQueries({ queryKey: ['nodes'] });
      // 成功后回到进入向导前的上下文。草稿条已经承担待提交状态，不再停留展示一份
      // 与提交前预览重复的“改了什么”结果页。
      clearUnsavedChanges();
      onDone({ appId: app, chainId: chainId.trim(), ingressId: ingressId.trim() });
    } catch (e) {
      setError(e);
    } finally {
      setSubmitting(false);
    }
  };

  // 向导的默认端口、可选机器、线路冲突、授权对象和 REALITY 站点分别来自这些查询。
  // 任何一项失败都不能以空数组/工厂默认值继续，否则“创建成功”的结果可能少授权、撞端口
  // 或写入错误的站点。
  // nodes 与上线引导共用缓存，后者每 3 秒轮询运行状态；已有机器数据时后台读取不撤销
  // 表单的就绪状态，否则整张表单会随轮询反复禁用。首次读取和刷新失败仍阻止创建。
  const dependenciesPending =
    snapshot.isPending ||
    snapshot.isFetching ||
    settings.isPending ||
    settings.isFetching ||
    nodes.isPending ||
    revisions.isPending ||
    revisions.isFetching ||
    users.isPending ||
    users.isFetching ||
    tenants.isPending ||
    tenants.isFetching ||
    (current != null && (compile.isPending || compile.isFetching || compile.isPlaceholderData));
  const dependencyError =
    snapshot.error ?? settings.error ?? nodes.error ?? revisions.error ?? users.error ?? tenants.error ?? compile.error;
  const dependenciesReady = !dependenciesPending && !dependencyError;
  const initialReady = useInitialReady(Boolean(dependenciesReady), guardScope);
  if (!initialReady) return dependencyError ? <ErrorBox error={dependencyError} /> : <Loading variant="form" />;

  const blockers = [
    dependenciesPending ? '等待最新配置加载完成' : null,
    dependencyError ? '配置读取失败，请重试' : null,
    !head ? '选择入口节点' : null,
    !targetApp ? '没有可用的分组' : null,
    appMode === 'new' && !appLabel.trim() ? '填写新分组名称' : null,
    !chainName.trim() ? '填写链名称' : null,
    chainClash,
    ingressClash,
    enabledProtocolCount === 0 ? '至少开启一个接入协议' : null,
    mtprotoEnabled ? mtprotoUnavailable : null,
    anyTlsEnabled && paddingMode === 'custom' && !customPadding.trim() ? '填写 AnyTLS Padding 规则' : null,
    hy2Enabled && Boolean(hy2Up.trim()) !== Boolean(hy2Down.trim()) ? 'Hysteria 2 上下行带宽需同时填写' : null,
    entryPortCollision,
    entryPortIssues[0] ?? null,
    anyTlsEnabled || hy2Enabled ? (!headCertificate ? `先为入口节点分配${headCertificateLabel}` : null) : null,
    !realityTargetReady ? '补全 VLESS · TLS/REALITY 伪装目标' : null,
    wizardRuleIssue(headId, stagedRules),
    activeDnsChanges.find(change => change.resolution && !!egressDnsIssue(change.resolution))
      ? '补全自定义 DNS 地址和端口'
      : null,
    pathTargetIssue,
    members.find(
      id =>
        (stagedRules[id] ?? []).some(rule => rule.a.t === 'egress') &&
        snapshot.data?.snapshot.nodes?.find(candidate => candidate.id === id)?.egress_allowed === false,
    )
      ? '路径中有不允许出网的机器，请改为转发或拒绝'
      : null,
    hopAddressIssues[0]?.msg ?? null,
    hopPortIssues[0]?.msg ?? null,
    listeners.some(host => hostSecOf(host) === 'reality') && !globalRealityReady
      ? '中转协议使用 REALITY 前，先配置全局伪装站点'
      : null,
    addUser.isPending ? '等待新增用户完成' : null,
  ].filter((message): message is string => !!message);
  const ready = blockers.length === 0;

  return (
    <WizardPaper
      onSubmit={e => {
        e.preventDefault();
        setAttempted(true);
        if (!ready || submitting) return;
        void submit();
      }}
    >
      <WizardPaperHeader
        title="新建链"
        icon="chains"
        meta={[
          chainName.trim() || '未命名',
          appMode === 'new' ? appLabel.trim() : existingApp?.label || existingApp?.id,
        ]
          .filter(Boolean)
          .join(' · ')}
        stages={[
          { label: '配置链', state: 'current' },
          { label: '提交草稿', state: 'next' },
          { label: '预览发布', state: 'next' },
        ]}
        aside={<span className="st st-gold">写入草稿</span>}
      />
      <div className="nd-paper-body pv-body">
        <fieldset className="pv-fields pv-chain-fields" disabled={submitting || !dependenciesReady}>
          <div className="nd-tab-config">
            <div>
              <WizardCard title="基本信息" icon="identity" className="cc-basic">
                <div className="fgrid one">
                  <WizardField label="分组" htmlFor="chain-wizard-app">
                    <select
                      id="chain-wizard-app"
                      className="f"
                      value={appMode === 'new' ? NEW_APP : pickedApp}
                      onChange={event => {
                        if (event.target.value === NEW_APP) setAppMode('new');
                        else {
                          setAppMode('existing');
                          setPickedApp(event.target.value);
                        }
                      }}
                    >
                      {apps.map(app => (
                        <option key={app.id} value={app.id}>
                          {app.label || app.id}
                        </option>
                      ))}
                      <option value={NEW_APP} disabled={!system}>
                        ＋ 新建分组…
                      </option>
                    </select>
                  </WizardField>
                  {appMode === 'new' && (
                    <WizardField label="分组名称" htmlFor="chain-wizard-app-name">
                      <input
                        id="chain-wizard-app-name"
                        className="f"
                        value={appLabel}
                        onChange={event => setAppLabel(event.target.value)}
                        placeholder="给分组起个名字"
                        aria-label="新分组名称"
                        aria-invalid={attempted && !appLabel.trim()}
                      />
                    </WizardField>
                  )}
                  <WizardField label="链名称" htmlFor="chain-wizard-name">
                    <input
                      id="chain-wizard-name"
                      className="f"
                      value={chainName}
                      placeholder="给这条链起个名字"
                      onChange={event => setChainName(event.target.value)}
                      aria-invalid={attempted && !chainName.trim()}
                    />
                  </WizardField>
                </div>
              </WizardCard>
              <WizardCard title="授权用户" icon="users" hint={`${grantedUsers.length} 人已选`}>
                <div className="toolbar cc-users-tools">
                  <span className="user-list-search">
                    <Icon of="search" size={13} className="user-list-search-icon" />
                    <input
                      type="search"
                      value={userQuery}
                      aria-label="搜索授权用户"
                      placeholder="搜索用户"
                      autoComplete="off"
                      spellCheck={false}
                      onChange={event => setUserQuery(event.target.value)}
                    />
                    {userQuery && (
                      <button type="button" aria-label="清空搜索" title="清空" onClick={() => setUserQuery('')}>
                        <Icon of="close" size={13} />
                      </button>
                    )}
                  </span>
                  <button
                    type="button"
                    className="btn sm"
                    disabled={
                      !visibleSelectableUsers.length || visibleSelectableUsers.every(user => pickedUsers.has(user.key))
                    }
                    onClick={() =>
                      setPickedUsers(
                        previous => new Set([...previous, ...visibleSelectableUsers.map(user => user.key)]),
                      )
                    }
                  >
                    全选
                  </button>
                  <button
                    type="button"
                    className="btn sm"
                    disabled={!pickedUsers.size}
                    onClick={() => setPickedUsers(new Set())}
                  >
                    清空
                  </button>
                  <button
                    type="button"
                    className="btn sm"
                    aria-label="新增用户"
                    aria-expanded={Boolean(newUser)}
                    disabled={addUser.isPending}
                    onClick={() => {
                      addUser.reset();
                      setNewUser(current => (current ? null : { id: '' }));
                    }}
                  >
                    ＋ 新增用户
                  </button>
                </div>
                {newUser && (
                  <div
                    className="user-new-row"
                    role="group"
                    aria-label="新增用户"
                    aria-busy={addUser.isPending}
                    onKeyDown={event => {
                      if (event.key === 'Enter') {
                        event.preventDefault();
                        event.stopPropagation();
                        if (newUserReady && !addUser.isPending)
                          addUser.mutate({ tenant_id: newUserTenant, id: newUserId });
                      }
                      if (event.key === 'Escape') {
                        event.preventDefault();
                        event.stopPropagation();
                        if (!addUser.isPending) {
                          addUser.reset();
                          setNewUser(null);
                        }
                      }
                    }}
                  >
                    <span className="user-new-intro">
                      <span className="user-avatar user-new-avatar" aria-hidden="true">
                        ＋
                      </span>
                      <span className="user-new-copy">
                        <b>新增用户</b>
                        <small>用户会立即创建；链与授权在「加入草稿」后暂存。</small>
                      </span>
                    </span>
                    <label className="user-new-field">
                      <span>用户 ID</span>
                      <input
                        className="f mono"
                        autoFocus
                        aria-label="新用户 ID"
                        value={newUser.id}
                        placeholder="例如 alice"
                        disabled={addUser.isPending}
                        onChange={event => setNewUser({ ...newUser, id: event.target.value })}
                      />
                    </label>
                    {(newUserBadSlug || newUserDuplicate || !newUserTenant) && (
                      <p className="user-new-error" role="alert">
                        {newUserBadSlug || newUserDuplicate || '当前配置下无法新增用户，请先选择入口机器。'}
                      </p>
                    )}
                    {addUser.error && <ErrorBox error={addUser.error} />}
                    <footer className="user-new-actions">
                      <button
                        type="button"
                        className="btn"
                        disabled={addUser.isPending}
                        onClick={() => {
                          addUser.reset();
                          setNewUser(null);
                        }}
                      >
                        取消
                      </button>
                      <button
                        type="button"
                        className="btn primary"
                        disabled={!newUserReady || addUser.isPending}
                        onClick={() => addUser.mutate({ tenant_id: newUserTenant, id: newUserId })}
                      >
                        {addUser.isPending ? '新增中…' : '新增并选中'}
                      </button>
                    </footer>
                  </div>
                )}
                <div className="cc-user-list" role="group" aria-label="选择可用用户">
                  <div className="cc-user-grid">
                    {visibleUsers.map(user => {
                      const selected = pickedUsers.has(user.key);
                      return (
                        <button
                          type="button"
                          key={user.key}
                          className="grant-card cc-user"
                          aria-pressed={selected}
                          disabled={Boolean(user.blocked) && !selected}
                          title={`${user.id}${user.blocked ? ` · ${user.blocked}` : ''}`}
                          onClick={() => toggleUser(user.key)}
                        >
                          <GeneratedUserAvatar id={user.id} variant="cell" />
                          <span className="cc-user-copy">
                            <span className="gc-app">{user.id}</span>
                            {user.blocked && (
                              <span className="gc-at">{user.status !== 'active' ? '已停用' : '不可授权'}</span>
                            )}
                          </span>
                          <span className="gc-mark" aria-hidden="true">
                            {selected ? '✓' : ''}
                          </span>
                        </button>
                      );
                    })}
                  </div>
                  {!visibleUsers.length && (
                    <p className="note">{normalizedUserQuery ? '没有匹配的用户' : '还没有用户，可以在这里新增。'}</p>
                  )}
                </div>
                {droppedUsers.length > 0 && (
                  <p className="note warn">
                    {droppedUsers.map(user => user.id).join('、')} 当前不可授权，本次不会写入。
                  </p>
                )}
              </WizardCard>
            </div>
            <div>
              <WizardCard title="接入协议" icon="ingress" hint={`${enabledProtocolCount} 种 · 端口自动避让`}>
                <div className="wzp" aria-label="接入协议">
                  <WizardProtocolTile
                    name="VLESS · TLS/REALITY"
                    issues={protocolIssues.vless}
                    note={INGRESS_PROTOCOL_DESCRIPTIONS.vless}
                    icon="xray"
                    enabled={vlessEnabled}
                    onToggle={setVlessEnabled}
                    port={
                      <>
                        <span>TCP</span>
                        <WizardPortInput
                          label="VLESS 监听端口"
                          value={portText}
                          disabled={!vlessEnabled}
                          invalid={tcpIssuesFor('VLESS · TLS/REALITY').length > 0}
                          onChange={setPort}
                        />
                        {vlessEnabled && portRaw !== null && (
                          <WizardPortAuto label="VLESS 监听端口" onReset={() => setPort(null)} />
                        )}
                      </>
                    }
                    params={
                      <div className="fgrid one">
                        <WizardField label="伪装目标" htmlFor="chain-wizard-reality">
                          <select
                            id="chain-wizard-reality"
                            className="f"
                            aria-label="REALITY 目标来源"
                            value={realityTarget}
                            onChange={event => setRealityTarget(event.target.value as RealityFallbackMode | '')}
                          >
                            <option value="">— 选择 REALITY 目标 —</option>
                            <option value="node-certificate" disabled={!headCertificate}>
                              {headCertificate
                                ? `${headCertificateLabel} ${headCertificate}`
                                : `${headCertificateLabel}（尚未签发）`}
                            </option>
                            <option value="global-site" disabled={!globalRealityReady}>
                              {globalRealityReady ? `全局站点 ${realitySite.dest}` : '全局站点（尚未配置）'}
                            </option>
                            <option value="custom-site">自定义站点</option>
                          </select>
                          {realityTarget === 'node-certificate' && headCertificate && (
                            <span className="sub">
                              客户端 SNI 使用 <span className="mono">{headCertificate}</span>
                            </span>
                          )}
                          {realityTarget === 'global-site' && (
                            <span className="sub">
                              <span className="mono">{realitySite.dest}</span> · SNI{' '}
                              <span className="mono">{realitySite.names.join(' · ')}</span>
                            </span>
                          )}
                        </WizardField>
                        {realityTarget === 'custom-site' && (
                          <>
                            <WizardField label="目标地址" htmlFor="chain-wizard-reality-dest">
                              <input
                                id="chain-wizard-reality-dest"
                                className="f mono"
                                value={customRealityDest}
                                placeholder="example.com:443"
                                aria-label="自定义 REALITY 目标"
                                onChange={event => setCustomRealityDest(event.target.value)}
                              />
                            </WizardField>
                            <WizardField label="SNI" htmlFor="chain-wizard-reality-names">
                              <input
                                id="chain-wizard-reality-names"
                                className="f mono"
                                value={customRealityNames}
                                placeholder="example.com"
                                aria-label="自定义 REALITY SNI"
                                onChange={event => setCustomRealityNames(event.target.value)}
                              />
                            </WizardField>
                            <WizardField label="指纹" htmlFor="chain-wizard-reality-fingerprint">
                              <select
                                id="chain-wizard-reality-fingerprint"
                                className="f"
                                aria-label="自定义 REALITY 指纹"
                                value={customRealityFingerprint}
                                onChange={event => setCustomRealityFingerprint(event.target.value)}
                              >
                                {REALITY_FINGERPRINT_OPTIONS.map(([value, label]) => (
                                  <option value={value} key={value}>
                                    {label}
                                  </option>
                                ))}
                              </select>
                            </WizardField>
                          </>
                        )}
                      </div>
                    }
                    expanded={openProtocol === 'vless' || !realityTargetReady}
                    onExpand={() => setOpenProtocol(openProtocol === 'vless' ? null : 'vless')}
                  />
                  <WizardProtocolTile
                    name="VLESS · Encryption"
                    issues={protocolIssues.encryption}
                    note={INGRESS_PROTOCOL_DESCRIPTIONS.encryption}
                    icon="xray"
                    enabled={encryptionEnabled}
                    onToggle={setEncryptionEnabled}
                    port={
                      <>
                        <span>TCP</span>
                        <WizardPortInput
                          label="VLESS Encryption 监听端口"
                          value={encryptionPortText}
                          disabled={!encryptionEnabled}
                          invalid={protocolIssues.encryption.length > 0}
                          onChange={setEncryptionPort}
                        />
                        {encryptionEnabled && encryptionPortRaw !== null && (
                          <WizardPortAuto label="VLESS Encryption 监听端口" onReset={() => setEncryptionPort(null)} />
                        )}
                      </>
                    }
                    params={
                      <div className="fgrid one">
                        <WizardField label="握手档位" htmlFor="chain-wizard-encryption-profile">
                          <select
                            id="chain-wizard-encryption-profile"
                            className="f"
                            value={encryptionProfile}
                            onChange={event => setEncryptionProfile(event.target.value as 'default' | 'native')}
                          >
                            <option value="default">默认 · random</option>
                            <option value="native">native</option>
                          </select>
                          <span className="sub">
                            会话票据有效期 <span className="mono">600s</span>
                          </span>
                        </WizardField>
                      </div>
                    }
                    expanded={openProtocol === 'encryption'}
                    onExpand={() => setOpenProtocol(openProtocol === 'encryption' ? null : 'encryption')}
                  />
                  <WizardProtocolTile
                    name="AnyTLS"
                    issues={protocolIssues.anytls}
                    note={INGRESS_PROTOCOL_DESCRIPTIONS.anytls}
                    icon="bolt"
                    enabled={anyTlsEnabled}
                    onToggle={setAnyTlsEnabled}
                    port={
                      <>
                        <span>TCP</span>
                        <WizardPortInput
                          label="AnyTLS 监听端口"
                          value={anyTlsPortText}
                          disabled={!anyTlsEnabled}
                          invalid={tcpIssuesFor('AnyTLS').length > 0}
                          onChange={setAnyTlsPort}
                        />
                        {anyTlsEnabled && anyTlsPortRaw !== null && (
                          <WizardPortAuto label="AnyTLS 监听端口" onReset={() => setAnyTlsPort(null)} />
                        )}
                      </>
                    }
                    params={
                      <div className="fgrid one">
                        <WizardField label="Padding" htmlFor="chain-wizard-padding">
                          <select
                            id="chain-wizard-padding"
                            className="f"
                            value={paddingMode}
                            onChange={event => setPaddingMode(event.target.value as 'default' | 'custom')}
                          >
                            <option value="default">默认方案</option>
                            <option value="custom">自定义…</option>
                          </select>
                          {paddingMode === 'custom' && (
                            <textarea
                              className="f wzp-padding-custom"
                              aria-label="AnyTLS 自定义 Padding"
                              value={customPadding}
                              placeholder="每行一条 Padding 规则"
                              onChange={event => setCustomPadding(event.target.value)}
                            />
                          )}
                        </WizardField>
                        {certificateField}
                      </div>
                    }
                    expanded={openProtocol === 'anytls'}
                    onExpand={() => setOpenProtocol(openProtocol === 'anytls' ? null : 'anytls')}
                  />
                  <WizardProtocolTile
                    name="Hysteria 2"
                    issues={protocolIssues.hy2}
                    portRange
                    note={INGRESS_PROTOCOL_DESCRIPTIONS.hy2}
                    icon="hysteria"
                    enabled={hy2Enabled}
                    onToggle={setHy2Enabled}
                    port={
                      <>
                        <span>UDP</span>
                        <WizardPortInput
                          label="Hysteria 2 起始端口"
                          value={hy2StartText}
                          disabled={!hy2Enabled}
                          invalid={entryPortIssues.some(issue => issue.startsWith('Hysteria 2'))}
                          onChange={setHy2Start}
                        />
                        <span>–</span>
                        <WizardPortInput
                          label="Hysteria 2 结束端口"
                          value={hy2EndText}
                          disabled={!hy2Enabled}
                          invalid={entryPortIssues.some(issue => issue.startsWith('Hysteria 2'))}
                          onChange={setHy2End}
                        />
                        {hy2Enabled && (hy2StartRaw !== null || hy2EndRaw !== null) && (
                          <WizardPortAuto
                            label="Hysteria 2 端口范围"
                            onReset={() => {
                              setHy2Start(null);
                              setHy2End(null);
                            }}
                          />
                        )}
                      </>
                    }
                    params={
                      <div className="fgrid one">
                        <WizardField label="上行带宽" htmlFor="chain-wizard-hy2-up">
                          <input
                            id="chain-wizard-hy2-up"
                            className="f mono"
                            aria-label="Hysteria 2 上行带宽"
                            placeholder="留空用 BBR"
                            value={hy2Up}
                            onChange={event => setHy2Up(event.target.value)}
                          />
                        </WizardField>
                        <WizardField label="下行带宽" htmlFor="chain-wizard-hy2-down">
                          <input
                            id="chain-wizard-hy2-down"
                            className="f mono"
                            aria-label="Hysteria 2 下行带宽"
                            placeholder="留空用 BBR"
                            value={hy2Down}
                            onChange={event => setHy2Down(event.target.value)}
                          />
                          <span className="sub">
                            留空使用 BBR；固定带宽需成对填写，如 <span className="mono">200 mbps</span>。
                          </span>
                        </WizardField>
                        {certificateField}
                      </div>
                    }
                    expanded={openProtocol === 'hy2'}
                    onExpand={() => setOpenProtocol(openProtocol === 'hy2' ? null : 'hy2')}
                  />
                  <WizardProtocolTile
                    name="MTProxy"
                    issues={protocolIssues.mtproto}
                    note={INGRESS_PROTOCOL_DESCRIPTIONS.mtproto}
                    icon="telegram"
                    enabled={mtprotoEnabled}
                    onToggle={setMtProtoEnabled}
                    port={
                      <>
                        <span>TCP</span>
                        <WizardPortInput
                          label="MTProxy 监听端口"
                          value={mtprotoPortText}
                          disabled={!mtprotoEnabled}
                          invalid={tcpIssuesFor('MTProxy').length > 0}
                          onChange={setMtProtoPort}
                        />
                        {mtprotoEnabled && mtprotoPortRaw !== null && (
                          <WizardPortAuto label="MTProxy 监听端口" onReset={() => setMtProtoPort(null)} />
                        )}
                      </>
                    }
                  />
                </div>
                {enabledProtocolCount === 0 && (
                  <p className="note bad" role="alert">
                    至少开启一个接入协议。
                  </p>
                )}
              </WizardCard>
            </div>
          </div>
          <WizardCard
            title="路径"
            icon="chains"
            hint={
              !headId
                ? '待选入口'
                : `${spine.length} 台 · ${spine.length === 1 ? '直出' : `${spine.length - 1} 跳`}${
                    exceptionCount ? ` · ${exceptionCount} 条例外` : ''
                  }`
            }
          >
            <WizardPathEditor
              root={headId}
              tables={pathRules}
              onRootChange={setHeadId}
              onTablesChange={setPathRules}
              fixedHead={!!node}
              nodes={nodes.data?.nodes ?? []}
              app={apps.find(candidate => candidate.id === targetApp) ?? null}
              apps={apps}
              chainId={chainId.trim()}
              tenant={head?.tenant_id ?? ''}
              outbounds={availableOutbounds}
              onOutboundCreated={outbound =>
                setImportedOutbounds(current => [...current.filter(item => item.id !== outbound.id), outbound])
              }
              taken={taken}
              hopBase={hopBase}
              portOf={hostPortOf}
              wireOf={hostSecOf}
              onPortChange={(host, value) => patchPort(host, { port: value })}
              onWireChange={(host, value) => patchPort(host, { sec: value })}
              portIssueOf={hopIssueOf}
              egressAllowed={id =>
                snapshot.data?.snapshot.nodes?.find(candidate => candidate.id === id)?.egress_allowed !== false
              }
              globalRealityReady={globalRealityReady}
              realitySite={realitySite}
              entryProtocolCount={enabledProtocolCount}
              dnsPolicies={dnsPolicies}
              dnsChanges={activeDnsChanges}
              onDnsChange={patchDns}
            />
          </WizardCard>
          {showOps && (
            <WizardCard title="配置预览" icon="artifacts">
              <ul className="wz-ops" id="chain-wizard-ops">
                {ops.map((o, i) => (
                  <li key={i}>
                    <span className="op">{o.op}</span>
                    <span className="arg">{o.arg}</span>
                  </li>
                ))}
              </ul>
            </WizardCard>
          )}
        </fieldset>
        {(chainClash || ingressClash) && <div className="callout err">{chainClash || ingressClash}</div>}
        {dependencyError && (
          <>
            <ErrorBox error={dependencyError} />
            <div className="toolbar">
              <button
                type="button"
                className="btn sm"
                onClick={() => {
                  for (const query of [snapshot, settings, nodes, revisions, users, tenants]) void query.refetch();
                  if (current != null) void compile.refetch();
                }}
              >
                重试读取配置
              </button>
            </div>
          </>
        )}
        {error != null && <ErrorBox error={error} />}
      </div>
      <WizardFooter
        id="chain-wizard-submit-note"
        tone={submitting ? 'busy' : ready ? 'ready' : 'idle'}
        title={submitting ? '加入草稿中…' : ready ? '配置完整' : blockers[0]}
        description={null}
      >
        {head && (
          <button
            type="button"
            className="btn"
            aria-expanded={showOps}
            aria-controls="chain-wizard-ops"
            onClick={() => setShowOps(v => !v)}
          >
            配置预览
          </button>
        )}
        <button type="button" className="btn" disabled={submitting} onClick={() => onDone()}>
          取消
        </button>
        <button
          className="btn primary"
          disabled={!ready || submitting}
          type="submit"
          aria-describedby="chain-wizard-submit-note"
          aria-busy={submitting}
        >
          {submitting ? '加入中…' : '加入草稿'}
        </button>
      </WizardFooter>
    </WizardPaper>
  );
}
