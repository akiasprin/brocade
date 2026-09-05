import { useMemo, useState } from 'react';
import { HOP_WIRE_OPTIONS, type HopWireKind } from '../ui/format';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import {
  createApp,
  createChain,
  createIngress,
  fetchCompileView,
  fetchNodes,
  fetchRevisions,
  fetchSettings,
  fetchSnapshot,
  fetchUsers,
  putStep,
  stageGrant,
  type HopDial,
  type HopInRequest,
  type NodeAgentStateItem,
  type RealityFallbackMode,
  type Rule,
} from '../api';
import { can, useSession } from '../session';
import {
  DIAL_LABEL,
  DIAL_ORDER,
  defaultHopDial,
  dialKindOf,
  dialUnavailable,
  hopDialOf,
  under,
  type DialKind,
  forwardAction,
} from './rules';
import { ErrorBox, Loading } from '../ui/bits';
import { freePortAcross, hopListener, hopListeners, isValidSlug, occupiedPorts, portClash } from './ports';
import { friendlyId } from '../friendly-id';
import { REALITY_FINGERPRINT_OPTIONS, realityFingerprintIsValid, realityServerNameIsValid } from '../reality';

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
// 现在主体是一跳一行：行序即流量方向，末位自动出网。每一跳的属性附在该行内——
// 入口包含监听端口，中继包含连接方式、中转端口和加密档位。该布局的第一个依据是
// 与链详情页保持一致（创建时看到的结构与创建后看到的相同）；第二个依据是
// 明文直连是逐跳的属性，警告需要显示在对应的跳上，修改也在该位置进行。
//
// 链和入口的两个内部 id 自动生成并隐藏；App id 是运营者维护的 slug，新建 App 时仍显示。
//
// # 两个入口，同一套界面
//
// 从线路页和机器页进入的是同一套界面，差异只在哪一项被预填：从线路页进入时线路一项
// 是固定文本，从机器页进入时是下拉框。位置和样式不变——此前线路页的该项位于向导之外
// 且使用另一套排版，同一功能存在两种形式。

// 校验规则来自 ir/validate.rs：链必须有接入面（chain.no-ingress）。链头即接入面所在的
// 机器，顺序由规则表表达——向导自动满足该要求：入口挂在本机，每台写入一条
// `any → 下一台`（末位为出网），顺序显式写入规则。

// 「＋ 新建线路…」在下拉框中的取值。使用不会与 app id 冲突的字符串——app id 的字符集为
// [a-z0-9._-]（ports.ts 的 isValidSlug），不包含空格和冒号。
const NEW_APP = ' :new-app:';

type HopSec = HopWireKind;

/** 某一跳上被手动修改的字段。未修改的一律实时计算（默认值需随数据变化，见下方说明）。 */
type HopEdit = { kind?: DialKind; addr?: string };

// 中转端口按**监听的机器**存储而非按跳存储——模型中 `hop_in` 关联在 `(chain, node)` 上，
// 一台机器在一条链上只有一个端口。常规档位由下游监听，反向两档由下游连接上游、
// 端口开在上游，两种跳可能位于同一台机器上（前一跳常规进入、后一跳反向发出），
// 此时它们本应是同一个端口。按跳存储会导致两份状态写入同一条记录，后写入的覆盖先写入的。
type PortEdit = { port?: string; sec?: HopSec };

// 该跳是否会以明文传输 UUID 和目标地址：连接的是具体地址（非 overlay 且非反向），
// 且中转端口未加密。走 overlay 时 wg 已对该跳加密，内层不加密是合理的。
// 判定与规则编辑器中的对应警告一致。
const plaintextHop = (dial: HopDial, sec: HopSec) => dial.t === 'addr' && sec === 'none';

