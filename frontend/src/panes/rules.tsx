import {
  Fragment,
  Suspense,
  createContext,
  lazy,
  useContext,
  useEffect,
  useId,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from 'react';
import { HOP_WIRE_OPTIONS } from '../ui/format';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  fetchCompileView,
  fetchNodes,
  fetchRevisions,
  fetchSettings,
  fetchSnapshot,
  pruneChain,
  putStep,
  reorderNodeEgressDns,
  setNodeEgressDns,
  upsertExternalOutbound,
  type DestMatch,
  type EgressDnsResolution,
  type ExternalOutbound,
  type ExternalOutboundProtocol,
  type ExternalOutboundSecurity,
  type ExternalVlessTransport,
  type HopDial,
  type HopInRequest,
  type HopPool,
  type SnapshotStep,
  type Rule,
  type RuleAction,
  type StepAccept,
  type XhttpMode,
} from '../api';
import { ErrorBox } from '../ui/bits';
import { freePortAcross, occupiedPorts, type PortOwners } from './ports';
import { externalImportCanSave, serverNameAfterAddressChange } from '../external-outbound';
import {
  REALITY_FINGERPRINT_OPTIONS,
  realityFingerprintIsValid,
  realityPublicKeyIsValid,
  realityServerNameIsValid,
  realityShortIdIsValid,
} from '../reality';

// The tunnel resource page is intentionally absent from primary navigation. Load its shared WARP
// lifecycle editor only when an operator asks to manage a WARP target from a rule; a dynamic edge
// also avoids turning `tunnels -> rules -> tunnels` into an eager module cycle.
const WarpRuleManager = lazy(() => import('./tunnels').then(module => ({ default: module.WarpRuleManager })));

// 未填写 dial 的规则读取后为 undefined，等同于 overlay。在此统一补全，
// 避免每处各写一次 `?? {t:'overlay'}`，遗漏其中一处会导致下拉框为空。
const forwardDial = (a: RuleAction): HopDial => (a.t === 'forward' ? (a.dial ?? { t: 'overlay' }) : { t: 'overlay' });

// 同理，未填写 pool 的规则读取后为 undefined，表示每次新建连接。
const forwardPool = (a: RuleAction): HopPool => (a.t === 'forward' ? (a.pool ?? { t: 'none' }) : { t: 'none' });

// 三档按复用程度递增排列，不按哪一档为默认值排列——新建的默认值是连接池（POOL_DEFAULT），
// 位于中间：该顺序表示代价梯度，按顺序阅读即可了解每一档的取舍。
// 与 DIAL_ORDER 一样将排列定义在模块层，避免下拉框的顺序和其他位置的判定分别定义后不一致。
export const POOL_ORDER: HopPool['t'][] = ['none', 'pool', 'merge'];
export const POOL_LABEL: Record<HopPool['t'], string> = {
  none: '每次新建',
  pool: '连接池',
  merge: '合并流',
};
/* 新建一跳时的出站连接配置。与上面 `forwardPool` 的回退值不同，两者必须区分：
   后者表示该规则中未填写 pool，只能取 none——模型中 HopPool 的 #[default] 即为 none，
   历史修订重新编译需要逐字节一致，修改读取逻辑会使机队中已有的跳全部启用连接池。
   本值是新建一跳时的初始值，与历史数据无关。 */
export const POOL_DEFAULT: HopPool = { t: 'pool' };

/* 新建转发规则的动作，四个建链入口共用同一份。分散定义时增加一档默认值需要修改四处，
   遗漏的一处不会报错，只是行为与其他位置不同。
   反向档不带 pool：本机不发起连接，携带该字段时编译器会报 rule.pool-on-reverse。 */
export const forwardAction = (to: string, dial: HopDial, pool: HopPool = POOL_DEFAULT): RuleAction =>
  dial.t === 'reverse' ? { t: 'forward', to, dial } : { t: 'forward', to, dial, pool };

// 选择合并流时输入框的初始值，同时也是 xray 的默认值。
export const MERGE_DEFAULT = 8;
export const MERGE_MIN = 2;
export const MERGE_MAX = 128;

// 没有跨机器通用的 REALITY 站点。新建中转入口保持空白，要求操作者明确填写；已有配置
// 始终显示其自身站点，不会被某个 UI 常量静默覆盖。
const EMPTY_REALITY_SITE = { dest: '', names: '' };

// 中转端口从该值开始向上查找空闲端口。选择高位段是为了与接入面和系统服务分开。
// 该值来自全局设置（`settings.ports.hop_base`），下面的常量只是设置尚未加载时的回退值——
// 加载完成后使用设置中的值。若使用硬编码，运营者修改设置后界面仍会填入 20000。
const HOP_PORT_BASE = 20000;

/** 中转端口的起始值，取自全局设置；设置尚未加载时使用回退值。 */
function useHopPortBase(): number {
  const settings = useQuery({ queryKey: ['settings'], queryFn: () => fetchSettings() });
  return settings.data?.ports?.hop_base || HOP_PORT_BASE;
}

