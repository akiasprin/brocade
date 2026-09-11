import {
  Fragment,
  createContext,
  useContext,
  useEffect,
  useId,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from 'react';
import { createPortal } from 'react-dom';
import { HOP_WIRE_OPTIONS, hopWireLabel } from '../ui/format';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  fetchCompileView,
  DEFAULT_HOP_MUX,
  chainMembers,
  fetchNodes,
  fetchRevisions,
  fetchSettings,
  fetchSnapshot,
  hopMuxError,
  pruneChain,
  putStep,
  reorderNodeEgressDns,
  setNodeEgressDns,
  upsertExternalOutbound,
  deleteExternalOutbound,
  type DestMatch,
  type EgressDnsResolution,
  type ExternalOutbound,
  type ExternalOutboundProtocol,
  type ExternalOutboundSecurity,
  type ExternalVlessTransport,
  type HopDial,
  type HopInRequest,
  type HopMux,
  type HopPool,
  type ListenerDial,
  type ListenerRef,
  type SnapshotStep,
  type SnapshotApp,
  type Rule,
  type RuleAction,
  type StepAccept,
  type XhttpMode,
} from '../api';
import { ErrorBox, Loading } from '../ui/bits';
import { Icon, PanelTitle } from '../ui/icons';
import { freePortAcross, occupiedPorts, type PortOwners } from './ports';
import { externalImportCanSave, serverNameAfterAddressChange, vlessEncryptionIsValid } from '../external-outbound';
import {
  REALITY_FINGERPRINT_OPTIONS,
  realityFingerprintIsValid,
  realityPublicKeyIsValid,
  realityServerNameIsValid,
  realityShortIdIsValid,
} from '../reality';
import { navigate } from '../forge/route';

type RelayRuleAction = Extract<RuleAction, { t: 'forward' | 'reuse_listener' }>;

type TargetMenuPlacement = {
  below: boolean;
  left: number;
  top: number;
  width: number;
  maxHeight: number;
};

type TargetPickerView = { t: 'targets' } | { t: 'listener-chains' } | { t: 'listener-endpoints'; chain: string };

const TARGET_MENU_EDGE = 10;
const TARGET_MENU_GAP = 5;
const TARGET_MENU_MAX_HEIGHT = 520;
const TARGET_MENU_MIN_HEIGHT = 80;
const TARGET_MENU_WIDTH = 430;

function placeTargetMenu(anchor: DOMRect, preferredBelow?: boolean, wantedHeight = TARGET_MENU_MAX_HEIGHT) {
  const viewport = window.visualViewport;
  const viewportTop = viewport?.offsetTop ?? 0;
  const viewportLeft = viewport?.offsetLeft ?? 0;
  const viewportHeight = viewport?.height ?? window.innerHeight;
  const viewportWidth = viewport?.width ?? window.innerWidth;
  const viewportBottom = viewportTop + viewportHeight;
  const viewportRight = viewportLeft + viewportWidth;
  const above = Math.max(0, anchor.top - viewportTop - TARGET_MENU_EDGE - TARGET_MENU_GAP);
  const below = Math.max(0, viewportBottom - anchor.bottom - TARGET_MENU_EDGE - TARGET_MENU_GAP);
  let openBelow = preferredBelow ?? (below >= 320 || below >= above);
  const chosen = openBelow ? below : above;
  const opposite = openBelow ? above : below;
  if (preferredBelow !== undefined && chosen < TARGET_MENU_MIN_HEIGHT && opposite > chosen) openBelow = !openBelow;

  const availableHeight = openBelow ? below : above;
  const maxHeight = Math.max(1, Math.min(wantedHeight, Math.floor(availableHeight)));
  const availableWidth = Math.max(1, Math.floor(viewportWidth - TARGET_MENU_EDGE * 2));
  const width = Math.min(TARGET_MENU_WIDTH, availableWidth);
  const minLeft = viewportLeft + TARGET_MENU_EDGE;
  const maxLeft = Math.max(minLeft, viewportRight - TARGET_MENU_EDGE - width);
  const left = Math.min(Math.max(anchor.left, minLeft), maxLeft);
  const top = openBelow ? anchor.bottom + TARGET_MENU_GAP : anchor.top - TARGET_MENU_GAP - maxHeight;

  return { below: openBelow, left, top, width, maxHeight } satisfies TargetMenuPlacement;
}

const isRelayAction = (a: RuleAction): a is RelayRuleAction => a.t === 'forward' || a.t === 'reuse_listener';

const forwardDial = (a: RuleAction): HopDial => (a.t === 'forward' ? a.dial : { t: 'overlay' });

const forwardPool = (a: RuleAction): HopPool => (isRelayAction(a) ? a.pool : { t: 'none' });

export type PoolChoice = 'none' | 'mux';
export const POOL_ORDER: PoolChoice[] = ['none', 'mux'];
export const POOL_LABEL: Record<PoolChoice, string> = {
  none: '每次新建',
  mux: 'Mux 复用',
};
/* 新建一跳时默认每条流单独建立连接。Mux.cool 的 concurrency=1 会复用未经借出前
   探活的空闲 worker，半失效连接可能一直卡到连接超时。 */
export const POOL_DEFAULT: HopPool = { t: 'none' };

/* 新建转发规则的动作，四个建链入口共用同一份。分散定义时增加一档默认值需要修改四处，
   遗漏的一处不会报错，只是行为与其他位置不同。
   反向档固定为 none：本机不发起可复用的出站连接。 */
export const forwardAction = (to: string, dial: HopDial, pool: HopPool = POOL_DEFAULT): RuleAction => ({
  t: 'forward',
  to,
  dial,
  pool: dial.t === 'reverse' ? POOL_DEFAULT : pool,
});

export const reuseListenerAction = (
  listener: ListenerRef,
  dial: ListenerDial,
  pool: HopPool = POOL_DEFAULT,
): RuleAction => ({ t: 'reuse_listener', listener, dial, pool });

// Mux.cool 的 concurrency。1 表示每条连接同时承载一条流，仍可复用空闲连接；
// 大于 1 时多条流共享连接。界面不再把这两种数值拆成不同连接类型。
export const MUX_DEFAULT = 1;
export const MUX_MIN = 1;
export const MUX_MAX = 128;
export const poolChoice = (pool: HopPool): PoolChoice => (pool.t === 'none' ? 'none' : 'mux');
export const muxConcurrency = (pool: HopPool): number =>
  pool.t === 'mux' ? (pool.v?.concurrency ?? MUX_DEFAULT) : MUX_DEFAULT;
export const poolFromMuxConcurrency = (value: number): HopPool => ({
  t: 'mux',
  v: { ...DEFAULT_HOP_MUX, concurrency: value },
});

// 没有跨机器通用的 REALITY 站点。新建中转入口保持空白，要求操作者明确填写；已有配置
// 始终显示其自身站点，不会被某个 UI 常量静默覆盖。
const EMPTY_REALITY_SITE = { dest: '', names: '' };

// 中转端口从该值开始向上查找空闲端口。选择高位段是为了与接入面和系统服务分开。
// 该值来自全局设置（`settings.ports.hop_base`），下面的常量只是设置尚未加载时的回退值——
// 加载完成后使用设置中的值。若使用硬编码，运营者修改设置后界面仍会填入 20000。
const HOP_PORT_BASE = 20000;

/** 中转端口的起始值，取自全局设置；设置尚未加载时使用回退值。 */
function useHopPortBase(enabled = true): number {
  const settings = useQuery({ queryKey: ['settings'], queryFn: () => fetchSettings(), enabled });
  return settings.data?.ports?.hop_base || HOP_PORT_BASE;
}

/** 各机器的 overlay 地址。只存在于编译结果中——它由系统层分配，不在模型中。 */
function useOverlayAddrs(enabled = true): (nodeId: string) => string {
  const revisions = useQuery({ queryKey: ['revisions'], queryFn: () => fetchRevisions(), enabled });
  const current = revisions.data?.current_revision;
  const compiled = useQuery({
    queryKey: ['compile', current],
    queryFn: () => fetchCompileView(current!),
    enabled: enabled && !!current,
  });
  const byId = useMemo(() => {
    const nodes =
      (compiled.data?.system as { nodes?: { id: string; overlay_addr: string | null }[] } | undefined)?.nodes ?? [];
    return new Map(nodes.map(n => [n.id, n.overlay_addr ?? '']));
  }, [compiled.data]);
  return (nodeId: string) => byId.get(nodeId) ?? '';
}

// 各机器上已被占用的端口。中转端口的默认值需要选择空闲端口，不能硬编码——
// 走 overlay 时界面上不显示该字段，但该值仍会写入模型、被 xray 绑定，
// 并参与冲突校验。查询键均为其他位置已使用的，通常命中缓存。
//
// 导出该函数是因为端口选择不止规则编辑器一处使用：链路页的「追加一跳」、画布上的
// 「接入主干」同样需要，且这两处选出的值必须与此处显示的一致。分别实现判定会导致
// 界面显示 20001 而库中存储 20000，需要排查一个实际不存在的端口冲突。
export function usePortPool(enabled = true): Map<string, PortOwners> {
  const snapshot = useQuery({ queryKey: ['snapshot'], queryFn: () => fetchSnapshot(), enabled });
  const nodeList = useQuery({ queryKey: ['nodes'], queryFn: () => fetchNodes(), enabled });
  const revisions = useQuery({ queryKey: ['revisions'], queryFn: () => fetchRevisions(), enabled });
  const current = revisions.data?.current_revision;
  const compiled = useQuery({
    queryKey: ['compile', current],
    queryFn: () => fetchCompileView(current!),
    enabled: enabled && !!current,
  });
  return useMemo(
    () => occupiedPorts(snapshot.data?.snapshot.apps ?? [], nodeList.data?.nodes ?? [], compiled.data?.system),
    [snapshot.data, nodeList.data, compiled.data],
  );
}

// 判断中转端口是否被修改。不能对整个对象做比较：快照中的 security 包含服务端生成、
// 已脱敏的密钥（private_key 为 `<redacted>`），而请求体中不含这些字段，直接 deep-equal
// 会导致每张表打开即判定为已修改。只比较可由用户修改的字段。
// 参数类型使用 HopInRequest 而非重新定义该联合类型：重复定义的代价已经出现过——
// 增加一档 wire 时该处未同步修改，且它与改动点相隔 800 行。
function hopInChanged(body: HopInRequest, prev: SnapshotStep['hop_in']): boolean {
  /* 对端尚无 step——保存该步骤时需要为其创建，计为一次改动 */
  if (!prev) return true;
  if (body.port !== prev.port) return true;
  /* 请求中不携带该字段表示不修改它，与其他位置的约定一致 */
  const wire = body.security;
  if (!wire) return false;
  if (wire.t !== prev.security.t) return true;
  if (wire.t === 'reality' && prev.security.t === 'reality') {
    return (
      wire.v.dest !== prev.security.v.dest || wire.v.server_names.join(',') !== prev.security.v.server_names.join(',')
    );
  }
  /* SS2022 和 VLESS-ENC 的密钥均由服务端生成、请求中不包含，因此档位未变即表示未修改 */
  return false;
}

// 一棵规则树中每台机器对应一张独立的规则表，但「保存到草稿」只应有一个按钮。
// 每张表将自身的「是否已修改 / 如何写入草稿」注册到该总线上，末尾的按钮按注册顺序
// 依次执行。顺序不能颠倒：上游的 putStep 会将对端 step 中的规则原样回传
// （见 RuleEditor 的 save），先执行下游时，上游随后会用快照中的旧规则覆盖它。
// effect 按深度优先、从左到右执行，因此注册顺序即入口在前、下游在后。
//
// 没有总线时——画布上拖动连线弹出的浮层只编辑一台机器——编辑器自带按钮。
interface RuleDraftHandle {
  id: string;
  dirty: boolean;
  /* 机器 DNS 单独保存，不应因为它变更而追加任何链路清理操作。 */
  structuralDirty: boolean;
  save: () => Promise<unknown>;
  // 该表对应的链和机器、草稿内容、链的当前状态和链头。
  // 判断哪些节点不再被引用需要整条链的规则表，单张表无法读取其他表的草稿
  // （见 orphansAfter 的注释），因此由总线收集完整后再计算。
  appId: string;
  chainId: string;
  nodeId: string;
  root: string | undefined;
  steps: SnapshotStep[];
  rules: Rule[];
}

interface RuleDraftBus {
  attach: (id: string) => () => void;
  update: (handle: RuleDraftHandle) => void;
}

const RuleDraftCtx = createContext<RuleDraftBus | null>(null);

function DraftSaveToolbar({
  hint,
  status,
  saving,
  canSave,
  onSave,
}: {
  hint?: string;
  status: string;
  saving: boolean;
  canSave: boolean;
  onSave: () => void;
}) {
  return (
    <div className="toolbar">
      {hint && <span className="note">{hint}</span>}
      <span className="sp" />
      <span className="note">{status}</span>
      <button className="btn primary" disabled={saving || !canSave} onClick={onSave}>
        {saving ? '保存中…' : '保存到草稿'}
      </button>
    </div>
  );
}

export function RuleDraftScope({ children, hint }: { children: ReactNode; hint?: string }) {
  // Registration order is structural (entry first, downstream later), while each editor's draft
  // changes on every keystroke. Keep the order and the current value together in React state:
  // reading or mutating a ref during render made the footer one render late and React 19 rightly
  // warns about it. A temporarily empty slot is possible only between the two mount effects and is
  // deliberately omitted from the derived list.
  const [slots, setSlots] = useState<Array<{ id: string; handle: RuleDraftHandle | null }>>([]);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const qc = useQueryClient();
  /* 不再被引用的机器显示名称而非 id——树中和诊断中使用的都是名称 */
  const nodeList = useQuery({ queryKey: ['nodes'], queryFn: () => fetchNodes() });
  const nameOf = (id: string) => nodeList.data?.nodes.find(n => n.node_id === id)?.name || id;

  const bus = useMemo<RuleDraftBus>(
    () => ({
      attach: id => {
        setSlots(current => (current.some(slot => slot.id === id) ? current : [...current, { id, handle: null }]));
        return () => {
          setSlots(current => current.filter(slot => slot.id !== id));
        };
      },
      update: handle => {
        setSlots(current =>
          current.map(slot => (slot.id === handle.id && slot.handle !== handle ? { ...slot, handle } : slot)),
        );
      },
    }),
    [],
  );

  const handles = slots.flatMap(slot => (slot.handle ? [slot.handle] : []));
  const pending = handles.filter(h => h.dirty);

  // 按链收集完整的规则表后再计算不再被引用的节点。每条链计算两次：库中当前已无引用的
  // （即已存在的悬空记录，可立即清除），以及这些草稿写入后将无引用的（即本次改动的
  // 结果，保存时一并移除）。两者含义不同，界面上也应分别表述。
  const chains = useMemo(() => {
    const byChain = new Map<
      string,
      {
        appId: string;
        chainId: string;
        root: string | undefined;
        steps: SnapshotStep[];
        drafts: Map<string, Rule[]>;
      }
    >();
    for (const h of handles) {
      const key = `${h.appId}/${h.chainId}`;
      const entry = byChain.get(key) ?? {
        appId: h.appId,
        chainId: h.chainId,
        root: h.root,
        steps: h.steps,
        drafts: new Map<string, Rule[]>(),
      };
      entry.drafts.set(h.nodeId, h.rules);
      byChain.set(key, entry);
    }
    return [...byChain.values()].map(c => {
      const now = orphansAfter({ steps: c.steps, root: c.root });
      const after = orphansAfter({ steps: c.steps, root: c.root, drafts: c.drafts });
      return {
        ...c,
        // 当前已无引用、且这些草稿写入后仍无引用的节点。
        // 已被其他表引用的不计入：该机器正等待保存，清理时移除它相当于删除刚建立的连接，
        // 而按钮的文案是「清理未被指向的节点」。
        stranded: now.filter(n => after.includes(n)),
        /* 当前有引用、但本次改动后将无引用的节点——保存时由服务端一并移除。 */
        willDrop: after.filter(n => !now.includes(n)),
      };
    });
  }, [handles]);

  const stranded = chains.filter(c => c.stranded.length > 0);
  const dropping = chains.filter(c => c.willDrop.length > 0);
  const strandedNames = [...new Set(stranded.flatMap(c => c.stranded))].map(nameOf);
  const droppingNames = [...new Set(dropping.flatMap(c => c.willDrop))].map(nameOf);

  const refresh = () => {
    qc.invalidateQueries({ queryKey: ['snapshot'] });
    qc.invalidateQueries({ queryKey: ['revisions'] });
    qc.invalidateQueries({ queryKey: ['compile'] });
  };

  const saveAll = async () => {
    setSaving(true);
    setError(null);
    try {
      for (const h of pending) await h.save();
      // 收尾：所有规则表写入完成后，由服务端按整条链的完整数据执行一次清理。
      // 不能在每张表保存后各自清理——此时其他表尚未写入，服务端看到的链不完整，
      // 刚建立连接的机器会被判定为无引用并删除（见 api.ts 的 pruneChain）。
      // 分隔符使用转义写法而非直接输入 NUL 字符：源码中包含 NUL 会使
      // grep / ripgrep 将该文件判定为二进制并**整体跳过**，且不报错——全仓库搜索 dial
      // 或 reverse 都不会命中该文件，可能据此得出前端不存在该功能的结论。运行时等价。
      for (const key of new Set(pending.filter(h => h.structuralDirty).map(h => `${h.appId}\u0000${h.chainId}`))) {
        const [appId, chainId] = key.split('\u0000');
        await pruneChain(appId, chainId);
      }
      refresh();
    } catch (e) {
      setError(e);
    } finally {
      setSaving(false);
    }
  };

  /* 库中已存在的悬空记录，立即清理——不涉及任何草稿，服务端按库中当前数据计算。 */
  const pruneNow = async () => {
    setSaving(true);
    setError(null);
    try {
      for (const c of stranded) await pruneChain(c.appId, c.chainId);
      refresh();
    } catch (e) {
      setError(e);
    } finally {
      setSaving(false);
    }
  };

  return (
    <RuleDraftCtx.Provider value={bus}>
      {children}
      {error != null && <ErrorBox error={error} />}
      {(strandedNames.length > 0 || droppingNames.length > 0) && (
        // 明确列出将被移除的机器，不做无提示删除。此处此前不显示任何内容，保存时按单张表的
        // 草稿计算一次即移除机器——无论移除是否正确，操作者都无从知晓。
        <div className="toolbar orphan-note">
          {strandedNames.length > 0 && (
            <span className="st st-warn" title="没有任何上游转发指向它们，编译会停在 chain.unreachable">
              {strandedNames.join('、')} 已无人指向
            </span>
          )}
          {droppingNames.length > 0 && (
            <span className="note">保存后 {droppingNames.join('、')} 将无人指向，会一并移出这条链</span>
          )}
          <span className="sp" />
          {strandedNames.length > 0 && (
            <button className="btn" disabled={saving} onClick={() => void pruneNow()}>
              清理未被指向的节点
            </button>
          )}
        </div>
      )}
      <DraftSaveToolbar
        hint={hint}
        status={pending.length === 0 ? '没有待保存的改动' : `${pending.length} 项配置有改动`}
        saving={saving}
        canSave={pending.length > 0}
        onSave={() => void saveAll()}
      />
    </RuleDraftCtx.Provider>
  );
}

// 一台机器在一条链中的规则表。
// 这是控制台中唯一修改选路的位置——画布上的连线操作同样调用它，不另行实现
// （同一写操作不做两套实现，两个入口共用一个组件）。
//
// 语义来自 ir.md 和 validate.rs：
// - 规则有序，自上而下第一条匹配的生效，因此末条应为兜底规则（匹配「任意」）
// - 动作三选一：转发给另一台 / 从该机器出网 / 拒绝
// - 非根节点必须从入口可达，转发图不能成环

const MATCH_KINDS: { t: DestMatch['t']; label: string; hint: string; list: boolean }[] = [
  { t: 'any', label: '任意', hint: '兜底用，放最后一条', list: false },
  {
    t: 'sniffing_failed',
    label: '嗅探失败兜底',
    hint: '原目标为 IP 且 200ms 内未取得域名；放在域名规则之后、任意规则之前',
    list: false,
  },
  { t: 'geosite', label: 'geosite', hint: '如 cn、netflix（要节点上有 geosite.dat）', list: true },
  { t: 'geoip', label: 'geoip', hint: '如 cn、private', list: true },
  { t: 'domain_suffix', label: '域名后缀', hint: '如 example.com', list: true },
  { t: 'domain_keyword', label: '域名关键词', hint: '如 google', list: true },
  { t: 'domain_regex', label: '域名正则', hint: '如 ^.+\\.example\\.com$', list: true },
  { t: 'ip_cidr', label: 'IP 段', hint: '如 10.0.0.0/8', list: true },
  { t: 'port', label: '端口', hint: '如 443 或 1000-2000', list: true },
  { t: 'network', label: '传输层', hint: 'tcp 或 udp', list: false },
];

const supportsEgressDns = (match: DestMatch): boolean =>
  match.t === 'domain_suffix' || match.t === 'domain_keyword' || match.t === 'domain_regex' || match.t === 'geosite';

const ruleActionTone = (action: RuleAction['t']): 'forward' | 'egress' | 'block' =>
  action === 'proxy' || action === 'reuse_listener' ? 'forward' : action;

const egressDnsSelectorKey = (match: DestMatch): string => {
  if (match.t === 'domain_suffix' || match.t === 'domain_keyword' || match.t === 'geosite') {
    return JSON.stringify({ t: match.t, v: [...match.v].sort() });
  }
  return JSON.stringify(match);
};

const newEgressDns = (): EgressDnsResolution => ({
  address: '',
  port: 53,
  transport: 'tcp',
  address_strategy: 'use_ip',
  fallback: 'stop',
});

function MachineEgressDnsControls({
  resolution,
  supported,
  onChange,
  readOnly,
  nodeName,
  accessibleSuffix = '',
  showChoice = true,
  showEditor = true,
}: {
  resolution: EgressDnsResolution | null;
  supported: boolean;
  onChange: (next: EgressDnsResolution | null) => void;
  readOnly: boolean;
  nodeName: string;
  accessibleSuffix?: string;
  showChoice?: boolean;
  showEditor?: boolean;
}) {
  const previousCustom = useRef<EgressDnsResolution | null>(resolution);
  useEffect(() => {
    if (resolution) previousCustom.current = resolution;
  }, [resolution]);
  const label = (name: string) => `${name}${accessibleSuffix}`;
  return (
    <>
      {showChoice && (
        <select
          className="f egress-dns-choice"
          aria-label={label('DNS 解析方式')}
          value={resolution ? 'custom' : 'machine'}
          title={supported ? `修改 ${nodeName} 的机器 DNS 策略` : '自定义 DNS 只支持域名类规则'}
          onChange={event =>
            onChange(event.target.value === 'custom' ? (previousCustom.current ?? newEgressDns()) : null)
          }
        >
          <option value="machine">默认 DNS 解析</option>
          <option value="custom" disabled={!supported}>
            自定义 DNS 解析
          </option>
        </select>
      )}
      {showEditor && resolution && supported && (
        <span className="egress-dns-editor">
          <span className="egress-dns-endpoint">
            <input
              className="f mono egress-dns-address"
              aria-label={label('DNS 地址')}
              title="DNS 地址"
              value={resolution.address}
              placeholder="66.66.66.66"
              spellCheck={false}
              onChange={event => onChange({ ...resolution, address: event.target.value })}
            />
            <i>:</i>
            <input
              className="f mono egress-dns-port"
              aria-label={label('端口')}
              title="端口"
              type={readOnly ? 'text' : 'number'}
              min={readOnly ? undefined : 1}
              max={readOnly ? undefined : 65535}
              value={resolution.port || ''}
              onChange={event => onChange({ ...resolution, port: Number(event.target.value) || 0 })}
            />
          </span>
          <select
            className="f words egress-dns-transport"
            aria-label={label('传输')}
            title="传输"
            value={resolution.transport}
            onChange={event =>
              onChange({
                ...resolution,
                transport: event.target.value as EgressDnsResolution['transport'],
              })
            }
          >
            <option value="tcp">TCP</option>
            <option value="udp">UDP</option>
          </select>
          <select
            className="f words egress-dns-family"
            aria-label={label('地址策略')}
            title="地址策略"
            value={resolution.address_strategy}
            onChange={event =>
              onChange({
                ...resolution,
                address_strategy: event.target.value as EgressDnsResolution['address_strategy'],
              })
            }
          >
            <option value="use_ip">UseIP</option>
            <option value="use_ipv4v6">UseIPv4v6</option>
            <option value="use_ipv6v4">UseIPv6v4</option>
            <option value="use_ipv4">UseIPv4</option>
            <option value="use_ipv6">UseIPv6</option>
          </select>
          <select
            className="f words egress-dns-fallback"
            aria-label={label('失败处理')}
            title="失败处理"
            value={resolution.fallback}
            onChange={event =>
              onChange({
                ...resolution,
                fallback: event.target.value as EgressDnsResolution['fallback'],
              })
            }
          >
            <option value="stop">停止连接</option>
            <option value="machine">回退机器 DNS</option>
          </select>
        </span>
      )}
    </>
  );
}

const matchValues = (m: DestMatch): string => ('v' in m ? (Array.isArray(m.v) ? m.v.join(', ') : String(m.v)) : '');

const isAnyRule = (rule: Rule | undefined): boolean => rule?.m.t === 'any';
const isSniffingFallbackRule = (rule: Rule | undefined): boolean => rule?.m.t === 'sniffing_failed';
const isPinnedTerminalRule = (rule: Rule | undefined): boolean => isSniffingFallbackRule(rule) || isAnyRule(rule);

// The failure selector is a second terminal. Keep every ordinary selector ahead of it so IP,
// port and explicit domain rules still get first refusal, while Any remains the absolute last
// resort. The same invariant is checked by brocade-core for writes that bypass this editor.
const pinTerminalRules = (rules: Rule[]): Rule[] => [
  ...rules.filter(rule => !isPinnedTerminalRule(rule)),
  ...rules.filter(isSniffingFallbackRule),
  ...rules.filter(isAnyRule),
];

const matchDependsOnSniffing = (match: DestMatch): boolean => {
  switch (match.t) {
    case 'domain_suffix':
    case 'domain_keyword':
    case 'domain_regex':
    case 'geosite':
      return true;
    case 'all':
      return match.v.some(matchDependsOnSniffing);
    default:
      return false;
  }
};