export function ChainWizard({
  node,
  fixedApp,
  onDone,
}: {
  // 预填的链头。机器页传入（入口即该机器），线路页不传入——由路径的第一行选择。
  // 不传入不表示使用另一套界面：该行本身存在，只是从固定文本变为
  // 用于选择入口机器的下拉框。
  node?: NodeAgentStateItem;
  // 从线路页进入时线路已确定：该项已在进入前指定，此处再次询问
  // （且提供新建选项）会造成对当前位置的误判。
  fixedApp?: { id: string; label: string };
  onDone: () => void;
}) {
  const { who } = useSession();
  const qc = useQueryClient();
  const system = can(who.role, 'system');
  const snapshot = useQuery({ queryKey: ['snapshot'], queryFn: () => fetchSnapshot() });
  const settings = useQuery({ queryKey: ['settings'], queryFn: () => fetchSettings() });
  const nodes = useQuery({ queryKey: ['nodes'], queryFn: () => fetchNodes() });
  const ingressBase = settings.data?.ports?.ingress_base || 8443;
  const hopBase = settings.data?.ports?.hop_base || 20000;
  // 中转端口选择 REALITY 时请求中需要填写的站点。它没有接入面的“本机证书”模式，必须
  // 使用已经明确配置的全局站点；没有站点时该选择会被拦截，而不是静默塞入工厂域名。
  const realitySite = {
    dest: settings.data?.reality_site?.dest ?? '',
    names: settings.data?.reality_site?.server_names ?? [],
  };
  const globalRealityReady = realitySite.dest.trim() !== '' && realitySite.names.length > 0;

  const apps = snapshot.data?.snapshot.apps ?? [];

  // 线路的选择：默认为第一个已有线路，没有任何线路时才使用新建。与 id、端口遵循同一规则——
  // state 存储的是是否手动选择过（null 表示未选择），显示值实时计算。不能使用 useState 的
  // 初始值：快照尚未返回时 `apps` 为空，此时计算的默认值始终是新建，且不会再更新。
  // 创建线路需要 system-admin 而创建链只需 editor，两种权限都不具备时该项无法给出取值：
  // 「＋ 新建线路…」照常列出但禁用，`targetApp` 为空使 `ready` 拦截提交。
  const [appModeRaw, setAppMode] = useState<'new' | 'existing' | null>(null);
  const [appIdRaw, setAppId] = useState<string | null>(null);
  const [appLabelRaw, setAppLabel] = useState<string | null>(null);
  const [pickedAppRaw, setPickedApp] = useState<string | null>(null);
  const appMode: 'new' | 'existing' = fixedApp ? 'existing' : (appModeRaw ?? (apps.length > 0 ? 'existing' : 'new'));
  const pickedApp = fixedApp?.id ?? pickedAppRaw ?? apps[0]?.id ?? '';
  const [chainNameRaw, setChainName] = useState<string | null>(null);
  // 该数组有序：第 0 台是接入面所在的机器（即链头），其后每台是下一跳。
  // 规则由该顺序推导得出，不需要理解规则表的结构。
  // 链头尚未选择时为空数组——此时路径只有选择入口机器的那一行。
  const [spine, setSpine] = useState<string[]>(node ? [node.node_id] : []);
  const [bind, setBind] = useState('0.0.0.0');
  const [hopEdits, setHopEdits] = useState<Record<string, HopEdit>>({});
  /* 键是监听的机器而非跳。见 PortEdit。 */
  const [portEdits, setPortEdits] = useState<Record<string, PortEdit>>({});
  const [showOps, setShowOps] = useState(false);
  const [realityTarget, setRealityTarget] = useState<RealityFallbackMode | ''>('');
  const [customRealityDest, setCustomRealityDest] = useState('');
  const [customRealityNames, setCustomRealityNames] = useState('');
  const [customRealityFingerprint, setCustomRealityFingerprint] = useState('chrome');
  const [error, setError] = useState<unknown>(null);

  const nameMap = new Map((nodes.data?.nodes ?? []).map(n => [n.node_id, n.name]));
  const nameOf = (id: string) => nameMap.get(id) || id;
  // 链头即接入面所在的机器，也是主干的第 0 位。整条链的租户、默认名称、默认端口都取自它，
  // 因此在未选择之前本页无法给出任何默认值——`ready` 会拦截提交。
  const head = (nodes.data?.nodes ?? []).find(n => n.node_id === spine[0]) ?? node ?? null;
  const headLabel = head ? head.name || head.node_id : '';
  const headCertificate =
    snapshot.data?.snapshot.nodes?.find(candidate => candidate.id === head?.node_id)?.certificate_name ?? null;
  const customRealityServerNames = customRealityNames
    .split(/[\s,]+/)
    .map(value => value.trim())
    .filter(Boolean);
  const realityTargetReady =
    (realityTarget === 'node-certificate' && !!headCertificate) ||
    (realityTarget === 'global-site' && globalRealityReady) ||
    (realityTarget === 'custom-site' &&
      /^\S+:[1-9]\d*$/.test(customRealityDest.trim()) &&
      Number(customRealityDest.trim().split(':').at(-1)) <= 65535 &&
      customRealityServerNames.length > 0 &&
      customRealityServerNames.every(realityServerNameIsValid) &&
      realityFingerprintIsValid(customRealityFingerprint));
  // App 是运营侧长期稳定的 slug，不参与随机 ID 迁移。新建时仍从入口机器给出一个可编辑
  // 的 slug 默认值；链和接入面的内部 ID 才使用短友好随机值。
  const slug = (head?.node_id ?? '')
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-|-$/g, '');
  const appId = appIdRaw ?? (slug ? `app-${slug}` : '');
  const appLabel = appLabelRaw ?? headLabel;
  const chainName = chainNameRaw ?? (headLabel ? `${headLabel} 直出` : '');
  /* 选择连接方式需要读取对端的公网地址，因此此处需要完整的节点数据而非只有名称。 */
  const nodeOf = (id: string) => (nodes.data?.nodes ?? []).find(n => n.node_id === id) ?? null;
  const addable = (nodes.data?.nodes ?? [])
    .filter(n => !n.retired_at && !spine.includes(n.node_id))
    .map(n => n.node_id);

  // 系统层的端口（WireGuard）只存在于编译产生的 IR 中。查询键与顶栏角标、检视窗相同，
  // 因此通常命中缓存。
  const revisions = useQuery({ queryKey: ['revisions'], queryFn: () => fetchRevisions() });
  const current = revisions.data?.current_revision;
  const compile = useQuery({
    queryKey: ['compile', current],
    queryFn: () => fetchCompileView(current!),
    enabled: !!current,
  });

  // 技术 ID 在向导的整个生命周期中保持不变：后续的链、接入面、授权与规则操作都引用
  // 同一组值。端口则随已占用端口计算，只有用户明确修改后才固定。
  const [chainId] = useState(() => friendlyId('chain'));
  const [ingressId] = useState(() => friendlyId('ingress', new Set([chainId])));
  const [portRaw, setPort] = useState<number | null>(null);

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

  // 起始值取自全局设置（settings.ports.ingress_base）：443 端口的用途由运营者决定，
  // 使用硬编码会导致每次建链时都填入该值。
  const port = portRaw ?? freePortAcross(taken, [head?.node_id ?? ''], ingressBase);

  // 该跳的默认连接方式，判定与规则编辑器共用同一实现（`defaultHopDial`）：对端有非 NAT
  // 公网地址时直连，否则回退到 overlay。**默认**不选择反向（`self: null`）：反向是一项
  // 拓扑决策，而非连接失败时的回退——下游位于 NAT 之后而上游有公网地址时，
  // 走 overlay 同样可用，自动改为反向相当于代为做出未经确认的决策。可手动选择（见下方
  // 的下拉框），选择后端口移到上游一侧。
  const autoKind = (id: string): DialKind =>
    dialKindOf(defaultHopDial({ peer: nodeOf(id), self: null, port: hopBase }), nodeOf(id));

  const hopKindOf = (id: string): DialKind => hopEdits[id]?.kind ?? autoKind(id);
  const isReverse = (kind: DialKind) => kind === 'reverse_v4' || kind === 'reverse_v6';

  // 第 i 跳（进入 spine[i] 的那一跳）的端口位于哪台机器。常规档位由下游监听；反向档位是
  // 下游连接上游，由上游监听——编译器的取值方式相同（ir/hops.rs 的
  // `entry_hop_in`：转发取对端的，反向取本机的）。
  const listenerOfHop = (i: number) => hopListener(spine, i, isReverse(hopKindOf(spine[i])));
  /* 该链上需要开启中转端口的机器。去重的原因见 `hopListeners`。 */
  const listeners = hopListeners(spine, i => isReverse(hopKindOf(spine[i])));

  // 为每台监听的机器选择一个未占用的端口。选择时不需要考虑该链上的其他机器——不同机器上的
  // 端口互不影响；同一台机器不会重复选择，由上面的去重保证。
  // 不使用 useMemo：主干只有少量机器，`freePortAcross` 的开销为一次 Map 查找起步的循环，
  // 而添加依赖数组需要传入每帧新建的数组或将其序列化为字符串再解析——两种方式都是为了
  // 适配记忆化而增加复杂度，React Compiler 会自行处理。
  const autoHostPorts = new Map<string, number>();
  for (const host of listeners) autoHostPorts.set(host, freePortAcross(taken, [host], hopBase));

  const hostPortOf = (host: string) => portEdits[host]?.port ?? String(autoHostPorts.get(host) ?? hopBase);
  const hostSecOf = (host: string): HopSec => portEdits[host]?.sec ?? 'none';
  /* 自定义档的地址需手动填写，其余各档可推导得出。 */
  const hopDialFor = (id: string): HopDial => {
    const kind = hopKindOf(id);
    /* 连接的是对端监听的端口。反向档不携带地址和端口（由编译器推导），传入值不影响结果。 */
    const p = Number(hostPortOf(id)) || hopBase;
    if (kind === 'custom') {
      const host = (hopEdits[id]?.addr ?? '').trim();
      return { t: 'addr', v: host ? `${host}:${p}` : `:${p}` };
    }
    return hopDialOf(kind, nodeOf(id), p);
  };
  const patchHop = (id: string, next: HopEdit) => setHopEdits(prev => ({ ...prev, [id]: { ...prev[id], ...next } }));
  const patchPort = (host: string, next: PortEdit) =>
    setPortEdits(prev => ({ ...prev, [host]: { ...prev[host], ...next } }));

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
  const portTaken = portClash(taken, [head?.node_id ?? ''], port);
  /* 各监听机器分别校验：端口冲突会导致 xray 启动失败，编译时报 node.port-clash。 */
  const hopPortIssues = listeners.flatMap(host => {
    const raw = hostPortOf(host);
    const p = Number(raw);
    if (!/^\d+$/.test(raw) || p <= 0 || p > 65535) {
      return [{ host, msg: '端口必须是 1-65535' }];
    }
    const clash = portClash(taken, [host], p);
    if (clash) return [{ host, msg: clash }];
    // 链头作为反向上游时，其上同时开启接入端口和该反向端口。两者都是本次新建的，
    // `taken` 中尚不包含，上面的校验无法覆盖——只能在此额外校验一次。
    if (host === head?.node_id && p === port) {
      return [{ host, msg: `与这条链的接入口 ${port} 冲突（同在 ${nameOf(host)} 上）` }];
    }
    return [];
  });
  const hopIssueOf = (host: string) => hopPortIssues.find(x => x.host === host)?.msg ?? null;

  // ── 该链的授权对象 ──
  // 链创建后仍不可用：接入面已开启但没有任何 grant，无法建立连接。此时需要离开向导、
  // 切换到用户页逐个授权——而该过程中没有新的决策，创建链时已确定授权对象。
  // 选中的用户各生成一条 `upsert_grant`，与链和接入面进入同一批草稿。
  const users = useQuery({ queryKey: ['users'], queryFn: () => fetchUsers(true) });
  /* 键使用 `租户/用户`：用户 id 只在租户内唯一（user.dup 只在单个租户内查重）。 */
  const [pickedUsers, setPickedUsers] = useState<Set<string>>(new Set());

  // 接入面的租户随链头机器确定，可授权对象由它决定（validate.rs 的 tenant.scope：
  // `under(grant.tenant, ingress.tenant)`）。链头未选择时该项没有取值——
  // 下方的说明会予以提示，而非留空。
  const ingressTenant = head?.tenant_id ?? '';
  const userRows = (users.data?.users ?? []).map(u => ({
    ...u,
    key: `${u.tenant_id}/${u.id}`,
    // 不可选的保留在列表中并说明原因，判定和处理方式与规则编辑器的下拉框一致：
    // 直接隐藏会导致该用户从列表中消失，需要到其他位置查找。
    blocked: under(u.tenant_id, ingressTenant) ? null : `归 ${u.tenant_id}，这条链（${ingressTenant}）看不见它`,
  }));

  // 实际会写入的授权。更换入口后重新计算，不清空 `pickedUsers`：更换机器可能使某个用户
  // 超出租户范围，此时不应写入；但若删除其勾选状态，切换回原机器时该选择会丢失——
  // 而反复切换是建链时的常见操作。因此保留失效的键，合法性每次实时计算。
  const grantedUsers = userRows.filter(u => pickedUsers.has(u.key) && !u.blocked);
  /* 有用户因当前入口而不可授权时给出提示。不提示时页脚的操作条数会少于预期且无法解释。 */
  const droppedUsers = userRows.filter(u => pickedUsers.has(u.key) && u.blocked);
  const toggleUser = (key: string) =>
    setPickedUsers(prev => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  const selectableUsers = userRows.filter(u => !u.blocked);

  // 将写入草稿的操作列表。该列表既用于页脚展示，也是提交时实际执行的内容——
  // 分两处实现会导致预览显示三条而实际写入四条，且该偏差没有任何提示。
  const ops = useMemo(() => {
    const list: { op: string; arg: string }[] = [];
    if (appMode === 'new') list.push({ op: 'upsert_app', arg: `${targetApp}「${appLabel.trim() || targetApp}」` });
    list.push({
      op: 'upsert_chain',
      arg: `${chainId.trim()}「${chainName.trim() || chainId.trim()}」· 租户 ${head?.tenant_id ?? '—'}`,
    });
    list.push({
      op: 'upsert_ingress',
      arg: `${ingressId.trim()} → ${headLabel} ${bind.trim()}:${port} · REALITY（${
        realityTarget === 'node-certificate'
          ? '本机证书'
          : realityTarget === 'global-site'
            ? '全局站点'
            : realityTarget === 'custom-site'
              ? '自定义站点'
              : '未选择目标'
      }）`,
    });
    if (spine.length > 1) {
      spine.forEach((id, i) => {
        if (i < spine.length - 1) {
          const to = spine[i + 1];
          const dial = hopDialFor(to);
          const how = dial.t === 'overlay' ? 'WireGuard' : dial.t === 'reverse' ? `反向 ${dial.v}` : dial.v;
          list.push({ op: 'put_step', arg: `${nameOf(id)}：任意 → 转发 ${nameOf(to)}（${how}）` });
        } else {
          list.push({ op: 'put_step', arg: `${nameOf(id)}：任意 → 落地` });
        }
      });
    }
    /* 授权排在最后：grant 引用接入面，接入面需要先创建。草稿按顺序回放。 */
    for (const u of grantedUsers) {
      list.push({
        op: 'upsert_grant',
        arg: `${u.tenant_id}/${u.id} → ${ingressId.trim()}`,
      });
    }
    return list;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    appMode,
    targetApp,
    appLabel,
    chainId,
    chainName,
    ingressId,
    bind,
    port,
    spine,
    hopEdits,
    portEdits,
    autoHostPorts,
    nodes.data,
    pickedUsers,
    users.data,
    ingressTenant,
    realityTarget,
  ]);

  const submit = async () => {
    setError(null);
    try {
      const app = targetApp;
      if (appMode === 'new') await createApp({ id: app, label: appLabel.trim() || app });
      await createChain(app, {
        id: chainId.trim(),
        tenant_id: head?.tenant_id ?? '',
        name: chainName.trim() || chainId.trim(),
        subscription_country: null,
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
              }
            : { fallback_mode: realityTarget as Exclude<RealityFallbackMode, 'custom-site'> },
      });

      // 线性中继：每台转发给下一台，最后一台出网。
      // 缺少这些规则时，入口机器会就地出网（规则表为空时编译器补全 Egress），
      // 中继不会被使用。分流等复杂选路在规则表中配置。
      if (spine.length > 1) {
        for (let i = 0; i < spine.length; i += 1) {
          const id = spine[i];
          const rules: Rule[] =
            i < spine.length - 1
              ? [{ m: { t: 'any' }, a: forwardAction(spine[i + 1], hopDialFor(spine[i + 1])) }]
              : [{ m: { t: 'any' }, a: { t: 'egress', send_through: null } }];
          // 除入口外，每一跳都是其他节点的转发目标，必须具备接受凭据，否则报 relay.no-accept。
          // 反向档同样需要：编译器取用的 credential 始终来自 `to` 的 accept（ir/hops.rs
          // 的 `credential`），只是含义相反——转发时它是连接对端使用的凭据，反向时它是
          // 下游连接时提供的身份标识，上游据此识别该连接并交给 portal。
          //
          // 中转端口只写给实际监听的机器：常规档位是下游，反向档位是上游。为不监听的机器
          // 也写入会占用其一个无用端口，并进入端口冲突校验。
          // 选择 REALITY 时必须携带站点：服务端要求 dest 且 server_names 非空
          // （console.rs 的 resolve_hop_security），留空会返回 400——与接入面不同，
          // 接入面留空表示使用全局设置中的站点，因此此处显式填入全局站点。
          const sec = hostSecOf(id);
          const hopIn: HopInRequest | undefined = listeners.includes(id)
            ? {
                port: Number(hostPortOf(id)) || hopBase,
                security:
                  sec === 'reality'
                    ? { t: 'reality', v: { dest: realitySite.dest, server_names: realitySite.names } }
                    : sec === 'encryption'
                      ? { t: 'encryption' }
                      : sec === 'shadowsocks2022'
                        ? { t: 'shadowsocks2022' }
                        : { t: 'none' },
              }
            : undefined;
          await putStep(app, chainId.trim(), id, {
            rules,
            ...(i > 0 ? { accept: {} } : {}),
            // 链头作为反向上游时同样需要开启端口。编译器为该档位放宽了链头不配置中转端口
            // 的限制（ir/routing.rs），accept 仍会被清除——链头不应持有供其他节点连接的凭据。
            ...(hopIn ? { hop_in: hopIn } : {}),
          });
        }
      }

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
      onDone();
    } catch (e) {
      setError(e);
    }
  };

  // 向导的默认端口、可选机器、线路冲突、授权对象和 REALITY 站点分别来自这些查询。
  // 任何一项失败都不能以空数组/工厂默认值继续，否则“创建成功”的结果可能少授权、撞端口
  // 或写入错误的站点。
  if (
    snapshot.isPending ||
    settings.isPending ||
    nodes.isPending ||
    revisions.isPending ||
    users.isPending ||
    (current != null && compile.isPending)
  )
    return <Loading />;
  const dependencyError =
    snapshot.error ?? settings.error ?? nodes.error ?? revisions.error ?? users.error ?? compile.error;
  if (dependencyError) return <ErrorBox error={dependencyError} />;

  const ready =
    !!head &&
    spine.length > 0 &&
    targetApp.length > 0 &&
    chainId.trim().length > 0 &&
    ingressId.trim().length > 0 &&
    port > 0 &&
    port < 65536 &&
    !chainClash &&
    !ingressClash &&
    !portTaken &&
    hopPortIssues.length === 0 &&
    realityTargetReady &&
    (!listeners.some(host => hostSecOf(host) === 'reality') || globalRealityReady);

  return (
    <form
      className="wz"
      onSubmit={e => {
        e.preventDefault();
        void submit();
      }}
    >
      {/* ── 标识：线路和链名。两个入口的差异集中在该项 ── */}
      <div className="wz-fields">
        <div className="wz-fld">
          <label>线路</label>
          {/* 下拉框与新建的两个输入框在同一行：它们对应同一项输入——选择哪个线路，
              取值要么是已有线路，要么是新建线路的 id 和名称。
              分为两行会被理解为两个问题，且第二行需要依靠缩进和竖线表明其从属关系。

              选择新建时下拉框收窄：此时它只显示「＋ 新建线路…」，
              占用半行宽度没有必要——宽度分配给需要填写的两个输入框。 */}
          <div className="wz-app">
            {/* 始终使用下拉框，即使只有一个选项。从线路页进入时它只包含该线路——
                改为只读文本时，同一字段在两个入口下是两种控件，需要先判断当前是否可修改。
                只有一个选项的下拉框本身即表明取值唯一。 */}
            <select
              className={`f${appMode === 'new' && !fixedApp ? ' narrow' : ''}`}
              value={appMode === 'new' ? NEW_APP : pickedApp}
              // 只有一项时不禁用：禁用的下拉框与异常状态使用同一视觉信号，
              // 而此处的实际情况是没有其他选项——该情况由选项数量本身表达。
              onChange={e => {
                if (e.target.value === NEW_APP) setAppMode('new');
                else {
                  setAppMode('existing');
                  setPickedApp(e.target.value);
                }
              }}
            >
              {fixedApp ? (
                <option value={fixedApp.id}>
                  {fixedApp.label || fixedApp.id}（{fixedApp.id}）
                </option>
              ) : (
                <>
                  {/* 不提供空选项。该字段始终有取值：第一个已有线路，没有任何线路时为新建。
                      空选项会增加一次点击以选择本应默认选中的项，且选中空选项时
                      `ready` 仍为禁用状态，会被理解为填写有误。 */}
                  {apps.map(a => (
                    <option key={a.id} value={a.id}>
                      {a.label || a.id}（{a.id}）
                    </option>
                  ))}
                  {/* 新建作为下拉框的最后一项，不再使用独立的单选组：它与其他选项是
                      同一问题的不同取值，使用两种控件相当于重复询问。
                      建线路要 system-admin，但该项照常列出、只是禁用——按角色隐藏时，
                      没有任何线路的只读视角会看到一个空下拉框。 */}
                  <option value={NEW_APP} disabled={!system}>
                    ＋ 新建线路…
                  </option>
                </>
              )}
            </select>
            {appMode === 'new' && !fixedApp && (
              <>
                <input
                  className="f mono id"
                  value={appId}
                  onChange={e => setAppId(e.target.value)}
                  placeholder="线路 ID"
                />
                <input
                  className="f"
                  value={appLabel}
                  onChange={e => setAppLabel(e.target.value)}
                  placeholder="线路名称"
                />
              </>
            )}
          </div>
          {/* 说明该字段的含义——「线路」一词本身不体现它是计费单元。 */}
          <p className="note">计费单元。填写你提供的服务内容。</p>
        </div>
        <div className="wz-fld">
          <label>链名称</label>
          <input
            className="f"
            value={chainName}
            disabled={!head}
            placeholder="选完入口自动填"
            onChange={e => setChainName(e.target.value)}
          />
          <p className="note">列表和面包屑上显示的名字，随时能改</p>
        </div>
      </div>

      {/* ── 路径：一跳一行 ── */}
      <h4 className="sec">
        路径
        <span className="rule" />
      </h4>
      <div className="wz-hops">
        {spine.length === 0 && (
          <div className="wz-hop add">
            <span className="idx">01</span>
            <span className="who">
              <select
                className="f"
                value=""
                onChange={e => {
                  if (e.target.value) setSpine([e.target.value]);
                }}
              >
                <option value="">— 选一台当入口 —</option>
                {addable.map(n => (
                  <option key={n} value={n}>
                    {nameOf(n)}（{n}）
                  </option>
                ))}
              </select>
            </span>
            <span className="ctl">
              <span className="note">接入面开在这台机器上，用户从这里接入</span>
            </span>
          </div>
        )}
        {spine.map((id, i) => {
          const entry = i === 0;
          const last = i === spine.length - 1;
          const kind = hopKindOf(id);
          const dial = hopDialFor(id);
          const peer = nodeOf(id);
          /* 该跳的端口位于哪台机器：常规档位是本台（下游），反向档位是上一台。 */
          const host = entry ? id : listenerOfHop(i);
          const rev = !entry && isReverse(kind);
          const issue = entry ? null : hopIssueOf(host);
          // 明文判定对反向档同样适用：该隧道使用上游端口的加密配置
          // （ir/hops.rs 的 `dial_security` 取 `entry_hop_in.security`），未加密即为明文。
          // 只有 overlay 档例外——wg 已对该跳加密。
          const plain = !entry && (plaintextHop(dial, hostSecOf(host)) || (rev && hostSecOf(host) === 'none'));
          return (
            <div className="wz-hop" key={`${id}/${i}`}>
              <span className="idx">{String(i + 1).padStart(2, '0')}</span>
              <span className="who">
                <b title={id}>{nameOf(id)}</b>
                {entry && <span className="st b-role">入口</span>}
                {last && !entry && <span className="st st-succeeded">落地</span>}
                {!entry && !last && <span className="st">中转</span>}
                <span className="mono dim">{id}</span>
              </span>
              <span className="ctl">
                {entry ? (
                  <>
                    <span className="note">用户从这里接入</span>
                    {/* 链头同样可更换：删除后回到选择入口机器的那一行。从机器页进入时不提供该操作——
                        该机器是进入本页的前提，在此更换不符合当前上下文。 */}
                    {!node && (
                      <button
                        className="del-ctl"
                        title="换一台当入口"
                        aria-label="换一台当入口"
                        onClick={e => {
                          e.preventDefault();
                          setSpine([]);
                        }}
                      >
                        ×
                      </button>
                    )}
                  </>
                ) : (
                  <button
                    className="del-ctl"
                    title="从这条链上去掉这一跳"
                    aria-label="从这条链上去掉这一跳"
                    onClick={e => {
                      e.preventDefault();
                      setSpine(spine.filter(x => x !== id));
                    }}
                  >
                    ×
                  </button>
                )}
              </span>

              <span className="attrs">
                {entry ? (
                  <>
                    <span className="attr">
                      <span className="k">监听</span>
                      <input
                        className="f mono"
                        style={{ width: 116 }}
                        value={bind}
                        onChange={e => setBind(e.target.value)}
                      />
                      <input
                        className="f mono"
                        style={{ width: 78 }}
                        value={port}
                        inputMode="numeric"
                        onChange={e => setPort(Number(e.target.value))}
                      />
                    </span>
                    <span className="attr">
                      <span className="k">伪装</span>
                      <select
                        className="f"
                        aria-label="REALITY 目标来源"
                        value={realityTarget}
                        onChange={event => setRealityTarget(event.target.value as RealityFallbackMode | '')}
                      >
                        <option value="">— 选择 REALITY 目标 —</option>
                        <option value="node-certificate" disabled={!headCertificate}>
                          本机证书{headCertificate ? ` · ${headCertificate}` : '（尚未签发）'}
                        </option>
                        <option value="global-site" disabled={!globalRealityReady}>
                          全局站点{globalRealityReady ? ` · ${realitySite.dest}` : '（尚未配置）'}
                        </option>
                        <option value="custom-site">自定义站点…</option>
                      </select>
                    </span>
                    {realityTarget === 'custom-site' && (
                      <span className="attr wz-reality-custom">
                        <span className="k">目标 / SNI</span>
                        <input
                          className="f mono"
                          value={customRealityDest}
                          placeholder="example.com:443"
                          onChange={event => setCustomRealityDest(event.target.value)}
                        />
                        <input
                          className="f mono"
                          value={customRealityNames}
                          placeholder="example.com"
                          onChange={event => setCustomRealityNames(event.target.value)}
                        />
                        <select
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
                      </span>
                    )}
                    {realityTarget !== '' && !realityTargetReady && (
                      <span className="note warn">该目标尚不完整，补齐后才能创建。</span>
                    )}
                  </>
                ) : (
                  <>
                    <span className="attr">
                      <span className="k">怎么到它</span>
                      <select
                        className="f"
                        value={kind}
                        onChange={e => patchHop(id, { kind: e.target.value as DialKind })}
                      >
                        {DIAL_ORDER.map(k => (
                          // 反向两档判定的是**上游**（即本行的上一台）是否有公网地址：
                          // 该档由下游连接上游，可达性取决于上游。将 peer 作为 self 传入
                          // 会使判定方向相反，导致下游有公网而上游没有时将不可用的档
                          // 显示为可用。
                          <option key={k} value={k} disabled={dialUnavailable(k, peer, nodeOf(spine[i - 1]))}>
                            {DIAL_LABEL[k]}
                          </option>
                        ))}
                      </select>
                      {kind === 'custom' ? (
                        <input
                          className="f mono"
                          style={{ width: 150 }}
                          placeholder="10.0.0.9 / 2001:db8::9"
                          value={hopEdits[id]?.addr ?? ''}
                          onChange={e => patchHop(id, { addr: e.target.value })}
                        />
                      ) : (
                        <span className="mono dim">
                          {dial.t === 'addr' ? dial.v : dial.t === 'overlay' ? 'overlay 地址' : '对端连过来'}
                        </span>
                      )}
                    </span>
                    {/* 端口和加密都是**监听机器**的属性，不属于该跳：反向档下它们位于上游，
                        而上游可能同时被前一跳以常规方式连接——两者本就是同一个端口，
                        两行绑定同一份状态，修改任一行结果相同。因此需要标明端口所在的机器，
                        否则会被理解为正在配置当前这一台。 */}
                    <span className="attr">
                      <span className="k">{rev ? '反向接入口' : '中转口'}</span>
                      <input
                        className="f mono"
                        style={{ width: 78 }}
                        value={hostPortOf(host)}
                        inputMode="numeric"
                        onChange={e => patchPort(host, { port: e.target.value })}
                      />
                      {rev && <span className="st">开在 {nameOf(host)} 上</span>}
                    </span>
                    <span className="attr">
                      <span className="k">协议</span>
                      <select
                        className="f"
                        value={hostSecOf(host)}
                        onChange={e => patchPort(host, { sec: e.target.value as HopSec })}
                      >
                        {/* `rev` 表示该跳是反向接入，该端口承载的是反向隧道。
                            隧道基于 VLESS 账号建立，shadowsocks 没有对应的账号机制。 */}
                        {HOP_WIRE_OPTIONS.map(option => (
                          <option
                            key={option.kind}
                            value={option.kind}
                            disabled={(rev && !option.reverseOk) || (option.kind === 'reality' && !globalRealityReady)}
                          >
                            {option.label}
                            {rev && !option.reverseOk ? ' — 反向隧道只有 VLESS 承载' : ''}
                            {option.kind === 'reality' && !globalRealityReady ? ' — 先配置全局站点' : ''}
                          </option>
                        ))}
                      </select>
                    </span>
                  </>
                )}
              </span>

              {issue && (
                <span className="attrs">
                  <p className="note warn">{issue}。端口冲突会导致 xray 无法启动，编译会报 node.port-clash。</p>
                </span>
              )}
              {!issue && entry && portTaken && (
                <span className="attrs">
                  <p className="note warn">{portTaken}。端口冲突会导致 xray 无法启动，编译会报 node.port-clash。</p>
                </span>
              )}
              {/* 警告显示在对应的跳上，处理方式写在同一句中——修改位置即左侧的两个字段，
                  无需到其他位置操作。（此前另有一块汇总提示和一键修改，已移除：
                  同一内容在一屏内重复表达。） */}
              {plain && (
                <span className="attrs">
                  <p className="note warn">
                    明文直连：会暴露 UUID 和目标地址。改用加密档，或将连接方式改为经 WireGuard。
                  </p>
                </span>
              )}
            </div>
          );
        })}

        {spine.length > 0 && (
          <div className="wz-hop add">
            <span className="idx">＋</span>
            <span className="who">
              <select
                className="f"
                value=""
                disabled={addable.length === 0}
                onChange={e => {
                  if (e.target.value) setSpine([...spine, e.target.value]);
                }}
              >
                <option value="">{addable.length ? '＋ 在末尾加一跳…' : '没有别的机器可加'}</option>
                {addable.map(n => (
                  <option key={n} value={n}>
                    {nameOf(n)}（{n}）
                  </option>
                ))}
              </select>
            </span>
            <span className="ctl">
              <span className="note">{spine.length === 1 ? '现在是直出：入口自己落地' : '末位自动落地'}</span>
            </span>
          </div>
        )}
      </div>
      {/* 说明该向导的适用范围：它只能创建线性路径。分流在规则表中配置——在此提供入口
          相当于把整张规则表并入建链步骤，而此时链路是否连通尚未确定。 */}
      {spine.length > 0 && (
        <p className="note" style={{ marginTop: 8 }}>
          分流规则需在本次简易建链向导完成后再编辑设置。
        </p>
      )}

      {/* ── 授权对象：选中的用户各生成一条 upsert_grant ── */}
      <h4 className="sec">
        谁能用
        <span className="rule" />
      </h4>
      {!head ? (
        // 链头未选择时无法计算：接入面的租户随其确定，而租户决定可授权的用户范围。
        // 此时列出全部用户供选择，会导致所选用户在后续被过滤掉且无提示。
        <p className="note">先在上面选一台当入口——接入面归它的租户，那决定了哪些用户能连。</p>
      ) : users.isPending ? (
        <Loading />
      ) : userRows.length === 0 ? (
        <p className="note">还没有用户。建完链去「用户」面开户，再回来授权——这一段不会消失。</p>
      ) : (
        <>
          <div className="wz-grantbar">
            <span className="n">
              <b>{grantedUsers.length}</b> / {selectableUsers.length} 人
            </span>
            {/* 提供全选：单人运营的常见配置是所有用户可使用所有线路，一个按钮即可完成。
                全不选与之配套，用于撤销误操作。 */}
            <button
              type="button"
              className="btn sm"
              disabled={selectableUsers.length === 0 || grantedUsers.length === selectableUsers.length}
              onClick={() => setPickedUsers(new Set(selectableUsers.map(u => u.key)))}
            >
              全选
            </button>
            <button
              type="button"
              className="btn sm"
              disabled={grantedUsers.length === 0}
              onClick={() => setPickedUsers(new Set())}
            >
              全不选
            </button>
          </div>
          <div className="wz-grants">
            {userRows.map(u => (
              <button
                key={u.key}
                type="button"
                className="wz-grant"
                aria-pressed={!u.blocked && pickedUsers.has(u.key)}
                aria-disabled={!!u.blocked}
                disabled={!!u.blocked}
                title={u.blocked ?? `授权 ${u.id} 连这条链`}
                onClick={() => toggleUser(u.key)}
              >
                <span className="tick" aria-hidden="true">
                  ✓
                </span>
                {u.id}
                <span className="t">{u.tenant_id}</span>
              </button>
            ))}
          </div>
          {droppedUsers.length > 0 && (
            // 更换入口使某些用户超出租户范围。需要提示：不提示时页脚的操作条数会
            // 少于预期且无法解释，而这些用户仍显示为已勾选。勾选状态保留——
            // 切换回原机器时它们会重新生效。
            <p className="note warn">
              换了入口之后 {droppedUsers.map(u => u.id).join('、')} 不在 <span className="mono">{ingressTenant}</span>{' '}
              之下，本次不会为其授权。
            </p>
          )}
        </>
      )}

      {(chainClash || ingressClash) && <div className="callout red">{chainClash || ingressClash}</div>}

      {error != null && <ErrorBox error={error} />}

      {/* ── 页脚：将写入草稿的操作，展开后列出 ── */}
      <div className="wz-foot">
        {head ? (
          <>
            <button type="button" className="wz-count" aria-expanded={showOps} onClick={() => setShowOps(v => !v)}>
              {showOps ? '▾' : '▸'} 会往草稿里加 {ops.length} 条操作
            </button>
            <span className="note">顶栏按「提交」才写进库。</span>
          </>
        ) : (
          // 链头未选择时无法计算这些操作（接入面所在的机器尚未确定），
          // 此时给出数量会与实际不符。
          <span className="note">先在上面选一台当入口。</span>
        )}
        <span className="sp" />
        <button type="button" className="btn" onClick={onDone}>
          取消
        </button>
        <button className="btn primary" disabled={!ready} type="submit">
          加进草稿
        </button>
      </div>
      {showOps && (
        <ul className="wz-ops">
          {ops.map((o, i) => (
            <li key={i}>
              <span className="op">{o.op}</span>
              <span className="arg">{o.arg}</span>
            </li>
          ))}
        </ul>
      )}
    </form>
  );
}