/** 各机器的 overlay 地址。只存在于编译结果中——它由系统层分配，不在模型中。 */
function useOverlayAddrs(): (nodeId: string) => string {
  const revisions = useQuery({ queryKey: ['revisions'], queryFn: () => fetchRevisions() });
  const current = revisions.data?.current_revision;
  const compiled = useQuery({
    queryKey: ['compile', current],
    queryFn: () => fetchCompileView(current!),
    enabled: !!current,
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
export function usePortPool(): Map<string, PortOwners> {
  const snapshot = useQuery({ queryKey: ['snapshot'], queryFn: () => fetchSnapshot() });
  const nodeList = useQuery({ queryKey: ['nodes'], queryFn: () => fetchNodes() });
  const revisions = useQuery({ queryKey: ['revisions'], queryFn: () => fetchRevisions() });
  const current = revisions.data?.current_revision;
  const compiled = useQuery({
    queryKey: ['compile', current],
    queryFn: () => fetchCompileView(current!),
    enabled: !!current,
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
  action === 'proxy' ? 'forward' : action;

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
          <span className="egress-dns-note">
            <b>Xray 全局生效</b> · {nodeName}
          </span>
        </span>
      )}
    </>
  );
}

const matchValues = (m: DestMatch): string => ('v' in m ? (Array.isArray(m.v) ? m.v.join(', ') : String(m.v)) : '');

const isAnyRule = (rule: Rule): boolean => rule.m.t === 'any';

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
          <h4>DNS 解析策略</h4>
          <span
            className="rule-sheet-meta"
            title="Xray 按 D1 起依次选择解析器；DNS 顺序不参与链路规则匹配"
          >
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
                      {(row.resolution === null || row.originalSelector) && (
                        <span className="egress-dns-usage">
                          {row.resolution === null ? '保存后移除' : '机器全局下发'}
                        </span>
                      )}
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
  spine: string[];
  tenant: string;
  /* 该链的全部 step。包括本机的那条——计算其他节点的上游时会跳过它。 */
  steps: SnapshotStep[];
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
  const { nodeId, spine, tenant, steps, nodes } = args;
  const root = spine[0] ?? null;
  const currentStep = steps.find(s => s.node === nodeId) ?? null;
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
  for (const r of currentStep?.rules ?? []) if (r.a.t === 'forward') addCurrentTarget(r.a.to);

  const graph = new Map<string, Set<string>>();
  const addEdge = (from: string, to: string) => {
    const edges = graph.get(from) ?? new Set<string>();
    edges.add(to);
    graph.set(from, edges);
  };

  for (const s of others) {
    for (const r of s.rules) if (r.a.t === 'forward' && r.a.to) addEdge(s.node, r.a.to);
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
          ? `归 ${n.tenant_id}，这条链（${tenant}）看不见它`
          : n.node_id === root
            ? '链入口'
            : reaches(n.node_id, nodeId)
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

export function RuleEditor({
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
  shared,
  saves = true,
  readOnly = false,
}: {
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
  // 是否参与 RuleDraftScope 的批量保存。同一台机器在树中出现多次、共用一份草稿时，
  // 只由其中一处注册——两处都注册会将同一内容写入两次。
  saves?: boolean;
  // 只读：readonly 角色看到的是同一张表，只是所有控件禁用、修改入口不渲染。
  // 不使用简化的展示形式——该机器的配置内容对只读角色和可编辑角色是同一项信息，
  // 两套展示意味着需要同步维护两处，而其中一套使用频率较低。
  // 禁用通过 `<fieldset disabled>` 实现：逐个控件添加 disabled 时，遗漏某个不会报错，
  // 表现为 readonly 角色可以修改，点击保存后才返回 403。
  readOnly?: boolean;
}) {
  const qc = useQueryClient();
  const snapshot = useQuery({ queryKey: ['snapshot'], queryFn: () => fetchSnapshot() });
  const app = snapshot.data?.snapshot.apps.find(candidate => candidate.id === appId);
  const chainTenant = app?.chains.find(chain => chain.id === chainId)?.tenant ?? '';
  const externalOutbounds = (snapshot.data?.snapshot.external_outbounds ?? []).filter(
    outbound => chainTenant === outbound.tenant || chainTenant.startsWith(`${outbound.tenant}.`),
  );
  const [externalEditor, setExternalEditor] = useState<{
    existing: ExternalOutbound | null;
    ruleIndex: number;
  } | null>(null);
  const [warpManagerId, setWarpManagerId] = useState<string | null>(null);
  const [targetPickerRule, setTargetPickerRule] = useState<number | null>(null);
  const [targetQuery, setTargetQuery] = useState('');
  const targetPickerRoot = useRef<HTMLSpanElement>(null);
  const targetMenu = useRef<HTMLSpanElement>(null);
  const [targetMenuPlacement, setTargetMenuPlacement] = useState({ below: false, maxHeight: 520 });
  useEffect(() => {
    if (targetPickerRule === null) return;
    const closeOutside = (event: PointerEvent) => {
      if (event.target instanceof Node && targetPickerRoot.current?.contains(event.target)) return;
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
      const viewportTop = window.visualViewport?.offsetTop ?? 0;
      const viewportHeight = window.visualViewport?.height ?? window.innerHeight;
      const viewportBottom = viewportTop + viewportHeight;
      const edge = 10;
      const gap = 5;
      const above = Math.max(0, rootRect.top - viewportTop - edge - gap);
      const below = Math.max(0, viewportBottom - rootRect.bottom - edge - gap);
      const wanted = Math.min(520, menu.scrollHeight);
      setTargetMenuPlacement(current => {
        // The click handler chooses the side before mounting. Keep it while filtering changes the
        // menu height; moving a live menu across the trigger looks like browser focus jumped.
        const chosen = current.below ? below : above;
        const opposite = current.below ? above : below;
        const openBelow = chosen < 80 && opposite > chosen ? !current.below : current.below;
        const available = openBelow ? below : above;
        const maxHeight = Math.max(80, Math.min(wanted, Math.floor(available)));
        return current.below === openBelow && current.maxHeight === maxHeight
          ? current
          : { below: openBelow, maxHeight };
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
  }, [targetPickerRule, targetQuery, externalOutbounds.length, peers.length]);
  // 两份草稿（规则表、各转发目标的中转端口）默认由本组件持有；`shared` 非空时交由上层持有，
  // 因为同一台机器在树中出现两次时，两处编辑的必须是同一份——它们对应库中的同一条记录
  // （steps 主键为 chain_id + node_id）。
  const ownRules = useState<Rule[]>(initial);
  const [rules, setRules] = shared ? [shared.rules, shared.setRules] : ownRules;
  const warpReferencedOnCurrentNode = (outboundId: string) =>
    rules.some(rule => rule.a.t === 'proxy' && rule.a.outbound === outboundId) ||
    (snapshot.data?.snapshot.apps ?? []).some(candidateApp =>
      candidateApp.steps.some(
        step =>
          step.node === nodeId &&
          !(candidateApp.id === appId && step.chain === chainId) &&
          step.rules.some(rule => rule.a.t === 'proxy' && rule.a.outbound === outboundId),
      ),
    );
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
  const visibleExternalOutbounds = externalOutbounds.filter(outbound =>
    targetMatches(outbound.id, outbound.name, outbound.address, externalProtocolLabel(outbound.protocol.t)),
  );
  const managedWarp = warpManagerId
    ? (externalOutbounds.find(outbound => outbound.id === warpManagerId && outbound.protocol.t === 'warp') ?? null)
    : null;
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

  // 每个目标对应一份中转端口表单状态。初始值取自对端 step 上已有的配置。
  // 将 `seedHops` 提取并导出，是因为同一台机器可能在树中出现两次（分叉后汇合），
  // 此时草稿由上层持有并共享，初始值需要由上层计算（见 ChainRulesPanel）。
  const portPool = usePortPool();
  const hopBase = useHopPortBase();
  const overlayOf = useOverlayAddrs();
  // 反向两档显示的是本机的对外地址（由编译器推导，此处只是同步显示）。
  // 查询键与其他位置一致，通常命中缓存。标记为 NAT 的不计入——该类地址无法接受反向接入，
  // 编译器的 `dialable_public_host` 判定相同，两处判定需保持一致。
  const selfNodes = useQuery({ queryKey: ['nodes'], queryFn: () => fetchNodes() });
  const selfNode = selfNodes.data?.nodes.find(n => n.node_id === nodeId);
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
    // Any 是整张表的兜底，不论动作是落地、转发还是拒绝，都不能让后续规则失效。
    // 在下拉框中把某行改成 Any 时，立即把该行移到末尾。
    if (isAnyRule(next) && i < updated.length - 1) {
      updated.splice(i, 1);
      updated.push(next);
    }
    setRules(updated);
  };
  const patchMatch = (i: number, rule: Rule, match: DestMatch) =>
    patch(i, {
      ...rule,
      m: match,
    });
  const move = (i: number, delta: number) => {
    const j = i + delta;
    if (j < 0 || j >= rules.length) return;
    // Any 可以向下归位，但不能向上；普通规则也不能越过已经位于末尾的 Any。
    if ((isAnyRule(rules[i]) && delta < 0) || (delta > 0 && isAnyRule(rules[j]))) return;
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
            { ...rule, a: { t: 'forward', to, dial, pool: dial.t === 'reverse' ? undefined : forwardPool(rule.a) } }
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

  /* 判定实现在模块层的 `hopDialOf` 中，与建链向导共用同一份。 */
  const dialOf = (to: string, kind: DialKind): HopDial =>
    hopDialOf(kind, peerOf(to), Number(hopOf(to).port) || hopBase);

  /* 判定实现在模块层的 `defaultHopDial` 中，与建链向导共用同一份。 */
  const defaultDial = (to: string): HopDial =>
    defaultHopDial({ peer: peerOf(to), self: selfAddrs, port: Number(hopOf(to).port) || hopBase });

  // 切换档位时重新计算地址。连接方式按目标统一：同一个 from -> to 只对应一个
  // outbound/tag，各规则不能使用不同的地址。
  const setDial = (i: number, to: string, kind: DialKind) => {
    const dial = dialOf(to, kind);
    setDialForTarget(to, dial, i);
  };

  const selectForwardTarget = (ruleIndex: number, rule: Rule, nextTo: string) => {
    const sameTarget = rules.find(
      (candidate, index) => index !== ruleIndex && candidate.a.t === 'forward' && candidate.a.to === nextTo,
    );
    let nextDial: HopDial = defaultDial(nextTo);
    if (sameTarget?.a.t === 'forward') nextDial = sameTarget.a.dial ?? { t: 'overlay' };
    patch(ruleIndex, { ...rule, a: forwardAction(nextTo, nextDial, poolOf(nextTo)) });
    setTargetPickerRule(null);
  };

  const selectExternalTarget = (ruleIndex: number, rule: Rule, outbound: string) => {
    patch(ruleIndex, { ...rule, a: { t: 'proxy', outbound } });
    setTargetPickerRule(null);
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
    const anyIndex = rules.findIndex(isAnyRule);
    const next: Rule = {
      // 已有兜底时，新行必须插在它之前；再创建一个 Any 会让原兜底之后的内容永远不可达。
      m: anyIndex >= 0 ? { t: 'domain_suffix', v: [] } : { t: 'any' },
      a: defaultTarget ? forwardAction(defaultTarget, defaultDial(defaultTarget)) : { t: 'egress', send_through: null },
    };
    const updated = [...rules];
    updated.splice(anyIndex >= 0 ? anyIndex : updated.length, 0, next);
    setRules(updated);
  };

  return (
    // 使用 `fieldset` 仅为其 disabled 属性（它是 HTML 中唯一能一次禁用整棵子树
    // 表单控件的元素），因此样式上重置为无视觉效果的一层，见 styles.css 的 .rule-ro。
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
            const to = r.a.t === 'forward' ? r.a.to : '';
            const externalId = r.a.t === 'proxy' ? r.a.outbound : '';
            const dial = forwardDial(r.a);
            const peer = peerOf(to);
            const dk = dialKindOf(dial, peer);
            const external = externalOutbounds.find(outbound => outbound.id === externalId) ?? null;
            const targetBadge = external
              ? externalProtocolBadge(external.protocol.t)
              : r.a.t === 'forward'
                ? 'NODE'
                : '';
            const targetLabel =
              external?.name || peer?.name || (r.a.t === 'proxy' ? '外部出站不可用' : to ? '内部节点不可用' : '');
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
                          disabled={k.t === 'any' && r.m.t !== 'any' && rules.some(isAnyRule)}
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
                      value={r.a.t === 'proxy' ? 'forward' : r.a.t}
                      onChange={e => {
                        const t = e.target.value as Exclude<RuleAction['t'], 'proxy'>;
                        const a: RuleAction =
                          t === 'forward'
                            ? // dial 要显式写：不写的语义就是 overlay（模型里 HopDial
                              // 的 #[default]），会绕过 defaultDial 的选择逻辑。
                              defaultTarget
                              ? forwardAction(defaultTarget, defaultDial(defaultTarget))
                              : externalOutbounds[0]
                                ? { t: 'proxy', outbound: externalOutbounds[0].id }
                                : { t: 'forward', to: '' }
                            : t === 'egress'
                              ? { t: 'egress', send_through: null }
                              : { t: 'block' };
                        patch(i, { ...r, a });
                        setTargetPickerRule(t === 'forward' ? i : null);
                      }}
                    >
                      <option value="forward">转发给</option>
                      <option value="egress">从这台落地</option>
                      <option value="block">拒绝</option>
                    </select>
                    {(r.a.t === 'forward' || r.a.t === 'proxy') && (
                      <span
                        className="external-target-picker"
                        ref={targetPickerRule === i ? targetPickerRoot : undefined}
                      >
                        <button
                          type="button"
                          className={`external-target-trigger${r.a.t === 'forward' ? ' node-target' : ''}`}
                          aria-expanded={targetPickerRule === i}
                          onClick={event => {
                            const opening = targetPickerRule !== i;
                            if (opening) {
                              const rect = event.currentTarget.getBoundingClientRect();
                              const viewportTop = window.visualViewport?.offsetTop ?? 0;
                              const viewportHeight = window.visualViewport?.height ?? window.innerHeight;
                              const viewportBottom = viewportTop + viewportHeight;
                              const edgeAndGap = 15;
                              const above = Math.max(0, rect.top - viewportTop - edgeAndGap);
                              const below = Math.max(0, viewportBottom - rect.bottom - edgeAndGap);
                              const openBelow = below >= 320 || below >= above;
                              setTargetMenuPlacement({
                                below: openBelow,
                                maxHeight: Math.max(80, Math.min(520, Math.floor(openBelow ? below : above))),
                              });
                            }
                            setTargetPickerRule(opening ? i : null);
                            if (opening) setTargetQuery('');
                          }}
                        >
                          <span className={`external-target-kind${external ? ' external' : ''}`}>{targetBadge}</span>
                          <span className="external-target-copy">
                            <b>{targetLabel || '选择内部节点或外部出站'}</b>
                          </span>
                          <span className="external-target-chevron">⌄</span>
                        </button>
                        {targetPickerRule === i && (
                          <span
                            ref={targetMenu}
                            className={`external-target-menu${targetMenuPlacement.below ? ' below' : ''}`}
                            style={{ maxHeight: targetMenuPlacement.maxHeight }}
                          >
                            <input
                              className="f external-target-search"
                              placeholder="搜索节点或外部出站"
                              value={targetQuery}
                              onChange={event => setTargetQuery(event.target.value)}
                            />
                            <span className="external-target-menu-label">Brocade 节点</span>
                            {[...visibleNextPeers, ...visibleInsidePeers, ...visibleForkPeers].map(candidate => (
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
                            {visibleBlockedPeers.map(candidate => (
                              <button type="button" disabled key={candidate.id} title={candidate.blocked ?? ''}>
                                <span className="external-target-kind">NODE</span>
                                <span className="external-target-copy">
                                  <b>{candidate.name || '未命名节点'}</b>
                                </span>
                                <span className="external-target-where">不能选</span>
                              </button>
                            ))}
                            <span className="external-target-menu-label">外部出站</span>
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
                                  <span className="external-target-where">租户资源</span>
                                </button>
                                {outbound.protocol.t === 'warp' && (
                                  <button
                                    type="button"
                                    className="external-target-manage"
                                    aria-label={`管理 ${outbound.name}`}
                                    title={`管理 ${selfNode?.name || nodeId} 的 WARP 注册与参数`}
                                    onClick={() => {
                                      setTargetPickerRule(null);
                                      setWarpManagerId(outbound.id);
                                    }}
                                  >
                                    管理
                                  </button>
                                )}
                              </span>
                            ))}
                            <button
                              type="button"
                              className="external-target-new"
                              onClick={() => {
                                setTargetPickerRule(null);
                                setExternalEditor({ existing: null, ruleIndex: i });
                              }}
                            >
                              <span>＋</span>
                              <b>创建外部出站</b>
                              <span>粘贴链接或手动填写</span>
                            </button>
                            {visibleNextPeers.length +
                              visibleInsidePeers.length +
                              visibleForkPeers.length +
                              visibleBlockedPeers.length +
                              visibleExternalOutbounds.length ===
                              0 && <span className="external-target-empty">没有匹配项</span>}
                          </span>
                        )}
                      </span>
                    )}
                    {r.a.t === 'forward' && (
                      <>
                        <select
                          className="f"
                          style={{ marginLeft: 6 }}
                          value={dk}
                          title="这一跳连接对端的哪个地址"
                          onChange={e => setDial(i, to, e.target.value as DialKind)}
                        >
                          {/* 排列和可用性判定都在模块层（DIAL_ORDER / dialUnavailable），
                            与默认档位的选择、建链向导的下拉框共用同一份。 */}
                          {DIAL_ORDER.map(k => (
                            <option key={k} value={k} disabled={dialUnavailable(k, peer, selfAddrs)}>
                              {DIAL_LABEL[k]}
                            </option>
                          ))}
                        </select>
                        {/* 前三档的地址由推导得出，只读；仅自定义档需要手动填写 */}
                        {dk === 'overlay' ? (
                          // 与公网两档一样直接显示地址。显示为「XX 的 overlay 地址」会要求
                          // 到其他位置查询该值——而它就在编译结果中，可直接获取。
                          // 获取失败只有一种情况：该机器尚未加入 overlay，这正是需要说明的内容。
                          <span className="mono dim" style={{ marginLeft: 6 }}>
                            {overlayOf(to) || (
                              <span style={{ color: 'var(--gold)' }}>{peer?.name || to} 不在 overlay 里</span>
                            )}
                          </span>
                        ) : dk === 'public_ipv4' ? (
                          <span className="mono dim" style={{ marginLeft: 6 }}>
                            {publicIpv4Of(peer) || (
                              <span style={{ color: 'var(--gold)' }}>这台机器没有可直连的公网 IPv4</span>
                            )}
                          </span>
                        ) : dk === 'public_ipv6' ? (
                          <span className="mono dim" style={{ marginLeft: 6 }}>
                            {publicIpv6Of(peer) || (
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
                              value={hostOf(dial)}
                              onChange={e =>
                                setDialForTarget(
                                  to,
                                  {
                                    t: 'addr',
                                    v: formatHostPort(e.target.value, Number(hopOf(to).port) || hopBase),
                                  },
                                  i,
                                )
                              }
                            />
                            {natPublicHostOf(peer, hostOf(dial)) && (
                              <span className="sub" style={{ color: 'var(--gold)' }}>
                                该地址为 {natPublicHostOf(peer, hostOf(dial))} 且标记为经 NAT，编译会拒绝。
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
                        disabled={i === 0 || isAnyRule(r)}
                        onClick={() => move(i, -1)}
                        title={isAnyRule(r) ? '任意是兜底规则，固定在末尾' : '上移'}
                      >
                        ↑
                      </button>
                      <button
                        className="btn"
                        disabled={i === rules.length - 1 || rules[i + 1]?.m.t === 'any'}
                        onClick={() => move(i, 1)}
                        title={rules[i + 1]?.m.t === 'any' ? '不能移动到任意兜底之后' : '下移'}
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
                  <span className="egress-dns-usage">机器全局下发</span>
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
              const to = r.a.t === 'forward' ? r.a.to : '';
              const externalId = r.a.t === 'proxy' ? r.a.outbound : '';
              const external = externalOutbounds.find(outbound => outbound.id === externalId) ?? null;
              const peer = peerOf(to);
              const targetBadge = external ? externalProtocolBadge(external.protocol.t) : 'NODE';
              const targetLabel =
                external?.name || peer?.name || (r.a.t === 'proxy' ? '外部出站不可用' : to || '内部节点不可用');
              const actionLabel =
                r.a.t === 'forward' || r.a.t === 'proxy' ? '转发给' : r.a.t === 'egress' ? '从这台落地' : '拒绝';
              return (
                <tr
                  className="rule-fallback-row"
                  aria-label="编译器生成的不可编辑兜底规则"
                  title="编译器根据当前配置自动生成，不能在这里修改"
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
                    <span className={`f rule-action-select rule-action-${ruleActionTone(r.a.t)} rule-readonly-select`}>
                      {actionLabel}
                    </span>
                    {(r.a.t === 'forward' || r.a.t === 'proxy') && (
                      <span className="external-target-picker">
                        <span className="external-target-trigger rule-readonly-target">
                          <span className={`external-target-kind${external ? ' external' : ''}`}>{targetBadge}</span>
                          <span className="external-target-copy">
                            <b>{targetLabel}</b>
                          </span>
                        </span>
                      </span>
                    )}
                    {readOnly && <span className="rule-fallback-sign inline">自动补齐 · 只读</span>}
                  </td>
                  {!readOnly && (
                    <td className="rule-fallback-sign" style={{ width: 120 }}>
                      自动补齐 · 只读
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
              外部出站 <code>{outboundId || '未选择'}</code> 不存在，请重新选择或创建资源。
            </div>
          );
        }
        const facts = externalOutboundFacts(outbound);
        return (
          <section className="external-outbound-summary" key={`external-${ruleIndex}`}>
            <header>
              <h4>外部出站</h4>
              <span className="external-summary-protocol">{externalProtocolLabel(outbound.protocol.t)}</span>
              <b>{outbound.name}</b>
              <span className="sp" />
              <span className="note">由 {selfNode?.name || nodeId} 发起</span>
              {!readOnly && (
                <button
                  type="button"
                  className="btn"
                  onClick={() =>
                    outbound.protocol.t === 'warp'
                      ? setWarpManagerId(outbound.id)
                      : setExternalEditor({ existing: outbound, ruleIndex })
                  }
                >
                  {outbound.protocol.t === 'warp' ? '机器设置' : '编辑'}
                </button>
              )}
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

      {/* 该跳在对端一侧的配置：使用哪个端口、如何加密。
          配置在此处而非对端页面，因为它属于该跳的组成部分。中转端口关联在
          `(chain, node)` 上，同一条链中多个上游连接同一目标时复用该入口配置。 */}
      {reverseTargets.length > 0 && (
        <div className="panel" style={{ marginTop: 10 }}>
          <header>
            <h4>反向接入口</h4>
            <span className="hint">
              {reverseTargets.map(to => peerOf(to)?.name || to).join('、')} 从本机的这个端口接入
            </span>
          </header>
          <div className="fgrid one">
            <div className="row">
              <span className="k auto">本机</span>
              <span className="v">
                <span className="hop-in">
                  <span className="hopfld">
                    <span className="hopfld-lbl">端口配置</span>
                    <input
                      className="f mono"
                      style={{ width: 90 }}
                      value={hopOf(nodeId).port}
                      placeholder={String(hopBase)}
                      onChange={e => setHopPort(nodeId, e.target.value)}
                    />
                  </span>
                  <span className="hopfld-sep" />
                  <span className="hopfld">
                    <span className="hopfld-lbl">协议</span>
                    <select
                      className="f"
                      value={hopOf(nodeId).kind}
                      onChange={e => patchHop(nodeId, { kind: e.target.value as ReturnType<typeof hopOf>['kind'] })}
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
                  </span>
                  {hopOf(nodeId).kind === 'reality' && (
                    <>
                      <input
                        className="f mono"
                        style={{ width: 180 }}
                        value={hopOf(nodeId).dest}
                        placeholder="example.com:443"
                        onChange={e => patchHop(nodeId, { dest: e.target.value })}
                      />
                      <input
                        className="f mono"
                        style={{ width: 180 }}
                        value={hopOf(nodeId).names}
                        placeholder="server_names"
                        onChange={e => patchHop(nodeId, { names: e.target.value })}
                      />
                    </>
                  )}
                </span>
                <span className="sub">
                  该端口开在本机，<b>同一台机器上各条链必须错开</b>，冲突时编译会报 node.port-clash。
                  {hopOf(nodeId).kind === 'none' && (
                    <b style={{ color: 'var(--gold)' }}> 明文接入可能暴露 UUID 和目标地址。</b>
                  )}
                </span>
              </span>
            </div>
          </div>
        </div>
      )}

      {normalTargets.length > 0 && (
        <div className="panel" style={{ marginTop: 10 }}>
          <header>
            {/* 标题由「转发目标的中转入口」改为当前名称：该表配置的一直是该跳的两端，
                而原名称只涵盖对端一侧。加入出站连接配置后，不修改名称会导致
                在「入口」标题下配置本机出站。 */}
            <h4>这一跳</h4>
            <span className="hint">对端在哪个端口接入、本机如何连接过去</span>
          </header>
          <div className="fgrid one">
            {normalTargets.map(to => {
              const h = hopOf(to);
              const peer = peerOf(to);
              const pool = poolOf(to);
              // 是否有连接从 wg 之外直接连接它。该判定决定 inbound 绑定的地址——
              // 存在直连时绑定 0.0.0.0，全部走 overlay 时才绑定 overlay 地址
              // （physical/node.rs 的 listen 判定）。
              const dialedDirectly = rules.some(
                r => r.a.t === 'forward' && r.a.to === to && forwardDial(r.a).t !== 'overlay',
              );
              return (
                <div key={to} className="row">
                  <span className="k auto" title={to}>
                    {peer?.name || to}
                  </span>
                  <span className="v">
                    <span className="hop-in">
                      <span className="hopfld">
                        <span className="hopfld-lbl">端口配置</span>
                        {/* 走 overlay 时同样显示。该端口会被实际绑定：
                            xray 的 inbound 需要监听一个端口，wg 只是封装了该跳，
                            端口仍然存在。它同样参与端口冲突校验，修改后会重启 xray。
                            此处此前显示为「已被 WireGuard 托管」，会被理解为不存在端口，
                            在排查时会导致方向错误。 */}
                        <input
                          className="f mono"
                          style={{ width: 90 }}
                          value={h.port}
                          placeholder={String(hopBase)}
                          onChange={e => setHopPort(to, e.target.value)}
                        />
                        {!dialedDirectly && <span className="sub">监听 overlay 地址，wg 之外无法连接</span>}
                      </span>
                      <span className="hopfld-sep" />
                      <span className="hopfld">
                        <span className="hopfld-lbl">协议</span>
                        <select
                          className="f"
                          value={h.kind}
                          onChange={e => patchHop(to, { kind: e.target.value as typeof h.kind })}
                        >
                          {/* 转发目标的端口，四档均可选 */}
                          {HOP_WIRE_OPTIONS.map(option => (
                            <option key={option.kind} value={option.kind}>
                              {option.label}
                            </option>
                          ))}
                        </select>
                      </span>
                      {h.kind === 'reality' && (
                        <>
                          <input
                            className="f mono"
                            style={{ width: 180 }}
                            value={h.dest}
                            placeholder="example.com:443"
                            onChange={e => patchHop(to, { dest: e.target.value })}
                          />
                          <input
                            className="f mono"
                            style={{ width: 180 }}
                            value={h.names}
                            placeholder="server_names"
                            onChange={e => patchHop(to, { names: e.target.value })}
                          />
                        </>
                      )}
                      {/* 竖线右侧是本机出站配置，左侧是对端入口配置。反向目标不在该表中
                          （它们由上方的「反向接入口」面板处理），因此此处无需判断
                          是否可配置——不可配置的目标不会出现。 */}
                      <span className="hopfld-sep" />
                      <span className="hopfld">
                        <span className="hopfld-lbl">出站连接</span>
                        <select
                          className="f"
                          value={pool.t}
                          onChange={e => {
                            const kind = e.target.value as HopPool['t'];
                            setPoolForTarget(to, kind === 'merge' ? { t: 'merge', v: MERGE_DEFAULT } : { t: kind });
                          }}
                        >
                          {POOL_ORDER.map(k => (
                            <option key={k} value={k}>
                              {POOL_LABEL[k]}
                            </option>
                          ))}
                        </select>
                      </span>
                      {pool.t === 'merge' && (
                        <span className="hopfld">
                          <span className="hopfld-lbl">每条连接</span>
                          <input
                            className="f mono"
                            style={{ width: 58 }}
                            value={String(pool.v)}
                            placeholder={String(MERGE_DEFAULT)}
                            onChange={e => setPoolForTarget(to, { t: 'merge', v: Number(e.target.value) || 0 })}
                          />
                          <span className="hopfld-unit">条流</span>
                        </span>
                      )}
                    </span>
                    <span className="sub">
                      {/* 该说明对两种连接方式都适用：走 overlay 的 inbound 同样需要绑定
                          一个端口，同样会与该机器上的其他端口冲突。 */}
                      端口为对端监听的端口，<b>同一台机器上各条链必须错开</b>，冲突时编译会报 node.port-clash。
                      {/* 明文警告只针对直连：走 overlay 时 wg 已对该跳加密，
                          内层不加密是合理的，再加一层会增加无效的 CPU 开销。 */}
                      {dialedDirectly && h.kind === 'none' && (
                        <b style={{ color: 'var(--gold)' }}> 明文直连可能暴露 UUID 和目标地址。</b>
                      )}
                    </span>
                    {/* 分别说明各档的影响：三档各有取舍，其中合并流需要明确说明——
                        它不是性能更高的连接池，在跨境丢包链路上可能劣于每次新建连接。 */}
                    {pool.t === 'pool' && (
                      <span className="sub">
                        连接在流结束后保留约 30 秒供下一条流复用，期间新流不再握手；同一时刻只跑一条流，彼此不影响。
                        超过空闲时间即回收，下一条重新建连。
                      </span>
                    )}
                    {pool.t === 'merge' && (
                      <span className="sub" style={{ color: 'var(--gold)' }}>
                        多条流合并到同一条 TCP 上，握手开销最低，但<b>一条流丢包会阻塞同一连接上的其他流</b>，
                        在跨境丢包链路上可能不如每次新建。取值 {MERGE_MIN}–{MERGE_MAX}；填 1
                        等同于连接池，请直接选那一档。
                      </span>
                    )}
                    {pool.t !== 'none' && (
                      <span className="sub">修改该项会重写 xray.json 并重启 xray，这台机器上的所有连接会断开。</span>
                    )}
                  </span>
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

      {externalEditor && (
        <ExternalOutboundEditor
          tenantId={chainTenant}
          existing={externalEditor.existing}
          onClose={() => setExternalEditor(null)}
          onSaved={outbound => {
            const rule = rules[externalEditor.ruleIndex];
            if (rule) patch(externalEditor.ruleIndex, { ...rule, a: { t: 'proxy', outbound: outbound.id } });
            setExternalEditor(null);
          }}
        />
      )}
      {managedWarp && (
        <Suspense
          fallback={
            <div className="external-outbound-wrap">
              <div className="loading">正在打开 WARP 设置…</div>
            </div>
          }
        >
          <WarpRuleManager
            tunnel={managedWarp}
            nodeId={nodeId}
            nodeName={selfNode?.name || nodeId}
            editable={!readOnly}
            removalBlockedReason={
              warpReferencedOnCurrentNode(managedWarp.id)
                ? '这台机器仍在规则中使用 WARP。请先解除引用、保存草稿并完成发布，再注销身份。'
                : undefined
            }
            onClose={() => setWarpManagerId(null)}
          />
        </Suspense>
      )}
    </fieldset>
  );
}

function externalProtocolLabel(protocol: ExternalOutboundProtocol['t']): string {
  return {
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
      security: externalSecurityLabel(outbound.security),
      credential: 'UUID · 已密封',
    };
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
  if (url.protocol === 'vless:') {
    const transport = parseExternalVlessTransport(url);
    const security = url.searchParams.get('security');
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
    if (securityValue.t === 'none') throw new Error('公网 VLESS 链接必须包含 TLS 或 REALITY');
    return {
      address: url.hostname,
      port: Number(url.port || 443),
      name,
      protocol: {
        t: 'vless',
        v: {
          credential: decodeURIComponent(url.username),
          encryption: url.searchParams.get('encryption') || 'none',
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
  throw new Error('支持 VLESS、SS2022、SOCKS5 和 HTTP(S) 分享链接；WireGuard 请手动填写');
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
  const [name, setName] = useState(existing?.name ?? '新外部出站');
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
    } else if (next === 'vless' && securityKind === 'none') {
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
    (protocolKind !== 'vless' || securityKind !== 'none') &&
    xhttpValid &&
    (securityKind === 'none' || !!serverName.trim()) &&
    primaryRealityValid;

  const save = async () => {
    if (!valid) return;
    setSaving(true);
    setError(null);
    try {
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
        protocolKind === 'vless'
          ? { t: 'vless', v: { credential, encryption: encryption || 'none', flow: flow || null, transport } }
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
            ? { t: 'tls', v: { server_name: serverName, fingerprint } }
            : {
                t: 'reality',
                v: { server_name: serverName, public_key: publicKey, short_id: shortId, fingerprint },
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
    <div className="external-outbound-wrap" role="dialog" aria-modal="true" aria-label="配置外部出站">
      <button className="external-outbound-scrim" aria-label="关闭" onClick={onClose} />
      <section className="external-outbound-drawer">
        <header>
          <b>{existing ? '配置外部出站' : '创建外部出站'}</b>
          <small>租户 · {tenantId}</small>
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
                    placeholder="vless://… / ss://… / socks5://… / https://…"
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
                        / {parsedShare.value.security.t.toUpperCase()}
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
                    {(['vless', 'shadowsocks2022', 'socks5', 'http_connect', 'wireguard'] as const).map(protocol => (
                      <button
                        type="button"
                        className={protocolKind === protocol ? 'on' : ''}
                        aria-pressed={protocolKind === protocol}
                        key={protocol}
                        onClick={() => chooseProtocol(protocol)}
                      >
                        {externalProtocolLabel(protocol)}
                      </button>
                    ))}
                  </span>
                  <small>这里只列外部代理；“从这台落地 / 拒绝”继续由规则动作表达。</small>
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
                <label className="row">
                  <span className="k">服务器</span>
                  <span className="v external-outbound-host">
                    <input
                      className="f mono"
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
                      value={port}
                      onChange={event => setPort(event.target.value)}
                    />
                  </span>
                </label>
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
                      <span className="k">Encryption</span>
                      <span className="v">
                        <input
                          className="f mono"
                          value={encryption}
                          onChange={event => setEncryption(event.target.value)}
                        />
                      </span>
                    </label>
                    <label className="row">
                      <span className="k">传输层</span>
                      <span className="v">
                        <span className="external-transport-options">
                          <button
                            type="button"
                            className={vlessTransport === 'raw' ? 'on' : ''}
                            aria-pressed={vlessTransport === 'raw'}
                            onClick={() => chooseVlessTransport('raw')}
                          >
                            <b>RAW / TCP</b>
                            <small>直连传输</small>
                          </button>
                          <button
                            type="button"
                            className={vlessTransport === 'xhttp' ? 'on' : ''}
                            aria-pressed={vlessTransport === 'xhttp'}
                            onClick={() => chooseVlessTransport('xhttp')}
                          >
                            <b>XHTTP</b>
                            <small>HTTP 分流传输</small>
                          </button>
                        </span>
                      </span>
                    </label>
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
                        <label className="row">
                          <span className="k">模式 / XMUX</span>
                          <span className="v external-xhttp-pair">
                            <select
                              className="f mono"
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
                              placeholder="并发 1–128"
                              value={xhttp.mux}
                              onChange={event => patchXhttp({ mux: event.target.value })}
                            />
                          </span>
                        </label>
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
                            <label className="row">
                              <span className="k">服务器</span>
                              <span className="v external-outbound-host">
                                <input
                                  className="f mono"
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
                                  value={xhttp.downloadPort}
                                  onChange={event => patchXhttp({ downloadPort: event.target.value })}
                                />
                              </span>
                            </label>
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
                            <label className="row">
                              <span className="k">模式 / XMUX</span>
                              <span className="v external-xhttp-pair">
                                <select
                                  className="f mono"
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
                                  placeholder="并发 1–128"
                                  value={xhttp.downloadMux}
                                  onChange={event => patchXhttp({ downloadMux: event.target.value })}
                                />
                              </span>
                            </label>
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
                            <label className="row">
                              <span className="k">SNI / 指纹</span>
                              <span className="v external-xhttp-pair">
                                <input
                                  className="f mono"
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
                            </label>
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
                    <label className="row">
                      <span className="k">MTU / Keepalive</span>
                      <span className="v external-outbound-host">
                        <input
                          className="f mono"
                          type="number"
                          min={576}
                          max={9000}
                          value={wireguardMtu}
                          onChange={event => setWireguardMtu(event.target.value)}
                        />
                        <input
                          className="f mono"
                          type="number"
                          min={0}
                          max={65535}
                          value={keepAlive}
                          onChange={event => setKeepAlive(event.target.value)}
                        />
                      </span>
                    </label>
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
                      <option value="none" disabled={protocolKind === 'vless'}>
                        无（RAW）
                      </option>
                      <option value="tls" disabled={rawOnly}>
                        TLS
                      </option>
                      <option value="reality" disabled={protocolKind !== 'vless'}>
                        REALITY
                      </option>
                    </select>
                    {protocolKind === 'vless' && <span className="sub">公网 VLESS 必须使用 TLS 或 REALITY。</span>}
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
          <button className="btn primary" disabled={!valid || saving} onClick={() => void save()}>
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