type MachineDnsDraftRow = {
  id: string;
  originalSelector: DestMatch | null;
  selector: DestMatch;
  resolution: EgressDnsResolution | null;
};

/** 机器详情页中的机器级 DNS 策略。它不属于任何一条链，所以单独读取并写入
 * `node_egress_dns`；保存后始终进入这台机器的 Xray 配置。 */
export function MachineEgressDnsRules({
  nodeId,
  nodeName,
  readOnly = false,
  showHeader = false,
}: {
  nodeId: string;
  nodeName: string;
  readOnly?: boolean;
  showHeader?: boolean;
}) {
  const qc = useQueryClient();
  const snapshot = useQuery({ queryKey: ['snapshot'], queryFn: () => fetchSnapshot() });
  const policies = sortedDnsPolicies((snapshot.data?.node_egress_dns ?? []).filter(policy => policy.node === nodeId));
  const rowsFromPolicies = (): MachineDnsDraftRow[] =>
    policies.map(policy => ({
      id: `stored:${egressDnsSelectorKey(policy.selector)}`,
      originalSelector: policy.selector,
      selector: policy.selector,
      resolution: policy.resolution,
    }));
  const [rows, setRows] = useState<MachineDnsDraftRow[] | null>(null);
  const nextRowId = useRef(1);
  const displayedRows = rows ?? rowsFromPolicies();
  const activeRows = displayedRows.filter(
    (row): row is MachineDnsDraftRow & { resolution: EgressDnsResolution } => row.resolution !== null,
  );
  const selectorCounts = new Map<string, number>();
  for (const row of activeRows) {
    const key = egressDnsSelectorKey(row.selector);
    selectorCounts.set(key, (selectorCounts.get(key) ?? 0) + 1);
  }
  const rowReady = (row: MachineDnsDraftRow): boolean =>
    row.resolution === null ||
    Boolean(
      matchValues(row.selector).trim() &&
      row.resolution.address.trim() &&
      row.resolution.port >= 1 &&
      row.resolution.port <= 65535 &&
      selectorCounts.get(egressDnsSelectorKey(row.selector)) === 1,
    );
  const baselineSignature = policies
    .map(policy => `${egressDnsSelectorKey(policy.selector)}\0${JSON.stringify(policy.resolution)}`)
    .join('\u0001');
  const currentSignature = activeRows
    .map(row => `${egressDnsSelectorKey(row.selector)}\0${JSON.stringify(row.resolution)}`)
    .join('\u0001');
  const selectorListDirty =
    activeRows.map(row => egressDnsSelectorKey(row.selector)).join('\0') !==
    policies.map(policy => egressDnsSelectorKey(policy.selector)).join('\0');
  const dirty = rows !== null && currentSignature !== baselineSignature;
  const changedRows = rows
    ? displayedRows.filter(row => {
        if (!row.originalSelector) return row.resolution !== null;
        const stored = policies.find(
          policy => egressDnsSelectorKey(policy.selector) === egressDnsSelectorKey(row.originalSelector!),
        );
        return (
          row.resolution === null ||
          egressDnsSelectorKey(row.selector) !== egressDnsSelectorKey(row.originalSelector) ||
          JSON.stringify(row.resolution) !== JSON.stringify(stored?.resolution)
        );
      }).length
    : 0;
  const retainedOriginalOrder = activeRows.flatMap(row =>
    row.originalSelector ? [egressDnsSelectorKey(row.originalSelector)] : [],
  );
  const retainedSet = new Set(retainedOriginalOrder);
  const baselineRetainedOrder = policies
    .map(policy => egressDnsSelectorKey(policy.selector))
    .filter(key => retainedSet.has(key));
  const orderDirty = retainedOriginalOrder.join('\0') !== baselineRetainedOrder.join('\0');
  const updateRow = (id: string, patch: (row: MachineDnsDraftRow) => MachineDnsDraftRow) =>
    setRows(current => (current ?? rowsFromPolicies()).map(row => (row.id === id ? patch(row) : row)));
  const removeNewRow = (id: string) => setRows(current => (current ?? rowsFromPolicies()).filter(row => row.id !== id));
  const movePolicy = (id: string, delta: number) => {
    const current = rows ?? rowsFromPolicies();
    const index = current.findIndex(row => row.id === id);
    const target = index + delta;
    if (index < 0 || target < 0 || target >= current.length) return;
    const next = [...current];
    [next[index], next[target]] = [next[target], next[index]];
    setRows(next);
  };
  const save = useMutation({
    mutationFn: async () => {
      if (!rows || !dirty || !rows.every(rowReady)) return;
      for (const row of rows) {
        if (
          row.originalSelector &&
          (row.resolution === null || egressDnsSelectorKey(row.originalSelector) !== egressDnsSelectorKey(row.selector))
        ) {
          await setNodeEgressDns(nodeId, row.originalSelector, null);
        }
      }
      for (const row of activeRows) {
        const stored = row.originalSelector
          ? policies.find(
              policy => egressDnsSelectorKey(policy.selector) === egressDnsSelectorKey(row.originalSelector!),
            )
          : null;
        if (
          !stored ||
          egressDnsSelectorKey(stored.selector) !== egressDnsSelectorKey(row.selector) ||
          JSON.stringify(stored.resolution) !== JSON.stringify(row.resolution)
        ) {
          await setNodeEgressDns(nodeId, row.selector, row.resolution);
        }
      }
      if (selectorListDirty) {
        await reorderNodeEgressDns(
          nodeId,
          activeRows.map(row => row.selector),
        );
      }
    },
    onSuccess: () => {
      setRows(null);
      qc.invalidateQueries({ queryKey: ['snapshot'] });
      qc.invalidateQueries({ queryKey: ['revisions'] });
      qc.invalidateQueries({ queryKey: ['compile'] });
    },
  });

  if (snapshot.error) return <ErrorBox error={snapshot.error} />;
  return (
    <>
      {showHeader && (
        <header>
          <PanelTitle of="dns">DNS 解析策略</PanelTitle>
          <span className="rule-sheet-meta" title="Xray 按 D1 起依次选择解析器；DNS 顺序不参与链路规则匹配">
            {activeRows.length} 条
          </span>
          <span className="sp" />
          <button
            type="button"
            className="btn"
            disabled={readOnly}
            onClick={() =>
              setRows(current => [
                ...(current ?? rowsFromPolicies()),
                {
                  id: `new:${nextRowId.current++}`,
                  originalSelector: null,
                  selector: { t: 'domain_suffix', v: [] },
                  resolution: newEgressDns(),
                },
              ])
            }
          >
            添加新策略
          </button>
        </header>
      )}
      <fieldset className="node-egress-rules rule-ro" disabled={readOnly}>
        <p className="note node-egress-dns-limit">
          Xray 的 DNS 选择不携带原路由和出站上下文；DNS 查询可以指定出口，但解析结果无法按出站隔离。
        </p>
        {displayedRows.length > 0 && (
          <table className="tbl rule-table node-egress-rules-table">
            <tbody>
              {displayedRows.map((row, index) => {
                const kind = MATCH_KINDS.find(candidate => candidate.t === row.selector.t);
                const value = matchValues(row.selector);
                const suffix = `（${kind?.label ?? row.selector.t}${value ? ` ${value}` : ''}）`;
                const duplicate =
                  row.resolution !== null &&
                  value.trim().length > 0 &&
                  selectorCounts.get(egressDnsSelectorKey(row.selector)) !== 1;
                return (
                  <tr key={row.id} className={row.originalSelector ? undefined : 'node-egress-dns-new-row'}>
                    <td className="mono dim" style={{ width: 24 }}>
                      D{index + 1}
                    </td>
                    <td className="rule-match-cell">
                      {readOnly ? (
                        <>
                          <span className="f rule-readonly-select">{kind?.label ?? row.selector.t}</span>
                          {value && <span className="f rule-readonly-value">{value}</span>}
                        </>
                      ) : (
                        <>
                          <select
                            className="f"
                            aria-label={`DNS 匹配类型${suffix}`}
                            disabled={row.resolution === null}
                            value={row.selector.t}
                            onChange={event =>
                              updateRow(row.id, current => ({
                                ...current,
                                selector: buildMatch(event.target.value as DestMatch['t'], ''),
                              }))
                            }
                          >
                            {MATCH_KINDS.filter(candidate => supportsEgressDns(buildMatch(candidate.t, 'value'))).map(
                              candidate => (
                                <option key={candidate.t} value={candidate.t}>
                                  {candidate.label}
                                </option>
                              ),
                            )}
                          </select>
                          <input
                            className="f rule-new-dns-value"
                            aria-label={`DNS 匹配内容${suffix}`}
                            disabled={row.resolution === null}
                            value={value}
                            placeholder={kind?.hint}
                            spellCheck={false}
                            onChange={event =>
                              updateRow(row.id, current => ({
                                ...current,
                                selector: buildMatch(current.selector.t, event.target.value),
                              }))
                            }
                          />
                        </>
                      )}
                      {duplicate && <span className="sub err">已有相同策略</span>}
                    </td>
                    <td className="rule-action-cell">
                      <MachineEgressDnsControls
                        key={row.id}
                        resolution={row.resolution}
                        supported
                        onChange={next =>
                          next === null && row.originalSelector === null
                            ? removeNewRow(row.id)
                            : updateRow(row.id, current => ({ ...current, resolution: next }))
                        }
                        readOnly={readOnly}
                        nodeName={nodeName}
                        accessibleSuffix={suffix}
                      />
                      {row.resolution === null && <span className="egress-dns-usage">保存后移除</span>}
                    </td>
                    <td className="dns-priority-cell">
                      <DnsPriorityControl
                        index={index}
                        count={displayedRows.length}
                        label={`${kind?.label ?? row.selector.t} ${value}`.trim()}
                        readOnly={readOnly}
                        showLabel={false}
                        onMove={delta => movePolicy(row.id, delta)}
                      />
                      {!row.originalSelector && !readOnly && (
                        <button type="button" className="btn danger" onClick={() => removeNewRow(row.id)}>
                          取消
                        </button>
                      )}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        )}
        {!readOnly && displayedRows.length > 0 && (
          <>
            {save.error && <ErrorBox error={save.error} />}
            <DraftSaveToolbar
              hint="改动落进草稿，顶栏按「提交」才写进库。"
              status={
                !dirty
                  ? '没有待保存的改动'
                  : `${changedRows || 1} 项配置有改动${orderDirty ? '，DNS 优先级已调整' : ''}`
              }
              saving={save.isPending}
              canSave={dirty && displayedRows.every(rowReady)}
              onSave={() => save.mutate()}
            />
          </>
        )}
      </fieldset>
    </>
  );
}

function buildMatch(t: DestMatch['t'], raw: string): DestMatch {
  const list = raw
    .split(/[,\s]+/)
    .map(v => v.trim())
    .filter(Boolean);
  switch (t) {
    case 'any':
      return { t: 'any' };
    case 'sniffing_failed':
      return { t: 'sniffing_failed' };
    case 'front_downstream':
      return { t: 'front_downstream' };
    case 'network':
      return { t: 'network', v: raw.trim() === 'udp' ? 'udp' : 'tcp' };
    case 'domain_regex':
      return { t: 'domain_regex', v: raw.trim() };
    default:
      return { t, v: list } as DestMatch;
  }
}

// 该跳连接对端时使用的地址。
// 除自定义外的各档地址均由推导得出，无需手动填写：公网 IPv4/IPv6 连接对端的
// 非 NAT 公网 IP，反向两档显示本机的接入地址，走 WireGuard 时连接对端的 overlay 地址。
// 只有自定义需要手动填写——该档对应编译器无法推导的地址（机房内网、专线），
// 推导错误的表现是该跳走了另一条路径且无提示。
// 顺序与下拉框中的数组一致，两处不一致时会被认为下拉框按此处排列。
// XRAY 四档排在前面、走 WireGuard 排在其后：前四档表示该跳自身如何连接，
// 而 WireGuard 是将该跳交由 overlay 承载，属于另一类选择。
// 自定义仍置于末尾作为兜底：前面各档都有确定的取值，只有它需要手动填写。
export type DialKind = 'public_ipv4' | 'public_ipv6' | 'reverse_v4' | 'reverse_v6' | 'overlay' | 'custom';

export const DIAL_LABEL: Record<DialKind, string> = {
  public_ipv4: '走 XRAY 公网 IPv4',
  public_ipv6: '走 XRAY 公网 IPv6',
  reverse_v4: '走 XRAY 反向 IPv4',
  reverse_v6: '走 XRAY 反向 IPv6',
  overlay: '走 WireGuard',
  custom: '自定义',
};

// 下拉框中的排列顺序。只定义一份——档位的优先级（`defaultHopDial` 取第一个可用的）
// 即依据该顺序，分别定义两份时，修改排列而未修改默认值会导致界面上的第一档与实际
// 默认值不一致。
export const DIAL_ORDER: DialKind[] = ['public_ipv4', 'public_ipv6', 'reverse_v4', 'reverse_v6', 'overlay', 'custom'];

// 从存储的 dial 反推对应的档位。`addr` 的主机部分与对端公网 IP 相同时为对应的
// 公网 IPv4/IPv6 档，否则为自定义档——三者在模型中是同一类型（都是具体地址），
// 分档只是为了在界面上区分该地址是自动填充还是手动填写。
function splitHostPort(raw: string): { host: string; port: string } {
  const value = raw.trim();
  if (value.startsWith('[')) {
    const end = value.indexOf(']');
    if (end >= 0 && value.slice(end + 1).startsWith(':')) {
      return { host: value.slice(1, end), port: value.slice(end + 2) };
    }
  }
  const i = value.lastIndexOf(':');
  return i >= 0 ? { host: value.slice(0, i), port: value.slice(i + 1) } : { host: value, port: '' };
}

const formatHostPort = (host: string, port: number) => {
  const value = host.trim();
  return value.includes(':') && !value.startsWith('[') ? `[${value}]:${port}` : `${value}:${port}`;
};

// 判断该机器的对外可达地址只需要这四个字段。收窄为该类型而非直接使用 `ForwardPeer`，
// 是因为需要判断公网可达性的不止转发目标：本机（反向两档连接的对象）
// 和建链向导中的节点行都使用同一判定，而它们都不是 `ForwardPeer`。
type PublicAddrs = Pick<ForwardPeer, 'public_ipv4' | 'public_ipv6' | 'public_ipv4_nat' | 'public_ipv6_nat'>;

/* 标记为 NAT 的不计入：该类地址无法接受入站连接，编译器的 `dialable_public_host` 判定一致。 */
const publicIpv4Of = (peer: PublicAddrs | null | undefined) =>
  peer?.public_ipv4 && !peer.public_ipv4_nat ? peer.public_ipv4 : '';

const publicIpv6Of = (peer: PublicAddrs | null | undefined) =>
  peer?.public_ipv6 && !peer.public_ipv6_nat ? peer.public_ipv6 : '';

const natPublicHostOf = (peer: PublicAddrs | null | undefined, host: string) => {
  if (peer?.public_ipv4_nat && peer.public_ipv4 === host) return '公网 IPv4';
  if (peer?.public_ipv6_nat && peer.public_ipv6 === host) return '公网 IPv6';
  return null;
};

// 新建转发规则时的默认档位。按下拉框的顺序取第一个可用的，不硬编码某一档：
// 下拉框的排列本身表达了优先级，默认值与之不一致时该顺序即失去作用——
// 每添加一条规则都需要手动改为同一档。
//
// 可用性判定与下拉框中 `disabled` 使用同一份：公网两档判断对端是否有该族的
// 非 NAT 地址（本机连接对端），反向两档判断本机是否有（对端连接本机）。均不可用时回退到
// overlay——该档不需要任何地址，始终可用。
//
// 选出的公网档为明文直连。中转端口的默认加密为 PLAIN（`seedHopIn`），因此选中后
// 编辑器中的「明文直连可能暴露 UUID 和目标地址」提示会同时显示，可在同一屏内修改加密方式。
//
// 导出该函数是因为档位选择不止规则编辑器一处使用：建链向导的线性中继同样需要，
// 且两处选出的必须是同一档——分别实现会导致向导创建的链走 wg 而手动添加的跳走公网，
// 而操作者并未做出该选择。
// 将某一档转换为模型中的 `dial`。overlay 档不带地址（符号引用），公网档填入对端对应的
// 公网 IP，反向两档同样不带地址（本机地址是节点属性，由编译器推导，地址族显式指定），
// 自定义档留空待填写。
//
// 端口只在带地址的档位中使用，取自对端该跳的 `hop_in.port`。
export function hopDialOf(kind: DialKind, peer: PublicAddrs | null, port: number): HopDial {
  if (kind === 'overlay') return { t: 'overlay' };
  if (kind === 'reverse_v4' || kind === 'reverse_v6') {
    return { t: 'reverse', v: kind === 'reverse_v6' ? 'v6' : 'v4' };
  }
  const host = kind === 'public_ipv4' ? publicIpv4Of(peer) : kind === 'public_ipv6' ? publicIpv6Of(peer) : '';
  return host ? { t: 'addr', v: formatHostPort(host, port) } : { t: 'addr', v: `:${port}` };
}

export type ListenerDialKind = Exclude<DialKind, 'reverse_v4' | 'reverse_v6'>;
export const LISTENER_DIAL_ORDER: ListenerDialKind[] = ['public_ipv4', 'public_ipv6', 'overlay', 'custom'];

/** Convert a transport choice into a durable listener dial without copying address or port. */
export function listenerDialOf(kind: ListenerDialKind): ListenerDial {
  if (kind === 'overlay') return { t: 'overlay' };
  if (kind === 'public_ipv4' || kind === 'public_ipv6') {
    // Availability is checked before this function is called. Keeping the family symbolic is the
    // important part: a later node-address change updates this reference instead of stranding it.
    return { t: 'public', v: kind === 'public_ipv6' ? 'v6' : 'v4' };
  }
  return { t: 'addr', v: '' };
}

export function listenerDialKindOf(dial: ListenerDial): ListenerDialKind {
  if (dial.t === 'overlay') return 'overlay';
  if (dial.t === 'public') return dial.v === 'v6' ? 'public_ipv6' : 'public_ipv4';
  return 'custom';
}

const listenerHostOf = (dial: ListenerDial) => (dial.t === 'addr' ? dial.v : '');

function defaultListenerDial(peer: PublicAddrs | null): ListenerDial {
  const kind =
    LISTENER_DIAL_ORDER.filter(candidate => candidate !== 'custom').find(
      candidate => !dialUnavailable(candidate, peer, null),
    ) ?? 'overlay';
  return listenerDialOf(kind);
}

// 该档当前是否可选。公网两档判断对端是否有该族的非 NAT 地址（本机连接对端），
// 反向两档判断本机是否有（对端连接本机）。overlay 和自定义始终可选。
export function dialUnavailable(kind: DialKind, peer: PublicAddrs | null, self: PublicAddrs | null): boolean {
  if (kind === 'public_ipv4') return !publicIpv4Of(peer);
  if (kind === 'public_ipv6') return !publicIpv6Of(peer);
  if (kind === 'reverse_v4') return !publicIpv4Of(self);
  if (kind === 'reverse_v6') return !publicIpv6Of(self);
  return false;
}

export function defaultHopDial(args: {
  /* 对端（本机要连接的机器）。链外的候选尚无 step，公网地址仍取自节点表。 */
  peer: PublicAddrs | null;
  /* 本机。反向两档中由对端连接本机。传 null 表示这两档不参与选择。 */
  self: PublicAddrs | null;
  /* 公网档需转换为 `host:port`，端口取自对端该跳的 `hop_in.port`。 */
  port: number;
}): HopDial {
  const { peer, self, port } = args;
  /* 自定义档不参与：其地址需手动填写，无法自动选择。 */
  const kind = DIAL_ORDER.filter(k => k !== 'custom').find(k => !dialUnavailable(k, peer, self)) ?? 'overlay';
  return hopDialOf(kind, peer, port);
}

export function dialKindOf(dial: HopDial, peer: PublicAddrs | null | undefined): DialKind {
  if (dial.t === 'overlay') return 'overlay';
  if (dial.t === 'reverse') return dial.v === 'v6' ? 'reverse_v6' : 'reverse_v4';
  const host = splitHostPort(dial.v).host;
  const publicIpv4 = publicIpv4Of(peer);
  const publicIpv6 = publicIpv6Of(peer);
  if (publicIpv4 && host === publicIpv4) return 'public_ipv4';
  if (publicIpv6 && host === publicIpv6) return 'public_ipv6';
  return 'custom';
}

const hostOf = (dial: HopDial) => (dial.t === 'addr' ? splitHostPort(dial.v).host : '');

// 转发目标的候选项。
// `where` 表示它与该链的关系：`next` 为本机的下游（主干下一跳，或已分叉指向的机器），
// `outside` 为链外，`inside` 为链内的其他节点。
// `blocked` 非空表示不可选，其内容直接作为提示文本显示。
export interface ForwardPeer {
  id: string;
  /* 界面上显示名称；id 只用作编辑值和回退值 */
  name: string;
  public_ipv4: string | null;
  public_ipv6: string | null;
  public_ipv4_nat: boolean;
  public_ipv6_nat: boolean;
  /* 该机器在该链上的当前状态。链外的候选尚无 step，取值为 null。 */
  step: SnapshotStep | null;
  where: 'next' | 'outside' | 'inside';
  blocked: string | null;
}

export interface ReusableListener {
  ref: ListenerRef;
  key: string;
  appId: string;
  appName: string;
  nodeName: string;
  ownerName: string;
  step: SnapshotStep;
  local: boolean;
  references: number;
  blocked: string | null;
}

export const listenerRefKey = (listener: ListenerRef): string => `${listener.chain}\u0000${listener.node}`;

/** Resolve both the historical same-chain edge and an explicit listener reference to one key. */
export const actionListenerRef = (action: RuleAction, sourceChain: string): ListenerRef | null =>
  action.t === 'forward'
    ? { chain: sourceChain, node: action.to }
    : action.t === 'reuse_listener'
      ? action.listener
      : null;

/**
 * Existing listener ports visible to one rule table, including cycle and tenant-scope verdicts.
 * Disabled choices stay in the result so the UI can explain why a known port cannot be selected.
 */
export function reusableListeners(args: {
  apps: SnapshotApp[];
  sourceApp: string;
  sourceChain: string;
  sourceNode: string;
  sourceRules: Rule[];
  /** Unsaved sibling rule tables in the source chain, keyed by node. */
  sourceDrafts?: Record<string, Rule[]>;
  nodes: {
    node_id: string;
    name: string;
    tenant_id: string;
    retired_at: string | null;
  }[];
}): ReusableListener[] {
  const { apps, sourceApp, sourceChain, sourceNode, sourceRules, sourceDrafts, nodes } = args;
  const source = { chain: sourceChain, node: sourceNode };
  const sourceKey = listenerRefKey(source);
  const nodeOf = new Map(nodes.map(node => [node.node_id, node]));
  const chainOf = new Map(apps.flatMap(app => app.chains.map(chain => [chain.id, { app, chain }] as const)));
  const sourceTenant = chainOf.get(sourceChain)?.chain.tenant ?? '';
  const rulesOf = (step: SnapshotStep) =>
    step.chain === sourceChain && step.node === sourceNode
      ? sourceRules
      : step.chain === sourceChain
        ? (sourceDrafts?.[step.node] ?? step.rules)
        : step.rules;

  const graph = new Map<string, Set<string>>();
  const allSteps = apps.flatMap(app => app.steps);
  const stepOf = new Map(allSteps.map(step => [listenerRefKey({ chain: step.chain, node: step.node }), step]));
  for (const step of allSteps) {
    const from = listenerRefKey({ chain: step.chain, node: step.node });
    for (const rule of rulesOf(step)) {
      const target = actionListenerRef(rule.a, step.chain);
      if (!target) continue;
      const edges = graph.get(from) ?? new Set<string>();
      edges.add(listenerRefKey(target));
      graph.set(from, edges);
    }
  }
  const subtreeIssue = (start: string): string | null => {
    const seen = new Set<string>();
    const stack = [start];
    while (stack.length > 0) {
      const at = stack.pop()!;
      if (seen.has(at)) continue;
      seen.add(at);
      const step = stepOf.get(at);
      if (!step) return '监听子树包含不存在的规则节点';
      const member = nodeOf.get(step.node);
      if (member?.retired_at) return `监听子树包含已退役机器 ${member.name || member.node_id}`;
      for (const next of graph.get(at) ?? []) stack.push(next);
    }
    return null;
  };
  const reachesSource = (from: string): boolean => {
    const seen = new Set<string>();
    const stack = [from];
    while (stack.length > 0) {
      const at = stack.pop()!;
      if (at === sourceKey) return true;
      if (seen.has(at)) continue;
      seen.add(at);
      for (const next of graph.get(at) ?? []) stack.push(next);
    }
    return false;
  };
  const referenceCount = (wanted: ListenerRef) =>
    allSteps.reduce(
      (count, step) =>
        count +
        rulesOf(step).filter(rule => {
          return (
            rule.a.t === 'reuse_listener' &&
            rule.a.listener.chain === wanted.chain &&
            rule.a.listener.node === wanted.node
          );
        }).length,
      0,
    );

  return allSteps
    .filter((step): step is SnapshotStep & { hop_in: NonNullable<SnapshotStep['hop_in']>; accept: StepAccept } =>
      Boolean(step.hop_in && step.accept),
    )
    .map(step => {
      const ref = { chain: step.chain, node: step.node };
      const key = listenerRefKey(ref);
      const owner = chainOf.get(step.chain);
      const node = nodeOf.get(step.node);
      const ownerHasIngress = owner?.app.ingresses.some(ingress => ingress.chain === step.chain) ?? false;
      const ownerHasRetiredMember = owner
        ? chainMembers(owner.app, owner.chain.id).some(member => Boolean(nodeOf.get(member)?.retired_at))
        : false;
      const blocked =
        key === sourceKey
          ? '当前规则表不能引用自身'
          : node?.retired_at
            ? '监听所在机器已退役'
            : !owner
              ? '监听所属线路不存在'
              : !ownerHasIngress
                ? '监听所属线路没有入口，当前不会运行'
                : ownerHasRetiredMember
                  ? '监听所属线路含已退役机器，当前不会运行'
                  : sourceTenant && !under(sourceTenant, owner.chain.tenant)
                    ? '不在当前线路的可用范围内'
                    : reachesSource(key)
                      ? '引用后会形成环路'
                      : subtreeIssue(key);
      return {
        ref,
        key,
        appId: owner?.app.id ?? sourceApp,
        appName: owner?.app.label || owner?.app.id || sourceApp,
        nodeName: node?.name || step.node,
        ownerName: owner?.chain.name || step.chain,
        step,
        local: step.node === sourceNode,
        references: referenceCount(ref),
        blocked,
      };
    })
    .sort(
      (left, right) =>
        Number(right.local) - Number(left.local) ||
        left.appName.localeCompare(right.appName, 'zh-CN') ||
        left.ownerName.localeCompare(right.ownerName, 'zh-CN') ||
        left.nodeName.localeCompare(right.nodeName, 'zh-CN') ||
        left.step.hop_in!.port - right.step.hop_in!.port,
    );
}

// 租户可见性：节点的租户必须是链的租户的祖先（validate.rs 的 under / tenant.scope）。
// 导出该函数是因为该判定不止用于选择转发目标：建链向导选择授权对象时，
// 被授权的租户同样必须在接入面租户之下（validate.rs 中使用同一个 `under`）。分别实现
// 会导致两处对同一对象给出不同结果，而其中一处需要在提交后由编译器纠正。
export const under = (child: string, parent: string) => child === parent || child.startsWith(`${parent}.`);

export function isForwardTargetInChain(args: { nodeId: string; steps: SnapshotStep[] }) {
  const { nodeId, steps } = args;
  /* 只统计显式规则：编译器不再补全主干默认边，上游未写转发即视为无转发。 */
  return steps.some(s => s.node !== nodeId && s.rules.some(r => r.a.t === 'forward' && r.a.to === nodeId));
}

// 该链上不再被任何规则指向的节点——只用于显示。
//
// 计算该结果的原因：steps 是链上成员的唯一数据来源，规则表表示流量路径。
// 删除一条 `Forward → my-01` 后，my-01 的 step 仍然存在，但没有任何上游指向它——编译器
// 会报 `chain.unreachable`（validate.rs，级别为 error 而非 warning），整条链无法发布。
// 需要先知道本次修改会使哪台机器失去引用，才能决定是否移除。
//
// 计算结果不用于执行删除。此处的结果此前直接驱动 `deleteStep`，该做法不正确：
// 一次保存同时修改两张表时，该函数只能看到传入的草稿，其他表仍是库中的旧规则——
// 按该不完整的图计算时，刚在另一张表中建立连接的机器会被判定为无引用并移除，
// 表现为提交后编译报 `relay.no-accept`（规则指向它，但其 step 已被删除）。实际删除由服务端
// 在整棵树写入完成后统一执行一次（`pruneChain` → store 的 `prune_chain`），此时数据完整。
// 此处计算的是同一判定的预览版本，不准确时只影响提示。
//
// 判定依据是从链头的可达性，而非入度。入度判定会遗漏环：A→B、B→A 的入度都不为 0，
// 但从链头都不可达。BFS 遍历可同时覆盖两种情况，级联也一并处理——删除 my-01 后
// 仅由 my-01 指向的 nz-01 同样变为不可达，一次计算即可得出。与服务端 `prune_unreachable_tx`
// 使用同一判定。
//
// 链头始终保留（它是入口所在的机器，没有上游属于正常）。root 未知或没有任何 step
// 时返回空：依据不完整的图报告无引用节点，可能导致对正常机器执行操作。
export function orphansAfter(args: {
  steps: SnapshotStep[];
  root: string | undefined;
  // 编辑中的草稿，按机器覆盖库中对应的记录。未包含的机器使用库中的当前数据。
  // 传空表示按库中当前数据计算——结果为当前已无引用的节点。
  drafts?: Map<string, Rule[]>;
}): string[] {
  const { steps, root, drafts } = args;
  if (!root || steps.length === 0) return [];
  const rulesOf = (node: string) => drafts?.get(node) ?? steps.find(s => s.node === node)?.rules ?? [];
  const members = new Set(steps.map(s => s.node));
  const seen = new Set<string>([root]);
  const queue = [root];
  while (queue.length > 0) {
    const at = queue.shift()!;
    for (const rule of rulesOf(at)) {
      if (rule.a.t !== 'forward') continue;
      const to = rule.a.to;
      // 指向链外的目标在本次保存时会为其创建 step（见 save），因此视为可达；
      // 但它尚不是成员，不参与无引用节点的判定。
      if (seen.has(to)) continue;
      seen.add(to);
      queue.push(to);
    }
  }
  return [...members].filter(n => !seen.has(n));
}

// 计算该机器在该链上可转发的目标。
// *
// * 主干不是转发目标的白名单，而是默认路径。主干是规则表中 `any → Forward`
// * 串联得出的路径——顺序写在规则中，模型不存储。`Forward.to` 不限于
// * 主干下一跳，指向主干之外即为分叉——分叉正是规则表的用途，将候选限定在
// * 主干内相当于禁用该功能。
// *
// * 实际的拓扑约束是可达且无环：`Forward` 是本机规则表中的局部下一跳，
// * 已由上游送达的节点可以在自身规则表中继续向下转发。不可选的是入口节点，
// * 以及已能回到当前节点的目标——从此处指向它们会形成环。
// *
// * 判定结论：候选为入口之外的可见机器，并按「当前下游 / 链内其他 / 链外」分组显示。
export function forwardPeers(args: {
  nodeId: string;
  sourceChain: string;
  spine: string[];
  tenant: string;
  /* 该链的全部 step。包括本机的那条——计算其他节点的上游时会跳过它。 */
  steps: SnapshotStep[];
  app?: SnapshotApp | null;
  drafts?: Record<string, Rule[]>;
  nodes: {
    node_id: string;
    name: string;
    tenant_id: string;
    public_ipv4: string | null;
    public_ipv6: string | null;
    public_ipv4_nat: boolean;
    public_ipv6_nat: boolean;
    retired_at: string | null;
  }[];
}): ForwardPeer[] {
  const { nodeId, sourceChain, spine, tenant, steps, app, drafts, nodes } = args;
  const root = spine[0] ?? null;
  const currentStep = steps.find(s => s.node === nodeId) ?? null;
  const currentRules = drafts?.[nodeId] ?? currentStep?.rules ?? [];
  const currentTargets = new Set<string>();

  // 链内包含的机器：主干节点、已有 step 的节点，以及被其他规则指向的节点。
  // 本机 step 的边不计入——它正在被编辑，不能将旧值作为当前状态。
  const others = steps.filter(s => s.node !== nodeId);
  const inChain = new Set(spine);
  for (const s of others) {
    inChain.add(s.node);
    for (const r of s.rules) if (r.a.t === 'forward' && r.a.to) inChain.add(r.a.to);
  }

  const addCurrentTarget = (to: string | undefined) => {
    if (to) currentTargets.add(to);
  };
  for (const r of currentRules) if (r.a.t === 'forward') addCurrentTarget(r.a.to);

  const graph = new Map<string, Set<string>>();
  const addEdge = (from: string, to: string) => {
    const edges = graph.get(from) ?? new Set<string>();
    edges.add(to);
    graph.set(from, edges);
  };

  const allSteps = new Map<string, SnapshotStep>();
  for (const step of app?.steps ?? steps) allSteps.set(listenerRefKey({ chain: step.chain, node: step.node }), step);
  for (const step of steps) allSteps.set(listenerRefKey({ chain: sourceChain, node: step.node }), step);
  const sourceKey = listenerRefKey({ chain: sourceChain, node: nodeId });
  for (const [from, step] of allSteps) {
    if (from === sourceKey) continue;
    const rules = step.chain === sourceChain ? (drafts?.[step.node] ?? step.rules) : step.rules;
    for (const rule of rules) {
      const target = actionListenerRef(rule.a, step.chain);
      if (target) addEdge(from, listenerRefKey(target));
    }
  }

  const reaches = (from: string, target: string): boolean => {
    const seen = new Set<string>();
    const stack = [from];
    while (stack.length) {
      const at = stack.pop()!;
      if (at === target) return true;
      if (seen.has(at)) continue;
      seen.add(at);
      for (const to of graph.get(at) ?? []) stack.push(to);
    }
    return false;
  };

  return nodes
    .filter(n => n.node_id !== nodeId)
    .map(n => {
      const where: ForwardPeer['where'] = !inChain.has(n.node_id)
        ? 'outside'
        : currentTargets.has(n.node_id)
          ? 'next'
          : 'inside';
      const blocked = n.retired_at
        ? '已退役'
        : !under(tenant, n.tenant_id)
          ? '不在当前线路的可用范围内'
          : n.node_id === root
            ? '链入口'
            : reaches(listenerRefKey({ chain: sourceChain, node: n.node_id }), sourceKey)
              ? '会成环'
              : null;
      return {
        id: n.node_id,
        name: n.name,
        public_ipv4: n.public_ipv4,
        public_ipv6: n.public_ipv6,
        public_ipv4_nat: n.public_ipv4_nat,
        public_ipv6_nat: n.public_ipv6_nat,
        step: steps.find(s => s.node === n.node_id) ?? null,
        where,
        blocked,
      };
    });
}

/** 一个转发目标的中转端口表单状态。 */
export type HopsDraft = Record<
  string,
  {
    port: string;
    kind: 'none' | 'encryption' | 'reality' | 'shadowsocks2022';
    dest: string;
    names: string;
  }
>;

/** Unsaved machine DNS policies, keyed by their canonical selector. Kept beside HopsDraft in the
 * chain panel because one machine may occur more than once in the rendered rule tree. */
export type EgressDnsDraft = Record<string, { selector: DestMatch; resolution: EgressDnsResolution | null }>;

/** Unsaved machine-wide DNS priority. `null` means use the snapshot's position order. */
export type EgressDnsOrderDraft = DestMatch[] | null;

type EgressDnsPolicy = {
  node: string;
  position: number;
  selector: DestMatch;
  resolution: EgressDnsResolution;
};

const sortedDnsPolicies = (policies: EgressDnsPolicy[]): EgressDnsPolicy[] =>
  [...policies].sort(
    (a, b) =>
      a.position - b.position || egressDnsSelectorKey(a.selector).localeCompare(egressDnsSelectorKey(b.selector)),
  );

function orderedDnsPolicies(policies: EgressDnsPolicy[], order: EgressDnsOrderDraft): EgressDnsPolicy[] {
  const baseline = sortedDnsPolicies(policies);
  if (!order) return baseline;
  const bySelector = new Map(baseline.map(policy => [egressDnsSelectorKey(policy.selector), policy]));
  const ordered = order.flatMap(selector => {
    const policy = bySelector.get(egressDnsSelectorKey(selector));
    if (!policy) return [];
    bySelector.delete(egressDnsSelectorKey(selector));
    return [policy];
  });
  return [...ordered, ...bySelector.values()];
}

function DnsPriorityControl({
  index,
  count,
  label,
  readOnly,
  showLabel = true,
  onMove,
}: {
  index: number;
  count: number;
  label: string;
  readOnly: boolean;
  showLabel?: boolean;
  onMove: (delta: number) => void;
}) {
  return (
    <span className="dns-priority" title="整台机器的 Xray DNS 匹配优先级">
      {showLabel && <b>D{index + 1}</b>}
      {!readOnly && count > 1 && (
        <span className="dns-priority-buttons">
          <button
            type="button"
            className="btn"
            disabled={index === 0}
            aria-label={`DNS 优先级上移（${label}）`}
            onClick={() => onMove(-1)}
          >
            ↑
          </button>
          <button
            type="button"
            className="btn"
            disabled={index === count - 1}
            aria-label={`DNS 优先级下移（${label}）`}
            onClick={() => onMove(1)}
          >
            ↓
          </button>
        </span>
      )}
    </span>
  );
}

/** 中转端口草稿的初始值：取自对端 step 上已有的配置，不存在时使用默认值。 */
// 中转端口的默认值。
// 对端已有中转端口时沿用；没有时选择该机器上空闲的端口，而非硬编码默认值。
// 走 overlay 时界面上不显示该字段（该类 inbound 绑定在 overlay 地址上，wg 之外无法访问，
// 端口取值不影响使用），但它仍会提交进模型、被 xray 绑定、参与端口冲突校验——
// 硬编码默认值会导致未经填写的取值占用其他配置的端口。
export function seedHops(peers: ForwardPeer[], taken?: Map<string, PortOwners>, base = HOP_PORT_BASE): HopsDraft {
  const seed: HopsDraft = {};
  for (const p of peers) {
    const h = p.step?.hop_in ?? null;
    const fallbackPort = taken ? freePortAcross(taken, [p.id], base) : base;
    seed[p.id] = {
      port: String(h?.port ?? fallbackPort),
      kind: h?.security.t ?? 'none',
      dest: h?.security.t === 'reality' ? h.security.v.dest : EMPTY_REALITY_SITE.dest,
      names: h?.security.t === 'reality' ? h.security.v.server_names.join(', ') : EMPTY_REALITY_SITE.names,
    };
  }
  return seed;
}

/** 新转发目标的中转端口初始值。`undefined` 表示该字段不修改（`hop_in` 缺省即表示不修改）。 */
// 写入新的转发目标时必须同时写入中转端口。编译器需要对端的 `hop_in.port` 才能
// 将该跳转换为 `地址:端口`（ir/hops.rs 的 compile_hops），缺少时会报 relay.no-hop-in，
// 而 `hop_in` 缺省表示不修改——新机器上不修改即表示始终不存在该配置。
// 这与连接方式无关：该检查排在 `dial_target` 之前，走 overlay 或直连都需要先满足它。
//
// 已有中转端口的一律不修改：重排主干顺序执行的是同一段代码，覆盖会替换已配置的
// 端口和传输层，导致该机器上的连接全部中断一次。
//
// 端口使用与 `seedHops` 相同的判定（`freePortAcross`，从 20000 起向上查找空闲端口），
// 因此写入的值与规则编辑器随后显示的一致。
export function seedHopIn(
  prev: SnapshotStep['hop_in'] | null | undefined,
  nodeId: string,
  taken: Map<string, PortOwners>,
  base = HOP_PORT_BASE,
): HopInRequest | undefined {
  if (prev) return undefined;
  return {
    port: freePortAcross(taken, [nodeId], base),
    // 明文。是否加密是该跳的策略，由规则编辑器中的下拉框控制，不应由「追加一跳」
    // 代为决定——走 overlay 时 wg 已对该跳加密，再加一层会增加无效的 CPU 开销。
    security: { t: 'none' },
  };
}

type RuleEditorProps = {
  appId: string;
  chainId: string;
  nodeId: string;
  initial: Rule[];
  /* 该机器已有的接受凭据。保存时必须原样回传，否则会被置为 NULL 并中断中继 */
  accept: StepAccept | null;
  // 该机器已有的中转端口。常规档位不使用它（配置的是对端的端口），反向接入需要：
  // 该档的端口开在本机，需要以当前值作为初始值，否则打开时即显示为已修改。
  hopIn?: SnapshotStep['hop_in'];
  // 转发目标的候选项，由 `forwardPeers` 计算——链外的机器也包含在内，指向链外即为分叉。
  // 地址和对端在该链上的当前状态都在其中：中转端口由连接它的那一跳配置，
  // 因此保存时需要同时写入对端的 step。
  peers: ForwardPeer[];
  /* 链内是否有其他节点转发给它——是则必须具备接受凭据（relay.no-accept） */
  isForwardTarget: boolean;
  // 整条链的当前状态和链头，用于计算保存后哪些节点将无引用（见 orphansAfter）。
  // 两者都提供时才执行该计算；缺少其一时只保存该表，不修改其他节点的 step。
  steps?: SnapshotStep[];
  root?: string;
  // 规则表末尾由编译器补全的规则。内容来自编译视图，不在浏览器里重新推算；
  // RuleEditor 只负责把它按普通规则的列布局渲染成不可编辑行。
  fallback?: { rules: Rule[]; pending: boolean };
  onClose?: () => void;
  /** 当前决策图中被圈出的监听子树；只改变视图，不打开或修改所有者规则。 */
  highlightedListener?: ListenerRef | null;
  onHighlightListener?: (listener: ListenerRef) => void;
  /* 由上层持有的共享草稿。不传入时由本组件自行管理（机器详情页中一台机器只出现一次，不需要共享）。 */
  shared?: {
    rules: Rule[];
    setRules: (next: Rule[]) => void;
    hops: HopsDraft;
    setHops: (next: HopsDraft) => void;
    dns: EgressDnsDraft;
    setDns: (next: EgressDnsDraft) => void;
    dnsOrder: EgressDnsOrderDraft;
    setDnsOrder: (next: EgressDnsOrderDraft) => void;
  };
  /** All unsaved rule tables in this chain, used to reject cycles before the tree is saved. */
  ruleDrafts?: Record<string, Rule[]>;
  // 是否参与 RuleDraftScope 的批量保存。同一台机器在树中出现多次、共用一份草稿时，
  // 只由其中一处注册——两处都注册会将同一内容写入两次。
  saves?: boolean;
  // 只读：readonly 角色看到的是同一张表，只是所有控件禁用、修改入口不渲染。
  // 不使用简化的展示形式——该机器的配置内容对只读角色和可编辑角色是同一项信息，
  // 两套展示意味着需要同步维护两处，而其中一套使用频率较低。
  // 禁用通过 `<fieldset disabled>` 实现：逐个控件添加 disabled 时，遗漏某个不会报错，
  // 表现为 readonly 角色可以修改，点击保存后才返回 403。
  readOnly?: boolean;
};

// 将依赖门放在持有表单 state 的组件之外：只有端口起点、占用表和编译结果都就绪后才挂载
// 真正的编辑器。若在同一组件中先用 fallback 初始化 useState、随后才显示 Loading，错误默认值
// 仍会被永久冻结在 state 里。
export function RuleEditor(props: RuleEditorProps) {
  const readOnly = props.readOnly ?? false;
  const snapshot = useQuery({ queryKey: ['snapshot'], queryFn: () => fetchSnapshot() });
  const nodes = useQuery({ queryKey: ['nodes'], queryFn: () => fetchNodes() });
  const settings = useQuery({ queryKey: ['settings'], queryFn: () => fetchSettings(), enabled: !readOnly });
  const revisions = useQuery({ queryKey: ['revisions'], queryFn: () => fetchRevisions(), enabled: !readOnly });
  const current = revisions.data?.current_revision;
  const compiled = useQuery({
    queryKey: ['compile', current],
    queryFn: () => fetchCompileView(current!),
    enabled: !readOnly && !!current,
  });
  const pending =
    snapshot.isPending ||
    nodes.isPending ||
    (!readOnly && (settings.isPending || revisions.isPending || (current != null && compiled.isPending)));
  const error =
    snapshot.error ?? nodes.error ?? (!readOnly ? (settings.error ?? revisions.error ?? compiled.error) : null);
  if (pending) return <Loading />;
  const blockingError =
    (!snapshot.data && snapshot.error) ||
    (!nodes.data && nodes.error) ||
    (!readOnly && ((!settings.data && settings.error) || (!revisions.data && revisions.error))) ||
    (!readOnly && current != null && !compiled.data && compiled.error);
  if (blockingError) return <ErrorBox error={blockingError} />;
  // 后台刷新失败但仍有完整缓存时保留内容供核对，同时禁用整棵表单；不能把旧数据当成
  // 最新结果继续写，也不应把正在查看的规则整页替换掉。
  return (
    <fieldset disabled={!!error} style={{ border: 0, margin: 0, padding: 0, minWidth: 0 }}>
      {error && <ErrorBox error={error} />}
      <RuleEditorReady {...props} editorDisabled={!!error} />
    </fieldset>
  );
}

function RuleEditorReady({
  appId,
  chainId,
  nodeId,
  initial,
  accept,
  peers,
  isForwardTarget,
  steps,
  root,
  hopIn: selfHopIn = null,
  fallback,
  onClose,
  highlightedListener = null,
  onHighlightListener,
  shared,
  ruleDrafts,
  saves = true,
  readOnly = false,
  editorDisabled,
}: RuleEditorProps & { editorDisabled: boolean }) {
  const qc = useQueryClient();
  const snapshot = useQuery({ queryKey: ['snapshot'], queryFn: () => fetchSnapshot() });
  const app = snapshot.data?.snapshot.apps.find(candidate => candidate.id === appId);
  const chainTenant = app?.chains.find(chain => chain.id === chainId)?.tenant ?? '';
  const externalOutbounds = (snapshot.data?.snapshot.external_outbounds ?? []).filter(
    outbound => chainTenant === outbound.tenant || chainTenant.startsWith(`${outbound.tenant}.`),
  );
  const globalRelayMux = snapshot.data?.snapshot.settings?.relay_mux ?? DEFAULT_HOP_MUX;
  const [muxEditor, setMuxEditor] = useState<MuxEditorState | null>(null);
  const [targetPickerRule, setTargetPickerRule] = useState<number | null>(null);
  const [targetPickerView, setTargetPickerView] = useState<TargetPickerView>({ t: 'targets' });
  const [targetQuery, setTargetQuery] = useState('');
  const targetPickerRoot = useRef<HTMLSpanElement>(null);
  const targetMenu = useRef<HTMLFieldSetElement>(null);
  const [targetMenuPlacement, setTargetMenuPlacement] = useState<TargetMenuPlacement>({
    below: false,
    left: 0,
    top: 0,
    width: TARGET_MENU_WIDTH,
    maxHeight: TARGET_MENU_MAX_HEIGHT,
  });
  useEffect(() => {
    if (targetPickerRule === null) return;
    const closeOutside = (event: PointerEvent) => {
      if (event.target instanceof Node && targetPickerRoot.current?.contains(event.target)) return;
      if (event.target instanceof Node && targetMenu.current?.contains(event.target)) return;
      setTargetPickerRule(null);
    };
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === 'Escape') setTargetPickerRule(null);
    };
    document.addEventListener('pointerdown', closeOutside, true);
    document.addEventListener('keydown', closeOnEscape);
    return () => {
      document.removeEventListener('pointerdown', closeOutside, true);
      document.removeEventListener('keydown', closeOnEscape);
    };
  }, [targetPickerRule]);
  /* 这是可搜索、可分组且带「新建」动作的自定义菜单，浏览器不会像处理原生
     <select> 那样替它选择展开方向。点击时先按可用空间选择一次方向；展开后搜索只更新
     可用高度，不跨过触发器翻面。滚动或缩放使原方向不可用时才切换。 */
  useLayoutEffect(() => {
    if (targetPickerRule === null) return;
    const place = () => {
      const root = targetPickerRoot.current;
      const menu = targetMenu.current;
      if (!root || !menu) return;
      const rootRect = root.getBoundingClientRect();
      const wanted = Math.min(TARGET_MENU_MAX_HEIGHT, Math.max(TARGET_MENU_MIN_HEIGHT, menu.scrollHeight + 2));
      setTargetMenuPlacement(current => {
        // The click handler chooses the side before mounting. Keep it while filtering changes the
        // menu height; moving a live menu across the trigger looks like browser focus jumped.
        const next = placeTargetMenu(rootRect, current.below, wanted);
        return current.below === next.below &&
          current.left === next.left &&
          current.top === next.top &&
          current.width === next.width &&
          current.maxHeight === next.maxHeight
          ? current
          : next;
      });
    };
    place();
    window.addEventListener('resize', place);
    window.addEventListener('scroll', place, true);
    window.visualViewport?.addEventListener('resize', place);
    window.visualViewport?.addEventListener('scroll', place);
    return () => {
      window.removeEventListener('resize', place);
      window.removeEventListener('scroll', place, true);
      window.visualViewport?.removeEventListener('resize', place);
      window.visualViewport?.removeEventListener('scroll', place);
    };
  }, [
    targetPickerRule,
    targetPickerView,
    targetQuery,
    externalOutbounds.length,
    peers.length,
    snapshot.data?.snapshot.apps,
  ]);
  // 两份草稿（规则表、各转发目标的中转端口）默认由本组件持有；`shared` 非空时交由上层持有，
  // 因为同一台机器在树中出现两次时，两处编辑的必须是同一份——它们对应库中的同一条记录
  // （steps 主键为 chain_id + node_id）。
  const ownRules = useState<Rule[]>(() => pinTerminalRules(initial));
  const [rules, setRules] = shared ? [shared.rules, shared.setRules] : ownRules;
  const nodeList = useQuery({ queryKey: ['nodes'], queryFn: () => fetchNodes() });
  const selfNode = nodeList.data?.nodes.find(n => n.node_id === nodeId);
  // Only local overrides live in the editor. The query remains the canonical baseline and is
  // replaced by the server-side draft preview after saving, so two chain pages never maintain
  // copied policy state of their own.
  const ownDnsOverrides = useState<EgressDnsDraft>({});
  const [dnsOverrides, setDnsOverrides] = shared ? [shared.dns, shared.setDns] : ownDnsOverrides;
  const ownDnsOrder = useState<EgressDnsOrderDraft>(null);
  const [dnsOrder, setDnsOrder] = shared ? [shared.dnsOrder, shared.setDnsOrder] : ownDnsOrder;
  const nodeDnsPolicies = sortedDnsPolicies(
    (snapshot.data?.node_egress_dns ?? []).filter(policy => policy.node === nodeId),
  );
  const storedDnsFor = (selector: DestMatch): EgressDnsResolution | null =>
    nodeDnsPolicies.find(policy => egressDnsSelectorKey(policy.selector) === egressDnsSelectorKey(selector))
      ?.resolution ?? null;
  const effectiveDnsFor = (selector: DestMatch): EgressDnsResolution | null => {
    const override = dnsOverrides[egressDnsSelectorKey(selector)];
    return override ? override.resolution : storedDnsFor(selector);
  };
  const patchMachineDns = (selector: DestMatch, resolution: EgressDnsResolution | null) => {
    const key = egressDnsSelectorKey(selector);
    const next = { ...dnsOverrides };
    if (JSON.stringify(resolution) === JSON.stringify(storedDnsFor(selector))) delete next[key];
    else next[key] = { selector, resolution };
    setDnsOverrides(next);
  };
  // A policy created from an egress row does not exist in the snapshot until the draft is saved.
  // Include it in the shared machine rows immediately; otherwise selecting “引用” appears to do
  // nothing now that resolver parameters live in their own row instead of inside the route row.
  const pendingDnsPolicies: EgressDnsPolicy[] = Object.values(dnsOverrides).flatMap((change, index) => {
    if (!change.resolution || storedDnsFor(change.selector)) return [];
    return [
      {
        node: nodeId,
        position: nodeDnsPolicies.length + index,
        selector: change.selector,
        resolution: change.resolution,
      },
    ];
  });
  const orderedNodeDnsPolicies = orderedDnsPolicies([...nodeDnsPolicies, ...pendingDnsPolicies], dnsOrder);
  const dnsPolicyIndex = (selector: DestMatch) =>
    orderedNodeDnsPolicies.findIndex(
      policy => egressDnsSelectorKey(policy.selector) === egressDnsSelectorKey(selector),
    );
  const moveMachineDns = (selector: DestMatch, delta: number) => {
    const index = dnsPolicyIndex(selector);
    const target = index + delta;
    if (index < 0 || target < 0 || target >= orderedNodeDnsPolicies.length) return;
    const next = orderedNodeDnsPolicies.map(policy => policy.selector);
    [next[index], next[target]] = [next[target], next[index]];
    setDnsOrder(next);
  };
  const peerOf = (id: string) => peers.find(p => p.id === id) ?? null;
  const snapshotApps = snapshot.data?.snapshot.apps ?? [];
  const listeners = app
    ? reusableListeners({
        apps: snapshotApps,
        sourceApp: appId,
        sourceChain: chainId,
        sourceNode: nodeId,
        sourceRules: rules,
        sourceDrafts: ruleDrafts,
        nodes: nodeList.data?.nodes ?? [],
      })
    : [];
  const listenerOf = (listener: ListenerRef) =>
    listeners.find(candidate => candidate.key === listenerRefKey(listener)) ?? null;

  // 下拉框按关系对可选目标分组；不可选的同样保留在列表中并说明原因，直接隐藏时
  // 该机器会从列表中消失，需要到其他位置查找。
  const nextPeers = peers.filter(p => !p.blocked && p.where === 'next');
  const insidePeers = peers.filter(p => !p.blocked && p.where === 'inside');
  const forkPeers = peers.filter(p => !p.blocked && p.where === 'outside');
  const blockedPeers = peers.filter(p => p.blocked);
  const selectable = [...nextPeers, ...insidePeers, ...forkPeers];
  const targetMatches = (...parts: Array<string | null | undefined>) =>
    !targetQuery.trim() || parts.join(' ').toLowerCase().includes(targetQuery.trim().toLowerCase());
  const visibleNextPeers = nextPeers.filter(peer => targetMatches(peer.id, peer.name));
  const visibleInsidePeers = insidePeers.filter(peer => targetMatches(peer.id, peer.name));
  const visibleForkPeers = forkPeers.filter(peer => targetMatches(peer.id, peer.name));
  const visibleBlockedPeers = blockedPeers.filter(peer => targetMatches(peer.id, peer.name, peer.blocked));
  // 本链监听已经作为普通 Forward 目标列在下一组。若这里再以 ReuseListener 提供一次，
  // 同一个 (chain, node) 会出现两种动作形状；它们编译到同一 outbound，却永远是两种 dial，
  // 最终只能等编译器报 rule.forward-dial-conflict。跨链监听才需要显式引用身份。
  const reusableListenerChoices = listeners.filter(listener => listener.ref.chain !== chainId);
  const visibleListeners = reusableListenerChoices.filter(
    listener =>
      (targetPickerView.t !== 'listener-endpoints' || listener.ref.chain === targetPickerView.chain) &&
      targetMatches(
        listener.appId,
        listener.appName,
        listener.ref.chain,
        listener.ref.node,
        listener.nodeName,
        listener.ownerName,
        String(listener.step.hop_in?.port ?? ''),
        listener.blocked,
      ),
  );
  const visibleListenerApps = snapshotApps.flatMap(candidateApp => {
    const chains = candidateApp.chains.flatMap(candidateChain => {
      if (candidateChain.id === chainId) return [];
      const candidates = reusableListenerChoices.filter(listener => listener.ref.chain === candidateChain.id);
      if (candidates.length === 0) return [];
      const matches = targetMatches(candidateApp.id, candidateApp.label, candidateChain.id, candidateChain.name)
        ? true
        : candidates.some(listener =>
            targetMatches(
              listener.nodeName,
              listener.ref.node,
              String(listener.step.hop_in?.port ?? ''),
              listener.blocked,
            ),
          );
      return matches ? [{ chain: candidateChain, candidates }] : [];
    });
    return chains.length > 0 ? [{ app: candidateApp, chains }] : [];
  });
  const selectedListenerChain =
    targetPickerView.t === 'listener-endpoints'
      ? (snapshotApps
          .flatMap(candidateApp =>
            candidateApp.chains.map(candidateChain => ({ app: candidateApp, chain: candidateChain })),
          )
          .find(candidate => candidate.chain.id === targetPickerView.chain) ?? null)
      : null;
  const visibleExternalOutbounds = externalOutbounds.filter(outbound =>
    targetMatches(outbound.id, outbound.name, outbound.address, externalProtocolLabel(outbound.protocol.t)),
  );
  /* 新增转发规则时的默认目标。优先使用主干下一跳：它是沿链继续的默认路径。 */
  const defaultTarget = selectable[0]?.id ?? '';

  // 该规则表的转发目标。中转端口按目标配置而非按规则配置：同一个对端可以被多条
  // 规则指向，但连接方式必须一致；不同地址意味着不同的逻辑边，应拆分 chain 或更换目标。
  const forwardTargets = [...new Set(rules.flatMap(r => (r.a.t === 'forward' && r.a.to ? [r.a.to] : [])))];
  // 反向接入的目标：端口开在本机，由它们连接进来，因此不进入下方的对端中转入口表——
  // 为它们各分配一个端口会占用无用端口，并进入端口冲突校验。
  const reverseTargets = forwardTargets.filter(to =>
    rules.some(r => r.a.t === 'forward' && r.a.to === to && forwardDial(r.a).t === 'reverse'),
  );
  const normalTargets = forwardTargets.filter(to => !reverseTargets.includes(to));
  const referencedTargets = [
    ...new Map(
      rules.flatMap(rule =>
        rule.a.t === 'reuse_listener' ? [[listenerRefKey(rule.a.listener), rule.a.listener] as const] : [],
      ),
    ).values(),
  ];

  // 每个目标对应一份中转端口表单状态。初始值取自对端 step 上已有的配置。
  // 将 `seedHops` 提取并导出，是因为同一台机器可能在树中出现两次（分叉后汇合），
  // 此时草稿由上层持有并共享，初始值需要由上层计算（见 ChainRulesPanel）。
  const portPool = usePortPool(!readOnly);
  const hopBase = useHopPortBase(!readOnly);
  const overlayOf = useOverlayAddrs(!readOnly);
  // 反向两档显示的是本机的对外地址（由编译器推导，此处只是同步显示）。
  // 查询键与其他位置一致，通常命中缓存。标记为 NAT 的不计入——该类地址无法接受反向接入，
  // 编译器的 `dialable_public_host` 判定相同，两处判定需保持一致。
  /* 本机的公网地址，反向两档判断对端能否连接本机时需要它。 */
  const selfAddrs = selfNode
    ? {
        public_ipv4: selfNode.public_ipv4,
        public_ipv6: selfNode.public_ipv6,
        public_ipv4_nat: selfNode.public_ipv4_nat,
        public_ipv6_nat: selfNode.public_ipv6_nat,
      }
    : null;
  const selfPublicHostOf = (family: 'v4' | 'v6') =>
    family === 'v6'
      ? (!selfNode?.public_ipv6_nat && selfNode?.public_ipv6) || ''
      : (!selfNode?.public_ipv4_nat && selfNode?.public_ipv4) || '';
  // `fallback` comes from a compiled table. A locally written Any may still be present in the
  // last fetched compilation; it is not a second row in this editor.
  const visibleFallbackRules = (fallback?.rules ?? []).filter(rule => {
    if (rules.some(written => JSON.stringify(written) === JSON.stringify(rule))) return false;
    return true;
  });
  const displayedRuleCount = rules.length;
  const ownHops = useState<HopsDraft>(() => seedHops(peers, portPool, hopBase));
  const [hops, setHops] = shared ? [shared.hops, shared.setHops] : ownHops;
  // `seedHops` 只初始化转发目标，不包含本机——反向接入时端口开在本机，
  // 因此走该回退分支。选择空闲端口而非硬编码 20000，原因同 seedHops：硬编码会导致
  // 未经填写的取值占用其他配置的端口。
  const hopOf = (id: string) =>
    hops[id] ??
    (id === nodeId && selfHopIn
      ? {
          port: String(selfHopIn.port),
          kind: selfHopIn.security.t,
          dest: selfHopIn.security.t === 'reality' ? selfHopIn.security.v.dest : EMPTY_REALITY_SITE.dest,
          names:
            selfHopIn.security.t === 'reality'
              ? selfHopIn.security.v.server_names.join(', ')
              : EMPTY_REALITY_SITE.names,
        }
      : {
          port: String(freePortAcross(portPool, [id], hopBase)),
          kind: 'none' as const,
          dest: EMPTY_REALITY_SITE.dest,
          names: EMPTY_REALITY_SITE.names,
        });
  const patchHop = (id: string, next: Partial<ReturnType<typeof hopOf>>) =>
    setHops({ ...hops, [id]: { ...hopOf(id), ...next } });
  const setHopPort = (id: string, port: string) => {
    patchHop(id, { port });
    const nextPort = Number(port) || 0;
    if (nextPort <= 0 || nextPort > 65535) return;
    setRules(
      rules.map(rule => {
        if (rule.a.t !== 'forward' || rule.a.to !== id) return rule;
        const dial = forwardDial(rule.a);
        if (dial.t !== 'addr') return rule;
        return {
          ...rule,
          a: {
            ...rule.a,
            dial: { t: 'addr', v: formatHostPort(hostOf(dial), nextPort) },
          },
        };
      }),
    );
  };

  const hopInBody = (id: string) => {
    const h = hopOf(id);
    return {
      port: Number(h.port) || 0,
      security:
        h.kind === 'reality'
          ? {
              t: 'reality' as const,
              v: {
                dest: h.dest.trim(),
                server_names: h.names
                  .split(/[,\s]+/)
                  .map(v => v.trim())
                  .filter(Boolean),
              },
            }
          : h.kind === 'encryption'
            ? { t: 'encryption' as const }
            : h.kind === 'shadowsocks2022'
              ? { t: 'shadowsocks2022' as const }
              : { t: 'none' as const },
    };
  };

  const dnsChanges = Object.values(dnsOverrides).filter(
    change => JSON.stringify(change.resolution) !== JSON.stringify(storedDnsFor(change.selector)),
  );
  const dnsOrderDirty =
    orderedNodeDnsPolicies.map(policy => egressDnsSelectorKey(policy.selector)).join('\0') !==
    nodeDnsPolicies.map(policy => egressDnsSelectorKey(policy.selector)).join('\0');
  const structuralDirty =
    JSON.stringify(rules) !== JSON.stringify(initial) ||
    (isForwardTarget && !accept) ||
    normalTargets.some(to => {
      const peer = peerOf(to);
      return peer ? hopInChanged(hopInBody(to), peer.step?.hop_in ?? null) : false;
    }) ||
    /* 反向接入的端口开在本机，修改它同样需要启用保存按钮 */
    (reverseTargets.length > 0 && hopInChanged(hopInBody(nodeId), selfHopIn));
  const dirty = structuralDirty || dnsChanges.length > 0 || dnsOrderDirty;

  const bus = useContext(RuleDraftCtx);
  const save = useMutation({
    mutationFn: async (_opts?: { keepOpen?: boolean }) => {
      let saved: unknown = { revision_id: 0 };
      if (structuralDirty) {
        // 先写入对端的中转端口，再写入本机的规则。顺序不可颠倒：规则中已引用该端口，
        // 先写入规则时，中间时段的编译结果为 relay.no-hop-in。
        for (const to of forwardTargets) {
          const peer = peerOf(to);
          if (!peer) continue;
          await putStep(appId, chainId, to, {
            // 对端自身的规则原样回传，不清空其规则表。
            // 分叉到链外时对端尚无 step，此处写入空表——不为其补全出网规则：
            // 该机器的 egress_allowed 可能为假，补全会导致 step.egress-denied。空表交由
            // 编译器按其规则补全，应补 Egress 时补 Egress，应补 Block 时补 Block。
            rules: peer.step?.rules ?? [],
            accept: peer.step?.accept ? { uuid: peer.step.accept.uuid, label: peer.step.accept.label } : {},
            // 反向档下对端不监听，中转端口开在本机（由下面的 putStep 写入）。为其也写入一个
            // 只会占用该机器上的一个无用端口，并进入端口冲突校验。
            ...(reverseTargets.includes(to) ? {} : { hop_in: hopInBody(to) }),
          });
        }
        saved = await putStep(appId, chainId, nodeId, {
          rules,
          /* 已有则原样回传（label 是统计键，不能变更）；不存在但被转发指向时由 store 生成一份 */
          ...(accept ? { accept: { uuid: accept.uuid, label: accept.label } } : isForwardTarget ? { accept: {} } : {}),
          // 存在反向接入的下游时，该端口开在本机——下游连接的即是它。链头同样需要开启：
          // 编译器为该档位放宽了链头不配置中转端口的限制（ir/routing.rs）。
          ...(reverseTargets.length > 0 ? { hop_in: hopInBody(nodeId) } : {}),
        });
        // 无引用的机器由服务端统一清理一次。在树中时不在此处追加该操作：其他表尚未保存，
        // 此时的链不完整，服务端按其计算会移除刚在另一张表中建立连接的机器。树中的清理
        // 由 RuleDraftScope 在所有表写入完成后执行，此处只处理画布浮层的单表编辑场景。
        if (!bus) await pruneChain(appId, chainId);
      }
      // This operation is independent from put_step. A DNS-only edit therefore adds only a
      // machine policy to the draft and cannot claim the currently open chain as its owner.
      for (const change of dnsChanges) {
        await setNodeEgressDns(nodeId, change.selector, change.resolution);
      }
      if (dnsOrderDirty) {
        const selectors = orderedNodeDnsPolicies
          .filter(policy => effectiveDnsFor(policy.selector) !== null)
          .map(policy => policy.selector);
        for (const change of dnsChanges) {
          if (
            change.resolution &&
            !selectors.some(selector => egressDnsSelectorKey(selector) === egressDnsSelectorKey(change.selector))
          ) {
            selectors.push(change.selector);
          }
        }
        await reorderNodeEgressDns(nodeId, selectors);
      }
      return saved;
    },
    onSuccess: (_data, opts) => {
      // Every DNS override in this editor has just been copied into the global browser draft.
      // Keeping a second local copy makes "discard draft" reveal it again against the committed
      // baseline, and lets an older chain page overwrite a newer machine-wide edit.
      setDnsOverrides({});
      setDnsOrder(null);
      qc.invalidateQueries({ queryKey: ['snapshot'] });
      qc.invalidateQueries({ queryKey: ['revisions'] });
      qc.invalidateQueries({ queryKey: ['compile'] });
      /* 删除规则的路径自行保存，不应同时关闭编辑器——操作仍在该表内进行。 */
      if (!opts?.keepOpen) onClose?.();
    },
  });

  const handleId = useId();
  const rulesKey = JSON.stringify(rules);
  const stepsKey = JSON.stringify(steps ?? []);
  // The tree can construct a fresh empty array while a newly referenced step has not been saved
  // yet. Register canonical copies keyed by content so a scope update does not turn that harmless
  // new reference into an update/render loop.
  const registeredRules = useMemo(() => JSON.parse(rulesKey) as Rule[], [rulesKey]);
  const registeredSteps = useMemo(() => JSON.parse(stepsKey) as SnapshotStep[], [stepsKey]);
  const saveAsync = save.mutateAsync;
  const handle = useMemo<RuleDraftHandle>(
    () => ({
      id: handleId,
      dirty,
      structuralDirty,
      save: () => saveAsync({}),
      appId,
      chainId,
      nodeId,
      root,
      steps: registeredSteps,
      rules: registeredRules,
    }),
    [handleId, dirty, structuralDirty, saveAsync, appId, chainId, nodeId, root, registeredSteps, registeredRules],
  );
  // 计算无引用节点需要整条链的规则表，因此草稿和链的当前状态都交由总线管理
  // （见 RuleDraftScope）。各表自行计算时无法读取其他表的草稿——这是此前误删的原因。
  // 只读时不注册到总线：注册后末尾的按钮会显示为存在待保存的改动，
  // 而该状态下没有任何可修改的入口。
  useEffect(() => (saves && !readOnly ? bus?.attach(handleId) : undefined), [bus, handleId, saves, readOnly]);
  useEffect(() => {
    if (saves && !readOnly) bus?.update(handle);
  }, [bus, handle, saves, readOnly]);

  const patch = (i: number, next: Rule) => {
    const updated = rules.map((rule, index) => (index === i ? next : rule));
    setRules(pinTerminalRules(updated));
  };
  const patchMatch = (i: number, rule: Rule, match: DestMatch) =>
    patch(i, {
      ...rule,
      m: match,
    });
  const move = (i: number, delta: number) => {
    const j = i + delta;
    if (j < 0 || j >= rules.length) return;
    // Both terminals are compiler invariants, not user-sortable rows. Ordinary rules cannot move
    // below the failure fallback or Any.
    if (isPinnedTerminalRule(rules[i]) || (delta > 0 && isPinnedTerminalRule(rules[j]))) return;
    const next = [...rules];
    [next[i], next[j]] = [next[j], next[i]];
    setRules(next);
  };

  const setDialForTarget = (to: string, dial: HopDial, primaryIndex: number) => {
    setRules(
      rules.map((rule, idx) =>
        idx === primaryIndex || (rule.a.t === 'forward' && rule.a.to === to)
          ? // 出站连接跟着一起写回去，否则换一次拨号方式就把它抹了——而换拨号和
            // 修改连接方式与此无关。唯一需要清除的是切换到反向档：此时本机不再发起连接，
            // 保留的取值指向一个不存在的出站（编译器会报 rule.pool-on-reverse）。
            { ...rule, a: forwardAction(to, dial, forwardPool(rule.a)) }
          : rule,
      ),
    );
  };

  // 该跳的出站连接配置。与连接方式一样按目标取值：指向同一对端的规则共用一个 outbound，
  // 因此取任意一条结果相同（不一致时由编译器报 rule.forward-pool-conflict）。
  const poolOf = (to: string): HopPool => {
    const rule = rules.find(r => r.a.t === 'forward' && r.a.to === to);
    return rule ? forwardPool(rule.a) : { t: 'none' };
  };

  // 与连接方式一样按目标统一：一个 from -> to 只对应一个 outbound，两条规则不能有两种连接方式。
  const setPoolForTarget = (to: string, pool: HopPool) => {
    setRules(
      rules.map(rule => (rule.a.t === 'forward' && rule.a.to === to ? { ...rule, a: { ...rule.a, pool } } : rule)),
    );
  };

  const sameListener = (left: ListenerRef, right: ListenerRef) =>
    left.chain === right.chain && left.node === right.node;
  const referencePoolOf = (listener: ListenerRef): HopPool => {
    const rule = rules.find(r => r.a.t === 'reuse_listener' && sameListener(r.a.listener, listener));
    return rule ? forwardPool(rule.a) : { t: 'none' };
  };
  const setPoolForListener = (listener: ListenerRef, pool: HopPool) => {
    setRules(
      rules.map(rule =>
        rule.a.t === 'reuse_listener' && sameListener(rule.a.listener, listener)
          ? { ...rule, a: { ...rule.a, pool } }
          : rule,
      ),
    );
  };
  const setDialForListener = (listener: ListenerRef, dial: ListenerDial, primaryIndex: number) => {
    setRules(
      rules.map((rule, index) =>
        index === primaryIndex || (rule.a.t === 'reuse_listener' && sameListener(rule.a.listener, listener))
          ? { ...rule, a: reuseListenerAction(listener, dial, forwardPool(rule.a)) }
          : rule,
      ),
    );
  };

  /* 判定实现在模块层的 `hopDialOf` 中，与建链向导共用同一份。 */
  const dialOf = (to: string, kind: DialKind): HopDial =>
    hopDialOf(kind, peerOf(to), Number(hopOf(to).port) || hopBase);

  /* 判定实现在模块层的 `defaultHopDial` 中，与建链向导共用同一份。 */
  const defaultDial = (to: string): HopDial =>
    defaultHopDial({ peer: peerOf(to), self: selfAddrs, port: Number(hopOf(to).port) || hopBase });
  const publicAddrsFor = (targetNode: string): PublicAddrs | null =>
    peerOf(targetNode) ?? nodeList.data?.nodes.find(node => node.node_id === targetNode) ?? null;
  const dialOfListener = (_listener: ListenerRef, kind: ListenerDialKind): ListenerDial => listenerDialOf(kind);
  const defaultDialForListener = (listener: ListenerRef): ListenerDial => {
    const target = listenerOf(listener);
    if (target?.local) return { t: 'overlay' };
    return defaultListenerDial(publicAddrsFor(listener.node));
  };

  const defaultRuleAction = (): RuleAction =>
    defaultTarget ? forwardAction(defaultTarget, defaultDial(defaultTarget)) : { t: 'egress', send_through: null };

  // 切换档位时重新计算地址。连接方式按目标统一：同一个 from -> to 只对应一个
  // outbound/tag，各规则不能使用不同的地址。
  const setDial = (i: number, to: string, kind: DialKind) => {
    const dial = dialOf(to, kind);
    setDialForTarget(to, dial, i);
  };

  const setReferenceDial = (i: number, listener: ListenerRef, kind: ListenerDialKind) => {
    setDialForListener(listener, dialOfListener(listener, kind), i);
  };

  const selectForwardTarget = (ruleIndex: number, rule: Rule, nextTo: string) => {
    const sameTarget = rules.find(
      (candidate, index) => index !== ruleIndex && candidate.a.t === 'forward' && candidate.a.to === nextTo,
    );
    let nextDial: HopDial = defaultDial(nextTo);
    if (sameTarget?.a.t === 'forward') nextDial = sameTarget.a.dial ?? { t: 'overlay' };
    patch(ruleIndex, { ...rule, a: forwardAction(nextTo, nextDial, poolOf(nextTo)) });
    setTargetPickerRule(null);
    setTargetPickerView({ t: 'targets' });
  };

  const selectListenerTarget = (ruleIndex: number, rule: Rule, listener: ReusableListener) => {
    const sameTarget = rules.find(
      (candidate, index) =>
        index !== ruleIndex && candidate.a.t === 'reuse_listener' && sameListener(candidate.a.listener, listener.ref),
    );
    const dial = sameTarget?.a.t === 'reuse_listener' ? sameTarget.a.dial : defaultDialForListener(listener.ref);
    patch(ruleIndex, {
      ...rule,
      a: reuseListenerAction(listener.ref, dial, referencePoolOf(listener.ref)),
    });
    setTargetPickerRule(null);
    setTargetPickerView({ t: 'targets' });
  };

  const selectExternalTarget = (ruleIndex: number, rule: Rule, outbound: string) => {
    patch(ruleIndex, { ...rule, a: { t: 'proxy', outbound } });
    setTargetPickerRule(null);
    setTargetPickerView({ t: 'targets' });
  };

  // 删除规则时立即写入草稿，不等待末尾的「保存到草稿」。修改常处于中间状态（已选匹配条件
  // 但未选动作），累积后统一保存是合理的；而删除是一次完成的操作，且会连带将无引用的机器
  // 移出链——该结果只有实际写入并重新渲染树之后才能看到。需要经过一轮 state 更新后再保存：
  // `save` 的 mutationFn 及其依赖的派生值都从渲染闭包读取，在 onClick 中直接调用
  // 会使用删除前的数据。
  const [flushing, setFlushing] = useState(false);
  useEffect(() => {
    if (!flushing) return;
    save.mutate(
      { keepOpen: true },
      {
        onSettled: () => setFlushing(false),
      },
    );
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [flushing]);

  const addRule = () => {
    const terminalIndex = rules.findIndex(isPinnedTerminalRule);
    const hasAny = rules.some(isAnyRule);
    const next: Rule = {
      // 已有终结规则时，新行必须插在它之前；再创建一个 Any 会让原兜底之后的内容永远不可达。
      m: hasAny ? { t: 'domain_suffix', v: [] } : { t: 'any' },
      a: defaultRuleAction(),
    };
    const updated = [...rules];
    updated.splice(terminalIndex >= 0 ? terminalIndex : updated.length, 0, next);
    setRules(pinTerminalRules(updated));
  };

  const hasSniffingDependentRule = rules.some(rule => matchDependsOnSniffing(rule.m));
  const hasSniffingFallback = rules.some(isSniffingFallbackRule);
  const addSniffingFallback = () =>
    setRules(pinTerminalRules([...rules, { m: { t: 'sniffing_failed' }, a: defaultRuleAction() }]));

  return (
    <>
      {/* 使用 `fieldset` 仅为其 disabled 属性（它是 HTML 中唯一能一次禁用整棵子树
          表单控件的元素），因此样式上重置为无视觉效果的一层，见 styles.css 的 .rule-ro。 */}
      <fieldset className="panel rule-editor rule-ro" disabled={readOnly}>
        <div className="toolbar" style={{ marginBottom: 6 }}>
          <b className="mono">
            {chainId} / {nodeId}
          </b>
          <span className="note">DNS 策略按机器生效；线路只提供编辑入口</span>
          <span className="sp" />
          {onClose && (
            <button className="btn" onClick={onClose}>
              关闭
            </button>
          )}
        </div>

        {(nodeDnsPolicies.length > 0 || rules.some(rule => rule.a.t === 'egress' && supportsEgressDns(rule.m))) && (
          <p className="note node-egress-dns-limit">
            Xray 的 DNS 选择不携带原路由和出站上下文；DNS 查询可以指定出口，但解析结果无法按出站隔离。
          </p>
        )}

        <table className="tbl rule-table">
          <tbody>
            {rules.map((r, i) => {
              const kind = MATCH_KINDS.find(k => k.t === r.m.t);
              const to = r.a.t === 'forward' ? r.a.to : r.a.t === 'reuse_listener' ? r.a.listener.node : '';
              const referenced = r.a.t === 'reuse_listener' ? listenerOf(r.a.listener) : null;
              const externalId = r.a.t === 'proxy' ? r.a.outbound : '';
              const dial = forwardDial(r.a);
              const peer = peerOf(to);
              const targetAddrs = r.a.t === 'reuse_listener' ? publicAddrsFor(to) : peer;
              const dk = r.a.t === 'reuse_listener' ? listenerDialKindOf(r.a.dial) : dialKindOf(dial, targetAddrs);
              const customHost = r.a.t === 'reuse_listener' ? listenerHostOf(r.a.dial) : hostOf(dial);
              const external = externalOutbounds.find(outbound => outbound.id === externalId) ?? null;
              const targetBadge = external
                ? externalProtocolBadge(external.protocol.t)
                : r.a.t === 'reuse_listener'
                  ? 'LISTENER'
                  : r.a.t === 'forward'
                    ? 'NODE'
                    : '';
              const targetLabel =
                external?.name ||
                (r.a.t === 'reuse_listener'
                  ? referenced
                    ? `${referenced.nodeName} · TCP ${referenced.step.hop_in?.port}`
                    : `${r.a.listener.chain}/${r.a.listener.node} · 引用不可用`
                  : peer?.name) ||
                (r.a.t === 'proxy' ? '代理出站不可用' : to ? '内部节点不可用' : '');
              return (
                <Fragment key={i}>
                  <tr>
                    <td className="mono dim" style={{ width: 24 }}>
                      {i + 1}
                    </td>
                    <td className="rule-match-cell">
                      <select
                        className="f"
                        value={r.m.t}
                        onChange={e => {
                          const m = buildMatch(e.target.value as DestMatch['t'], '');
                          patchMatch(i, r, m);
                        }}
                      >
                        {MATCH_KINDS.map(k => (
                          <option
                            key={k.t}
                            value={k.t}
                            disabled={
                              (k.t === 'any' && r.m.t !== 'any' && rules.some(isAnyRule)) ||
                              (k.t === 'sniffing_failed' &&
                                r.m.t !== 'sniffing_failed' &&
                                rules.some(isSniffingFallbackRule))
                            }
                          >
                            {k.label}
                          </option>
                        ))}
                      </select>
                      {kind?.list !== false && r.m.t !== 'any' && r.m.t !== 'front_downstream' && (
                        <input
                          className="f"
                          placeholder={kind?.hint}
                          value={matchValues(r.m)}
                          onChange={e => patchMatch(i, r, buildMatch(r.m.t, e.target.value))}
                        />
                      )}
                    </td>
                    <td className="rule-action-cell">
                      <select
                        className={`f rule-action-select rule-action-${ruleActionTone(r.a.t)}`}
                        value={r.a.t === 'proxy' || r.a.t === 'reuse_listener' ? 'forward' : r.a.t}
                        onChange={e => {
                          const t = e.target.value as 'forward' | 'egress' | 'block';
                          const a: RuleAction =
                            t === 'forward'
                              ? // dial 要显式写：不写的语义就是 overlay（模型里 HopDial
                                // 的 #[default]），会绕过 defaultDial 的选择逻辑。
                                defaultTarget
                                ? forwardAction(defaultTarget, defaultDial(defaultTarget))
                                : externalOutbounds[0]
                                  ? { t: 'proxy', outbound: externalOutbounds[0].id }
                                  : forwardAction('', { t: 'overlay' })
                              : t === 'egress'
                                ? { t: 'egress', send_through: null }
                                : { t: 'block' };
                          patch(i, { ...r, a });
                          setTargetPickerRule(t === 'forward' ? i : null);
                          setTargetPickerView({ t: 'targets' });
                        }}
                      >
                        <option value="forward">转发给</option>
                        <option value="egress">从本机出网</option>
                        <option value="block">拒绝</option>
                      </select>
                      {(isRelayAction(r.a) || r.a.t === 'proxy') && (
                        <span
                          className="external-target-picker"
                          ref={targetPickerRule === i ? targetPickerRoot : undefined}
                        >
                          <button
                            type="button"
                            className={`external-target-trigger${r.a.t === 'forward' ? ' node-target' : ''}${
                              r.a.t === 'reuse_listener' ? ' listener-target' : ''
                            }`}
                            title={referenced?.blocked ?? undefined}
                            aria-expanded={targetPickerRule === i}
                            onClick={event => {
                              const opening = targetPickerRule !== i;
                              if (opening) {
                                const rect = event.currentTarget.getBoundingClientRect();
                                setTargetMenuPlacement(placeTargetMenu(rect));
                                setTargetPickerView(
                                  r.a.t === 'reuse_listener'
                                    ? { t: 'listener-endpoints', chain: r.a.listener.chain }
                                    : { t: 'targets' },
                                );
                              }
                              setTargetPickerRule(opening ? i : null);
                              if (opening) setTargetQuery('');
                            }}
                          >
                            <span className={`external-target-kind${external ? ' external' : ''}`}>{targetBadge}</span>
                            <span className="external-target-copy">
                              <b>{targetLabel || '选择内部节点或代理出站'}</b>
                              {referenced && (
                                <small>
                                  {referenced.blocked
                                    ? `不可发布 · ${referenced.blocked}`
                                    : `引用子树 · ${referenced.ownerName} · ${referenced.references} 处使用`}
                                </small>
                              )}
                            </span>
                            <span className="external-target-chevron">⌄</span>
                          </button>
                          {targetPickerRule === i &&
                            typeof document !== 'undefined' &&
                            createPortal(
                              <fieldset
                                ref={targetMenu}
                                disabled={editorDisabled}
                                className={`external-target-menu${targetMenuPlacement.below ? ' below' : ''}`}
                                style={{
                                  left: targetMenuPlacement.left,
                                  top: targetMenuPlacement.top,
                                  width: targetMenuPlacement.width,
                                  maxHeight: targetMenuPlacement.maxHeight,
                                }}
                              >
                                {targetPickerView.t === 'targets' ? (
                                  <>
                                    <input
                                      className="f external-target-search"
                                      placeholder="搜索节点或代理出站"
                                      value={targetQuery}
                                      onChange={event => setTargetQuery(event.target.value)}
                                    />
                                    <span className="external-target-menu-label">本链已有监听节点</span>
                                    {[...visibleNextPeers, ...visibleInsidePeers].map(candidate => (
                                      <button
                                        type="button"
                                        className={r.a.t === 'forward' && r.a.to === candidate.id ? 'on' : ''}
                                        key={candidate.id}
                                        onClick={() => selectForwardTarget(i, r, candidate.id)}
                                      >
                                        <span className="external-target-kind">NODE</span>
                                        <span className="external-target-copy">
                                          <b>{candidate.name || '未命名节点'}</b>
                                        </span>
                                        <span className="external-target-where">
                                          {candidate.where === 'next'
                                            ? '当前下游'
                                            : candidate.where === 'inside'
                                              ? '链内其它节点'
                                              : '主干之外'}
                                        </span>
                                      </button>
                                    ))}
                                    <span className="external-target-menu-label">在机器上新建本链监听</span>
                                    {visibleForkPeers.map(candidate => (
                                      <button
                                        type="button"
                                        className={r.a.t === 'forward' && r.a.to === candidate.id ? 'on' : ''}
                                        key={`new-${candidate.id}`}
                                        title="保存时在这台机器创建本链监听和一棵空规则子树"
                                        onClick={() => selectForwardTarget(i, r, candidate.id)}
                                      >
                                        <span className="external-target-kind">NODE</span>
                                        <span className="external-target-copy">
                                          <b>{candidate.name || '未命名节点'}</b>
                                        </span>
                                        <span className="external-target-where">加入本链</span>
                                      </button>
                                    ))}
                                    {visibleBlockedPeers.length > 0 && (
                                      <span className="external-target-menu-label">不可用的机器</span>
                                    )}
                                    {visibleBlockedPeers.map(candidate => (
                                      <button type="button" disabled key={candidate.id} title={candidate.blocked ?? ''}>
                                        <span className="external-target-kind">NODE</span>
                                        <span className="external-target-copy">
                                          <b>{candidate.name || '未命名节点'}</b>
                                        </span>
                                        <span className="external-target-where">不能选</span>
                                      </button>
                                    ))}
                                    <span className="external-target-menu-label">代理出站</span>
                                    {visibleExternalOutbounds.map(outbound => (
                                      <span className="external-target-option" key={outbound.id}>
                                        <button
                                          type="button"
                                          className={`external-target-option-select${
                                            r.a.t === 'proxy' && r.a.outbound === outbound.id ? ' on' : ''
                                          }`}
                                          onClick={() => selectExternalTarget(i, r, outbound.id)}
                                        >
                                          <span className="external-target-kind external">
                                            {externalProtocolBadge(outbound.protocol.t)}
                                          </span>
                                          <span className="external-target-copy">
                                            <b>{outbound.name}</b>
                                          </span>
                                          <span className="external-target-where">共享资源</span>
                                        </button>
                                        <button
                                          type="button"
                                          className="external-target-manage"
                                          aria-label={`打开隧道 ${outbound.name}`}
                                          onClick={() => {
                                            setTargetPickerRule(null);
                                            navigate('tunnels', {
                                              p: 'tunnel',
                                              tenant: outbound.tenant,
                                              id: outbound.id,
                                            });
                                          }}
                                        >
                                          查看
                                        </button>
                                      </span>
                                    ))}
                                    <button
                                      type="button"
                                      className="external-target-new"
                                      aria-label="管理隧道"
                                      onClick={() => {
                                        setTargetPickerRule(null);
                                        navigate('tunnels');
                                      }}
                                    >
                                      <span>↗</span>
                                      <b>管理隧道</b>
                                      <span>新建、编辑和删除都在隧道页</span>
                                    </button>
                                    <button
                                      type="button"
                                      className="external-target-new external-target-custom"
                                      onClick={() => {
                                        setTargetPickerView({ t: 'listener-chains' });
                                        setTargetQuery('');
                                      }}
                                    >
                                      <Icon of="link" size={12} className="external-target-custom-icon" />
                                      <b>自定义</b>
                                      <span>复用已有监听</span>
                                    </button>
                                    {visibleNextPeers.length +
                                      visibleInsidePeers.length +
                                      visibleForkPeers.length +
                                      visibleBlockedPeers.length +
                                      visibleExternalOutbounds.length ===
                                      0 && <span className="external-target-empty">没有匹配项</span>}
                                  </>
                                ) : targetPickerView.t === 'listener-chains' ? (
                                  <>
                                    <div className="external-target-menu-nav">
                                      <button
                                        type="button"
                                        aria-label="返回目标列表"
                                        onClick={() => {
                                          setTargetPickerView({ t: 'targets' });
                                          setTargetQuery('');
                                        }}
                                      >
                                        ←
                                      </button>
                                      <span>
                                        <b>自定义 · 复用已有监听</b>
                                        <small>先选择 App 和链</small>
                                      </span>
                                    </div>
                                    <input
                                      autoFocus
                                      className="f external-target-search"
                                      placeholder="跨 App 搜索链、节点或端口"
                                      value={targetQuery}
                                      onChange={event => setTargetQuery(event.target.value)}
                                    />
                                    {visibleListenerApps.map(group => (
                                      <Fragment key={group.app.id}>
                                        <span className="external-target-menu-label">
                                          APP · {group.app.label || group.app.id}
                                        </span>
                                        {group.chains.map(({ chain: candidateChain, candidates }) => (
                                          <button
                                            type="button"
                                            key={candidateChain.id}
                                            onClick={() => {
                                              setTargetPickerView({
                                                t: 'listener-endpoints',
                                                chain: candidateChain.id,
                                              });
                                              setTargetQuery('');
                                            }}
                                          >
                                            <span className="external-target-kind listener">链</span>
                                            <span className="external-target-copy">
                                              <b>{candidateChain.name || candidateChain.id}</b>
                                              <small>{candidateChain.id}</small>
                                            </span>
                                            <span className="external-target-where">
                                              {candidates.length} 个监听端点
                                            </span>
                                          </button>
                                        ))}
                                      </Fragment>
                                    ))}
                                    {visibleListenerApps.length === 0 && (
                                      <span className="external-target-empty">没有匹配的链</span>
                                    )}
                                  </>
                                ) : (
                                  <>
                                    <div className="external-target-menu-nav">
                                      <button
                                        type="button"
                                        aria-label="返回链列表"
                                        onClick={() => {
                                          setTargetPickerView({ t: 'listener-chains' });
                                          setTargetQuery('');
                                        }}
                                      >
                                        ←
                                      </button>
                                      <span>
                                        <b>{selectedListenerChain?.chain.name || targetPickerView.chain}</b>
                                        <small>
                                          {selectedListenerChain?.app.label ||
                                            selectedListenerChain?.app.id ||
                                            '未知 App'}
                                        </small>
                                      </span>
                                    </div>
                                    <input
                                      autoFocus
                                      className="f external-target-search"
                                      placeholder="搜索监听端点、节点或端口"
                                      value={targetQuery}
                                      onChange={event => setTargetQuery(event.target.value)}
                                    />
                                    <span className="external-target-menu-label">链上监听端点</span>
                                    {visibleListeners.map(candidate => (
                                      <button
                                        type="button"
                                        disabled={Boolean(candidate.blocked)}
                                        title={candidate.blocked ?? '只保存引用；端口、安全参数和规则由源线路维护'}
                                        className={
                                          r.a.t === 'reuse_listener' && sameListener(r.a.listener, candidate.ref)
                                            ? 'on'
                                            : ''
                                        }
                                        key={`listener-${candidate.key}`}
                                        onClick={() => selectListenerTarget(i, r, candidate)}
                                      >
                                        <span className="external-target-kind listener">
                                          {candidate.local ? '本机' : '监听'}
                                        </span>
                                        <span className="external-target-copy">
                                          <b>
                                            {candidate.nodeName} · TCP {candidate.step.hop_in?.port}
                                          </b>
                                          <small>{candidate.step.rules.length} 条规则</small>
                                        </span>
                                        <span className="external-target-where">
                                          {candidate.blocked ?? `${candidate.references} 处引用`}
                                        </span>
                                      </button>
                                    ))}
                                    {visibleListeners.length === 0 && (
                                      <span className="external-target-empty">这条链没有匹配的监听端点</span>
                                    )}
                                  </>
                                )}
                              </fieldset>,
                              document.body,
                            )}
                        </span>
                      )}
                      {isRelayAction(r.a) && (
                        <>
                          {r.a.t === 'reuse_listener' && referenced?.local ? (
                            <span className="listener-local-route mono">
                              本机内部 · 回环:{referenced.step.hop_in?.port}
                            </span>
                          ) : (
                            <select
                              className="f"
                              style={{ marginLeft: 6 }}
                              value={dk}
                              title="这一跳连接目标监听的地址"
                              onChange={e =>
                                r.a.t === 'reuse_listener'
                                  ? setReferenceDial(i, r.a.listener, e.target.value as ListenerDialKind)
                                  : setDial(i, to, e.target.value as DialKind)
                              }
                            >
                              {(r.a.t === 'reuse_listener' ? LISTENER_DIAL_ORDER : DIAL_ORDER).map(k => (
                                <option
                                  key={k}
                                  value={k}
                                  disabled={dialUnavailable(
                                    k,
                                    targetAddrs,
                                    r.a.t === 'reuse_listener' ? null : selfAddrs,
                                  )}
                                >
                                  {DIAL_LABEL[k]}
                                </option>
                              ))}
                            </select>
                          )}
                          {/* 前三档的地址由推导得出，只读；仅自定义档需要手动填写 */}
                          {r.a.t === 'reuse_listener' && referenced?.local ? null : dk === 'overlay' ? (
                            // 与公网两档一样直接显示地址。显示为「XX 的 overlay 地址」会要求
                            // 到其他位置查询该值——而它就在编译结果中，可直接获取。
                            // 获取失败只有一种情况：该机器尚未加入 overlay，这正是需要说明的内容。
                            <span className="mono dim" style={{ marginLeft: 6 }}>
                              {overlayOf(to) || (
                                <span style={{ color: 'var(--gold)' }}>
                                  {referenced?.nodeName || peer?.name || to} 不在 overlay 里
                                </span>
                              )}
                            </span>
                          ) : dk === 'public_ipv4' ? (
                            <span className="mono dim" style={{ marginLeft: 6 }}>
                              {publicIpv4Of(targetAddrs) || (
                                <span style={{ color: 'var(--gold)' }}>这台机器没有可直连的公网 IPv4</span>
                              )}
                            </span>
                          ) : dk === 'public_ipv6' ? (
                            <span className="mono dim" style={{ marginLeft: 6 }}>
                              {publicIpv6Of(targetAddrs) || (
                                <span style={{ color: 'var(--gold)' }}>这台机器没有可直连的公网 IPv6</span>
                              )}
                            </span>
                          ) : dk === 'reverse_v4' || dk === 'reverse_v6' ? (
                            // 与前三档一样由推导得出，只读。显示的是本机的接入地址——
                            // 对端从该地址接入。连接由哪一方发起、通道如何建立属于传输层的内容，
                            // 界面不涉及。
                            <span className="mono dim" style={{ marginLeft: 6 }}>
                              {selfPublicHostOf(dk === 'reverse_v6' ? 'v6' : 'v4') || (
                                <span style={{ color: 'var(--gold)' }}>
                                  这台机器没有可直连的{dk === 'reverse_v6' ? '公网 IPv6' : '公网 IPv4'}
                                </span>
                              )}
                            </span>
                          ) : (
                            <>
                              <input
                                className="f mono"
                                style={{ marginLeft: 6, width: 150 }}
                                placeholder="10.0.0.9 / 2001:db8::9"
                                value={customHost}
                                onChange={e => {
                                  if (r.a.t === 'reuse_listener') {
                                    setDialForListener(r.a.listener, { t: 'addr', v: e.target.value }, i);
                                  } else {
                                    const port = Number(hopOf(to).port) || hopBase;
                                    setDialForTarget(to, { t: 'addr', v: formatHostPort(e.target.value, port) }, i);
                                  }
                                }}
                              />
                              {natPublicHostOf(targetAddrs, customHost) && (
                                <span className="sub" style={{ color: 'var(--gold)' }}>
                                  该地址为 {natPublicHostOf(targetAddrs, customHost)} 且标记为经 NAT，编译会拒绝。
                                </span>
                              )}
                            </>
                          )}
                        </>
                      )}
                      {r.a.t === 'egress' && (
                        <span className="egress-dns-reference">
                          <MachineEgressDnsControls
                            resolution={effectiveDnsFor(r.m)}
                            supported={supportsEgressDns(r.m)}
                            onChange={next => patchMachineDns(r.m, next)}
                            readOnly={readOnly}
                            nodeName={selfNode?.name || nodeId}
                            accessibleSuffix={`（线路规则：${kind?.label ?? r.m.t}${matchValues(r.m) ? ` ${matchValues(r.m)}` : ''}）`}
                            showEditor={false}
                          />
                        </span>
                      )}
                    </td>
                    {/* 只读时整列不渲染：保留一列禁用按钮表示此处有操作但不可执行，
                    而规则顺序已由左侧的序号表示。 */}
                    {!readOnly && (
                      <td style={{ width: 120, textAlign: 'right' }}>
                        <button
                          className="btn"
                          disabled={i === 0 || isPinnedTerminalRule(r)}
                          onClick={() => move(i, -1)}
                          title={
                            isAnyRule(r)
                              ? '任意固定在末尾'
                              : isSniffingFallbackRule(r)
                                ? '嗅探失败兜底固定在任意之前'
                                : '上移'
                          }
                        >
                          ↑
                        </button>
                        <button
                          className="btn"
                          disabled={
                            i === rules.length - 1 || isPinnedTerminalRule(r) || isPinnedTerminalRule(rules[i + 1])
                          }
                          onClick={() => move(i, 1)}
                          title={
                            isPinnedTerminalRule(r)
                              ? '终结规则位置固定'
                              : isPinnedTerminalRule(rules[i + 1])
                                ? '不能移动到终结规则之后'
                                : '下移'
                          }
                        >
                          ↓
                        </button>
                        <button
                          className="btn danger"
                          disabled={flushing || save.isPending}
                          onClick={() => {
                            setRules(rules.filter((_, x) => x !== i));
                            setFlushing(true);
                          }}
                        >
                          删
                        </button>
                      </td>
                    )}
                  </tr>
                </Fragment>
              );
            })}
            {orderedNodeDnsPolicies.map((policy, index) => {
              const kind = MATCH_KINDS.find(candidate => candidate.t === policy.selector.t);
              const value = matchValues(policy.selector);
              return (
                <tr className="machine-dns-shared-row" key={`machine-dns-${egressDnsSelectorKey(policy.selector)}`}>
                  <td className="mono dim" style={{ width: 24 }}>
                    D{index + 1}
                  </td>
                  <td className="rule-match-cell">
                    <span className="f rule-readonly-select">{kind?.label ?? policy.selector.t}</span>
                    {value && <span className="f rule-readonly-value">{value}</span>}
                  </td>
                  <td className="rule-action-cell">
                    <MachineEgressDnsControls
                      resolution={effectiveDnsFor(policy.selector)}
                      supported
                      onChange={next => patchMachineDns(policy.selector, next)}
                      readOnly={readOnly}
                      nodeName={selfNode?.name || nodeId}
                      accessibleSuffix={`（机器策略：${kind?.label ?? policy.selector.t}${value ? ` ${value}` : ''}）`}
                    />
                  </td>
                  {!readOnly && (
                    <td className="dns-priority-cell" style={{ width: 120, textAlign: 'right' }}>
                      <DnsPriorityControl
                        index={index}
                        count={orderedNodeDnsPolicies.length}
                        label={`${kind?.label ?? policy.selector.t} ${value}`.trim()}
                        readOnly={readOnly}
                        showLabel={false}
                        onMove={delta => moveMachineDns(policy.selector, delta)}
                      />
                    </td>
                  )}
                </tr>
              );
            })}
            {fallback?.pending && (
              <tr className="rule-fallback-row" aria-label="正在计算编译器兜底规则">
                <td className="mono dim" style={{ width: 24 }}>
                  *
                </td>
                <td className="rule-match-cell">
                  <span className="f rule-readonly-select">正在计算…</span>
                </td>
                <td className="rule-action-cell">
                  <span className="dim">兜底规则尚未生成</span>
                  {readOnly && <span className="rule-fallback-sign inline">自动补齐 · 计算中</span>}
                </td>
                {!readOnly && (
                  <td className="rule-fallback-sign" style={{ width: 120 }}>
                    自动补齐 · 计算中
                  </td>
                )}
              </tr>
            )}
            {!fallback?.pending &&
              visibleFallbackRules.map((r, fallbackIndex) => {
                const kind = MATCH_KINDS.find(candidate => candidate.t === r.m.t);
                const value = matchValues(r.m);
                const to = r.a.t === 'forward' ? r.a.to : r.a.t === 'reuse_listener' ? r.a.listener.node : '';
                const referenced = r.a.t === 'reuse_listener' ? listenerOf(r.a.listener) : null;
                const externalId = r.a.t === 'proxy' ? r.a.outbound : '';
                const external = externalOutbounds.find(outbound => outbound.id === externalId) ?? null;
                const peer = peerOf(to);
                const targetBadge = external
                  ? externalProtocolBadge(external.protocol.t)
                  : r.a.t === 'reuse_listener'
                    ? 'LISTENER'
                    : 'NODE';
                const targetLabel =
                  external?.name ||
                  (referenced ? `${referenced.nodeName} · TCP ${referenced.step.hop_in?.port}` : peer?.name) ||
                  (r.a.t === 'proxy' ? '代理出站不可用' : to || '内部节点不可用');
                const actionLabel =
                  isRelayAction(r.a) || r.a.t === 'proxy' ? '转发给' : r.a.t === 'egress' ? '从本机出网' : '拒绝';
                return (
                  <tr
                    className="rule-fallback-row"
                    aria-label="编译器生成的兜底规则"
                    title="编译器根据当前配置自动生成"
                    key={`fallback-${fallbackIndex}`}
                  >
                    <td className="mono dim" style={{ width: 24 }}>
                      {fallbackIndex === visibleFallbackRules.length - 1 ? '*' : displayedRuleCount + fallbackIndex + 1}
                    </td>
                    <td className="rule-match-cell">
                      <span className="f rule-readonly-select">{kind?.label ?? r.m.t}</span>
                      {value && <span className="f rule-readonly-value">{value}</span>}
                    </td>
                    <td className="rule-action-cell">
                      <span
                        className={`f rule-action-select rule-action-${ruleActionTone(r.a.t)} rule-readonly-select`}
                      >
                        {actionLabel}
                      </span>
                      {(isRelayAction(r.a) || r.a.t === 'proxy') && (
                        <span className="external-target-picker">
                          <span className="external-target-trigger rule-readonly-target">
                            <span className={`external-target-kind${external ? ' external' : ''}`}>{targetBadge}</span>
                            <span className="external-target-copy">
                              <b>{targetLabel}</b>
                            </span>
                          </span>
                        </span>
                      )}
                      {readOnly && <span className="rule-fallback-sign inline">自动补齐</span>}
                    </td>
                    {!readOnly && (
                      <td className="rule-fallback-sign" style={{ width: 120 }}>
                        自动补齐
                      </td>
                    )}
                  </tr>
                );
              })}
          </tbody>
        </table>

        {rules.map((rule, ruleIndex) => {
          if (rule.a.t !== 'proxy') return null;
          const outboundId = rule.a.outbound;
          const outbound = externalOutbounds.find(candidate => candidate.id === outboundId);
          if (!outbound) {
            return (
              <div className="external-outbound-summary missing" key={`external-${ruleIndex}`}>
                代理出站 <code>{outboundId || '未选择'}</code> 不存在，请重新选择或创建资源。
              </div>
            );
          }
          const facts = externalOutboundFacts(outbound);
          return (
            <section className="external-outbound-summary" key={`external-${ruleIndex}`}>
              <header>
                <PanelTitle of="outbound">代理出站</PanelTitle>
                <span className="external-summary-protocol">{externalProtocolLabel(outbound.protocol.t)}</span>
                <b>{outbound.name}</b>
                <span className="sp" />
                <span className="note">由 {selfNode?.name || nodeId} 发起</span>
                <button
                  type="button"
                  className="btn"
                  onClick={() => navigate('tunnels', { p: 'tunnel', tenant: outbound.tenant, id: outbound.id })}
                >
                  隧道详情
                </button>
              </header>
              <div className="external-summary-facts">
                <span>
                  <small>服务器</small>
                  <b className="mono">
                    {outbound.address}:{outbound.port}
                  </b>
                </span>
                <span>
                  <small>传输</small>
                  <b>{facts.transport}</b>
                </span>
                <span>
                  <small>安全</small>
                  <b>{facts.security}</b>
                </span>
                <span>
                  <small>凭据</small>
                  <b>{facts.credential}</b>
                </span>
              </div>
              <p className="external-summary-impact">
                <span>OUT</span>
                这里只在 <b>{selfNode?.name || nodeId}</b> 的 <code>xray.json</code> 生成
                outbound；不会为外部服务器创建节点、
                <code>step</code>、<code>hop_in</code> 或发布目标。修改会重启引用它的内部节点上的 XRAY。
              </p>
            </section>
          );
        })}

        {referencedTargets.length > 0 && (
          <div className="panel listener-reference-panel" style={{ marginTop: 10 }}>
            <header>
              <PanelTitle of="chains">引用监听</PanelTitle>
              <span className="hint">这里只配置本机到目标的承载；端口、安全参数和下游规则均由源监听维护</span>
            </header>
            <div className="listener-reference-list">
              {referencedTargets.map(listener => {
                const target = listenerOf(listener);
                const pool = referencePoolOf(listener);
                const key = listenerRefKey(listener);
                const referenceRule = rules.find(
                  (rule): rule is Rule & { a: Extract<RuleAction, { t: 'reuse_listener' }> } =>
                    rule.a.t === 'reuse_listener' && sameListener(rule.a.listener, listener),
                );
                const highlighted = Boolean(highlightedListener && sameListener(highlightedListener, listener));
                if (!target) {
                  return (
                    <div className="listener-reference-row missing" key={key}>
                      <div>
                        <b className="mono">
                          {listener.chain}/{listener.node}
                        </b>
                        <small>源监听不存在或当前不可见，编译会阻止发布</small>
                      </div>
                    </div>
                  );
                }
                const openMux = () =>
                  setMuxEditor({
                    to: key,
                    listener,
                    targetName: `${target.nodeName} · TCP ${target.step.hop_in?.port}`,
                    followGlobal: pool.t === 'mux' && !pool.v,
                    value: pool.t === 'mux' && pool.v ? { ...pool.v } : { ...globalRelayMux },
                  });
                return (
                  <div
                    className={`listener-reference-row${target.blocked ? ' blocked' : ''}${
                      onHighlightListener ? ' is-highlightable' : ''
                    }${highlighted ? ' is-highlighted' : ''}`}
                    key={key}
                  >
                    {onHighlightListener && (
                      <span
                        className="listener-reference-highlight-hitbox"
                        role="button"
                        tabIndex={0}
                        aria-label={`${highlighted ? '取消高亮' : '高亮'} ${target.nodeName} 的规则子树`}
                        aria-pressed={highlighted}
                        title={highlighted ? '取消图中的规则子树高亮' : '在决策图中圈出这棵规则子树'}
                        onClick={() => onHighlightListener(listener)}
                        onKeyDown={event => {
                          if (event.key !== 'Enter' && event.key !== ' ') return;
                          event.preventDefault();
                          onHighlightListener(listener);
                        }}
                      />
                    )}
                    <div className="listener-reference-main">
                      <span className="external-target-kind listener">{target.local ? '本机' : '引用'}</span>
                      <span className="listener-reference-copy">
                        <b>
                          {target.nodeName} · TCP {target.step.hop_in?.port}
                        </b>
                        <small>
                          {target.blocked
                            ? `不可发布 · ${target.blocked}`
                            : `子树归属「${target.ownerName}」 · ${target.step.rules.length} 条规则 · 当前 ${target.references} 处引用`}
                        </small>
                      </span>
                    </div>
                    <div className="listener-reference-facts">
                      <span>
                        <small>安全</small>
                        <b>{hopWireLabel(target.step.hop_in?.security.t ?? 'none')}</b>
                      </span>
                      <span>
                        <small>连接</small>
                        <b>
                          {target.local
                            ? '本机回环'
                            : DIAL_LABEL[listenerDialKindOf(referenceRule?.a.dial ?? { t: 'overlay' })]}
                        </b>
                      </span>
                      <label>
                        <small>连接复用</small>
                        <select
                          className="f"
                          value={poolChoice(pool)}
                          onChange={event =>
                            setPoolForListener(listener, event.target.value === 'mux' ? { t: 'mux' } : { t: 'none' })
                          }
                        >
                          {POOL_ORDER.map(choice => (
                            <option key={choice} value={choice}>
                              {POOL_LABEL[choice]}
                            </option>
                          ))}
                        </select>
                      </label>
                      {pool.t === 'mux' && (
                        <button type="button" className="btn sm" onClick={openMux}>
                          {readOnly ? '查看参数' : '配置参数'}
                        </button>
                      )}
                    </div>
                    <p>
                      {target.local
                        ? '本机经回环进入该监听，不建立机器间链路。'
                        : `只保存 ${listener.chain}/${listener.node} 的身份和拨号方式，不复制目标配置。`}
                      目标监听的变更会作用于所有引用。
                      {pool.t === 'mux'
                        ? ' 同一规则表中指向该监听的规则共用此 Mux 池；“跟随全局”只继承参数。'
                        : ' 当前每条业务流单独建连。'}
                    </p>
                  </div>
                );
              })}
            </div>
          </div>
        )}

        {/* 该跳在对端一侧的配置：使用哪个端口、如何加密。
          配置在此处而非对端页面，因为它属于该跳的组成部分。中转端口关联在
          `(chain, node)` 上，同一条链中多个上游连接同一目标时复用该入口配置。 */}
        {reverseTargets.length > 0 && (
          <div className="panel listener-reference-panel hop-target-panel" style={{ marginTop: 10 }}>
            <header>
              <PanelTitle of="ingress">反向接入口</PanelTitle>
              <span className="hint">
                {reverseTargets.map(to => peerOf(to)?.name || to).join('、')} 从本机的这个端口接入
              </span>
            </header>
            <div className="listener-reference-list">
              <div className="listener-reference-row hop-target-row">
                <div className="listener-reference-main">
                  <span className="external-target-kind node">反向</span>
                  <span className="listener-reference-copy">
                    <b title={nodeId}>
                      {selfNode?.name || nodeId} · TCP {hopOf(nodeId).port || '未设置'}
                    </b>
                  </span>
                </div>
                <div className="listener-reference-facts hop-target-facts">
                  <label className="hopfld hop-target-port">
                    <small>本机端口</small>
                    <input
                      className="f mono"
                      value={hopOf(nodeId).port}
                      placeholder={String(hopBase)}
                      onChange={event => setHopPort(nodeId, event.target.value)}
                    />
                  </label>
                  <label className="hopfld hop-target-wire">
                    <small>承载协议</small>
                    <select
                      className="f"
                      value={hopOf(nodeId).kind}
                      onChange={event =>
                        patchHop(nodeId, { kind: event.target.value as ReturnType<typeof hopOf>['kind'] })
                      }
                    >
                      {/* 该区块只在存在反向目标时渲染，因此该端口一定是反向接入使用的端口
                        ——此处不提供 SS2022 选项。 */}
                      {HOP_WIRE_OPTIONS.map(option => (
                        <option key={option.kind} value={option.kind} disabled={!option.reverseOk}>
                          {option.label}
                          {option.reverseOk ? '' : ' — 反向隧道只有 VLESS 承载'}
                        </option>
                      ))}
                    </select>
                  </label>
                </div>
                {hopOf(nodeId).kind === 'reality' && (
                  <div className="hop-target-security">
                    <label>
                      <small>伪装目标</small>
                      <input
                        className="f mono"
                        value={hopOf(nodeId).dest}
                        placeholder="example.com:443"
                        onChange={event => patchHop(nodeId, { dest: event.target.value })}
                      />
                    </label>
                    <label>
                      <small>服务端名称</small>
                      <input
                        className="f mono"
                        value={hopOf(nodeId).names}
                        placeholder="server_names"
                        onChange={event => patchHop(nodeId, { names: event.target.value })}
                      />
                    </label>
                  </div>
                )}
                <p className="hop-target-note">
                  <span>
                    端口开在 {selfNode?.name || nodeId}，连接由{' '}
                    {reverseTargets.map(to => peerOf(to)?.name || to).join('、')} 发起；同机各链端口必须错开。
                  </span>
                  {hopOf(nodeId).kind === 'none' && <strong>明文接入可能暴露 UUID 和目标地址。</strong>}
                  <span>发布会重启受影响的 Xray。</span>
                </p>
              </div>
            </div>
          </div>
        )}

        {normalTargets.length > 0 && (
          <div className="panel listener-reference-panel hop-target-panel" style={{ marginTop: 10 }}>
            <header>
              <PanelTitle of="chains">本链监听</PanelTitle>
              <span className="hint">端口和协议属于目标监听；拨号和 Mux 属于当前链边</span>
            </header>
            <div className="listener-reference-list">
              {normalTargets.map(to => {
                const h = hopOf(to);
                const peer = peerOf(to);
                const pool = poolOf(to);
                const peerName = peer?.name || to;
                // 是否有连接从 wg 之外直接连接它。该判定决定 inbound 绑定的地址——
                // 存在直连时绑定 0.0.0.0，全部走 overlay 时才绑定 overlay 地址
                // （physical/node.rs 的 listen 判定）。
                const dialedDirectly = rules.some(
                  r => r.a.t === 'forward' && r.a.to === to && forwardDial(r.a).t !== 'overlay',
                );
                return (
                  <div key={to} className="listener-reference-row hop-target-row">
                    <div className="listener-reference-main">
                      <span className="external-target-kind node">本链</span>
                      <span className="listener-reference-copy">
                        <b title={to}>
                          {peerName} · TCP {h.port || '未设置'}
                        </b>
                      </span>
                    </div>
                    <div className="listener-reference-facts hop-target-facts">
                      <label className="hopfld hop-target-port">
                        <small>目标端口</small>
                        <input
                          className="f mono"
                          value={h.port}
                          placeholder={String(hopBase)}
                          onChange={event => setHopPort(to, event.target.value)}
                        />
                      </label>
                      <label className="hopfld hop-target-wire">
                        <small>承载协议</small>
                        <select
                          className="f"
                          value={h.kind}
                          onChange={event => patchHop(to, { kind: event.target.value as typeof h.kind })}
                        >
                          {HOP_WIRE_OPTIONS.map(option => (
                            <option key={option.kind} value={option.kind}>
                              {option.label}
                            </option>
                          ))}
                        </select>
                      </label>
                      <label className="hopfld hop-target-pool">
                        <small>连接复用</small>
                        <select
                          className="f"
                          value={poolChoice(pool)}
                          onChange={event => {
                            const choice = event.target.value as PoolChoice;
                            setPoolForTarget(to, choice === 'mux' ? { t: 'mux' } : { t: 'none' });
                          }}
                        >
                          {POOL_ORDER.map(choice => (
                            <option key={choice} value={choice}>
                              {POOL_LABEL[choice]}
                            </option>
                          ))}
                        </select>
                      </label>
                      {pool.t === 'mux' && (
                        <span className="hop-target-mux">
                          <small>Mux 参数</small>
                          <span>
                            <b>{pool.v ? '单独配置' : '跟随全局'}</b>
                            {readOnly ? (
                              <span
                                className="btn sm"
                                role="button"
                                tabIndex={0}
                                onClick={() =>
                                  setMuxEditor({
                                    to,
                                    followGlobal: !pool.v,
                                    value: pool.v ? { ...pool.v } : { ...globalRelayMux },
                                  })
                                }
                                onKeyDown={event => {
                                  if (event.key !== 'Enter' && event.key !== ' ') return;
                                  event.preventDefault();
                                  setMuxEditor({
                                    to,
                                    followGlobal: !pool.v,
                                    value: pool.v ? { ...pool.v } : { ...globalRelayMux },
                                  });
                                }}
                              >
                                查看参数
                              </span>
                            ) : (
                              <button
                                type="button"
                                className="btn sm"
                                onClick={() =>
                                  setMuxEditor({
                                    to,
                                    followGlobal: !pool.v,
                                    value: pool.v ? { ...pool.v } : { ...globalRelayMux },
                                  })
                                }
                              >
                                配置参数
                              </button>
                            )}
                          </span>
                        </span>
                      )}
                    </div>
                    {h.kind === 'reality' && (
                      <div className="hop-target-security">
                        <label>
                          <small>伪装目标</small>
                          <input
                            className="f mono"
                            value={h.dest}
                            placeholder="example.com:443"
                            onChange={event => patchHop(to, { dest: event.target.value })}
                          />
                        </label>
                        <label>
                          <small>服务端名称</small>
                          <input
                            className="f mono"
                            value={h.names}
                            placeholder="server_names"
                            onChange={event => patchHop(to, { names: event.target.value })}
                          />
                        </label>
                      </div>
                    )}
                    <p className="hop-target-note">
                      <span>{`端口开在 ${peerName}，连接由 ${selfNode?.name || nodeId} 发起；目标端口必须唯一。`}</span>
                      <span>
                        {pool.t === 'mux'
                          ? '同一规则表中指向该监听的规则共用此 Mux 池；“跟随全局”只继承参数，不与其他边共池。'
                          : '当前每条业务流单独建连。'}
                      </span>
                      {dialedDirectly && h.kind === 'none' && <strong>明文直连会暴露 UUID 和目标地址。</strong>}
                      <span>发布会重启受影响的 Xray。</span>
                    </p>
                  </div>
                );
              })}
            </div>
          </div>
        )}

        {save.error && <ErrorBox error={save.error} />}

        {/* 整块常驻，只读时由外层 `fieldset disabled` 一并禁用：按角色隐藏会使只读视角
          看不到这张表可以增行和保存，页面读起来像是一份静态清单。 */}
        <div className="toolbar">
          <button className="btn" onClick={addRule}>
            ＋ 加一条
          </button>
          {hasSniffingDependentRule && !hasSniffingFallback && (
            <>
              <button className="btn" onClick={addSniffingFallback}>
                ＋ 嗅探失败兜底
              </button>
              <span className="note">域名/Geosite 规则依赖嗅探；匹配条件与转发、从本机出网或拒绝动作独立配置。</span>
            </>
          )}
          <span className="sp" />
          {/* 位于规则树中时按钮统一收敛到末尾，此处只保留该表已修改的标记 */}
          {bus ? (
            dirty && <span className="st st-warn">有改动，未落草稿</span>
          ) : (
            <button className="btn primary" disabled={save.isPending} onClick={() => save.mutate({})}>
              {save.isPending ? '保存中…' : '保存到草稿'}
            </button>
          )}
        </div>
      </fieldset>
      {muxEditor && (
        <MuxConfigDrawer
          targetName={muxEditor.targetName || peerOf(muxEditor.to)?.name || muxEditor.to}
          globalValue={globalRelayMux}
          state={muxEditor}
          readOnly={readOnly}
          onChange={setMuxEditor}
          onClose={() => setMuxEditor(null)}
          onApply={() => {
            const pool = muxEditor.followGlobal
              ? { t: 'mux' as const }
              : { t: 'mux' as const, v: { ...muxEditor.value } };
            if (muxEditor.listener) setPoolForListener(muxEditor.listener, pool);
            else setPoolForTarget(muxEditor.to, pool);
            setMuxEditor(null);
          }}
        />
      )}
    </>
  );
}

type MuxEditorState = {
  to: string;
  listener?: ListenerRef;
  targetName?: string;
  followGlobal: boolean;
  value: HopMux;
};

function MuxConfigDrawer({
  targetName,
  globalValue,
  state,
  readOnly,
  onChange,
  onClose,
  onApply,
}: {
  targetName: string;
  globalValue: HopMux;
  state: MuxEditorState;
  readOnly: boolean;
  onChange: (next: MuxEditorState) => void;
  onClose: () => void;
  onApply: () => void;
}) {
  const value = state.followGlobal ? globalValue : state.value;
  const error = hopMuxError(value);
  const patch = (field: keyof HopMux, raw: string) => {
    if (raw.trim() === '') return;
    const number = Number(raw);
    onChange({ ...state, value: { ...state.value, [field]: number } });
  };
  const field = (label: string, key: keyof HopMux, min: number, max?: number, unit?: string) => (
    <label className="mux-drawer-field">
      <span>{label}</span>
      <span>
        <input
          className="f mono"
          type="number"
          min={min}
          max={max}
          value={Number(value[key])}
          disabled={readOnly || state.followGlobal}
          onChange={event => patch(key, event.target.value)}
        />
        {unit && <small>{unit}</small>}
      </span>
    </label>
  );

  return (
    <div className="external-outbound-wrap" role="dialog" aria-modal="true" aria-label={`配置 ${targetName} 的 Mux`}>
      <button className="external-outbound-scrim" aria-label="关闭" onClick={onClose} />
      <section className="external-outbound-drawer mux-config-drawer">
        <header>
          <b>{targetName} · Mux 复用</b>
          <span className="sp" />
          <button className="btn" onClick={onClose}>
            关闭
          </button>
        </header>
        <div className="external-outbound-body">
          <div className="segsw mux-source-switch" role="group" aria-label="Mux 参数来源">
            <button
              type="button"
              aria-pressed={state.followGlobal}
              disabled={readOnly}
              onClick={() => onChange({ ...state, followGlobal: true })}
            >
              跟随全局
            </button>
            <button
              type="button"
              aria-pressed={!state.followGlobal}
              disabled={readOnly}
              onClick={() => onChange({ ...state, followGlobal: false, value: { ...globalValue } })}
            >
              单独配置
            </button>
          </div>
          <p className="note">
            当前生效：复用流 {value.concurrency} · 预热目标 {value.prewarm_workers} · 复用阈值 {value.reuse_threshold} ·
            探活 {value.probe_interval_ms}ms/{value.probe_timeout_ms}ms · 超额空闲寿命 {value.idle_ttl_ms}ms
          </p>
          {state.followGlobal && <p className="note">这些值来自设置页的“连接策略 / 中继 Mux”。</p>}
          <div className="mux-drawer-groups">
            <section>
              <p className="eyebrow">复用容量</p>
              <div className="mux-drawer-grid">
                {field('复用流数量', 'concurrency', 1, 128)}
                {field('累计子连接', 'max_requests_per_worker', 1, 65535)}
              </div>
            </section>
            <section>
              <p className="eyebrow">连接池</p>
              <div className="mux-drawer-grid">
                {field('预热目标', 'prewarm_workers', 0)}
                {field('复用阈值', 'reuse_threshold', 1)}
                {field('超额空闲寿命', 'idle_ttl_ms', 1000, undefined, 'ms')}
              </div>
            </section>
            <section>
              <p className="eyebrow">探活</p>
              <div className="mux-drawer-grid">
                {field('探活并发', 'max_probing_workers', 1, value.reuse_threshold)}
                {field('探活周期', 'probe_interval_ms', 2000, 60000, 'ms')}
                {field('探活超时', 'probe_timeout_ms', 200, 10000, 'ms')}
              </div>
            </section>
          </div>
          <p className="note">
            优先使用已验证的空闲 Worker；没有空闲时先建到复用阈值，再复用活跃 Worker
            的槽位。可用槽位用尽后允许突发扩容，超额 Worker 空闲后回收。
          </p>
          <p className="note">
            预热只在复用阈值内尽力补足空闲连接，不保证业务繁忙时仍有空闲。健康探测会保留预热目标内 的空闲
            Worker；寿命只回收超出预热目标的空闲容量。
          </p>
          <p className="note">
            探活或收尾中的 Worker 不承接新流；End 写入使用独立 10 秒宽限，超时后只转为排空，
            不会因此关闭同载的其他业务流。
          </p>
          {error && (
            <p className="note" style={{ color: 'var(--err)' }}>
              {error}
            </p>
          )}
        </div>
        <footer className="mux-drawer-actions">
          <button className="btn" onClick={onClose}>
            取消
          </button>
          {!readOnly && (
            <button className="btn primary" disabled={error !== null} onClick={onApply}>
              应用
            </button>
          )}
        </footer>
      </section>
    </div>
  );
}

function externalProtocolLabel(protocol: ExternalOutboundProtocol['t']): string {
  return {
    anytls: 'AnyTLS',
    vless: 'VLESS',
    shadowsocks2022: 'Shadowsocks 2022',
    socks5: 'SOCKS5',
    http_connect: 'HTTP CONNECT',
    wireguard: 'WireGuard',
    warp: 'Cloudflare WARP',
  }[protocol];
}

function externalProtocolBadge(protocol: ExternalOutboundProtocol['t']): string {
  return {
    anytls: 'AnyTLS',
    vless: 'VLESS',
    shadowsocks2022: 'SS2022',
    socks5: 'SOCKS5',
    http_connect: 'HTTP',
    wireguard: 'WG',
    warp: 'WARP',
  }[protocol];
}

function externalSecurityLabel(security: ExternalOutboundSecurity): string {
  if (security.t === 'none') return 'RAW';
  return `${security.t.toUpperCase()} · ${security.v.fingerprint}`;
}

function externalOutboundFacts(outbound: ExternalOutbound): {
  transport: string;
  security: string;
  credential: string;
} {
  const protocol = outbound.protocol;
  if (protocol.t === 'vless') {
    const transport = protocol.v.transport.t === 'xhttp' ? 'XHTTP' : 'RAW';
    return {
      transport: `${transport}${protocol.v.flow ? ` · ${protocol.v.flow}` : ''}`,
      security:
        protocol.v.encryption !== 'none'
          ? `VLESS Encryption${outbound.security.t === 'none' ? '' : ` · ${externalSecurityLabel(outbound.security)}`}`
          : externalSecurityLabel(outbound.security),
      credential: 'UUID · 已密封',
    };
  }
  if (protocol.t === 'anytls') {
    return { transport: 'TCP', security: externalSecurityLabel(outbound.security), credential: '密码 · 已密封' };
  }
  if (protocol.t === 'shadowsocks2022') {
    return { transport: `RAW · ${protocol.v.method}`, security: 'SS2022', credential: 'PSK · 已密封' };
  }
  if (protocol.t === 'wireguard') {
    return {
      transport: `UDP · MTU ${protocol.v.mtu}`,
      security: 'WireGuard',
      credential: '私钥 · 已密封',
    };
  }
  if (protocol.t === 'warp') {
    return {
      transport: `WireGuard · MTU ${protocol.v.mtu}`,
      security: 'Cloudflare WARP',
      credential: `${outbound.bindings.length} 台机器已绑定`,
    };
  }
  const authenticated = !!protocol.v.username;
  return {
    transport: protocol.t === 'http_connect' ? 'CONNECT · TCP' : 'SOCKS5',
    security: externalSecurityLabel(outbound.security),
    credential: authenticated ? '账号密码 · 已密封' : '无需认证',
  };
}

type ParsedExternalShare = {
  address: string;
  port: number;
  name: string;
  protocol: ExternalOutboundProtocol;
  security: ExternalOutboundSecurity;
};

function decodeShareBase64(value: string): string {
  const normalized = value
    .replace(/-/g, '+')
    .replace(/_/g, '/')
    .padEnd(Math.ceil(value.length / 4) * 4, '=');
  return window.atob(normalized);
}

function externalObject(value: unknown): Record<string, unknown> | null {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null;
}

function externalString(value: unknown): string | null {
  return typeof value === 'string' && value.trim() ? value : null;
}

function parseExternalXhttpMode(value: unknown, field: string): XhttpMode {
  if (value === undefined || value === null || value === '' || value === 'auto') return 'auto';
  if (value === 'packet-up' || value === 'stream-up' || value === 'stream-one') return value;
  throw new Error(`${field} 不是有效的 XHTTP 模式`);
}

function parseExternalXhttpMux(value: unknown, field: string): number | null {
  if (value === undefined || value === null || value === '') return null;
  const parsed = Number(value);
  if (!Number.isInteger(parsed) || parsed < 1 || parsed > 128) throw new Error(`${field} 必须是 1–128`);
  return parsed;
}

function externalXhttpHost(value: unknown): string | null {
  if (Array.isArray(value)) return externalString(value[0]);
  return externalString(value);
}

function parseExternalDownloadSecurity(
  download: Record<string, unknown>,
  fallbackAddress: string,
): ExternalOutboundSecurity {
  const kind = externalString(download.security)?.toLowerCase();
  if (kind !== 'tls' && kind !== 'reality') {
    throw new Error('XHTTP 独立下载必须包含 TLS 或 REALITY');
  }
  const settings = externalObject(download[kind === 'tls' ? 'tlsSettings' : 'realitySettings']) ?? {};
  const serverName = externalString(settings.serverName) ?? fallbackAddress;
  const fingerprint = externalString(settings.fingerprint) ?? 'chrome';
  if (kind === 'tls') return { t: 'tls', v: { server_name: serverName, fingerprint } };
  return {
    t: 'reality',
    v: {
      server_name: serverName,
      public_key: externalString(settings.publicKey) ?? '',
      short_id: externalString(settings.shortId) ?? '',
      fingerprint,
    },
  };
}

function parseExternalVlessTransport(url: URL): ExternalVlessTransport {
  const network = (url.searchParams.get('type') || 'tcp').toLowerCase();
  if (network === 'tcp' || network === 'raw') return { t: 'raw' };
  if (network !== 'xhttp') throw new Error('当前外部 VLESS 支持 RAW/TCP 和 XHTTP');

  const flow = url.searchParams.get('flow');
  if (flow) throw new Error('XHTTP 不能和 Vision Flow 同时使用');
  const path = url.searchParams.get('path') || '/';
  const host = url.searchParams.get('host') || null;
  const mode = parseExternalXhttpMode(url.searchParams.get('mode'), '上传模式');
  let extra: Record<string, unknown> = {};
  const encodedExtra = url.searchParams.get('extra');
  if (encodedExtra) {
    try {
      extra = externalObject(JSON.parse(encodedExtra)) ?? {};
    } catch {
      throw new Error('XHTTP extra 不是有效的 JSON');
    }
  }
  const xmux = externalObject(extra.xmux);
  const mux = parseExternalXhttpMux(xmux?.maxConcurrency, '上传 XMUX');
  const downloadValue = externalObject(extra.downloadSettings);
  if (!downloadValue) return { t: 'xhttp', v: { path, host, mux, mode, download: null } };

  const address = externalString(downloadValue.address);
  const port = Number(downloadValue.port);
  if (!address || !Number.isInteger(port) || port < 1 || port > 65535) {
    throw new Error('XHTTP 独立下载缺少有效的服务器或端口');
  }
  const downloadNetwork = externalString(downloadValue.network)?.toLowerCase() ?? 'xhttp';
  if (downloadNetwork !== 'xhttp') throw new Error('独立下载的 network 必须是 xhttp');
  const downloadXhttp = externalObject(downloadValue.xhttpSettings) ?? {};
  const downloadXmux = externalObject(downloadXhttp.xmux);
  return {
    t: 'xhttp',
    v: {
      path,
      host,
      mux,
      mode,
      download: {
        address,
        port,
        security: parseExternalDownloadSecurity(downloadValue, address),
        path: externalString(downloadXhttp.path) ?? path,
        host: externalXhttpHost(downloadXhttp.host),
        mux: parseExternalXhttpMux(downloadXmux?.maxConcurrency, '下载 XMUX'),
        mode: parseExternalXhttpMode(downloadXhttp.mode, '下载模式'),
      },
    },
  };
}

function parseExternalShareLink(raw: string): ParsedExternalShare {
  const value = raw.trim();
  if (!value) throw new Error('粘贴一条分享链接');
  if (value.startsWith('ss://')) {
    const [withoutHash, hash = ''] = value.slice(5).split('#', 2);
    let authority = withoutHash;
    if (!authority.includes('@')) authority = decodeShareBase64(authority);
    const at = authority.lastIndexOf('@');
    if (at < 1) throw new Error('Shadowsocks 链接缺少服务器');
    let userInfo = authority.slice(0, at);
    if (!userInfo.includes(':')) userInfo = decodeShareBase64(userInfo);
    const separator = userInfo.indexOf(':');
    if (separator < 1) throw new Error('Shadowsocks 链接缺少加密方式或密钥');
    const method = userInfo.slice(0, separator);
    if (!method.startsWith('2022-blake3-')) throw new Error('这里只接受 Shadowsocks 2022 链接');
    const endpoint = new URL(`http://${authority.slice(at + 1)}`);
    return {
      address: endpoint.hostname,
      port: Number(endpoint.port),
      name: decodeURIComponent(hash),
      protocol: {
        t: 'shadowsocks2022',
        v: { credential: decodeURIComponent(userInfo.slice(separator + 1)), method },
      },
      security: { t: 'none' },
    };
  }

  const url = new URL(value);
  const name = decodeURIComponent(url.hash.replace(/^#/, ''));
  if (url.protocol === 'anytls:') {
    if (url.password) throw new Error('AnyTLS 密码中的特殊字符必须使用百分号编码');
    if (!url.username) throw new Error('AnyTLS 链接缺少密码');
    const insecure = url.searchParams.get('insecure');
    if (insecure && insecure !== '0') throw new Error('暂不支持跳过 AnyTLS 证书验证，请使用有效证书');
    const requestedSecurity = url.searchParams.get('security');
    if (requestedSecurity && requestedSecurity !== 'tls') throw new Error('AnyTLS 代理出站必须使用 TLS');
    const address = url.hostname.replace(/^\[|\]$/g, '');
    return {
      address,
      port: Number(url.port || 443),
      name,
      protocol: { t: 'anytls', v: { credential: decodeURIComponent(url.username) } },
      security: {
        t: 'tls',
        v: { server_name: url.searchParams.get('sni') || address, fingerprint: url.searchParams.get('fp') || 'chrome' },
      },
    };
  }
  if (url.protocol === 'vless:') {
    const transport = parseExternalVlessTransport(url);
    const security = url.searchParams.get('security');
    const encryption = url.searchParams.get('encryption') || 'none';
    if (!vlessEncryptionIsValid(encryption))
      throw new Error('VLESS Encryption 参数无效，请粘贴完整的客户端 encryption 值');
    if (security && !['none', 'tls', 'reality'].includes(security)) throw new Error('不支持此 VLESS 安全层');
    const serverName = url.searchParams.get('sni') || url.hostname;
    const fingerprint = url.searchParams.get('fp') || 'chrome';
    const securityValue: ExternalOutboundSecurity =
      security === 'reality'
        ? {
            t: 'reality',
            v: {
              server_name: serverName,
              public_key: url.searchParams.get('pbk') || '',
              short_id: url.searchParams.get('sid') || '',
              fingerprint,
            },
          }
        : security === 'tls'
          ? { t: 'tls', v: { server_name: serverName, fingerprint } }
          : { t: 'none' };
    if (securityValue.t === 'none' && encryption === 'none')
      throw new Error('VLESS 链接必须启用 Encryption、TLS 或 REALITY');
    return {
      address: url.hostname,
      port: Number(url.port || 443),
      name,
      protocol: {
        t: 'vless',
        v: {
          credential: decodeURIComponent(url.username),
          encryption,
          flow: url.searchParams.get('flow') || null,
          transport,
        },
      },
      security: securityValue,
    };
  }
  if (url.protocol === 'socks5:' || url.protocol === 'socks:') {
    return {
      address: url.hostname,
      port: Number(url.port || 1080),
      name,
      protocol: {
        t: 'socks5',
        v: { username: decodeURIComponent(url.username) || null, credential: decodeURIComponent(url.password) },
      },
      security: { t: 'none' },
    };
  }
  if (url.protocol === 'http:' || url.protocol === 'https:') {
    return {
      address: url.hostname,
      port: Number(url.port || (url.protocol === 'https:' ? 443 : 80)),
      name,
      protocol: {
        t: 'http_connect',
        v: { username: decodeURIComponent(url.username) || null, credential: decodeURIComponent(url.password) },
      },
      security:
        url.protocol === 'https:'
          ? { t: 'tls', v: { server_name: url.hostname, fingerprint: 'chrome' } }
          : { t: 'none' },
    };
  }
  throw new Error('支持 AnyTLS、VLESS、SS2022、SOCKS5 和 HTTP(S) 分享链接；WireGuard 请手动填写');
}

function ss2022KeyIsValid(method: string, credential: string): boolean {
  if (credential === '<redacted>') return true;
  const expected = method === '2022-blake3-aes-128-gcm' ? 16 : 32;
  return (
    !!credential &&
    credential.split(':').every(part => {
      try {
        return window.atob(part).length === expected;
      } catch {
        return false;
      }
    })
  );
}

function wireguardKeyIsValid(key: string): boolean {
  if (key === '<redacted>') return true;
  try {
    return window.atob(key).length === 32;
  } catch {
    return false;
  }
}

function externalList(value: string): string[] {
  return value
    .split(/[\s,]+/)
    .map(item => item.trim())
    .filter(Boolean);
}

function externalReserved(value: string): number[] | null {
  if (!value.trim()) return [];
  const bytes = value.split(',').map(item => Number(item.trim()));
  return bytes.length === 3 && bytes.every(byte => Number.isInteger(byte) && byte >= 0 && byte <= 255) ? bytes : null;
}

function stableJson(value: unknown): string {
  return (
    JSON.stringify(value, (_key, current) => {
      if (!current || typeof current !== 'object' || Array.isArray(current)) return current;
      return Object.fromEntries(
        Object.entries(current as Record<string, unknown>).sort(([left], [right]) => left.localeCompare(right)),
      );
    }) ?? 'undefined'
  );
}

type ExternalXhttpDraft = {
  path: string;
  host: string;
  mux: string;
  mode: XhttpMode;
  downloadEnabled: boolean;
  downloadAddress: string;
  downloadPort: string;
  downloadPath: string;
  downloadHost: string;
  downloadMux: string;
  downloadMode: XhttpMode;
  downloadSecurityKind: 'tls' | 'reality';
  downloadServerName: string;
  downloadFingerprint: string;
  downloadPublicKey: string;
  downloadShortId: string;
};

function externalXhttpDraft(protocol: ExternalOutboundProtocol | null | undefined): ExternalXhttpDraft {
  const transport = protocol?.t === 'vless' ? protocol.v.transport : null;
  const xhttp = transport?.t === 'xhttp' ? transport.v : null;
  const download = xhttp?.download ?? null;
  const downloadSecurity = download?.security;
  return {
    path: xhttp?.path ?? '/',
    host: xhttp?.host ?? '',
    mux: xhttp?.mux ? String(xhttp.mux) : '',
    mode: xhttp?.mode ?? 'auto',
    downloadEnabled: !!download,
    downloadAddress: download?.address ?? '',
    downloadPort: String(download?.port ?? 443),
    downloadPath: download?.path ?? xhttp?.path ?? '/',
    downloadHost: download?.host ?? '',
    downloadMux: download?.mux ? String(download.mux) : '',
    downloadMode: download?.mode ?? 'auto',
    downloadSecurityKind: downloadSecurity?.t === 'reality' ? 'reality' : 'tls',
    downloadServerName: downloadSecurity && downloadSecurity.t !== 'none' ? downloadSecurity.v.server_name : '',
    downloadFingerprint: downloadSecurity && downloadSecurity.t !== 'none' ? downloadSecurity.v.fingerprint : 'chrome',
    downloadPublicKey: downloadSecurity?.t === 'reality' ? downloadSecurity.v.public_key : '',
    downloadShortId: downloadSecurity?.t === 'reality' ? downloadSecurity.v.short_id : '',
  };
}

function externalXhttpPathIsValid(value: string): boolean {
  return value.startsWith('/') && !/[\s?#]/.test(value);
}

function externalOptionalMuxIsValid(value: string): boolean {
  if (!value.trim()) return true;
  const parsed = Number(value);
  return Number.isInteger(parsed) && parsed >= 1 && parsed <= 128;
}

export function TunnelDeleteDialog({
  outbound,
  apps,
  onClose,
  onDeleted,
}: {
  outbound: ExternalOutbound;
  apps: SnapshotApp[];
  onClose: () => void;
  onDeleted?: () => void;
}) {
  const qc = useQueryClient();
  const references: string[] = [];
  for (const app of apps) {
    for (const step of app.steps) {
      step.rules.forEach((rule, index) => {
        if (rule.a.t === 'proxy' && rule.a.outbound === outbound.id) {
          const chain = app.chains.find(candidate => candidate.id === step.chain);
          references.push(`${app.label || app.id} / ${chain?.name || step.chain} / ${step.node} / 规则 ${index + 1}`);
        }
      });
    }
    for (const front of app.fronts) {
      if (front.external_via?.includes(outbound.id))
        references.push(`${app.label || app.id} / 前置组 ${front.name || front.id}`);
    }
  }
  const bound = outbound.bindings.length > 0;
  const remove = useMutation({
    mutationFn: () => deleteExternalOutbound(outbound.tenant, outbound.id),
    onSuccess: async () => {
      await qc.invalidateQueries({ queryKey: ['snapshot'] });
      onDeleted?.();
      onClose();
    },
  });
  return (
    <div className="external-outbound-wrap" role="dialog" aria-modal="true" aria-label="删除隧道">
      <button className="external-outbound-scrim" aria-label="关闭" onClick={onClose} />
      <section className="external-outbound-drawer">
        <header>
          <b>删除隧道 · {outbound.name}</b>
          <span className="sp" />
          <button className="btn" onClick={onClose}>
            关闭
          </button>
        </header>
        <div className="external-outbound-body">
          {references.length > 0 ? (
            <>
              <p>请先解除以下引用并保存到草稿，再删除此代理出站。</p>
              <ul>
                {references.map((reference, index) => (
                  <li key={index}>{reference}</li>
                ))}
              </ul>
            </>
          ) : (
            <p>删除「{outbound.name}」将保存到草稿，提交前可以撤销。</p>
          )}
          {bound && <p>请先在隧道详情中注销 {outbound.bindings.length} 台机器的 WARP 身份。</p>}
          {remove.error && <ErrorBox error={remove.error} />}
          <button
            className="btn danger"
            disabled={references.length > 0 || bound || remove.isPending}
            onClick={() => remove.mutate()}
          >
            删除隧道
          </button>
        </div>
      </section>
    </div>
  );
}

type EditableExternalProtocol = Exclude<ExternalOutboundProtocol['t'], 'warp'>;

export function ExternalOutboundEditor({
  tenantId,
  existing,
  onClose,
  onSaved,
  purpose = 'rule',
}: {
  tenantId: string;
  existing: ExternalOutbound | null;
  onClose: () => void;
  onSaved: (outbound: ExternalOutbound) => void;
  purpose?: 'rule' | 'resource';
}) {
  const qc = useQueryClient();
  const initialProtocol: EditableExternalProtocol =
    existing?.protocol.t && existing.protocol.t !== 'warp' ? existing.protocol.t : 'vless';
  const [entryMode, setEntryMode] = useState<'import' | 'manual'>(existing ? 'manual' : 'import');
  const [shareLink, setShareLink] = useState('');
  const parsedShare = useMemo(() => {
    if (!shareLink.trim()) return { value: null, error: '' };
    try {
      return { value: parseExternalShareLink(shareLink), error: '' };
    } catch (error) {
      return { value: null, error: error instanceof Error ? error.message : '无法解析分享链接' };
    }
  }, [shareLink]);
  const [id, setId] = useState(existing?.id ?? 'external-1');
  const [name, setName] = useState(existing?.name ?? '新代理出站');
  const [address, setAddress] = useState(existing?.address ?? '');
  const [port, setPort] = useState(String(existing?.port ?? 443));
  const [protocolKind, setProtocolKind] = useState<EditableExternalProtocol>(initialProtocol);
  const [credential, setCredential] = useState(
    existing?.protocol.t && existing.protocol.t !== 'warp' ? existing.protocol.v.credential : '',
  );
  const [encryption, setEncryption] = useState(
    existing?.protocol.t === 'vless' ? existing.protocol.v.encryption : 'none',
  );
  const [flow, setFlow] = useState(existing?.protocol.t === 'vless' ? (existing.protocol.v.flow ?? '') : '');
  const [vlessTransport, setVlessTransport] = useState<ExternalVlessTransport['t']>(
    existing?.protocol.t === 'vless' ? existing.protocol.v.transport.t : 'raw',
  );
  const [xhttp, setXhttp] = useState<ExternalXhttpDraft>(() => externalXhttpDraft(existing?.protocol));
  const [method, setMethod] = useState(
    existing?.protocol.t === 'shadowsocks2022' ? existing.protocol.v.method : '2022-blake3-aes-256-gcm',
  );
  const [username, setUsername] = useState(
    existing?.protocol.t === 'socks5' || existing?.protocol.t === 'http_connect'
      ? (existing.protocol.v.username ?? '')
      : '',
  );
  const [peerPublicKey, setPeerPublicKey] = useState(
    existing?.protocol.t === 'wireguard' ? existing.protocol.v.peer_public_key : '',
  );
  const [localAddresses, setLocalAddresses] = useState(
    existing?.protocol.t === 'wireguard' ? existing.protocol.v.local_addresses.join(', ') : '10.0.0.2/32',
  );
  const [wireguardMtu, setWireguardMtu] = useState(
    String(existing?.protocol.t === 'wireguard' ? existing.protocol.v.mtu : 1420),
  );
  const [reserved, setReserved] = useState(
    existing?.protocol.t === 'wireguard' ? existing.protocol.v.reserved.join(',') : '',
  );
  const [keepAlive, setKeepAlive] = useState(
    String(existing?.protocol.t === 'wireguard' ? existing.protocol.v.keep_alive : 0),
  );
  const [allowedIps, setAllowedIps] = useState(
    existing?.protocol.t === 'wireguard' ? existing.protocol.v.allowed_ips.join(', ') : '0.0.0.0/0, ::/0',
  );
  const [noKernelTun, setNoKernelTun] = useState(
    existing?.protocol.t === 'wireguard' ? existing.protocol.v.no_kernel_tun : false,
  );
  const [wireguardDomainStrategy, setWireguardDomainStrategy] = useState<
    Extract<ExternalOutboundProtocol, { t: 'wireguard' }>['v']['domain_strategy']
  >(existing?.protocol.t === 'wireguard' ? existing.protocol.v.domain_strategy : 'ForceIP');
  const [securityKind, setSecurityKind] = useState<ExternalOutboundSecurity['t']>(existing?.security.t ?? 'tls');
  const [serverName, setServerName] = useState(
    existing?.security.t === 'none' ? '' : (existing?.security.v.server_name ?? ''),
  );
  const [fingerprint, setFingerprint] = useState(
    existing?.security.t === 'none' ? 'chrome' : (existing?.security.v.fingerprint ?? 'chrome'),
  );
  const [publicKey, setPublicKey] = useState(existing?.security.t === 'reality' ? existing.security.v.public_key : '');
  const [shortId, setShortId] = useState(existing?.security.t === 'reality' ? existing.security.v.short_id : '');
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<unknown>(null);

  const applyParsedShare = (parsed: ParsedExternalShare) => {
    if (parsed.protocol.t === 'warp') return;
    setAddress(parsed.address);
    setPort(String(parsed.port));
    if (parsed.name) setName(parsed.name);
    setProtocolKind(parsed.protocol.t);
    setCredential(parsed.protocol.v.credential);
    if (parsed.protocol.t === 'vless') {
      setEncryption(parsed.protocol.v.encryption);
      setFlow(parsed.protocol.v.flow ?? '');
      setVlessTransport(parsed.protocol.v.transport.t);
      setXhttp(externalXhttpDraft(parsed.protocol));
    } else if (parsed.protocol.t === 'shadowsocks2022') {
      setMethod(parsed.protocol.v.method);
    } else if (parsed.protocol.t === 'socks5' || parsed.protocol.t === 'http_connect') {
      setUsername(parsed.protocol.v.username ?? '');
    }
    setSecurityKind(parsed.security.t);
    if (parsed.security.t !== 'none') {
      setServerName(parsed.security.v.server_name);
      setFingerprint(parsed.security.v.fingerprint);
    }
    if (parsed.security.t === 'reality') {
      setPublicKey(parsed.security.v.public_key);
      setShortId(parsed.security.v.short_id);
    }
  };

  const patchXhttp = (patch: Partial<ExternalXhttpDraft>) => setXhttp(current => ({ ...current, ...patch }));

  const chooseVlessTransport = (next: ExternalVlessTransport['t']) => {
    setVlessTransport(next);
    if (next === 'xhttp') setFlow('');
  };

  const chooseProtocol = (next: EditableExternalProtocol) => {
    if (next !== protocolKind) {
      setCredential(
        next === initialProtocol && existing?.protocol.t !== 'warp' ? (existing?.protocol.v.credential ?? '') : '',
      );
      setUsername(
        next === initialProtocol && (existing?.protocol.t === 'socks5' || existing?.protocol.t === 'http_connect')
          ? (existing.protocol.v.username ?? '')
          : '',
      );
    }
    setProtocolKind(next);
    if (next === 'shadowsocks2022' || next === 'socks5' || next === 'wireguard') {
      setSecurityKind('none');
    } else if (next === 'anytls' || (next === 'vless' && securityKind === 'none')) {
      setSecurityKind('tls');
    } else if (next === 'http_connect' && securityKind === 'reality') {
      setSecurityKind('tls');
    }
  };

  const authenticatedProxy = protocolKind === 'socks5' || protocolKind === 'http_connect';
  const authPairValid =
    !authenticatedProxy ||
    (!username.trim() && (!credential || credential === '<redacted>')) ||
    (!!username.trim() && !!credential);
  const reservedBytes = externalReserved(reserved);
  const rawOnly = protocolKind === 'shadowsocks2022' || protocolKind === 'socks5' || protocolKind === 'wireguard';
  const currentEntryCanSave = externalImportCanSave(entryMode, parsedShare.value !== null);
  const primaryRealityValid =
    securityKind !== 'reality' ||
    (realityServerNameIsValid(serverName) &&
      realityPublicKeyIsValid(publicKey) &&
      realityShortIdIsValid(shortId) &&
      realityFingerprintIsValid(fingerprint));
  const downloadRealityValid =
    xhttp.downloadSecurityKind !== 'reality' ||
    (realityServerNameIsValid(xhttp.downloadServerName) &&
      realityPublicKeyIsValid(xhttp.downloadPublicKey) &&
      realityShortIdIsValid(xhttp.downloadShortId) &&
      realityFingerprintIsValid(xhttp.downloadFingerprint));
  const xhttpValid =
    protocolKind !== 'vless' ||
    vlessTransport !== 'xhttp' ||
    (!flow &&
      externalXhttpPathIsValid(xhttp.path) &&
      externalOptionalMuxIsValid(xhttp.mux) &&
      (!xhttp.downloadEnabled ||
        (xhttp.mode !== 'stream-one' &&
          !!xhttp.downloadAddress.trim() &&
          Number.isInteger(Number(xhttp.downloadPort)) &&
          Number(xhttp.downloadPort) >= 1 &&
          Number(xhttp.downloadPort) <= 65535 &&
          externalXhttpPathIsValid(xhttp.downloadPath) &&
          externalOptionalMuxIsValid(xhttp.downloadMux) &&
          !!xhttp.downloadServerName.trim() &&
          downloadRealityValid)));
  const valid =
    currentEntryCanSave &&
    /^[a-z0-9][a-z0-9_-]*$/.test(id) &&
    !!tenantId &&
    !!name.trim() &&
    !!address.trim() &&
    Number(port) >= 1 &&
    Number(port) <= 65535 &&
    (authenticatedProxy || !!credential.trim()) &&
    authPairValid &&
    (protocolKind !== 'shadowsocks2022' || ss2022KeyIsValid(method, credential)) &&
    (protocolKind !== 'wireguard' ||
      (wireguardKeyIsValid(credential) &&
        wireguardKeyIsValid(peerPublicKey) &&
        externalList(localAddresses).length > 0 &&
        Number(wireguardMtu) >= 576 &&
        Number(wireguardMtu) <= 9000 &&
        reservedBytes !== null &&
        Number(keepAlive) >= 0 &&
        Number(keepAlive) <= 65535 &&
        externalList(allowedIps).length > 0)) &&
    (!rawOnly || securityKind === 'none') &&
    (protocolKind !== 'http_connect' || securityKind !== 'reality') &&
    (protocolKind !== 'vless' ||
      (vlessEncryptionIsValid(encryption) && (encryption !== 'none' || securityKind !== 'none'))) &&
    (protocolKind !== 'anytls' || securityKind === 'tls') &&
    xhttpValid &&
    (securityKind === 'none' || !!serverName.trim()) &&
    primaryRealityValid;

  const downloadSecurity: ExternalOutboundSecurity =
    xhttp.downloadSecurityKind === 'tls'
      ? {
          t: 'tls',
          v: {
            server_name: xhttp.downloadServerName.trim(),
            fingerprint: xhttp.downloadFingerprint.trim() || 'chrome',
          },
        }
      : {
          t: 'reality',
          v: {
            server_name: xhttp.downloadServerName.trim(),
            public_key: xhttp.downloadPublicKey.trim(),
            short_id: xhttp.downloadShortId.trim(),
            fingerprint: xhttp.downloadFingerprint.trim() || 'chrome',
          },
        };
  const transport: ExternalVlessTransport =
    vlessTransport === 'raw'
      ? { t: 'raw' }
      : {
          t: 'xhttp',
          v: {
            path: xhttp.path.trim(),
            host: xhttp.host.trim() || null,
            mux: xhttp.mux.trim() ? Number(xhttp.mux) : null,
            mode: xhttp.mode,
            download: xhttp.downloadEnabled
              ? {
                  address: xhttp.downloadAddress.trim(),
                  port: Number(xhttp.downloadPort),
                  security: downloadSecurity,
                  path: xhttp.downloadPath.trim(),
                  host: xhttp.downloadHost.trim() || null,
                  mux: xhttp.downloadMux.trim() ? Number(xhttp.downloadMux) : null,
                  mode: xhttp.downloadMode,
                }
              : null,
          },
        };
  const protocol: ExternalOutboundProtocol =
    protocolKind === 'anytls'
      ? { t: 'anytls', v: { credential } }
      : protocolKind === 'vless'
        ? { t: 'vless', v: { credential, encryption, flow: flow || null, transport } }
        : protocolKind === 'shadowsocks2022'
          ? { t: 'shadowsocks2022', v: { credential, method } }
          : protocolKind === 'socks5'
            ? { t: 'socks5', v: { username: username.trim() || null, credential } }
            : protocolKind === 'http_connect'
              ? { t: 'http_connect', v: { username: username.trim() || null, credential } }
              : {
                  t: 'wireguard',
                  v: {
                    credential,
                    peer_public_key: peerPublicKey.trim(),
                    local_addresses: externalList(localAddresses),
                    mtu: Number(wireguardMtu),
                    reserved: reservedBytes ?? [],
                    keep_alive: Number(keepAlive),
                    allowed_ips: externalList(allowedIps),
                    no_kernel_tun: noKernelTun,
                    domain_strategy: wireguardDomainStrategy,
                  },
                };
  const security: ExternalOutboundSecurity =
    securityKind === 'none'
      ? { t: 'none' }
      : securityKind === 'tls'
        ? {
            t: 'tls',
            v: { server_name: serverName.trim(), fingerprint: fingerprint.trim() || 'chrome' },
          }
        : {
            t: 'reality',
            v: {
              server_name: serverName.trim(),
              public_key: publicKey.trim(),
              short_id: shortId.trim(),
              fingerprint: fingerprint.trim() || 'chrome',
            },
          };
  const outbound: ExternalOutbound = {
    id,
    tenant: tenantId,
    name: name.trim(),
    address: address.trim(),
    port: Number(port),
    protocol,
    security,
    bindings: existing?.bindings ?? [],
  };
  const dirty =
    existing === null ||
    stableJson(outbound) !==
      stableJson({
        id: existing.id,
        tenant: existing.tenant,
        name: existing.name,
        address: existing.address,
        port: existing.port,
        protocol: existing.protocol,
        security: existing.security,
        bindings: existing.bindings,
      });

  const save = async () => {
    if (!valid || !dirty) return;
    setSaving(true);
    setError(null);
    try {
      await upsertExternalOutbound({
        id,
        tenant_id: tenantId,
        name: outbound.name,
        address: outbound.address,
        port: outbound.port,
        protocol,
        security,
      });
      await qc.invalidateQueries({ queryKey: ['snapshot'] });
      onSaved(outbound);
    } catch (nextError) {
      setError(nextError);
    } finally {
      setSaving(false);
    }
  };

  return (
    <div
      className="external-outbound-wrap"
      role="dialog"
      aria-modal="true"
      aria-label={purpose === 'resource' ? (existing ? '编辑隧道' : '创建隧道') : '配置代理出站'}
    >
      <button className="external-outbound-scrim" aria-label="关闭" onClick={onClose} />
      <section className="external-outbound-drawer">
        <header>
          <b>
            {purpose === 'resource' ? (existing ? '编辑隧道' : '创建隧道') : existing ? '配置代理出站' : '创建代理出站'}
          </b>
          <span className="sp" />
          <button className="btn" onClick={onClose}>
            关闭
          </button>
        </header>
        <nav className="external-outbound-tabs" aria-label="录入方式">
          <button
            type="button"
            className={entryMode === 'import' ? 'on' : ''}
            aria-pressed={entryMode === 'import'}
            onClick={() => setEntryMode('import')}
          >
            粘贴分享链接
          </button>
          <button
            type="button"
            className={entryMode === 'manual' ? 'on' : ''}
            aria-pressed={entryMode === 'manual'}
            onClick={() => setEntryMode('manual')}
          >
            手动填写
          </button>
        </nav>
        <div className="external-outbound-body">
          {entryMode === 'import' ? (
            <section className="external-import-pane">
              <div className="external-form-grid">
                <span className="external-form-label">名称</span>
                <span className="external-form-value">
                  <input className="f" value={name} onChange={event => setName(event.target.value)} />
                  <small>给规则选择器看的名称；链接中的片段名称会自动带入。</small>
                </span>
                <span className="external-form-label">分享链接</span>
                <span className="external-form-value">
                  <textarea
                    className="f mono external-share-link"
                    placeholder="anytls://… / vless://… / ss://… / socks5://… / https://…"
                    value={shareLink}
                    onChange={event => {
                      const value = event.target.value;
                      setShareLink(value);
                      if (!value.trim()) return;
                      try {
                        applyParsedShare(parseExternalShareLink(value));
                      } catch {
                        /* 错误由 parsedShare 统一显示；上一次有效值保留，方便修正 URI。 */
                      }
                    }}
                  />
                  <small>在浏览器内解析；只接受单条 URI，不请求订阅地址。</small>
                </span>
              </div>
              {parsedShare.value ? (
                <article className="external-parse-card">
                  <header>✓ 已识别，可以生成 Xray outbound</header>
                  <div>
                    <span>
                      <small>协议</small>
                      <b>{externalProtocolLabel(parsedShare.value.protocol.t)}</b>
                    </span>
                    <span>
                      <small>服务器</small>
                      <b className="mono">
                        {parsedShare.value.address}:{parsedShare.value.port}
                      </b>
                    </span>
                    <span>
                      <small>传输 / 安全</small>
                      <b>
                        {parsedShare.value.protocol.t === 'vless' &&
                        parsedShare.value.protocol.v.transport.t === 'xhttp'
                          ? 'XHTTP'
                          : 'RAW'}{' '}
                        /{' '}
                        {parsedShare.value.protocol.t === 'vless' && parsedShare.value.protocol.v.encryption !== 'none'
                          ? `VLESS Encryption${parsedShare.value.security.t === 'none' ? '' : ` + ${parsedShare.value.security.t.toUpperCase()}`}`
                          : parsedShare.value.security.t.toUpperCase()}
                      </b>
                    </span>
                    <span>
                      <small>流控 / 指纹</small>
                      <b>
                        {parsedShare.value.protocol.t === 'vless'
                          ? (parsedShare.value.protocol.v.flow ?? '无').replace('xtls-rprx-', '').toUpperCase()
                          : '—'}{' '}
                        / {parsedShare.value.security.t === 'none' ? '—' : parsedShare.value.security.v.fingerprint}
                      </b>
                    </span>
                  </div>
                </article>
              ) : (
                shareLink.trim() && <div className="external-parse-error">{parsedShare.error}</div>
              )}
              <div className="external-form-grid compact">
                <span className="external-form-label">出站 ID</span>
                <span className="external-form-value">
                  <input
                    className="f mono"
                    disabled={!!existing}
                    value={id}
                    onChange={event => setId(event.target.value)}
                  />
                  <small>全局唯一。规则只保存这个 ID，不复制协议字段。</small>
                </span>
                <span className="external-form-label">解析策略</span>
                <span className="external-form-value">
                  <span className="external-readonly-field">保持域名（AsIs）</span>
                </span>
              </div>
              <p className="external-secret-note">
                🔒 UUID、密码、私钥等敏感字段保存为 secret；后续编辑只显示“已设置”，不会重新下发明文。
              </p>
            </section>
          ) : (
            <>
              <section className="external-manual-protocol">
                <span className="external-form-label">协议</span>
                <span className="external-form-value">
                  <span className="external-protocols">
                    <button
                      type="button"
                      className={protocolKind === 'vless' && encryption !== 'none' ? 'on' : ''}
                      aria-pressed={protocolKind === 'vless' && encryption !== 'none'}
                      onClick={() => {
                        chooseProtocol('vless');
                        setEncryption(encryption !== 'none' ? encryption : 'mlkem768x25519plus.native.1rtt.');
                        setSecurityKind('none');
                      }}
                    >
                      VLESS Encryption
                    </button>
                    {(['anytls', 'vless', 'shadowsocks2022', 'socks5', 'http_connect', 'wireguard'] as const).map(
                      protocol => (
                        <button
                          type="button"
                          className={
                            protocolKind === protocol && (protocol !== 'vless' || encryption === 'none') ? 'on' : ''
                          }
                          aria-pressed={protocolKind === protocol && (protocol !== 'vless' || encryption === 'none')}
                          key={protocol}
                          onClick={() => {
                            chooseProtocol(protocol);
                            if (protocol === 'vless') setEncryption('none');
                          }}
                        >
                          {externalProtocolLabel(protocol)}
                        </button>
                      ),
                    )}
                  </span>
                  <small>这里只列外部代理；“从本机出网 / 拒绝”继续由规则动作表达。</small>
                </span>
              </section>
              <div className="fgrid one external-manual-fields">
                <label className="row">
                  <span className="k">名称</span>
                  <span className="v">
                    <input className="f" value={name} onChange={event => setName(event.target.value)} />
                  </span>
                </label>
                <label className="row">
                  <span className="k">资源 ID</span>
                  <span className="v">
                    <input
                      className="f mono"
                      disabled={!!existing}
                      value={id}
                      onChange={event => setId(event.target.value)}
                    />
                    <span className="sub">全局唯一；创建后不变。</span>
                  </span>
                </label>
                <div className="row">
                  <span className="k">服务器</span>
                  <span className="v external-outbound-host">
                    <input
                      className="f mono"
                      aria-label="服务器地址"
                      placeholder="edge.example.com"
                      value={address}
                      onChange={event => {
                        const nextAddress = event.target.value;
                        setServerName(current => serverNameAfterAddressChange(address, current, nextAddress));
                        setAddress(nextAddress);
                      }}
                    />
                    <input
                      className="f mono"
                      type="number"
                      min={1}
                      max={65535}
                      aria-label="服务器端口"
                      value={port}
                      onChange={event => setPort(event.target.value)}
                    />
                  </span>
                </div>
                {authenticatedProxy && (
                  <label className="row">
                    <span className="k">用户名（可选）</span>
                    <span className="v">
                      <input
                        className="f mono"
                        autoComplete="off"
                        value={username}
                        onChange={event => {
                          setUsername(event.target.value);
                          if (!event.target.value.trim() && credential === '<redacted>') setCredential('');
                        }}
                      />
                      <span className="sub">用户名和密码都留空即不认证；填写时必须成对。</span>
                    </span>
                  </label>
                )}
                <label className="row">
                  <span className="k">
                    {protocolKind === 'vless'
                      ? 'UUID'
                      : protocolKind === 'shadowsocks2022'
                        ? '预共享密钥'
                        : protocolKind === 'wireguard'
                          ? '本地私钥'
                          : protocolKind === 'anytls'
                            ? '密码'
                            : '密码（可选）'}
                  </span>
                  <span className="v">
                    <input
                      className="f mono"
                      type="password"
                      autoComplete="new-password"
                      value={credential}
                      onChange={event => setCredential(event.target.value)}
                    />
                    {existing && <span className="sub">保持 &lt;redacted&gt; 可沿用已密封的凭据。</span>}
                    {protocolKind === 'shadowsocks2022' && (
                      <span className="sub">
                        Base64 编码的 {method === '2022-blake3-aes-128-gcm' ? 16 : 32} 字节 PSK；多用户服务端填写
                        server-key:user-key。
                      </span>
                    )}
                    {protocolKind === 'wireguard' && (
                      <span className="sub">Base64 编码的 32 字节 WireGuard 私钥；只会以密文保存。</span>
                    )}
                  </span>
                </label>
                {protocolKind === 'vless' && (
                  <>
                    <label className="row">
                      <span className="k">VLESS Encryption</span>
                      <span className="v">
                        <input
                          className="f mono"
                          aria-label="VLESS Encryption 参数"
                          placeholder="mlkem768x25519plus.native.1rtt.服务端公钥"
                          value={encryption}
                          aria-invalid={!vlessEncryptionIsValid(encryption)}
                          onChange={event => setEncryption(event.target.value.trim())}
                        />
                        <span className="sub">
                          粘贴服务端提供的客户端 encryption 参数；启用后无需叠加 TLS。填 none 则使用 TLS 或 REALITY。
                        </span>
                        {!vlessEncryptionIsValid(encryption) && (
                          <span className="sub external-field-error">
                            请输入完整、有效的 Encryption 参数及服务端公钥。
                          </span>
                        )}
                      </span>
                    </label>
                    <div className="row">
                      <span className="k">传输层</span>
                      <span className="v">
                        <span className="external-transport-options">
                          <button
                            type="button"
                            aria-label="RAW / TCP"
                            className={vlessTransport === 'raw' ? 'on' : ''}
                            aria-pressed={vlessTransport === 'raw'}
                            onClick={() => chooseVlessTransport('raw')}
                          >
                            <b>RAW / TCP</b>
                            <small>直连传输</small>
                          </button>
                          <button
                            type="button"
                            aria-label="XHTTP"
                            className={vlessTransport === 'xhttp' ? 'on' : ''}
                            aria-pressed={vlessTransport === 'xhttp'}
                            onClick={() => chooseVlessTransport('xhttp')}
                          >
                            <b>XHTTP</b>
                            <small>HTTP 分流传输</small>
                          </button>
                        </span>
                      </span>
                    </div>
                    <label className="row">
                      <span className="k">Flow</span>
                      <span className="v">
                        <select
                          className="f"
                          disabled={vlessTransport === 'xhttp'}
                          value={flow}
                          onChange={event => setFlow(event.target.value)}
                        >
                          <option value="">不设置</option>
                          <option value="xtls-rprx-vision">xtls-rprx-vision</option>
                          <option value="xtls-rprx-vision-udp443">xtls-rprx-vision-udp443</option>
                        </select>
                        {vlessTransport === 'xhttp' && <span className="sub">XHTTP 与 Vision Flow 不兼容。</span>}
                      </span>
                    </label>
                    {vlessTransport === 'xhttp' && (
                      <section className="external-xhttp-panel">
                        <header>
                          <span>
                            <b>XHTTP 上传链路</b>
                            <small>连接主服务器时使用的 HTTP 路径与复用参数</small>
                          </span>
                          <i>XHTTP</i>
                        </header>
                        <label className="row">
                          <span className="k">Path</span>
                          <span className="v">
                            <input
                              className="f mono"
                              placeholder="/"
                              value={xhttp.path}
                              onChange={event => patchXhttp({ path: event.target.value })}
                            />
                            <span className="sub">以 / 开头，不能包含空格、? 或 #。</span>
                          </span>
                        </label>
                        <label className="row">
                          <span className="k">HTTP Host（可选）</span>
                          <span className="v">
                            <input
                              className="f mono"
                              placeholder="默认不覆盖"
                              value={xhttp.host}
                              onChange={event => patchXhttp({ host: event.target.value })}
                            />
                          </span>
                        </label>
                        <div className="row">
                          <span className="k">模式 / XMUX</span>
                          <span className="v external-xhttp-pair">
                            <select
                              className="f mono"
                              aria-label="XHTTP 上传模式"
                              value={xhttp.mode}
                              onChange={event => patchXhttp({ mode: event.target.value as XhttpMode })}
                            >
                              <option value="auto">自动</option>
                              <option value="packet-up">packet-up</option>
                              <option value="stream-up">stream-up</option>
                              <option value="stream-one">stream-one</option>
                            </select>
                            <input
                              className="f mono"
                              type="number"
                              min={1}
                              max={128}
                              aria-label="XHTTP 上传 XMUX"
                              placeholder="并发 1–128"
                              value={xhttp.mux}
                              onChange={event => patchXhttp({ mux: event.target.value })}
                            />
                          </span>
                        </div>
                        <label className="row external-xhttp-download-switch">
                          <span className="k">独立下载链路</span>
                          <span className="v">
                            <span className="checkline">
                              <input
                                type="checkbox"
                                checked={xhttp.downloadEnabled}
                                onChange={event =>
                                  patchXhttp({
                                    downloadEnabled: event.target.checked,
                                    downloadPath: xhttp.downloadPath || xhttp.path,
                                  })
                                }
                              />
                              下载使用另一组服务器、TLS 和 XHTTP 参数
                            </span>
                            {xhttp.downloadEnabled && xhttp.mode === 'stream-one' && (
                              <span className="sub external-field-error">stream-one 无法拆分独立下载链路。</span>
                            )}
                          </span>
                        </label>
                        {xhttp.downloadEnabled && (
                          <div className="external-xhttp-download">
                            <header>
                              <b>下载链路</b>
                              <small>对应 Xray downloadSettings，不沿用上传端安全参数</small>
                            </header>
                            <div className="row">
                              <span className="k">服务器</span>
                              <span className="v external-outbound-host">
                                <input
                                  className="f mono"
                                  aria-label="下载服务器地址"
                                  placeholder="download.example.com"
                                  value={xhttp.downloadAddress}
                                  onChange={event => {
                                    const downloadAddress = event.target.value;
                                    setXhttp(current => ({
                                      ...current,
                                      downloadAddress,
                                      downloadServerName: serverNameAfterAddressChange(
                                        current.downloadAddress,
                                        current.downloadServerName,
                                        downloadAddress,
                                      ),
                                    }));
                                  }}
                                />
                                <input
                                  className="f mono"
                                  type="number"
                                  min={1}
                                  max={65535}
                                  aria-label="下载服务器端口"
                                  value={xhttp.downloadPort}
                                  onChange={event => patchXhttp({ downloadPort: event.target.value })}
                                />
                              </span>
                            </div>
                            <label className="row">
                              <span className="k">Path</span>
                              <span className="v">
                                <input
                                  className="f mono"
                                  value={xhttp.downloadPath}
                                  onChange={event => patchXhttp({ downloadPath: event.target.value })}
                                />
                              </span>
                            </label>
                            <label className="row">
                              <span className="k">HTTP Host（可选）</span>
                              <span className="v">
                                <input
                                  className="f mono"
                                  placeholder="默认不覆盖"
                                  value={xhttp.downloadHost}
                                  onChange={event => patchXhttp({ downloadHost: event.target.value })}
                                />
                              </span>
                            </label>
                            <div className="row">
                              <span className="k">模式 / XMUX</span>
                              <span className="v external-xhttp-pair">
                                <select
                                  className="f mono"
                                  aria-label="XHTTP 下载模式"
                                  value={xhttp.downloadMode}
                                  onChange={event => patchXhttp({ downloadMode: event.target.value as XhttpMode })}
                                >
                                  <option value="auto">自动</option>
                                  <option value="packet-up">packet-up</option>
                                  <option value="stream-up">stream-up</option>
                                  <option value="stream-one">stream-one</option>
                                </select>
                                <input
                                  className="f mono"
                                  type="number"
                                  min={1}
                                  max={128}
                                  aria-label="XHTTP 下载 XMUX"
                                  placeholder="并发 1–128"
                                  value={xhttp.downloadMux}
                                  onChange={event => patchXhttp({ downloadMux: event.target.value })}
                                />
                              </span>
                            </div>
                            <label className="row">
                              <span className="k">安全层</span>
                              <span className="v">
                                <select
                                  className="f"
                                  value={xhttp.downloadSecurityKind}
                                  onChange={event =>
                                    patchXhttp({ downloadSecurityKind: event.target.value as 'tls' | 'reality' })
                                  }
                                >
                                  <option value="tls">TLS</option>
                                  <option value="reality">REALITY</option>
                                </select>
                              </span>
                            </label>
                            <div className="row">
                              <span className="k">SNI / 指纹</span>
                              <span className="v external-xhttp-pair">
                                <input
                                  className="f mono"
                                  aria-label="下载 SNI"
                                  placeholder="download.example.com"
                                  value={xhttp.downloadServerName}
                                  aria-invalid={
                                    xhttp.downloadSecurityKind === 'reality' &&
                                    !realityServerNameIsValid(xhttp.downloadServerName)
                                  }
                                  onChange={event => patchXhttp({ downloadServerName: event.target.value })}
                                />
                                {xhttp.downloadSecurityKind === 'reality' ? (
                                  <select
                                    className="f mono"
                                    value={xhttp.downloadFingerprint}
                                    aria-label="下载 REALITY 指纹"
                                    onChange={event => patchXhttp({ downloadFingerprint: event.target.value })}
                                  >
                                    {REALITY_FINGERPRINT_OPTIONS.map(([value, label]) => (
                                      <option value={value} key={value}>
                                        {label}
                                      </option>
                                    ))}
                                  </select>
                                ) : (
                                  <input
                                    className="f mono"
                                    aria-label="下载 TLS 指纹"
                                    placeholder="chrome"
                                    value={xhttp.downloadFingerprint}
                                    onChange={event => patchXhttp({ downloadFingerprint: event.target.value })}
                                  />
                                )}
                              </span>
                              {xhttp.downloadSecurityKind === 'reality' &&
                                !realityFingerprintIsValid(xhttp.downloadFingerprint) && (
                                  <span className="sub bad">
                                    当前 Xray 不支持该 REALITY 指纹；unsafe / hellogolang 不可用。
                                  </span>
                                )}
                            </div>
                            {xhttp.downloadSecurityKind === 'reality' && (
                              <>
                                <label className="row">
                                  <span className="k">REALITY 公钥</span>
                                  <span className="v">
                                    <input
                                      className="f mono"
                                      value={xhttp.downloadPublicKey}
                                      aria-invalid={!realityPublicKeyIsValid(xhttp.downloadPublicKey)}
                                      onChange={event => patchXhttp({ downloadPublicKey: event.target.value })}
                                    />
                                    {!realityPublicKeyIsValid(xhttp.downloadPublicKey) && (
                                      <span className="sub bad">
                                        需要 base64url（无 =）编码的 32 字节 X25519 公钥。
                                      </span>
                                    )}
                                  </span>
                                </label>
                                <label className="row">
                                  <span className="k">Short ID</span>
                                  <span className="v">
                                    <input
                                      className="f mono"
                                      value={xhttp.downloadShortId}
                                      aria-invalid={!realityShortIdIsValid(xhttp.downloadShortId)}
                                      onChange={event => patchXhttp({ downloadShortId: event.target.value })}
                                    />
                                    {!realityShortIdIsValid(xhttp.downloadShortId) && (
                                      <span className="sub bad">需要 2–16 位、偶数长度的十六进制字符串。</span>
                                    )}
                                  </span>
                                </label>
                              </>
                            )}
                          </div>
                        )}
                      </section>
                    )}
                  </>
                )}
                {protocolKind === 'shadowsocks2022' && (
                  <label className="row">
                    <span className="k">加密方式</span>
                    <span className="v">
                      <select className="f mono" value={method} onChange={event => setMethod(event.target.value)}>
                        <option value="2022-blake3-aes-128-gcm">2022-blake3-aes-128-gcm</option>
                        <option value="2022-blake3-aes-256-gcm">2022-blake3-aes-256-gcm</option>
                        <option value="2022-blake3-chacha20-poly1305">2022-blake3-chacha20-poly1305</option>
                      </select>
                    </span>
                  </label>
                )}
                {protocolKind === 'wireguard' && (
                  <>
                    <label className="row">
                      <span className="k">Peer 公钥</span>
                      <span className="v">
                        <input
                          className="f mono"
                          value={peerPublicKey}
                          onChange={event => setPeerPublicKey(event.target.value)}
                        />
                      </span>
                    </label>
                    <label className="row">
                      <span className="k">隧道地址</span>
                      <span className="v">
                        <input
                          className="f mono"
                          value={localAddresses}
                          onChange={event => setLocalAddresses(event.target.value)}
                        />
                        <span className="sub">一个或多个 CIDR，以逗号或空格分隔。</span>
                      </span>
                    </label>
                    <label className="row">
                      <span className="k">Allowed IPs</span>
                      <span className="v">
                        <input
                          className="f mono"
                          value={allowedIps}
                          onChange={event => setAllowedIps(event.target.value)}
                        />
                      </span>
                    </label>
                    <div className="row">
                      <span className="k">MTU / Keepalive</span>
                      <span className="v external-outbound-host">
                        <input
                          className="f mono"
                          type="number"
                          min={576}
                          max={9000}
                          aria-label="WireGuard MTU"
                          value={wireguardMtu}
                          onChange={event => setWireguardMtu(event.target.value)}
                        />
                        <input
                          className="f mono"
                          type="number"
                          min={0}
                          max={65535}
                          aria-label="WireGuard Keepalive"
                          value={keepAlive}
                          onChange={event => setKeepAlive(event.target.value)}
                        />
                      </span>
                    </div>
                    <label className="row">
                      <span className="k">Reserved</span>
                      <span className="v">
                        <input
                          className="f mono"
                          placeholder="留空，或 0,0,0"
                          value={reserved}
                          onChange={event => setReserved(event.target.value)}
                        />
                      </span>
                    </label>
                    <label className="row">
                      <span className="k">域名策略</span>
                      <span className="v">
                        <select
                          className="f mono"
                          value={wireguardDomainStrategy}
                          onChange={event =>
                            setWireguardDomainStrategy(
                              event.target.value as Extract<
                                ExternalOutboundProtocol,
                                { t: 'wireguard' }
                              >['v']['domain_strategy'],
                            )
                          }
                        >
                          <option value="ForceIP">ForceIP</option>
                          <option value="ForceIPv4">ForceIPv4</option>
                          <option value="ForceIPv6">ForceIPv6</option>
                          <option value="ForceIPv4v6">ForceIPv4v6</option>
                          <option value="ForceIPv6v4">ForceIPv6v4</option>
                        </select>
                      </span>
                    </label>
                    <label className="row">
                      <span className="k">用户态 TUN</span>
                      <span className="v">
                        <span className="checkline">
                          <input
                            type="checkbox"
                            checked={noKernelTun}
                            onChange={event => setNoKernelTun(event.target.checked)}
                          />
                          强制 noKernelTun（无 CAP_NET_ADMIN / 多 Xray 实例时启用）
                        </span>
                      </span>
                    </label>
                  </>
                )}
                <label className="row">
                  <span className="k">安全层</span>
                  <span className="v">
                    <select
                      className="f"
                      value={securityKind}
                      onChange={event => setSecurityKind(event.target.value as ExternalOutboundSecurity['t'])}
                    >
                      <option
                        value="none"
                        disabled={(protocolKind === 'vless' && encryption === 'none') || protocolKind === 'anytls'}
                      >
                        无（RAW）
                      </option>
                      <option value="tls" disabled={rawOnly}>
                        TLS
                      </option>
                      <option value="reality" disabled={protocolKind !== 'vless'}>
                        REALITY
                      </option>
                    </select>
                    {protocolKind === 'vless' && (
                      <span className="sub">
                        VLESS Encryption 已加密时可选择「无（RAW）」；encryption 为 none 时需要 TLS 或 REALITY。
                      </span>
                    )}
                    {protocolKind === 'http_connect' && (
                      <span className="sub">HTTP CONNECT 可用 RAW 或 TLS；RAW 不适合公网直连，且只能代理 TCP。</span>
                    )}
                    {protocolKind === 'socks5' && <span className="sub">SOCKS5 本身不加密，不适合公网直连。</span>}
                    {protocolKind === 'wireguard' && (
                      <span className="sub">Xray 的 WireGuard outbound 不支持 streamSettings。</span>
                    )}
                  </span>
                </label>
                {securityKind !== 'none' && (
                  <>
                    <label className="row">
                      <span className="k">SNI</span>
                      <span className="v">
                        <input
                          className="f mono"
                          value={serverName}
                          aria-invalid={securityKind === 'reality' && !realityServerNameIsValid(serverName)}
                          onChange={event => setServerName(event.target.value)}
                        />
                        {securityKind === 'reality' && !realityServerNameIsValid(serverName) && (
                          <span className="sub bad">SNI 不能为空，且不能包含端口、空白或通配符。</span>
                        )}
                      </span>
                    </label>
                    <label className="row">
                      <span className="k">指纹</span>
                      <span className="v">
                        {securityKind === 'reality' ? (
                          <select
                            className="f mono"
                            value={fingerprint}
                            aria-label="REALITY 指纹"
                            onChange={event => setFingerprint(event.target.value)}
                          >
                            {REALITY_FINGERPRINT_OPTIONS.map(([value, label]) => (
                              <option value={value} key={value}>
                                {label}
                              </option>
                            ))}
                          </select>
                        ) : (
                          <input
                            className="f mono"
                            value={fingerprint}
                            onChange={event => setFingerprint(event.target.value)}
                          />
                        )}
                        {securityKind === 'reality' && !realityFingerprintIsValid(fingerprint) && (
                          <span className="sub bad">
                            当前 Xray 不支持该 REALITY 指纹；unsafe / hellogolang 不可用。
                          </span>
                        )}
                      </span>
                    </label>
                  </>
                )}
                {securityKind === 'reality' && (
                  <>
                    <label className="row">
                      <span className="k">公钥</span>
                      <span className="v">
                        <input
                          className="f mono"
                          value={publicKey}
                          aria-invalid={!realityPublicKeyIsValid(publicKey)}
                          onChange={event => setPublicKey(event.target.value)}
                        />
                        {!realityPublicKeyIsValid(publicKey) && (
                          <span className="sub bad">需要 base64url（无 =）编码的 32 字节 X25519 公钥。</span>
                        )}
                      </span>
                    </label>
                    <label className="row">
                      <span className="k">Short ID</span>
                      <span className="v">
                        <input
                          className="f mono"
                          value={shortId}
                          aria-invalid={!realityShortIdIsValid(shortId)}
                          onChange={event => setShortId(event.target.value)}
                        />
                        {!realityShortIdIsValid(shortId) && (
                          <span className="sub bad">需要 2–16 位、偶数长度的十六进制字符串。</span>
                        )}
                      </span>
                    </label>
                  </>
                )}
              </div>
            </>
          )}
          {error !== null && <ErrorBox error={error} />}
        </div>
        <footer>
          <span className="note">
            {purpose === 'rule'
              ? '保存后自动选到当前规则，仍需“保存到草稿”。'
              : '修改会先进入草稿，提交后才成为正式配置。'}
          </span>
          <span className="sp" />
          <button className="btn" onClick={onClose}>
            取消
          </button>
          <button
            className="btn primary"
            disabled={!valid || saving || !dirty}
            title={!dirty ? '没有修改' : undefined}
            onClick={() => void save()}
          >
            {saving
              ? '保存中…'
              : purpose === 'rule'
                ? existing
                  ? '保存并选中'
                  : '创建并选中'
                : existing
                  ? '保存到草稿'
                  : '创建到草稿'}
          </button>
        </footer>
      </section>
    </div>
  );
}
