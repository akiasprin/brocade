// admin 面的数据获取入口。浏览器默认使用 HttpOnly session cookie；Bearer token 仅作为
// API 和自动化调用的可选方式。类型定义与服务端的序列化结果逐字段对应。

import { draft, type ModelOp } from './draft';

export type AdminRole = 'readonly' | 'editor' | 'publisher' | 'tenant-admin' | 'system-admin';

/* GET /whoami = brocade_store::AuthenticatedAdmin */
export interface Whoami {
  operator_id: string;
  role: AdminRole;
  tenant_scope: string | null;
  token_prefix: string | null;
  /**
   * 该角色看到的 IP、域名和端口均为 `123.123.***.***` 形式的掩码值，也无法获取产物。
   * 掩码在服务端出口处生成，与界面无关；该字段只用于决定是否渲染那些点击后会被拒绝的入口。
   */
  masked_assets: boolean;
}

export class ApiError extends Error {
  constructor(
    public status: number,
    message: string,
  ) {
    super(message);
  }
}

export async function api<T>(path: string, token = '', init?: RequestInit): Promise<T> {
  const headers: Record<string, string> = {
    ...(init?.body ? { 'content-type': 'application/json' } : {}),
    ...((init?.headers as Record<string, string> | undefined) ?? {}),
  };
  const bearer = token.trim();
  if (bearer) headers.authorization = `Bearer ${bearer}`;

  const res = await fetch(path, {
    ...init,
    credentials: 'same-origin',
    headers,
  });
  if (!res.ok) {
    /* 错误响应体格式为 {"error": "..."}（http.rs 的 ErrorBody），解析失败时回退到状态行 */
    let message = `${res.status} ${res.statusText}`;
    try {
      const body = (await res.json()) as { error?: string };
      if (body.error) message = body.error;
    } catch {
      /* 非 JSON 响应，保留状态行 */
    }
    throw new ApiError(res.status, message);
  }
  if (res.status === 204) return undefined as T;
  return res.json() as Promise<T>;
}

const req = <T>(path: string, init: RequestInit) => api<T>(path, '', init);

// ── 草稿 ──
// 编辑在前端累积，提交时才写库。读取也遵循同一规则——存在草稿时读取的是草稿全部生效后的
// 结果，由服务端在一个最终回滚的事务中计算（见 draft.ts 开头）。
export interface DraftPreview {
  snapshot: ConsoleSnapshot;
  compile: CompileView;
  // 草稿生效后的产物索引。产物是快照的纯函数，因此草稿同样有产物——本次改动会修改
  // 哪些配置、修改为什么内容，是提交前需要确认的信息。
  artifacts: { revision: number; artifacts: ArtifactIndexEntry[] };
}

export const previewDraft = (ops: ModelOp[]) => post<DraftPreview>('/model/preview', { ops });

// 一次渲染中读取快照的页面不止一个，编译摘要还需再读一次——同一份草稿不应重复预览三次。
// 缓存键包含草稿版本号，草稿变化时缓存自动失效；另加一个较短的 TTL，
// 使服务端被其他人修改后不会长期读取到旧数据。
let previewCache: { key: string; at: number; p: Promise<DraftPreview> } | null = null;
const PREVIEW_TTL_MS = 1500;

export function draftPreview(): Promise<DraftPreview> {
  const ops = draft.ops();
  const key = `${draft.version()}:${JSON.stringify(ops)}`;
  const now = Date.now();
  if (previewCache && previewCache.key === key && now - previewCache.at < PREVIEW_TTL_MS) {
    return previewCache.p;
  }
  const p = previewDraft(ops);
  previewCache = { key, at: now, p };
  /* 失败结果不写入缓存，否则一次网络异常会导致该草稿在 TTL 内始终无法读取 */
  p.catch(() => {
    if (previewCache?.p === p) previewCache = null;
  });
  return p;
}

export const applyDraft = (ops: ModelOp[], note?: string) =>
  post<{
    revision_id: number;
    changed: number;
    client_config: {
      snapshot_id: number;
      status: 'unchanged' | 'activated' | 'awaiting-first-topology';
      serving_generation: number | null;
      pending_topology: string[];
    };
  }>('/model/apply', { ops, note: note ?? null });

/* 草稿中某一份产物的内容。索引只提供清单和 sha256，展开某一份时才拉取其内容。 */
export const previewDraftArtifact = (ops: ModelOp[], targetKind: string, targetId: string, artifactKind: string) =>
  post<ArtifactContent>('/model/preview/artifact', {
    ops,
    target_kind: targetKind,
    target_id: targetId,
    artifact_kind: artifactKind,
  });

const post = <T>(path: string, body?: unknown, token = '') =>
  api<T>(path, token, {
    method: 'POST',
    body: body === undefined ? undefined : JSON.stringify(body),
  });

export const fetchWhoami = (token = '') => api<Whoami>('/whoami', token);

export interface AuthState {
  initialized: boolean;
  /** 存在 `public` 操作者且未设置密码：任何访问该页面的人都可直接进入。见 `PUBLIC_ID`。 */
  public_open: boolean;
}

export interface IssuedAdminToken {
  operator_id: string;
  token: string;
  token_prefix: string;
}

export interface InitAdminResponse {
  admin: Whoami;
  session_expires_at: string;
}

export interface LoginAdminResponse {
  admin: Whoami;
  session_expires_at: string;
}

export const fetchAuthState = () => api<AuthState>('/auth/state');
export const fetchSessionWhoami = () => api<Whoami>('/whoami');
export const initAdmin = (body: { operator_id: string; display_name: string; password: string }) =>
  api<InitAdminResponse>('/auth/init', '', { method: 'POST', body: JSON.stringify(body) });
export const loginAdmin = (body: { operator_id: string; password: string }) =>
  api<LoginAdminResponse>('/auth/login', '', { method: 'POST', body: JSON.stringify(body) });
export const logoutAdmin = () => api<{ revoked: boolean }>('/auth/logout', '', { method: 'POST' });

/* ── 修订与编译 ── */

export interface RevisionListItem {
  id: number;
  created_at: string;
  author: string | null;
  note: string | null;
  status: string;
  current: boolean;
  has_snapshot: boolean;
}
export interface RevisionList {
  current_revision: number;
  revisions: RevisionListItem[];
}

export interface Diagnostic {
  level: 'error' | 'warn' | 'info';
  code: string;
  location: string;
  message: string;
}
export interface CompileView {
  revision: number;
  summary: { errors: number; warnings: number; infos: number; can_publish: boolean };
  diagnostics: Diagnostic[];
  system: unknown;
  apps: unknown;
  redacted: boolean;
}

// 此处曾有一份硬编码的静音列表（`MUTED_DIAGNOSTIC_CODES`），其中只有 `hop.plaintext`。
// 引入它的原因成立：明文跳在当前部署中是常态，编译器对每一条跳各报一次，导致面板上
// 长期存在大量相同的警告，需要关注的内容被淹没。
//
// 但按诊断码静音是在错误的层面处理该问题：编译器将一项无法判定的事项报为警告，
// 界面只能逐条隐藏，而隐藏后「该跳是明文」这一事实也一并丢失。
//
// 编译器现已支持 `info` 级别（`Level::Info`，见 ir.md §10.3）：无法判定实际风险的诊断
// 报为 info，不计入警告数、不触发顶栏角标，但正常显示为灰色。噪音问题由级别机制解决，
// 静音列表不再需要，已删除。
//
// 删除它同时修复了一个关联问题：静音会使面板上的条数与 `summary` 不一致，而概览页
// 当时用「总数 - errors」反推警告数，导致 2 条 info 被计为 2 条警告。

/** 面板上需要显示的诊断。两处渲染（顶栏气泡、概览页）都调用它，避免两处过滤结果不一致。 */
export const visibleDiagnostics = (list: Diagnostic[] | undefined): Diagnostic[] => list ?? [];

// 诊断的 `location` 是供程序读取的 id 字符串，编译器只能生成该形式——它不涉及名称，
// 名称是控制面的概念（`ir.md`：IR 中只有 id）。但面板面向使用者，`app-hk-01.c2/sg-01->au-01`
// 中的四个 id 都不便于识别，而同一信息用名称表示为
// 「港新线路 · 新加坡中转 → 澳洲落地」。
//
// 有两种格式，均来自 crates/：
//   `{chain}/{from}->{to}`  —— 链上的一跳（hops.rs 拼接的 `at`）
//   `{a}|{b}`               —— 一对 overlay 成员（system.rs 的链路诊断）
// 无法识别的格式原样返回：显示 id 优于显示推测的名称。
export interface DiagNames {
  node: (id: string) => string | undefined;
  chain: (id: string) => string | undefined;
}

export function formatLocation(location: string, names: DiagNames): string {
  const node = (id: string) => names.node(id) ?? id;

  const hop = location.match(/^(.+?)\/(.+?)->(.+)$/);
  if (hop) {
    const [, chainId, from, to] = hop;
    const chain = names.chain(chainId);
    const path = `${node(from)} → ${node(to)}`;
    return chain ? `${chain} · ${path}` : path;
  }

  const pair = location.match(/^([^|]+)\|([^|]+)$/);
  if (pair) return `${node(pair[1])} ↔ ${node(pair[2])}`;

  return location;
}

export const fetchRevisions = (token = '') => api<RevisionList>('/revisions?limit=50', token);
export const fetchCompile = (revision: number, token = '') => api<CompileView>(`/compile/${revision}`, token);

// 顶栏角标和诊断窗使用的数据。存在草稿时编译草稿——角标显示「0 错」而草稿中存在
// 无法通过校验的改动，会给出错误的状态。
export const fetchCompileView = (revision: number): Promise<CompileView> =>
  draft.isEmpty() ? fetchCompile(revision) : draftPreview().then(p => p.compile);

/* ── 节点 ── */

/* 与 brocade_deployment::protocol 中的同名结构逐字段对应 */
export interface NodeVersions {
  agent: string;
  xray: string | null;
  phantun: string | null;
  wg_tools: string | null;
  /** WG 启用时为 `kernel` 或 `userspace`；WG 关闭时为 null。内核版本低于 5.6 时
      wg-quick 回退到 wireguard-go，`wg show` 的输出与内核态相同，但吞吐相差一个数量级。 */
  wg_backend: string | null;
}
export interface SpoolBacklog {
  observation: number;
  usage: number;
  /** 单调递增。非零表示存在永久丢失的用量记录。 */
  dropped: number;
}
export interface LocalReconcileReport {
  at: number;
  actions: string[];
  /** 有值表示配置已偏移且 agent 无法自行修复，严重程度高于 actions 非空。 */
  error: string | null;
}
export interface GeodataFileState {
  sha256: string;
  bytes: number;
  modified_at: number;
}
export interface GeodataObservation {
  geoip: GeodataFileState | null;
  geosite: GeodataFileState | null;
  asset_dir: string;
}

export interface NodeAgentStateItem {
  node_id: string;
  tenant_id: string;
  name: string;
  public_ipv4: string | null;
  public_ipv4_country?: string | null;
  public_ipv6: string | null;
  public_ipv4_nat: boolean;
  public_ipv6_nat: boolean;
  route_ipv4: string | null;
  route_ipv6: string | null;
  token_prefix: string | null;
  token_created_at: string | null;
  token_last_used_at: string | null;
  token_revoked_at: string | null;
  agent_version: string | null;
  agent_protocol_version: number | null;
  // 运行时对账。null 表示尚未上报（旧版本 agent，或刚纳管尚未轮到），
  // 需要与上报值为零区分——界面显示为「—」而非 0，否则状态最差的机器
  // 会显示为状态最好的。
  runtime_versions: NodeVersions | null;
  spool_backlog: SpoolBacklog | null;
  last_local_reconcile: LocalReconcileReport | null;
  runtime_reported_at: string | null;
  geodata_observed: GeodataObservation | null;
  last_poll_at: string | null;
  last_usage_report_at: string | null;
  usage_generation_id?: number | null;
  usage_last_result?: {
    accepted_readings: number;
    inserted_samples: number;
    skipped_counters: number;
    /** 仅由控制面进程内的相邻轮次比较产生；重启后重新建立基线。 */
    growing_unknown_counters?: number;
    rejected_counters: number;
    gap_samples: number;
    duplicate?: boolean;
  } | null;
  xray_started_at: string | null;
  /* 该机器单独设置的 wg0 MTU；null 表示使用全局默认值 */
  mtu: number | null;
  /* 该机器覆盖的连接策略项，各字段为 null 表示使用全局默认值。
     不是合并后的结果：详情页需要区分该机器设置为 600 和全局值为 600 两种情况，
     否则清除某个字段后无法确认是否发生变化。 */
  connection: NodeConnection;
  // 是否加入 overlay。false 表示不生成 wg 配置、不加入任何链路；该机器仍可作为中继，
  // 只要有链在其上开启了中转端口——此时使用公网地址。
  overlay: boolean;
  // 该机器允许出网。编译器为链末端补全默认动作时读取该字段：允许时补 Egress，
  // 不允许时补 Block 并报 step.no-egress。它与「该链从此处出网」不同——后者取决于规则表中
  // 是否显式写有 Egress，允许出网的中继同样可能只做转发。与 overlay 一致：修改时当前值
  // 读取编译视图（经过草稿预览的那一份），此字段是编译结果尚未返回时的回退值。
  egress_allowed: boolean;
  // 该机器的 DNS 解析器及其解析结果的使用方式。两者都是模型字段而非 agent 观测值，
  // 在此返回是为了详情页能够显示和修改——此前它们只出现在建机器表单中。
  dns: Dns;
  domain_strategy: DomainStrategy;
  // 非空表示已有退役意图。该机器在停用收敛前仍在发布计划中，期望四类产物全部关闭——
  // agent 据此移除 wg0 和 xray，而不是保持原状继续运行。
  retired_at: string | null;
  lifecycle_phase: 'active' | 'retiring' | 'retired' | 'abandoned';
  lifecycle_epoch: number;
  lifecycle_deployment_id: number | null;
  lifecycle_completed_at: string | null;
  lifecycle_last_error: string | null;
  /* 外部连接该机器 wg 端口的方式。`fake_tcp` 表示入站 UDP 被封禁，使用 phantun 的 TCP 封装。 */
  wg_transport_kind: 'udp' | 'fake_tcp';
  wg_fake_tcp_port: number | null;
  applied: Record<string, unknown> | null;
}

export interface NodeLifecycleTransitionResult {
  revision_id: number;
  node_id: string;
  lifecycle: {
    node_id: string;
    lifecycle_epoch: number;
    phase: 'active' | 'retiring' | 'retired' | 'abandoned';
    intent_revision: number | null;
    deployment_id: number | null;
    requested_at: string | null;
    completed_at: string | null;
    completed_by: string | null;
    reason: string | null;
    last_error: string | null;
  };
  deployment_id: number | null;
  canceled_deployment_ids: number[];
}

// 退役与恢复是操作流程，不进入浏览器草稿。服务端在一个事务中提交模型意图、使旧发布
// 失效，并创建替代配置单。
export const setNodeStatus = (id: string, status: 'active' | 'retired') =>
  api<NodeLifecycleTransitionResult>(`/nodes/${encodeURIComponent(id)}/status`, '', {
    method: 'PUT',
    body: JSON.stringify({ status }),
  });

export const abandonNode = (id: string, reason: string, unregisterWarp = true) =>
  post<NodeLifecycleTransitionResult>(`/nodes/${encodeURIComponent(id)}/lifecycle/abandon`, {
    reason,
    unregister_warp: unregisterWarp,
  });

export const fetchNodes = (token = '') => api<{ nodes: NodeAgentStateItem[] }>('/nodes/agent-state', token);

/* Dns 是带标签的枚举：{"t":"system"} 或 {"t":"servers","v":["1.1.1.1"]} */
export type Dns = { t: 'system' } | { t: 'servers'; v: string[] };

/* 该机器出网时的域名解析方式。取值即产物中的原值，只是拼写遵循模型的约定
   （xray 的 UseIPv4v6 在此为 use_ipv4v6，大小写由 artifacts/xray.rs 还原）。 */
export type DomainStrategy = 'use_ip' | 'use_ipv4' | 'use_ipv6' | 'use_ipv4v6' | 'use_ipv6v4' | 'as_is';

export interface ProvisionNodeRequest {
  id: string;
  tenant_id: string;
  name: string;
  public_ipv4?: string | null;
  public_ipv6?: string | null;
  public_ipv4_nat?: boolean;
  public_ipv6_nat?: boolean;
  wg_listen_port: number;
  api_port?: number | null;
  overlay: boolean;
  egress_allowed: boolean;
  dns: Dns;
  domain_strategy: DomainStrategy;
  note?: string | null;
  /** 这台机器从哪个证书组取 TLS / Hysteria 2 证书。不填即不属于任何组，那样它没有本机证书，
      其上的 TLS 与 Hysteria 2 接入面会在编译时被拒绝——REALITY 指向外部站点的不受影响。
      组决定这台机器的 SNI，建完再改会让已发出去的订阅失效，所以在这里选。 */
  cert_label_id?: string | null;
}

export interface ProvisionNodeResult {
  revision_id: number;
  node: {
    id: string;
    tenant_id: string;
    name: string;
    public_ipv4: string | null;
    public_ipv6: string | null;
    public_ipv4_nat: boolean;
    public_ipv6_nat: boolean;
    overlay_addr: string;
    wg_public_key: string;
    wg_listen_port: number;
    api_port: number | null;
    overlay: boolean;
    egress_allowed: boolean;
    dns: Dns;
    domain_strategy: DomainStrategy;
  };
  enrollment: {
    token: string;
    token_prefix: string;
    /* null 表示不过期 */
    expires_at: string | null;
    script_url: string;
    script_sha256: string;
    install_command: string;
  };
}

export const provisionNode = (body: ProvisionNodeRequest, token = '') =>
  post<ProvisionNodeResult>('/nodes/provision', body, token);

export interface TenantListItem {
  id: string;
  name: string;
  node_count: number;
  user_count: number;
  operator_count: number;
}

export const fetchTenants = (token = '') => api<{ tenants: TenantListItem[] }>('/tenants', token);

/* 首次安装的流程需要立即写库：该阶段还没有控制台界面，因此不存在提交操作。 */
export const createTenantNow = (body: { id: string; name: string }, token = '') =>
  post<unknown>('/tenants', body, token);

export const createTenant = async (body: { id: string; name: string }) => {
  draft.push({ op: 'create_tenant', tenant: body });
  return {} as unknown;
};

/* ── 发布 ── */

/* 服务端使用 PlannedAction 的 kebab-case 形式：ApplyWireGuard → apply-wire-guard（WireGuard 被拆为两段） */
export type PlannedAction =
  | 'apply-phantun'
  | 'apply-hy2-port-hop'
  | 'apply-wire-guard'
  | 'apply-xray'
  | 'sync-grants'
  | 'disable-phantun'
  | 'disable-hy2-port-hop'
  | 'disable-wire-guard'
  | 'disable-xray';

export interface PlanSummary {
  total_targets: number;
  changed_targets: number;
  skipped_targets: number;
  disruptive_targets: number;
  max_wave: number;
}
export interface PlannedTarget {
  node_id: string;
  status: 'pending' | 'skipped';
  wave: number;
  disruptive: boolean;
  actions: PlannedAction[];
}
export interface DeploymentPlan {
  revision: number;
  targets: PlannedTarget[];
  summary: PlanSummary;
  warnings: { code: string; location: string; message: string }[];
  // 预览页「变更内容」的基线：同类上一次成功推送的 revision，与创建工单时确定的取值
  // 语义相同。null 表示该类尚未成功发布过，无可比较的基线。
  base_revision_id: number | null;
}

export interface DeploymentListItem {
  id: number;
  revision_id: number;
  status: string;
  active: boolean | null;
  actor: string | null;
  // 配置单需要写盘并重启；权限单只在运行中的 xray 内增删名单，不断开连接。
  // 两者代价相差一个数量级，界面上分别称为「变更单」和「自动化授权单」。
  kind: 'config' | 'grants';
  note: string | null;
  // 变更的起始版本：同类上一次成功推送的 revision，在创建时确定。
  // null 表示该类尚未成功发布过。
  base_revision_id: number | null;
  rollback_of_deployment_id: number | null;
  sync_of_deployment_id: number | null;
  created_at: string;
  started_at: string | null;
  finished_at: string | null;
  total_targets: number;
  changed_targets: number;
  skipped_targets: number;
  failed_targets: number;
  disruptive_targets: number;
  max_wave: number;
  // 处于等待人工确认状态而非等待机器上报。含破坏性动作的波需要人工确认后才继续下发——
  // 两种等待在列表上显示相同（均为「推送中」），但一个等待机器回报、一个等待人工操作。
  awaiting_confirmation: boolean;
}

export interface DeploymentTargetDetail {
  node_id: string;
  status: string;
  error: string | null;
  wave: number;
  disruptive: boolean;
  desired_structure: unknown;
  observed_before: unknown;
  observed_after: unknown;
  verdict: unknown;
  dispatched_at: string | null;
}

export interface DeploymentDetail {
  id: number;
  revision_id: number;
  status: string;
  active: boolean | null;
  actor: string | null;
  note: string | null;
  base_revision_id: number | null;
  warnings: unknown;
  created_at: string;
  started_at: string | null;
  halted_at: string | null;
  finished_at: string | null;
  rollback_of_deployment_id: number | null;
  sync_of_deployment_id: number | null;
  divergence_cleared_at: string | null;
  targets: DeploymentTargetDetail[];
}

export interface DeploymentCommandResult {
  deployment_id: number;
  status: string;
  active: boolean | null;
  sync_deployment_id: number | null;
  rollback_deployment_id: number | null;
}

export const planDeployment = (revision_id: number, token = '') =>
  post<DeploymentPlan>('/deployments/plan', { revision_id }, token);

// 不传 kind 表示两类都返回。筛选在服务端执行：在前端筛选时 limit 会先被高频的权限单
// （人工权限操作和配额执行都会产生）占满，导致配置单无法返回。
export const fetchDeployments = (kind?: 'config' | 'grants', token = '') =>
  api<{ deployments: DeploymentListItem[] }>(`/deployments?limit=50${kind ? `&kind=${kind}` : ''}`, token);

export const fetchDeployment = (id: number, token = '') => api<DeploymentDetail>(`/deployments/${id}`, token);

/**
 * 权限变更先进入 durable outbox，之后才会生成自动化授权单。如果规划阶段持续失败，
 * deployments 列表里没有任何记录，因此必须单独读取这层状态。
 */
export interface GrantAutomationStatus {
  pending_jobs: number;
  retrying_jobs: number;
  failed_jobs: number;
  max_attempts: number;
  latest_revision_id: number | null;
  oldest_pending_at: string | null;
  last_attempt_at: string | null;
  last_error: string | null;
}

export const fetchGrantAutomationStatus = (token = '') => api<GrantAutomationStatus>('/grants/automation', token);

export const createDeployment = (
  body: { revision_id: number; idempotency_key: string; note?: string | null },
  token = '',
) =>
  post<{ deployment_id: number; status: string; reused: boolean; plan: DeploymentPlan }>('/deployments', body, token);

export const createRollback = (
  body: { target_deployment_id: number; idempotency_key: string; note?: string | null },
  token = '',
) => post<{ deployment_id: number; status: string; reused: boolean; plan: DeploymentPlan }>('/rollback', body, token);

export const confirmWave = (id: number, wave: number, token = '') =>
  post<unknown>(`/deployments/${id}/waves/${wave}/confirm`, { actor: null }, token);

export const haltDeployment = (id: number, token = '') =>
  post<DeploymentCommandResult>(`/deployments/${id}/halt`, undefined, token);

// 丢弃已提交但从未下发的改动：模型回退到最近一次成功发布的版本，不需要操作任何机器。
//
// 回退到已发布的版本而非回退一个版本：计划预览显示的 diff 是「已发布 → 当前」的全部差异，
// 只回退一个版本时中间同样未下发的版本仍然存在，回退后 diff 仍然很大。
//
// 与回滚分开是因为两者代价相差一整轮收敛——改动从未离开控制面，机器上运行的仍是已发布
// 版本的产物，模型回退后下一次 verify 即为「已收敛」，不产生任何 deployment。
// 路径中携带客户端认为的当前修订，服务端会校验：期间有其他人提交时该操作失败，
// 不会连同他人刚写入的内容一并丢弃。
export interface DiscardPendingResult {
  /** 被作废的修订号，按从旧到新排列。它们在历史中标记为 aborted。 */
  discarded_revisions: number[];
  /** 丢弃后的当前修订：内容与基准版本相同的新修订号。修订号单调递增，不回退。 */
  current_revision: number;
  restored_from_revision: number;
}
export const discardPendingChanges = (id: number, token = '') =>
  post<DiscardPendingResult>(`/revisions/${id}/discard-pending`, undefined, token);

export const cancelDeployment = (id: number, token = '') =>
  post<DeploymentCommandResult>(`/deployments/${id}/cancel`, undefined, token);

export const cancelRollbackDeployment = (id: number, token = '') =>
  post<DeploymentCommandResult>(`/deployments/${id}/cancel-rollback`, undefined, token);

export const retryTarget = (id: number, nodeId: string, token = '') =>
  post<unknown>(`/deployments/${id}/targets/${nodeId}/retry`, undefined, token);

/* ── 用户与授权 ── */

export interface UserListItem {
  tenant_id: string;
  id: string;
  // Readonly responses remove usable credentials at the server boundary.
  uuid?: string;
  status: string;
  created_at: string;
  created_revision: number | null;
}

export const fetchUsers = (includeDisabled = true) =>
  api<{ users: UserListItem[] }>(`/users?include_disabled=${includeDisabled}`);

export const createUser = async (body: { tenant_id: string; id: string }) => {
  draft.push({ op: 'create_user', user: body });
  return {} as unknown;
};

// Permission changes bypass the browser draft.  The server commits a revision and a durable
// automatic-grants job together; the worker creates the non-disruptive grants deployment.
export const setUserStatus = (tenant: string, user: string, status: 'active' | 'disabled') =>
  api<{ revision_id: number; user: UserListItem }>(`/users/${tenant}/${user}/status`, '', {
    method: 'PUT',
    body: JSON.stringify({ status }),
  });

export const rotateUserUuid = (tenant: string, user: string) =>
  post<{ revision_id: number }>(`/users/${tenant}/${user}/rotate-uuid`, undefined);

export interface GrantProbeCapability {
  available: boolean;
  version: string | null;
  reason: string | null;
  concurrency: number;
}

export interface GrantProbePlanItem {
  id: string;
  name: string;
  app_id: string;
  app_name: string;
  chain_id: string;
  ingress_id: string;
  family: 'ipv4' | 'ipv6' | 'unknown';
  protocol: 'vless' | 'anytls' | 'hysteria2';
}

export interface GrantProbePlan {
  serving_revision: number;
  serving_generation: number;
  items: GrantProbePlanItem[];
}

export type GrantProbeItemStatus = 'waiting' | 'running' | 'passed' | 'failed' | 'canceled';
export interface GrantProbeJobItem extends GrantProbePlanItem {
  status: GrantProbeItemStatus;
  ttfb_ms: number | null;
  detail: string | null;
}

export type GrantProbeJobStatus = 'running' | 'completed' | 'canceled' | 'superseded';
export interface GrantProbeJob {
  id: string;
  tenant_id: string;
  user_id: string;
  serving_revision: number;
  serving_generation: number;
  status: GrantProbeJobStatus;
  message: string | null;
  created_at_unix_secs: number;
  finished_at_unix_secs: number | null;
  items: GrantProbeJobItem[];
}

export const fetchGrantProbeCapability = () => api<GrantProbeCapability>('/grant-probes/capability');
export const fetchUserGrantProbePlan = (tenant: string, user: string) =>
  api<GrantProbePlan>(`/users/${tenant}/${user}/grant-probes`);
export const startUserGrantProbe = (tenant: string, user: string, itemIds: string[] = []) =>
  post<{ job: GrantProbeJob; reused: boolean }>(`/users/${tenant}/${user}/grant-probes`, {
    item_ids: itemIds,
  });
export const fetchGrantProbeJob = (id: string) => api<GrantProbeJob>(`/grant-probes/${id}`);
export const cancelGrantProbe = (id: string) => api<GrantProbeJob>(`/grant-probes/${id}`, '', { method: 'DELETE' });
export const grantProbeEventsUrl = (id: string) => `/grant-probes/${encodeURIComponent(id)}/events`;

export interface ClashSubscriptionInfo {
  url: string;
  urls: {
    both: string;
    v4: string;
    v6: string;
  };
  template: string;
  haitun: ClashHaitunSubscriptionInfo;
  remaining_bytes: number | null;
  reset_at: string;
  usage_has_gap: boolean;
}

export interface ClashHaitunSubscriptionInfo {
  template: string;
  status: 'not-created' | 'active' | 'revoked';
  urls: ClashSubscriptionInfo['urls'] | null;
  created_at: string | null;
  revoked_at: string | null;
}

// The URL is a bearer credential, so it is fetched only when the operator opens the Clash
// dialog. It must not ride along in the ordinary user-list response or initial page DOM.
export const fetchClashSubscription = (tenant: string, user: string) =>
  api<ClashSubscriptionInfo>(`/users/${tenant}/${user}/clash-subscription`);

export const issueClashHaitunSubscription = (tenant: string, user: string) =>
  api<ClashHaitunSubscriptionInfo>(`/users/${tenant}/${user}/clash-subscription/haitun`, '', {
    method: 'POST',
  });

export const revokeClashHaitunSubscription = (tenant: string, user: string) =>
  api<ClashHaitunSubscriptionInfo>(`/users/${tenant}/${user}/clash-subscription/haitun`, '', {
    method: 'DELETE',
  });

// ── 流量额度（用户 × 项目）──
// 额度不修改任何产物——xray.json 中不写入用户——因此它是直接写入的运营参数；
// 实际超限时由配额执行器修改 grant，并走同类的自动化授权单（论证见 migrations/0002）。
export interface UserAppQuota {
  tenant_id: string;
  user_id: string;
  app_id: string;
  limit_bytes: number;
  updated_at: string;
  // 该项目下被配额执行器撤销的接入面。必须与从未授权区分——撤销后 grants 中不再有这些记录，
  // 不说明原因时行内会显示「未授权」，与实际情况不符。
  suspended_ingresses: string[];
}

export const fetchQuotas = () => api<{ quotas: UserAppQuota[] }>('/quotas');

// limit_bytes 传 null 表示取消该额度。服务端拒绝 0——限制为 0 字节和不限量必须是
// 两个不同的取值，用同一个 0 表示会无法区分。
export const setQuota = (body: { tenant_id: string; user_id: string; app_id: string; limit_bytes: number | null }) =>
  api<{ quota: UserAppQuota | null }>('/quotas', '', {
    method: 'PUT',
    body: JSON.stringify(body),
  });

// ── 应用层：项目 / 链 / 接入面 ──
// 机器是否运行 xray 由计算得出（physical/node.rs 的 xray_plan：inbounds 来自该机器上的
// 接入面，outbounds 来自链的转发边），因此使节点运行 xray 的方式是为其创建链和接入面，
// 而不是在节点上增加开关。创建项目需要 system-admin，链和接入面需要 editor。
// 三个请求体都设置了 deny_unknown_fields，只能发送下列字段。
export interface ModelWriteResult {
  revision_id: number;
}

// 下列函数不再直接写库，而是向草稿中追加一条操作（`draft.ts`）。函数签名保持不变，
// 因此面板侧仍然调用 `await upsertChain(...)`，只是结果写入浏览器本地，
// 点击顶栏的「提交」后整批写库并产生一个修订。需要立即写入的（如签发安装令牌）使用 `*Now`。
export const upsertApp = async (body: { id: string; label: string; note?: string }) => {
  draft.push({ op: 'upsert_app', app: { id: body.id, label: body.label } });
  return { revision_id: 0 } as ModelWriteResult;
};

export const createApp = async (body: { id: string; label: string }) => {
  draft.push({ op: 'create_app', app: body });
  return { revision_id: 0 } as ModelWriteResult;
};

/** Persist the complete final line order produced by a drag gesture. */
export const reorderApps = async (ids: string[]) => {
  draft.push({ op: 'reorder_apps', ids });
  return { revision_id: 0 } as ModelWriteResult;
};

/** Persist one line's complete final chain order without changing any stable chain ID. */
export const reorderChains = async (appId: string, ids: string[]) => {
  draft.push({ op: 'reorder_chains', app_id: appId, ids });
  return { revision_id: 0 } as ModelWriteResult;
};

export const upsertChain = async (
  appId: string,
  body: { id: string; tenant_id: string; name: string; subscription_country: string | null; note?: string },
) => {
  draft.push({
    op: 'upsert_chain',
    app_id: appId,
    chain: {
      id: body.id,
      tenant_id: body.tenant_id,
      name: body.name,
      subscription_country: body.subscription_country ?? null,
    },
  });
  return { revision_id: 0 } as ModelWriteResult;
};

export const createChain = async (
  appId: string,
  body: { id: string; tenant_id: string; name: string; subscription_country?: string | null },
) => {
  draft.push({ op: 'create_chain', app_id: appId, chain: body });
  return { revision_id: 0 } as ModelWriteResult;
};

// 全部留空时使用全局设置中的 REALITY 站点（settings.reality_site），
// 填写后作为该接入面的覆盖值。密钥对由 store 生成，不经过浏览器。
export interface CreateRealityIngress {
  fallback_mode?: RealityFallbackMode;
  fallback_limits?: RealityFallbackLimits;
  fallback_guard?: boolean;
  dest?: string;
  server_names?: string[];
  fingerprint?: string;
  flow?: string;
}

export type RealityFallbackMode = 'global-site' | 'node-certificate' | 'custom-site';

export interface RealityFallbackRateLimit {
  after_bytes: number;
  bytes_per_sec: number;
  burst_bytes_per_sec: number;
}

export type RealityFallbackLimits =
  | { mode: 'off' }
  | { mode: 'balanced' }
  | { mode: 'strict' }
  | { mode: 'custom'; upload: RealityFallbackRateLimit; download: RealityFallbackRateLimit };

/* 上行模式。四个取值 xray 均支持，拼写错误时它在启动阶段即拒绝。
 *
 * 该字段在客户端表示发送方式，在服务端表示接受哪一种——同一字段两侧含义不同。
 * 实测十六种服务端/客户端组合（26.4.25）：服务端不设置时接受全部；设置后只接受该一种。
 *
 * 服务端设置后只接受该一种；而客户端未设置（auto）时发送哪一种取决于其下的安全层——
 * 实测 REALITY 下发送 stream-one，TLS 下发送 packet-up。因此设置后旧订阅是否仍可用
 * 无法仅由该值判断。
 *
 * 'auto' 只是界面上的一个档位，产物两侧都不写入该值——两端各自解析到相同结果，
 * 写入会将一个默认值变为一项限制条件。 */
export type XhttpMode = 'auto' | 'packet-up' | 'stream-up' | 'stream-one';

export interface XhttpXmuxRange {
  from: number;
  to: number;
}

export interface XhttpXmux {
  max_concurrency?: number | null;
  max_connections?: number | null;
  h_max_request_times: XhttpXmuxRange;
  h_max_reusable_secs: XhttpXmuxRange;
  h_keep_alive_period_secs?: number | null;
}

export const DEFAULT_XHTTP_XMUX: XhttpXmux = {
  max_concurrency: 1,
  max_connections: null,
  h_max_request_times: { from: 600, to: 900 },
  h_max_reusable_secs: { from: 1800, to: 3000 },
  h_keep_alive_period_secs: null,
};

export interface XhttpTuning {
  x_padding_bytes?: XhttpXmuxRange | null;
}

export const DEFAULT_XHTTP_TUNING = {
  x_padding_bytes: { from: 100, to: 1000 },
} as const;

export interface Xhttp {
  path: string;
  host?: string | null;
  xmux?: XhttpXmux | null;
  tuning?: XhttpTuning | null;
  mode?: XhttpMode;
  download?: XhttpDownload | null;
}

export interface XhttpDownload {
  v4?: ProjectionDownloadEndpoint | null;
  v6?: ProjectionDownloadEndpoint | null;
}

/* 接入面的完整链路配置：协议、安全层、传输层作为一个**经过验证的组合**统一命名，
 * 而不是三个独立的维度。
 *
 * 不拆分为正交字段的原因：这几个维度并不独立。Vision 流控 + XHTTP 可以编译通过、
 * `xray -test` 也通过，但运行后所有连接都被拒绝。字段正交意味着该组合可以被表达出来，
 * 只能依靠校验器事后拦截。
 *
 * `vless-reality` 是默认值，也是该字段引入之前所有接入面的配置。
 * `vless-reality-xhttp` 将多条流复用到一条 HTTP/2 连接上，握手次数因此减少——而在 REALITY 下，
 * 每次握手都需要服务端建立到借用站点的连接以获取证书，因此减少的不只是握手，还有相应流量。
 * 代价是队头阻塞：多路复用共用一条 TCP，丢包会阻塞该连接上的所有流。链路质量越差，
 * 并发数应设置得越小。 */
export type TransportKind = 'vless-reality' | 'vless-reality-xhttp' | 'vless-tls' | 'vless-tls-xhttp';

export type HysteriaCongestion = 'brutal' | 'bbr' | 'reno' | 'force-brutal';

/* BBR 策略。只在该连接实际使用 BBR 时被读取：congestion 为 bbr，或为 brutal 且带宽未填写。 */
export type HysteriaBbrProfile = 'standard' | 'conservative' | 'aggressive';

/* finalmask.quicParams 中除拥塞控制和带宽之外的调优项。
 *
 * 每一项均可缺省，缺省表示不写入该键，由 xray 使用其自身版本的默认值。写入我们认为的默认值
 * 会把当前上游的默认值固化进产物，使产物不再随部署的版本变化。
 *
 * 取值范围取自 xray 的构建期校验，超出范围会导致启动失败：窗口 ≥ 16384 字节，
 * 空闲超时 4–120 秒，保活 2–60 秒，并发流 ≥ 8。 */
export interface HysteriaQuic {
  init_stream_receive_window?: number | null;
  max_stream_receive_window?: number | null;
  init_connection_receive_window?: number | null;
  max_connection_receive_window?: number | null;
  max_idle_timeout_secs?: number | null;
  keep_alive_period_secs?: number | null;
  max_incoming_streams?: number | null;
  disable_path_mtu_discovery?: boolean;
}

export const HY2_QUIC_LIMITS = {
  window: { min: 16384 },
  idle: { min: 4, max: 120 },
  keepalive: { min: 2, max: 60 },
  streams: { min: 8 },
} as const;

export interface Hysteria2Settings {
  /* 该线路自身的 UDP 监听端口，不是接入面的端口。两条线此前共用 ingress.port，
   * 引入端口跳转后不再可行：跳转是对一段端口的重定向规则，遗漏 `-p udp` 会同时影响
   * 同号的 TCP 一侧。 */
  port: number;
  /* 客户端轮换使用的 UDP 端口区间，闭区间；null 表示只连接 port。
   * 该区间必须包含 port——服务端只监听该端口，其余端口通过机器上的 DNAT 转发到它。 */
  hop?: { start: number; end: number } | null;
  bandwidth: { up?: string | null; down?: string | null };
  congestion: HysteriaCongestion;
  bbr_profile?: HysteriaBbrProfile;
  quic?: HysteriaQuic;
  obfs?: { kind: 'salamander'; password: string } | null;
  masquerade: { kind: 'not-found' } | { kind: 'proxy'; url: string };
}

export type AnyTlsMasquerade =
  | { kind: 'not-found'; headers?: Record<string, string> }
  | { kind: 'string'; content: string; headers?: Record<string, string>; status_code?: number };

export interface AnyTlsSettings {
  /** AnyTLS owns its own TCP listener and does not reuse `Ingress.port`. */
  port: number;
  /** One padding grammar line per array item. Empty means Xray's built-in scheme. */
  padding_scheme?: string[];
  /** Default is the explicit 404 masquerade. */
  masquerade: AnyTlsMasquerade;
}

export type Transport =
  | { kind: 'vless-reality' }
  | { kind: 'vless-reality-xhttp'; xhttp: Xhttp }
  | { kind: 'vless-tls' }
  | { kind: 'vless-tls-xhttp'; xhttp: Xhttp };

/* 一个接入面启用哪几条线。两条不是二选一：TCP 和 UDP 各建立一个 inbound，共用同一份凭据和
 * 同一条授权，客户端可使用其中任意一条。至少需要启用一条——服务端的类型定义和库中的
 * CHECK 约束都有此要求，前端通过 `wiresAreValid` 在保存前拦截。 */
export interface Wires {
  vless?: Transport | null;
  anytls?: AnyTlsSettings | null;
  hysteria2?: Hysteria2Settings | null;
}

export const wiresAreValid = (wires: Wires) => !!wires.vless || !!wires.anytls || !!wires.hysteria2;

/* 该档位是否需要机器自有证书。REALITY 借用其他站点，机器上没有证书；TLS 使用自有证书，
 * 未签发时该接入面不可用（编译器会拦截，`ingress.tls-no-certificate`）。 */
export const transportNeedsCertificate = (kind: TransportKind) => kind === 'vless-tls' || kind === 'vless-tls-xhttp';

/* 只要有一条线使用自有证书就需要证书。hy2 必然使用，TLS 两档同样使用。 */
export const wiresNeedCertificate = (wires: Wires) =>
  !!wires.anytls || !!wires.hysteria2 || (!!wires.vless && transportNeedsCertificate(wires.vless.kind));

export const transportIsXhttp = (kind: TransportKind) => kind === 'vless-reality-xhttp' || kind === 'vless-tls-xhttp';

export interface UpsertIngressBody {
  id: string;
  chain_id: string;
  node_id: string;
  bind: string;
  port: number;
  front_id?: string;
  reality: CreateRealityIngress;
  /** 不传表示只有 VLESS + REALITY 一条线。 */
  wires?: Wires;
  projection?: IngressProjection;
  /** 该入口拒绝承载的流量类型。不传表示四项默认启用——服务端同样如此，遗漏时取受保护的一侧。 */
  guard?: IngressGuard;
  note?: string;
}

/** 入口级的限制。每一项编译为一条 block 规则，排在该链自身规则之前。 */
export interface IngressGuard {
  /** 机房内网和机队自身的覆盖网段。用于补足隔离，不属于滥用防护。 */
  no_private: boolean;
  /** BT，按协议识别而非端口。要求该入口启用嗅探，使用前置代理的入口在编译时会被拒绝。 */
  no_bittorrent: boolean;
  /** 25 / 465 / 587。发送垃圾邮件是机器 IP 被列入黑名单的主要原因。 */
  no_mail: boolean;
  /** 反射放大攻击常用的几个 UDP 端口。 */
  no_udp_amplification: boolean;
  /** 除 443 外的全部 UDP。限制范围最大的一档，会影响游戏和语音，默认关闭。 */
  tcp_and_quic_only: boolean;
}

// 对外投影：订阅中写入的地址。两个地址族相互独立，某一族缺省（或为 null）表示该族不投影。
//
// 「不投影」不能用空 host 表示——空串表示已启用但未填写，服务端通过
// `ingress.projection-blank` 拒绝。该类型中不存在 `host: ''` 这一合法状态：
// 关闭时不传该族即可。
export interface IngressProjection {
  v4?: ProjectionEndpoint | null;
  v6?: ProjectionEndpoint | null;
}

export interface ProjectionEndpoint {
  host: string;
  port: number;
  /** Legacy snapshots only. New independent download settings live on Xhttp.download. */
  download?: ProjectionDownloadEndpoint | null;
}

export interface ProjectionDownloadEndpoint {
  host: string;
  port: number;
  /** REALITY 分离下载在节点上实际监听的 TLS 端口；TLS + XHTTP 不需要该字段。 */
  origin_port?: number | null;
  http_host?: string | null;
  mux?: number | null;
}

export const upsertIngress = async (appId: string, body: UpsertIngressBody, base?: UpsertIngressBody) => {
  const { note: _note, ...ingress } = body;
  if (base) {
    const { note: _baseNote, ...baseIngress } = base;
    draft.pushIngress(appId, baseIngress, ingress);
  } else {
    draft.push({ op: 'upsert_ingress', app_id: appId, ingress });
  }
  return { revision_id: 0 } as ModelWriteResult;
};

export const createIngress = async (appId: string, body: UpsertIngressBody) => {
  const { note: _note, ...ingress } = body;
  draft.push({ op: 'create_ingress', app_id: appId, ingress });
  return { revision_id: 0 } as ModelWriteResult;
};

export type GrantWrite = {
  app_id: string;
  tenant_id: string;
  user_id: string;
  ingress_id: string;
  enabled: boolean;
};

export const upsertGrant = (grant: GrantWrite) => post<{ revision_id: number }>('/grants', grant);

// A new chain's ingress does not exist until its structural draft commits, so the wizard stages
// its initial grants in that same transaction.  Normal permission edits use `upsertGrant` above.
export const stageGrant = (grant: GrantWrite) => {
  draft.push({ op: 'upsert_grant', grant });
};

// ── 规则表 ──
// 与 brocade-core::model 逐字段对应。两个枚举的 tag 风格不同，注意区分：
// DestMatch 使用邻接标签（内容在 v 中），Action 使用内部标签（字段平铺）。
//   {"m":{"t":"geosite","v":["cn"]},"a":{"t":"forward","to":"sg-01"}}
// Rule 的键是 m 和 a（服务端通过 #[serde(rename)] 指定）。
export type DestMatch =
  | { t: 'any' }
  | { t: 'domain_suffix'; v: string[] }
  | { t: 'domain_keyword'; v: string[] }
  | { t: 'domain_regex'; v: string }
  | { t: 'geosite'; v: string[] }
  | { t: 'ip_cidr'; v: string[] }
  | { t: 'geoip'; v: string[] }
  | { t: 'port'; v: string[] }
  | { t: 'network'; v: 'tcp' | 'udp' }
  | { t: 'all'; v: DestMatch[] }
  | { t: 'front_downstream' };

// 该跳连接对端时使用的地址。不填写表示使用 overlay。
//
// 地址配置在链上，不从节点推导。一台机器可能有多个可达地址（公网、机房内网），
// 使用哪一个由该链决定：同一台中继，机房内的邻居应走内网，外部应走公网。
// 编译器无法判断哪两台之间内网可直达，判断错误的表现是该跳走了另一条路径且无提示。
//
// overlay 档使用符号引用而非固定地址：overlay 地址由系统分配，手动填写容易出错，
// 且修改网段时会导致整条链失效。
export type HopDial =
  | { t: 'overlay' }
  | { t: 'addr'; v: string }
  // 反向接入：不主动连接对端，由对端连接本机。与 overlay 一样使用符号引用——地址是本机的，
  // 由编译器从节点属性推导，端口取本机在该链上的中转端口。
  // 地址族需要显式指定：两个族的可达性相互独立，不做自动选择。
  | { t: 'reverse'; v: 'v4' | 'v6' };

// 该跳发起的连接的使用方式。不填写表示每条流单独建立连接、结束后关闭，
// 即该字段引入之前的行为。
//
// 同一维度上的三档：一条 TCP 同时承载多少条流、流结束后是否保留连接。使用三个名称而非
// 直接暴露 xray 的 concurrency（1–128），是因为 1 是 Mux.cool 的特殊用法：每条流仍独占
// 一个 worker，流结束后留下的连接可被下一条复用；从 2 开始才是多条流复用一条连接。
// 单并发 worker 借出前不探活，半失效连接可能卡到超时，因此 pool 只为已有配置和明确选择保留，
// 新建规则默认 none。
//
// 只对本机发起的跳有效。reverse 是对端连接本机，本机没有可复用的出站连接——该档位
// 在界面上不显示（reverseTargets 使用另一个面板），编译器也会拒绝。
export type HopPool =
  | { t: 'none' }
  | { t: 'pool' }
  // v ∈ 2..=128。1 对应有卡顿风险的连接池档位，129 及以上 xray 会截断为 128 且不提示，
  // 两侧编译器均拒绝。
  | { t: 'merge'; v: number };

export type RuleAction =
  | { t: 'forward'; to: string; dial?: HopDial; pool?: HopPool }
  | { t: 'egress'; send_through?: string | null }
  | { t: 'proxy'; outbound: string }
  | { t: 'block' };

export interface EgressDnsResolution {
  address: string;
  port: number;
  transport: 'udp' | 'tcp';
  address_strategy: 'use_ip' | 'use_ipv4v6' | 'use_ipv6v4' | 'use_ipv4' | 'use_ipv6';
  fallback: 'stop' | 'machine';
}

export type ExternalOutboundProtocol =
  | {
      t: 'vless';
      v: {
        credential: string;
        encryption: string;
        flow?: string | null;
        transport: ExternalVlessTransport;
      };
    }
  | { t: 'shadowsocks2022'; v: { credential: string; method: string } }
  | { t: 'socks5'; v: { username?: string | null; credential: string } }
  | { t: 'http_connect'; v: { username?: string | null; credential: string } }
  | {
      t: 'wireguard';
      v: {
        credential: string;
        peer_public_key: string;
        local_addresses: string[];
        mtu: number;
        reserved: number[];
        keep_alive: number;
        allowed_ips: string[];
        no_kernel_tun: boolean;
        domain_strategy: 'ForceIP' | 'ForceIPv4' | 'ForceIPv6' | 'ForceIPv4v6' | 'ForceIPv6v4';
      };
    }
  | {
      t: 'warp';
      v: {
        mtu: number;
        keep_alive: number;
        allowed_ips: string[];
        no_kernel_tun: boolean;
        domain_strategy: 'ForceIP' | 'ForceIPv4' | 'ForceIPv6' | 'ForceIPv4v6' | 'ForceIPv6v4';
        /** 0 表示交给 Xray/wireguard-go 自动决定。 */
        workers: number;
      };
    };

export interface ExternalWarpBinding {
  node: string;
  device_id: string;
  account_id: string;
  registered_at: string;
  /** null 表示跟随逻辑隧道默认值；地址策略的两个底层字段始终成对覆盖。 */
  endpoint_address?: string | null;
  endpoint_port?: number | null;
  mtu?: number | null;
  keep_alive?: number | null;
  allowed_ips?: string[] | null;
  no_kernel_tun?: boolean | null;
  domain_strategy?: 'ForceIP' | 'ForceIPv4' | 'ForceIPv6' | 'ForceIPv4v6' | 'ForceIPv6v4' | null;
  workers?: number | null;
  /* 服务端会完全移除私钥；仅历史/未打码数据形状允许它存在。 */
  private_key?: string;
  peer_public_key: string;
  local_addresses: string[];
  reserved: number[];
}

export type ExternalVlessTransport =
  | { t: 'raw' }
  | {
      t: 'xhttp';
      v: {
        path: string;
        host?: string | null;
        mux?: number | null;
        mode?: XhttpMode;
        download?: {
          address: string;
          port: number;
          security: ExternalOutboundSecurity;
          path: string;
          host?: string | null;
          mux?: number | null;
          mode?: XhttpMode;
        } | null;
      };
    };

export type ExternalOutboundSecurity =
  | { t: 'none' }
  | { t: 'tls'; v: { server_name: string; fingerprint: string } }
  | {
      t: 'reality';
      v: { server_name: string; public_key: string; short_id: string; fingerprint: string };
    };

/** A reusable proxy managed outside the Brocade fleet. Credentials arrive redacted. */
export interface ExternalOutbound {
  id: string;
  tenant: string;
  name: string;
  address: string;
  port: number;
  protocol: ExternalOutboundProtocol;
  security: ExternalOutboundSecurity;
  bindings: ExternalWarpBinding[];
}

export type ExternalOutboundWrite = Omit<ExternalOutbound, 'tenant' | 'bindings'> & {
  tenant_id: string;
};

export const upsertExternalOutbound = async (outbound: ExternalOutboundWrite) => {
  draft.push({ op: 'upsert_external_outbound', outbound });
  return { revision_id: 0 } as ModelWriteResult;
};

export interface WarpBindingResult {
  revision_id: number;
  binding: ExternalWarpBinding;
  suggested_endpoint?: string | null;
}

export interface RemoveWarpBindingResult {
  revision_id: number;
  node_id: string;
  device_id: string;
  removed: boolean;
}

export interface WarpBindingOverrides {
  endpoint_address: string | null;
  endpoint_port: number | null;
  mtu: number | null;
  keep_alive: number | null;
  allowed_ips: string[] | null;
  no_kernel_tun: boolean | null;
  domain_strategy: 'ForceIP' | 'ForceIPv4' | 'ForceIPv6' | 'ForceIPv4v6' | 'ForceIPv6v4' | null;
  workers: number | null;
}

export const registerWarpBinding = (tenantId: string, outboundId: string, nodeId: string): Promise<WarpBindingResult> =>
  post<WarpBindingResult>(
    `/tenants/${encodeURIComponent(tenantId)}/tunnels/${encodeURIComponent(outboundId)}/warp-bindings`,
    { node_id: nodeId, accept_terms: true },
  );

export const updateWarpBinding = (
  tenantId: string,
  outboundId: string,
  nodeId: string,
  overrides: WarpBindingOverrides,
): Promise<WarpBindingResult> =>
  api<WarpBindingResult>(
    `/tenants/${encodeURIComponent(tenantId)}/tunnels/${encodeURIComponent(outboundId)}/warp-bindings/${encodeURIComponent(nodeId)}`,
    '',
    { method: 'PUT', body: JSON.stringify(overrides) },
  );

export const removeWarpBinding = (
  tenantId: string,
  outboundId: string,
  nodeId: string,
): Promise<RemoveWarpBindingResult> =>
  api<RemoveWarpBindingResult>(
    `/tenants/${encodeURIComponent(tenantId)}/tunnels/${encodeURIComponent(outboundId)}/warp-bindings/${encodeURIComponent(nodeId)}`,
    '',
    { method: 'DELETE' },
  );

// 该链在该机器上的中转 inbound：监听端口和传输层。
// 一条链对应一个 inbound，因此同一台中继服务两条链时使用两个端口和各自的密钥。
//
// `port: 0` 表示关闭该链在该机器上的中转端口；字段不出现表示不修改。
export interface HopInRequest {
  /* 0 表示关闭该链在该机器上的中转端口 */
  port: number;
  security?: HopWireRequest;
}

export interface Rule {
  m: DestMatch;
  a: RuleAction;
}

export interface StepAccept {
  uuid: string;
  label: string;
}

// accept 是上游连接该机器时使用的接受凭据。转发目标必须具备，否则编译报 relay.no-accept。
// 传 {} 表示沿用已有 uuid，不存在时生成；完全不传表示将其置为 NULL（会中断中继）。
// 已有的需要连同 label 一起回传：label 是统计指标的键，更换后统计曲线会中断。
export const putStep = (
  appId: string,
  chainId: string,
  nodeId: string,
  body: {
    rules: Rule[];
    accept?: { uuid?: string; label?: string };
    hop_in?: HopInRequest;
    note?: string;
  },
) => {
  const { note: _note, ...step } = body;
  draft.push({ op: 'put_step', app_id: appId, chain_id: chainId, node_id: nodeId, step });
  return Promise.resolve({ revision_id: 0 });
};

// DNS 策略是机器级配置，保存后由该机器始终下发。线路编辑器只是其中一个编辑入口；
// 独立草稿操作写入或移除 `(node, selector)`，避免保存、删除链路时取得策略所有权。
export const setNodeEgressDns = (nodeId: string, selector: DestMatch, resolution: EgressDnsResolution | null) => {
  draft.push({ op: 'set_node_egress_dns', node_id: nodeId, selector, resolution });
  return Promise.resolve({ revision_id: 0 });
};

export const reorderNodeEgressDns = (nodeId: string, selectors: DestMatch[]) => {
  draft.push({ op: 'reorder_node_egress_dns', node_id: nodeId, selectors });
  return Promise.resolve({ revision_id: 0 });
};

// 从链中移除一台机器。steps 的记录是链上成员的唯一数据来源，不删除记录时
// 该机器在编译产物中仍然存在（端口仍开启、转发仍生成），因此这是实际删除。
// 级联行为：删除中间跳会同时移除其下依赖的整棵子树；删除链头（入口机器）会使整条链
// 消失——更换入口是另一项操作（upsertIngress）。
export const deleteStep = (appId: string, chainId: string, nodeId: string) => {
  draft.push({ op: 'delete_step', app_id: appId, chain_id: chainId, node_id: nodeId });
  return Promise.resolve({ revision_id: 0 });
};

// 移除该链上不再被引用的 step——保存整棵规则树时作为最后一条操作。
// 判定在服务端执行。判断哪些 step 不再被引用需要整条链的规则表，而浏览器中一次只有
// 一张表是最新草稿，其余表仍是库中的旧状态；按该不完整的图执行删除时，
// 刚在另一张表中连接的机器会被判定为不再被引用并删除，表现为提交后编译报 relay.no-accept
// （规则指向它，但其 step 已被删除）。因此在逐张 putStep 完成后追加该操作，由服务端按完整数据计算。
// 界面上计算的结果（orphansAfter）只用于提示，不触发删除。
export const pruneChain = (appId: string, chainId: string) => {
  draft.push({ op: 'prune_chain', app_id: appId, chain_id: chainId });
  return Promise.resolve({ revision_id: 0 });
};

// 删除整条链：链声明、接入面、授权关系一并清除（在删除草稿提交时由服务端级联执行）。
// 列表页的删除按钮使用该接口——比对链头调用 deleteStep 更直接，且没有接入面的链同样可删除。
export const deleteChain = (appId: string, chainId: string) => {
  draft.push({ op: 'delete_chain', app_id: appId, chain_id: chainId });
  return Promise.resolve({ revision_id: 0 });
};

// 模型快照：授权矩阵使用其中的 apps → ingresses / grants 作为列和当前状态
// （服务端已移除密钥并按租户过滤）；规则编辑还需要 chains（主干顺序）和 steps（现有规则）。
export interface SnapshotIngress {
  id: string;
  chain: string;
  node: string;
  bind: string;
  port: number;
  front?: string | null;
  projection?: IngressProjection | null;
  guard: IngressGuard;
  identity: {
    public_key: string;
    short_ids: string[];
  };
  /* 快照中的结构包含 REALITY 的生效参数，请求中的不包含（密钥由服务端生成，不接受调用方
   * 传入）。因此这两个类型结构相似但不相同。
   *
   * 两条线各占一个字段，字段存在即表示该线启用。读取前需判空：只启用 UDP 的接入面
   * 其 `vless` 为 undefined。 */
  wires: {
    vless?: {
      kind: TransportKind;
      xhttp?: Xhttp | null;
      /* 下列借用站点字段只在 REALITY 两档中存在。 */
      dest?: string;
      server_names?: string[];
      fingerprint?: string | null;
      flow?: string | null;
      fallback_mode?: RealityFallbackMode;
      fallback_limits?: RealityFallbackLimits;
      fallback_guard?: boolean;
    } | null;
    anytls?: AnyTlsSettings | null;
    hysteria2?: Hysteria2Settings | null;
  };
}

/* 将快照中的结构原样回传。全量覆盖的请求不携带该字段即表示切换回 REALITY+TCP，
 * 一次端口修改会使所有客户端配置失效——与 projection 处属同一类问题。 */
function currentVless(ingress: SnapshotIngress): Transport | null {
  const vless = ingress.wires.vless;
  if (!vless) return null;
  const { kind, xhttp } = vless;
  if (!transportIsXhttp(kind)) {
    return kind === 'vless-tls' ? { kind } : { kind: 'vless-reality' };
  }
  // 缺少 xhttp 的 XHTTP 档在库中无法表示（有 CHECK 约束），若出现则回退到同一安全层的
  // TCP 档——回退到 REALITY 会将使用自有证书的接入面切换为借用站点。
  if (!xhttp) {
    return kind === 'vless-tls-xhttp' ? { kind: 'vless-tls' } : { kind: 'vless-reality' };
  }
  return { kind, xhttp } as Transport;
}

export function currentWires(ingress: SnapshotIngress): Wires {
  return {
    vless: currentVless(ingress),
    anytls: ingress.wires.anytls ?? null,
    hysteria2: ingress.wires.hysteria2 ?? null,
  };
}

export function ingressUpsertBody(
  ingress: SnapshotIngress,
  patch: Partial<
    Pick<UpsertIngressBody, 'node_id' | 'bind' | 'port' | 'front_id' | 'projection' | 'wires' | 'guard'>
  > = {},
): UpsertIngressBody {
  // 只启用 UDP 的接入面没有 VLESS 一侧，也不包含 REALITY 的相关字段。回传空表示跟随全局，
  // 与其在库中的状态一致（这些列本身为 NULL）。
  const transport: NonNullable<SnapshotIngress['wires']['vless']> = ingress.wires.vless ?? {
    kind: 'vless-reality',
  };
  return {
    id: ingress.id,
    chain_id: ingress.chain,
    node_id: patch.node_id ?? ingress.node,
    bind: patch.bind ?? ingress.bind,
    port: patch.port ?? ingress.port,
    front_id: patch.front_id ?? ingress.front ?? undefined,
    reality: {
      // TLS 档没有借用站点，这两个字段为 undefined。回传空表示跟随全局，与其在库中的
      // 状态一致（这两列本身为 NULL）。
      dest: transport.dest ?? '',
      server_names: [...(transport.server_names ?? [])],
      fingerprint: transport.kind.startsWith('vless-reality') ? (transport.fingerprint ?? undefined) : undefined,
      fallback_mode: transport.fallback_mode ?? 'global-site',
      fallback_limits: transport.fallback_limits ?? { mode: 'off' },
      // 需要显式回写，不能依赖「不携带即默认启用」：否则任何一次修改端口或迁移机器，
      // 都会将已关闭回落防护的接入面重新启用，且界面上没有任何提示。
      fallback_guard: transport.fallback_guard ?? true,
      // 快照中的 flow 是**生效值**，无法区分是该接入面自行设置还是跟随全局。因此此处
      // 一律按生效值显式回写：写 '' 而非 undefined，因为 undefined 表示跟随全局，
      // 而全局默认启用 Vision——这会导致一次端口修改就将已关闭流控的接入面重新启用，
      // 若该接入面同时启用了 XHTTP，运行时会拒绝所有连接。
      flow: transport.flow ?? '',
    },
    // 该请求是全量覆盖而非 PATCH：不携带 projection 即表示两个地址族都不投影。
    // 因此未修改投影的调用方（修改端口、迁移机器）也必须将现有值原样带上，
    // 否则一次端口修改会清除投影配置。
    projection: patch.projection ?? ingress.projection ?? {},
    // 同样属于全量覆盖的问题：不携带 wires 即表示回到只有 VLESS+REALITY 一条线，
    // 因此一次端口修改会改变 XHTTP 或整条 QUIC 线路，且已下发的客户端配置全部失效——
    // 与上面 projection 处属同一类问题。
    wires: patch.wires ?? currentWires(ingress),
    // 与 projection、wires 属同一类问题，且后果更不易察觉：不携带 guard 即表示四项默认启用，
    // 因此已关闭某条限制的入口，会在其他人修改一次端口后重新启用——界面上没有任何提示，
    // 而该限制可能正是该入口的配置目的。
    guard: patch.guard ?? ingress.guard,
  };
}
export interface SnapshotChain {
  id: string;
  tenant: string;
  name: string;
  subscription_country?: string | null;
}

// 主干是派生概念：链头是接入面所在的机器（`ingress.node`），主干是从链头沿
// `any → Forward` 规则得出的路径。每台机器的规则表中「任意」兜底只会命中一条，
// 因此路径唯一；模型中不存储主干。
export function chainHead(app: SnapshotApp, chainId: string): string | null {
  // 一条链可以有多个入口（HY2 一个、VLESS 一个），但它们必须位于同一台机器上——编译器
  // 通过 `chain.multi-ingress` 拒绝分布在两台机器上的配置。在该配置尚未修正而界面仍需渲染时，
  // 取 id 最小的一个，与编译器的判定一致；取数组第一个会使该页面随接口返回顺序变化，
  // 而编译器的结果是稳定的。
  const head = app.ingresses
    .filter(i => i.chain === chainId)
    .reduce<SnapshotIngress | null>((min, i) => (!min || i.id < min.id ? i : min), null);
  return head?.node ?? null;
}

export function chainSpine(app: SnapshotApp, chainId: string): string[] {
  const head = chainHead(app, chainId);
  if (!head) return [];
  const byNode = new Map(app.steps.filter(s => s.chain === chainId).map(s => [s.node, s]));
  const path = [head];
  const seen = new Set([head]);
  let cur = head;
  while (true) {
    /* 规则表有序，第一条匹配的「任意 → 转发」即为兜底规则 */
    const anyForward = byNode.get(cur)?.rules.find((r): boolean => r.m.t === 'any' && r.a.t === 'forward');
    const next = anyForward?.a.t === 'forward' ? anyForward.a.to : undefined;
    if (!next || seen.has(next)) break;
    path.push(next);
    seen.add(next);
    cur = next;
  }
  return path;
}

// 该链包含的机器——从链头沿 Forward 可达的全部机器，不限于主干路径。
// 与编译器的 `chain_members` 判定一致（ir/routing.rs）：链的成员没有独立声明，
// 规则 Forward 指向的机器即为成员，分叉的机器同样计入。
//
// `chainSpine` 是其中的一条子路径，只表示主干包含哪些机器及其顺序。将 spine 作为
// 成员集合会遗漏所有分叉——表现为分叉上有机器退役时界面显示该链正常，而编译器
// 已将整条链判定为停用（`disabled_chains` 使用成员而非主干）。
export function chainMembers(app: SnapshotApp, chainId: string): string[] {
  const head = chainHead(app, chainId);
  if (!head) return [];
  const byNode = new Map(app.steps.filter(s => s.chain === chainId).map(s => [s.node, s]));
  const seen = new Set([head]);
  const queue = [head];
  while (queue.length > 0) {
    const at = queue.shift()!;
    for (const rule of byNode.get(at)?.rules ?? []) {
      if (rule.a.t !== 'forward' || seen.has(rule.a.to)) continue;
      seen.add(rule.a.to);
      queue.push(rule.a.to);
    }
  }
  return [...seen];
}

export interface SnapshotStep {
  chain: string;
  node: string;
  accept: StepAccept | null;
  // 该链在该机器上的中转端口。密钥已在服务端脱敏（`redacted_value`），
  // 因此 security 中 REALITY 的 private_key 为 `<redacted>`。
  hop_in: { port: number; security: HopWireSnapshot } | null;
  rules: Rule[];
}

// 快照中的传输层配置。与 `HopWireRequest` 不同：请求只指定使用哪一种，
// 该类型是存储后的实际配置（密钥已脱敏）。
export type HopWireSnapshot =
  | { t: 'none' }
  | { t: 'encryption'; v: { public_key: string; private_key?: string } }
  | {
      t: 'reality';
      v: { dest: string; server_names: string[]; fingerprint?: string; public_key: string };
    }
  /* psk 对只读操作员完全移除：对称密钥没有公钥部分，可见即等同于可用 */
  | { t: 'shadowsocks2022'; v: { psk?: string } };
// 控制台中称其为项目。
// `app` 这一字段名是准确的——它与 Project 属同一类概念：一个有名称、有边界、有归属的
// 对象，链和接入面因归属于它而被归为一组。中文界面使用「项目」，两者指同一对象。
//
// 注意：不要与 `AppIr` 中的 App 混淆：后者指应用层（与系统层相对的分层，见 ir.md 开篇），
// 同一个词在本代码库中指代两种不同的概念。
export interface SnapshotApp {
  id: string;
  label: string;
  chains: SnapshotChain[];
  steps: SnapshotStep[];
  ingresses: SnapshotIngress[];
  fronts: SnapshotFront[];
  grants: { tenant: string; user: string; ingress: string }[];
}

export interface SnapshotFront {
  id: string;
  tenant: string;
  name: string;
  strategy: 'url-test' | 'select' | 'fallback';
  via: string[];
  external_via: string[];
}

export const upsertFront = async (
  appId: string,
  front: {
    id: string;
    tenant_id: string;
    name: string;
    strategy: SnapshotFront['strategy'];
    via: string[];
    external_via: string[];
  },
) => {
  draft.push({ op: 'upsert_front', app_id: appId, front });
  return { revision_id: 0 } as ModelWriteResult;
};
export interface ConsoleSnapshot {
  /* 服务端返回完整的 ModelSnapshot，此处只声明需要使用的部分——完整声明相当于在浏览器中
     维护第二份模型定义，最终会与 model.rs 产生差异。 */
  snapshot: {
    revision: number;
    apps: SnapshotApp[];
    external_outbounds?: ExternalOutbound[];
    /* 机器的模型字段。产物相关的字段（overlay / egress / dns 等）各页面有各自的数据来源，
       此处只声明连接策略：它没有其他支持草稿的读取方式。 */
    nodes?: { id: string; overlay?: boolean; certificate_name?: string | null; connection?: NodeConnection }[];
    settings?: { connection?: ConnectionSettings };
  };
  /* DNS 策略由机器持有，存在即下发，不由链路 Egress 规则启用。 */
  node_egress_dns: { node: string; position: number; selector: DestMatch; resolution: EgressDnsResolution }[];
  redacted: boolean;
}

// 存在草稿时读取的是草稿全部生效后的结果。所有面板统一使用该函数，不需自行判断是否
// 存在草稿——修改后界面未更新是草稿机制下最常见的问题。
export const fetchSnapshot = (): Promise<ConsoleSnapshot> =>
  draft.isEmpty() ? api<ConsoleSnapshot>('/model/snapshot') : draftPreview().then(p => p.snapshot);

/* ── 操作者 ── */

export interface AdminOperator {
  id: string;
  display_name: string;
  role: AdminRole;
  tenant_scope: string | null;
  /* 只有固定的 public readonly 账号允许免密。 */
  passwordless: boolean;
  token_prefix: string | null;
  token_created_at: string | null;
  token_last_used_at: string | null;
  token_revoked_at: string | null;
}

export const fetchOperators = () => api<{ operators: AdminOperator[] }>('/admin/operators');

export const createOperator = (body: {
  id: string;
  display_name: string;
  role: AdminRole;
  tenant_scope: string | null;
  /* 只有 id=public 且 role=readonly 时可留空；其他账号必须设置密码。 */
  password?: string;
}) => post<AdminOperator>('/admin/operators', body);

export interface ResetPasswordResult {
  operator_id: string;
  password: string;
  sessions_revoked: number;
}

export const resetOperatorPassword = (id: string) =>
  post<ResetPasswordResult>(`/admin/operators/${encodeURIComponent(id)}/password`);

export const changeMyPassword = (body: { current_password: string; new_password: string }) =>
  post<{ operator_id: string; sessions_revoked: number }>('/admin/password', body);

export const issueOperatorToken = (id: string) =>
  post<IssuedAdminToken>(`/admin/operators/${encodeURIComponent(id)}/token`);

export const revokeOperatorToken = (id: string) =>
  req<{ operator_id: string; revoked: boolean }>(`/admin/operators/${encodeURIComponent(id)}/token`, {
    method: 'DELETE',
  });

/* ── 用量 ── */

export interface UsageSample {
  id: number;
  sampled_at: string;
  window_start: string;
  window_end: string;
  node_id: string;
  tenant_id: string;
  user_id: string;
  ingress_id: string;
  grant_label: string;
  uplink_bytes: number;
  downlink_bytes: number;
  has_gap: boolean;
  revision_id: number | null;
  deployment_id: number | null;
}

// 中继链上某一跳的用量。它没有 user 维度——该跳的凭据是 `{chain}@{node}`，
// 不区分用户。它表示运营者的带宽成本而非用户账单，
// 因此使用独立的类型和列表，不计入用户用量。
export interface UsageChainSample {
  id: number;
  sampled_at: string;
  window_start: string;
  window_end: string;
  node_id: string;
  tenant_id: string;
  app_id: string;
  chain_id: string;
  hop_label: string;
  uplink_bytes: number;
  downlink_bytes: number;
  has_gap: boolean;
  revision_id: number | null;
  deployment_id: number | null;
}

export const fetchUsage = (filter: {
  tenant_id?: string;
  user_id?: string;
  node_id?: string;
  // 拓扑需要按「链 × 接收方」聚合，100 行只能覆盖几分钟。该接口仍是明细接口、
  // 仍有服务端上限，因此返回的始终是最近一段时间而非整月数据。
  limit?: string;
}) => {
  const q = new URLSearchParams({ limit: '100' });
  for (const [k, v] of Object.entries(filter)) if (v) q.set(k, v);
  return api<{ samples: UsageSample[]; chain_samples?: UsageChainSample[] }>(`/usage/samples?${q}`);
};

// 自然月汇总：一行对应一个（用户 × 项目）组合。项目带有 label（如「日本 rfc 入口」），
// 一行即该项目下所有接入点对该用户当月的合计。
// 月份字符串使用 +08 本地时间（如「2026-08-01 00:00:00」），不随数据库会话时区变化。
export interface UsageMonthlyViewRow {
  tenant_id: string;
  user_id: string;
  app_id: string;
  uplink_bytes: number;
  downlink_bytes: number;
  has_gap: boolean;
}

export interface UsageMonthlySummary {
  month_start: string;
  month_end: string;
  views: UsageMonthlyViewRow[];
}

export const fetchUsageMonthly = () => api<UsageMonthlySummary>('/usage/monthly-summary');

// 机器列表右端的柱状图：一格对应一个 USAGE 上报窗口（30s），服务端已按机器聚合。
// 不要改回使用 /usage/samples 自行汇总——那是明细行，一台机器有数十个用户即会超出其 500 行上限。
// 用户流量和中继流量分为两组：入口机器的字节归属于具体用户，中转机器的字节归属于
// 链路跳、不区分用户。计算该机器的总承载量需要将两组相加——入口和中转只是
// 同一台机器在不同链上的角色（每一跳恰好计一次）。
export interface UsageNodeBucket {
  window_end: string;
  user_uplink_bytes: number;
  user_downlink_bytes: number;
  relay_uplink_bytes: number;
  relay_downlink_bytes: number;
}

export interface UsageNodeSeries {
  node_id: string;
  buckets: UsageNodeBucket[];
  month_user_uplink_bytes: number;
  month_user_downlink_bytes: number;
  month_relay_uplink_bytes: number;
  month_relay_downlink_bytes: number;
  month_has_gap: boolean;
}

/** 一个窗口内该机器转发的全部字节：用户流量加中继流量 */
export const bucketBytes = (b: UsageNodeBucket) =>
  b.user_uplink_bytes + b.user_downlink_bytes + b.relay_uplink_bytes + b.relay_downlink_bytes;

/** 该机器本月转发的全部字节 */
export const monthBytes = (s: UsageNodeSeries) =>
  s.month_user_uplink_bytes + s.month_user_downlink_bytes + s.month_relay_uplink_bytes + s.month_relay_downlink_bytes;

export interface UsageNodeSeriesList {
  since: string;
  month_start: string;
  nodes: UsageNodeSeries[];
}

export const fetchUsageNodeSeries = (windowSecs = 1800, nodeId?: string) => {
  const node = nodeId ? `&node_id=${encodeURIComponent(nodeId)}` : '';
  return api<UsageNodeSeriesList>(`/usage/node-series?window_secs=${windowSecs}${node}`);
};

/* ── 设置 ── */

export interface BrandingSettings {
  site_name: string;
  /** PNG、JPEG 或 WebP 的 data URL；null 使用内置织格图标。 */
  icon_data_url: string | null;
}

export const DEFAULT_BRANDING: BrandingSettings = { site_name: 'Brocade', icon_data_url: null };
export const fetchBranding = () => api<BrandingSettings>('/branding');
export const saveBranding = (body: BrandingSettings) =>
  api<BrandingSettings>('/branding', '', { method: 'PUT', body: JSON.stringify(body) });

export interface PingProbeTarget {
  name: string;
  /** 同时作为序列标识；URI 选择 TCP Connect 或 ICMP Echo。 */
  address: string;
}

export interface PingProbeSettings {
  targets: PingProbeTarget[];
  interval_secs: number;
  timeout_ms: number;
}

export const fetchPingProbeSettings = () => api<PingProbeSettings>('/ping-probe/settings');
export const savePingProbeSettings = (body: PingProbeSettings) =>
  api<PingProbeSettings>('/ping-probe/settings', '', { method: 'PUT', body: JSON.stringify(body) });

export interface ModelSettings {
  reality_client: {
    min_client_ver: string | null;
    max_client_ver: string | null;
    max_time_diff_ms: number | null;
  };
  /* REALITY 借用的站点：全局设置一次，接入面未填写时使用该值 */
  reality_site: {
    dest: string | null;
    server_names: string[];
    fingerprint: string | null;
    /* XTLS 流控。新建库的默认值为 'xtls-rprx-vision'；null 表示运营者已显式关闭 */
    flow: string | null;
  };
  /* overlay 链路参数。设为全局配置是因为链路由全互联规则计算得出，不逐条配置 */
  overlay: {
    /* 只在单向连接（一端不可被直接连接）时写入 wg 配置 */
    keepalive_secs: number;
    mtu: number;
  };
  // 自动分配端口的起始值。只影响新建时的默认值——已写入模型的端口不受影响，
  // 修改已有端口会导致 xray 配置变更、进程重启、该机器上所有连接中断。
  probe: {
    // 端到端探测的目标地址。要求返回纯文本且包含 `ip=` 一行——出口核对依据该行。
    // 使用明文 HTTP 是有意的：测量对象是链路本身，不应包含目标站点的 TLS 握手时间。
    endpoint_url: string;
    timeout_secs: number;
    interval_secs: number;
  };
  ports: {
    ingress_base: number;
    hop_base: number;
    hy2_base: number;
  };
  // geoip.dat / geosite.dat 的自动更新。不提供开关——规则表中的 `geosite:` /
  // `geoip:` 匹配依赖这两个文件，文件过期不会报错，而是导致规则匹配失败且无提示。
  // 只提供 cron 和两个 URL：落地文件名固定（`geosite:` 在 xray 中被改写为
  // `ext:geosite.dat:`），而 URL 必须可配置——xray 下载 .dat 时既不验签也不校验哈希，
  // 完全信任配置的地址，因此需要支持指向自建镜像。
  geodata: {
    cron: string;
    geoip_url: string;
    geosite_url: string;
  };
  /* 连接的存活时长和内存占用。此处是默认值，机器可逐字段覆盖（Node.connection），
     机制与 overlay.mtu / Node.mtu 相同。 */
  connection: ConnectionSettings;
  /* 使 xray 统计每个账号当前有多少个不同的来源地址在使用。
     设为全局而非逐机器配置：是否统计共享账号是机队级的决定，而上面一组是各机器
     各自的容量参数。它只做统计，xray 不会因统计值高而拒绝连接。 */
  stats_user_online: boolean;
}

export interface ConnectionSettings {
  /* 无数据多久后回收。中转的内存主要消耗在此：空闲连接累积，每条占用一个缓冲区 */
  conn_idle_secs: number;
  /* 半关闭后的等待时长。0 是有效取值，表示对端关闭后立即关闭 */
  uplink_only_secs: number;
  downlink_only_secs: number;
  /* 每条连接的缓冲区大小。null 表示产物中不写入该键，由 xray 按 CPU 架构决定
     （x86_64 为 512 KB、arm64 为 4 KB、arm/mips 为 0）。填写具体数值会使不同架构使用同一取值。 */
  buffer_size_kb: number | null;
  /* 握手超时。全机队使用同一取值，不支持逐机器覆盖：xray 取 60 是为对齐 nginx 的
     client_header_timeout，使该值不暴露后端服务类型；各机器分别设置会使机器之间
     可通过该值区分。 */
  handshake_secs: number;
}

/* 单台机器覆盖的配置项。null 表示使用全局默认值。
   不含 handshake_secs，原因见上。 */
export interface NodeConnection {
  conn_idle_secs: number | null;
  uplink_only_secs: number | null;
  downlink_only_secs: number | null;
  buffer_size_kb: number | null;
}

export const fetchSettings = () => api<ModelSettings>('/settings');
export const saveSettings = async (body: ModelSettings) => {
  draft.push({ op: 'update_settings', settings: body });
  return { revision_id: 0 };
};

/* ── 分发设置：节点访问控制面的地址，以及安装哪个版本的 xray ──
 *
 * 与上面一组分开，因为两者性质不同：模型设置会编入产物，修改一次产生一个修订并需要一次发布；
 * 这两项不进入任何产物，只决定安装命令中的地址和 /enroll/dist 清单，在安装时被读取。
 * 因此它不进入草稿而是直接 PUT——上面的 saveSettings 写入草稿而此处直接写库，
 * 该差异在客户端同样可见。
 *
 * `stored` 是在此处填写的值，`effective` 是实际生效的值（已填写时即为该值，未填写时
 * 回退到进程启动时的环境变量，再回退到内置默认值）。两者都需要：仍使用环境变量的部署中
 * stored 为空，只显示 stored 会呈现为两个空输入框，而实际配置正在生效。 */
export interface DistributionSettings {
  agent_public_url: string | null;
  xray_version: string | null;
}

export interface DistributionView {
  stored: DistributionSettings;
  effective: DistributionSettings;
}

export const fetchDistribution = () => api<DistributionView>('/distribution');
export const saveDistribution = (body: DistributionSettings) =>
  api<DistributionView>('/distribution', '', { method: 'PUT', body: JSON.stringify(body) });

/* ── Agent 日志上限：运行时策略，不进入修订，也不需要发布 ── */
export interface AgentLogPolicyNode {
  node_id: string;
  tenant_id: string;
  name: string;
  /* null 持续继承全局值；不是把当时的全局数字复制到机器上。 */
  override_max_mib: number | null;
  effective_max_mib: number;
}

export interface AgentLogPolicyView {
  global_max_mib: number;
  nodes: AgentLogPolicyNode[];
}

export const fetchAgentLogPolicy = () => api<AgentLogPolicyView>('/agent-log-policy');
export const saveAgentLogDefault = (maxMib: number) =>
  api<AgentLogPolicyView>('/agent-log-policy', '', {
    method: 'PUT',
    body: JSON.stringify({ max_mib: maxMib }),
  });
export const saveNodeLogPolicy = (nodeId: string, maxMib: number | null) =>
  api<AgentLogPolicyView>(`/agent-log-policy/nodes/${encodeURIComponent(nodeId)}`, '', {
    method: 'PUT',
    body: JSON.stringify({ max_mib: maxMib }),
  });

/* ── agent 发布 ──
 *
 * 与分发一样不产生修订、不需要发布，但作用对象不同：机队上运行的 agent 二进制本身。
 *
 * `release_id` 标识的是一次**构建**而非版本号——控制面在编译期将两个架构的 agent 嵌入自身，
 * 该 id 即这两个 sha256 的哈希。控制面只能识别自身携带的那一批，因此重新部署控制面后，
 * 原先批准的 id 不再对应任何可下发的内容，机队保持当前状态等待再次批准。
 * 若改为自动升级开关，每次部署控制面都会同时替换每台机器上的 agent。
 *
 * 因此此处无法表达「回滚整个机队」，这与实际能力一致：控制面只持有自身编译的那批字节。
 * 需要回退到上一版本时重新部署上一版控制面，其中包含上一版 agent。 */
export type AgentReleaseScope = 'off' | 'nodes' | 'all';

export interface AgentRelease {
  release_id: string | null;
  scope: AgentReleaseScope;
  /** 只在 scope 为 `nodes` 时有效。切换后返回时不丢失，因此另外两档下保持原值。 */
  nodes: string[];
  /** 本次发布的说明。下面四项中唯一由人填写的——构建号标识的是字节内容，不说明原因。 */
  note: string | null;
  /* 下列四项由服务端在批准时记录，客户端传入的值无效：若 `released_by` 可由请求设置，
     即相当于可以用他人身份记录一次全机队二进制替换。
     记录的是**批准时**的构建信息而非当前进程的信息——控制面重新部署后，进程描述的是它当前
     携带的那一批，而这几个字段表示的是批准时的那一批。 */
  version: string | null;
  commit: string | null;
  released_at: string | null;
  released_by: string | null;
}

export interface AgentBuild {
  arch: string;
  sha256: string;
}

export interface AgentReleaseView {
  released: AgentRelease;
  /** 该控制面**当前可下发**的那一批。与 released.release_id 不一致时不会执行升级。 */
  available_release_id: string;
  available_agents: AgentBuild[];
  /** 控制面和 agent 分别上报，尽管当前两者使用同一个 workspace 版本号：它们表示两个对象——
      当前通信的进程，以及它将安装到机器上的二进制。阅读者不应需要先了解两者由同一次构建
      产生才能理解本页内容。 */
  console_version: string;
  agent_version: string;
  /** 控制面构建对应的 commit。不在 git 仓库中时为 `unknown`，工作区有未提交改动时附加
      `-改动未提交`。它描述的不是 agent 的字节内容——将 commit 编入 agent 会使每次文档提交
      都产生一个新的 agent 构建。 */
  build_commit: string;
}

export const fetchAgentRelease = () => api<AgentReleaseView>('/agent-release');
export const saveAgentRelease = (body: AgentRelease) =>
  api<AgentReleaseView>('/agent-release', '', { method: 'PUT', body: JSON.stringify(body) });

/* ── 证书 ──
 *
 * 与分发、agent 发布同类：不产生修订、不需要发布。但它多一项机制——后台有签发 worker，
 * 因此除读写外还有第三个操作 `scanCerts`，表示立即执行一轮，而不是在请求中完成签发。
 *
 * 证书由**控制面**签发、入库，并随下发包送达节点。曾考虑由节点自行签发、私钥不离开机器，
 * 评估后未采用：控制面已持有 DNS 凭据，控制面被攻破时攻击者本就可以为任意名称另行签发证书、
 * 冒充任何节点；节点自签只能防御「库被单独窃取而凭据未被窃取」这一种情形，
 * 却需要在 agent 中实现一套 ACME、每台一个账号，还需开放节点请求控制面写 DNS 的接口。
 *
 * 每台机器一张独立的通配证书，名称为 `*.<随机标签>.<域名>` 加上裸名两个——通配符只覆盖一层
 * 且不覆盖自身，因此两个都需要。标签使用随机值不是因为它是机密（每张公共信任证书都会进入
 * Certificate Transparency 公开日志，名称本身是公开的），而是为了避免该名称**描述**
 * 该机器的用途。 */
export interface CertDomain {
  id: string;
  domain: string;
  dns_provider: string;
  acme_directory: string;
  acme_contact: string | null;
  /** 提前多少天续期。证书有效期 90 天，预留 30 天意味着有 30 天的重试窗口。 */
  renew_before_days: number;
  /** 是否已存储 DNS 凭据——**不包含凭据本身**。页面需要能显示「已配置」但无法读取其内容。 */
  has_credential: boolean;
  /** 是否已在该 ACME 目录下注册账号。首次签发之前为否属于正常状态。 */
  has_account: boolean;
}

/** 可写入的部分。凭据是只写的：不填写表示保持原值，使表单可以在无法读取机密的情况下保存。 */
export interface CertDomainInput {
  domain: string;
  dns_credential?: string | null;
  acme_directory?: string | null;
  acme_contact?: string | null;
  renew_before_days?: number | null;
}

/** 一张证书在组内的位置。
    `pending` 尚未签发；`ready` 已签好、待命；`serving` 该组机器当前出示的这张；
    `superseded` 曾经出示、已被换下；`failed` 签发失败，该行留着记录原因。 */
export type CertStatus = 'pending' | 'ready' | 'serving' | 'superseded' | 'failed';

/** 这张证书为什么存在，决定它签好之后是否立即接管。
    `renewal`：扫描发现当前这张快到期而自动排的，签好即接管——留着等人点，就是证书过期而替代品
    躺在库里的那条路径。`spare`：手动加签的备用，停在 `ready` 等人选时机，这正是提前要一张的意义。 */
export type CertOrigin = 'renewal' | 'spare';

export interface GroupCertificate {
  id: string;
  status: CertStatus;
  origin: CertOrigin;
  /** 签发者的 CN，从证书**字节中**解析得出，不是根据设置推导。两者可能不一致，而只有前者
      反映实际情况——若机队几个月前设为 staging 且未改回，每张都显示已签发但没有任何客户端
      信任它，该字段是唯一能反映该问题的位置。Let's Encrypt 的 staging 名称刻意使用明显的
      标识（根证书为 `(STAGING) Pretend Pear X1`），正是用于此场景。 */
  issuer: string | null;
  issued_at: string | null;
  expires_at: string | null;
  /** 证书字节的 sha256，与机器上报的值比对后得出该机器持有的是不是这一张。 */
  sha256: string | null;
  attempts: number;
  last_error: string | null;
  last_attempt_at: string | null;
}

/** 一个证书组：一个共享的 SNI 身份。
    机器加入组，证书在组内滚动，组的 `label` 就是这些机器出示的 SNI。组内换证书不改 SNI，
    已发出去的订阅继续可用；换组才会改 SNI。 */
export interface CertGroup {
  id: string;
  domain: string;
  /** 随机十六进制，进证书名，给 TLS 看。 */
  label: string;
  /** operator 起的名字，给人看，域内唯一。 */
  name: string;
  note: string | null;
  status: 'active' | 'draining' | 'retired';
  /** 证书中的两个名称：通配名和裸名。 */
  names: string[];
  /** 从这个组取证书的机器。 */
  nodes: string[];
  /** 组内全部证书，serving 在前。 */
  certificates: GroupCertificate[];
}

/** 一台机器实际持有的证书状态。
    按机器而非按证书，因为同组十台机器共用一张证书却有十个独立的答案——轮换过程中这个分歧
    正是要看的东西。 */
export interface NodeCertificateState {
  node_id: string;
  label_id: string;
  group_name: string;
  /** 裸名，也就是这台机器的订阅里写的 sni。 */
  certificate_name: string;
  /** `unknown` 从未上报（agent 版本过旧，不管理证书）、`absent` 已检查且不存在、
      `current` 与该组正在出示的一致、`stale` 有证书但不是这一张（轮换后一小时内属正常，
      xray 按自己的周期热重载）。
      没有它时页面显示的是控制面**已签发的内容**，而不是机器上**实际存在的内容**——
      写盘失败、文件被覆盖、agent 版本过旧，三种情况的显示与正常状态相同。 */
  on_disk: 'unknown' | 'absent' | 'current' | 'stale';
  observed_at: string | null;
}

export interface CertsView {
  /** 该控制面是否具备加密存储能力（是否配置了 `BROCADE_SECRET_KEY`）。页面需要在**输入之前**
      即提示该控制面无法存储凭据，而不是在保存失败后提示。它是进程配置而非数据，没有其他接口
      可以查询。 */
  sealing_available: boolean;
  domain: CertDomain | null;
  groups: CertGroup[];
  nodes: NodeCertificateState[];
  letsencrypt: string;
  letsencrypt_staging: string;
}

export const fetchCerts = () => api<CertsView>('/certs');
export const saveCertDomain = (body: CertDomainInput) =>
  api<CertsView>('/certs/domain', '', { method: 'PUT', body: JSON.stringify(body) });
/** 触发 worker 立即执行一轮。返回的是当前状态而非本轮结果——一轮中每台约需半分钟，
    保持请求等待会使页面依赖一个不确定的时长。页面通过轮询获取进展，具体状态显示在行内。 */
export const scanCerts = () => api<CertsView>('/certs/scan', '', { method: 'POST' });

export const createCertGroup = (body: { name: string; note?: string | null }) =>
  api<{ id: string }>('/certs/groups', '', { method: 'POST', body: JSON.stringify(body) });
export const updateCertGroup = (id: string, body: { name?: string; note?: string | null }) =>
  api<void>(`/certs/groups/${encodeURIComponent(id)}`, '', {
    method: 'PUT',
    body: JSON.stringify(body),
  });
export const deleteCertGroup = (id: string) =>
  api<void>(`/certs/groups/${encodeURIComponent(id)}`, '', { method: 'DELETE' });
/** 给这个组多签一张备用。它停在 `ready`，由人决定何时启用——自动续期那张不经过这里。 */
export const requestSpareCertificate = (id: string) =>
  api<{ id: string }>(`/certs/groups/${encodeURIComponent(id)}/spare`, '', { method: 'POST' });
/** 把一张待命的证书变成该组机器出示的那张。SNI 不变，只换字节，因此不需要发布。 */
export const serveCertificate = (certId: string) =>
  api<void>(`/certs/certificates/${encodeURIComponent(certId)}/serve`, '', { method: 'POST' });
/** 改一台机器所属的证书组，`null` 表示不属于任何组。
    这会改变该机器的 SNI：已发出去的订阅里写的是旧组的名字，改完就连不上，需要重新拉取。 */
export const setNodeCertGroup = (nodeId: string, labelId: string | null) =>
  api<void>(`/nodes/${encodeURIComponent(nodeId)}/cert-group`, '', {
    method: 'PUT',
    body: JSON.stringify({ label_id: labelId }),
  });

/* ── 链路探测：wg MTU ── */

export interface LinkMtuItem {
  node_id: string;
  peer_node_id: string;
  endpoint_host: string;
  status: 'ok' | 'unreachable' | 'blocked' | 'unsupported';
  path_mtu: number | null;
  suggested_wg_mtu: number | null;
  probed_at: string;
}

export interface NodeMtuItem {
  node_id: string;
  /* 生效值：单独设置时为该值，未设置时为全局默认值 */
  current_mtu: number;
  overridden: boolean;
  suggested_mtu: number | null;
  /* 决定该建议值的对端（路径 MTU 最小的一条） */
  tightest_peer: string | null;
  inconclusive: number;
}

export interface LinkMtuView {
  /* 每一对的原始探测结果：路径 MTU 是路径的属性 */
  links: LinkMtuItem[];
  /* 每台机器的当前值和建议值：该设置是节点级的，一个 wg0 对应一个 MTU */
  nodes: NodeMtuItem[];
  default_mtu: number;
}

export const fetchLinkMtu = (token = '') => api<LinkMtuView>('/links/mtu', token);

// 中继跳的连通状态。判定依据是 outbound 计数器的增量——产物中的 observatory 每 10 秒
// 通过每个转发出口探测一次，因此该跳连通时 downlink 会持续增长。
//
// 这是链路层面的事实，不是控制面层面的事实。配置是否已下发在节点的「已应用」字段中，
// 两者必须分别查看：配置已下发不代表链路连通，链路连通也不代表配置是最新的。
export interface LinkHealthItem {
  node_id: string;
  chain_id: string;
  peer_node_id: string;
  alive: boolean;
  downlink_bytes: number;
  /* 两次读数之间的时间间隔。窗口过短时零增量不可靠——探测结果可能尚未计入计数器 */
  window_secs: number;
  checked_at: string;
}

export const fetchLinkHealth = (token = '') => api<{ hops: LinkHealthItem[] }>('/links/health', token);

// 端到端探测：一条链的整条数据面当前是否连通。
//
// 与上面两类的分工——`LinkMtuItem` 表示两台机器之间 underlay 链路的路径 MTU，
// `LinkHealthItem` 表示到下一跳的计数器是否仍在增长，本类表示
// 用户从入口接入后能否穿过整条链出网。
//
// 前两者无法覆盖该场景的原因：REALITY 参数配置错误、路由规则遗漏、出口被封禁——
// 这三种情况都不会使任何一跳的计数器停止增长，但用户已经无法使用。
//
// `exit_verdict` 的三个档位是本类存在的主要原因。「连通但出口 IP 不符」表示流量
// 未穿过完整的链（通常从链头直接出网），而规则表合法、每跳均连通、编译无警告——
// 静态校验无法发现该情况。因此它既不属于成功也不属于失败，必须作为独立档位。
export type E2eProbeStatus = 'ok' | 'handshake-failed' | 'chain-broken' | 'timeout' | 'unsupported';
export type E2eExitVerdict = 'match' | 'mismatch' | 'unknown';

export interface E2eProbeSample {
  probed_at: string;
  status: E2eProbeStatus;
  ttfb_ms: number | null;
}

export interface E2eProbeItem {
  app_id: string;
  chain_id: string;
  chain_name: string;
  /* 执行探测的机器。始终是链头——入口只存在于该机器上，其他位置无法连接用户使用的端口 */
  node_id: string;
  status: E2eProbeStatus;
  /* 首字节时间。只有连通时才有取值——失败时的耗时等于超时值，与链路速度无关 */
  ttfb_ms: number | null;
  exit_ip: string | null;
  /* 探测目标返回的国家码。无法核对 IP 时，它可以表明出网的国家 */
  exit_loc: string | null;
  exit_verdict: E2eExitVerdict;
  detail: string | null;
  probed_at: string;
  /* 最近 6 小时结果，按时间正序（旧到新）排列；前端按 probed_at 放到真实时间轴上。 */
  samples: E2eProbeSample[];
}

export const fetchE2eProbes = (token = '') => api<{ chains: E2eProbeItem[] }>('/probes/e2e', token);

/* ── 节点：更新与 token ── */

// 中转端口的传输层。只指定使用哪一种，密钥由服务端生成——控制台不接触私钥。
// 类型不变时服务端保留原有密钥，因此修改 REALITY 的站点不会中断链路。
export type HopWireRequest =
  | { t: 'none' }
  | { t: 'encryption' }
  | { t: 'reality'; v: { dest: string; server_names: string[]; fingerprint?: string | null } }
  /* 密钥由服务端生成，因此请求中不含 v——与 encryption 一样只指定使用哪一种 */
  | { t: 'shadowsocks2022' };

export const updateNode = (
  id: string,
  body: {
    tenant_id?: string;
    name?: string;
    // 空串表示清空；字段不出现表示不修改。类型中不包含 null 是有意的：服务端将该字段读为
    // `Option<String>`，JSON 的 `null` 和字段缺失在服务端都是 `None`，无法区分，
    // 因此清空只能用空串表示（对应 `update_node` 中的 `public_ipv4.is_some()` 判断）。
    // 若保留 `| null`，`x || null` 可以通过类型检查但运行时不产生任何效果——
    // 保存后界面无变化且不报错。
    public_ipv4?: string;
    public_ipv6?: string;
    public_ipv4_nat?: boolean;
    public_ipv6_nat?: boolean;
    wg_listen_port?: number;
    api_port?: number | null;
    overlay?: boolean;
    egress_allowed?: boolean;
    dns?: Dns;
    domain_strategy?: DomainStrategy;
    /* 0 表示清空并回退到全局默认值；字段不出现表示不修改 */
    mtu?: number;
    /* 四项一并提交。字段不出现表示本次不涉及连接策略；出现时以其中的四项为准，
       其中的 null 表示该项回退到全局默认值。
       不像 mtu 那样用 0 表示清空：0 在此是合法的缓冲区大小（xray 解释为不缓冲），
       不能复用。 */
    connection?: NodeConnection;
    /* 外部连接该机器 wg 端口的方式。TCP 封装只在上游封禁入站 UDP 时使用。 */
    wg_transport?: { t: 'udp' } | { t: 'fake_tcp'; v: { port: number } };
  },
) => {
  draft.push({ op: 'update_node', node_id: id, node: body });
  return Promise.resolve({} as unknown);
};

/* 重签会立即使机器上的旧 token 失效，因此响应中包含一条可将新 token 写入机器的完整命令 */
export const issueNodeToken = (id: string) =>
  post<{ node_id: string; token: string; token_prefix: string; install_command: string }>(
    `/nodes/${encodeURIComponent(id)}/agent-token`,
  );

export const revokeNodeToken = (id: string) =>
  req<{ node_id: string; revoked: boolean }>(`/nodes/${encodeURIComponent(id)}/agent-token`, {
    method: 'DELETE',
  });

/* 验证接入：执行一次不做任何修改的空收敛，确认该机器是否已收敛到当前模型 */
export const verifyDeployment = (body: { revision_id?: number; node_id?: string }) =>
  post<{
    revision_id: number;
    converged: boolean;
    summary: PlanSummary;
    targets: PlannedTarget[];
  }>('/deployments/verify', body);

/* ── 产物 ── */

export interface ArtifactIndexEntry {
  target_kind: string;
  target_id: string;
  artifact_kind: string;
  state: string;
  sha256: string | null;
  byte_len: number | null;
}

// 传入 revision 用于与上一版本比较：索引中包含每份产物的 sha256，
// 比较两个版本的索引即可确定哪些机器会发生变更，无需拉取内容。
export const fetchArtifactIndex = (revision?: number) =>
  api<{ revision: number; artifacts: ArtifactIndexEntry[] }>(
    revision == null ? '/artifacts/index' : `/artifacts/index?revision=${revision}`,
  );

// 当前版本的产物索引。存在草稿时返回草稿的索引——产物栏不随草稿更新时，
// 三栏中最右一栏显示的内容与实际不符：模型已修改，而它仍显示上次提交的配置。
export const fetchArtifactIndexView = (revision?: number) =>
  draft.isEmpty() ? fetchArtifactIndex(revision) : draftPreview().then(p => p.artifacts);

export interface ArtifactContent {
  revision: number;
  target_kind: string;
  target_id: string;
  artifact_kind: string;
  state: string;
  sha256: string | null;
  byte_len: number | null;
  content: string | null;
  redacted: boolean;
}

// 内容同样随草稿变化。只有当前版本使用草稿：diff 的另一侧是已提交的修订，
// 该侧必须按修订号获取，否则两侧都是草稿，比较结果始终为无差异。
export const fetchArtifactContentView = (
  targetKind: string,
  targetId: string,
  artifactKind: string,
  revision?: number,
) =>
  draft.isEmpty()
    ? fetchArtifactContent(targetKind, targetId, artifactKind, revision)
    : previewDraftArtifact(draft.ops(), targetKind, targetId, artifactKind);

// `family` 与 `protocol` 只对用户订阅（uri / clash）有效：可以按地址族、接入协议或二者
// 交集收窄。不传表示全量，与机队实际提供的内容一致。过滤在服务端执行——Clash 的
// proxy-groups 按代理名引用成员，在浏览器端按行删除会产生引用不存在代理的组，mihomo 会
// 拒绝导入。
export type ArtifactFamily = 'v4' | 'v6';
export type ArtifactProtocol = 'vless' | 'anytls' | 'hysteria2';

export const fetchArtifactContent = (
  targetKind: string,
  targetId: string,
  artifactKind: string,
  revision?: number,
  family?: ArtifactFamily,
  protocol?: ArtifactProtocol,
  serving = false,
) => {
  const query = [
    revision == null ? null : `revision=${revision}`,
    family == null ? null : `family=${family}`,
    protocol == null ? null : `protocol=${protocol}`,
    serving ? 'serving=true' : null,
  ].filter(Boolean);
  return api<ArtifactContent>(
    `/artifacts/content/${encodeURIComponent(targetKind)}/${encodeURIComponent(targetId)}/${encodeURIComponent(artifactKind)}` +
      (query.length === 0 ? '' : `?${query.join('&')}`),
  );
};

// ── 遥测：主机负载 + 逐跳链路质量 ────────────────────────────────
//
// 与上面三类探测的分工：`LinkMtuItem` 表示两台机器之间链路的路径 MTU，`LinkHealthItem`
// 表示到下一跳的计数器是否仍在增长，`E2eProbeItem` 表示用户能否穿过完整的链——三者都是
// **主动探测**，各自发送探测包。
//
// 本类不发送任何探测包：BBR 在每条实际转发连接上、每个 RTT 都会更新其对瓶颈带宽和最小 RTT
// 的估计，agent 读取一次 netlink 即可获取。因此它可以回答前三类无法回答的问题——链路的瓶颈
// 位于哪一跳、速度下降是本端还是线路导致、应调整哪台机器的缓冲区。
//
// 时间统一使用 unix 秒，不使用其他位置的 timestamptz 文本格式。服务端两侧共用同一组结构体
// （protocol.rs 的 LoadSample / HopLinkSample），十七个数值字段分别定义两次容易出错，
// 而浏览器侧将秒转换为 Date 只需一行代码。

/** 变化频率低的部分：不进入时序数据，只存储最新值。 */
export interface HostFacts {
  kernel: string;
  /** /proc/cpuinfo 的可读处理器型号；旧 Agent 上报为空串 */
  cpu_model?: string;
  cores: number;
  /** 虚拟机经常不暴露 cpufreq；缺失表示不支持，不是频率为零。 */
  cpu_freq_max_mhz?: number | null;
  cpu_governor?: string | null;
  /** 不应假设只有 cubic / bbr——低价 VPS 上常见通过脚本安装的定制内核，其中包含 bbrplus、bbr2 */
  cc_algo: string;
  /** 可切换的拥塞算法列表。不含 bbr 表示模块未加载，或该内核不支持 */
  available_cc: string[];
  default_qdisc: string;
  /** 网卡上实际使用的算法。与上一项不一致时，表示已修改默认值但现有网卡未切换 */
  nic_qdisc: string;
  nic: string;
  /** 默认路由物理网卡的实际 MTU；与链路探测得到的 path_mtu 不是同一个层面的值 */
  nic_mtu: number | null;
  mem_total_bytes: number;
  disk_total_bytes: number;
  disk_mount?: string | null;
  disk_filesystem?: string | null;
  disk_device?: string | null;
  disk_read_only?: boolean | null;
  /** null 表示未加载 nf_conntrack。该机器未配置 NAT，不属于故障 */
  conntrack_max: number | null;
  ephemeral_port_low?: number | null;
  ephemeral_port_high?: number | null;
  ephemeral_port_capacity?: number | null;
  /** 安装时是否写入过 /etc/sysctl.d/99-brocade.conf。用于区分原本即为 bbr 和被改回默认值 */
  sysctl_managed: boolean;
  /** x86_64 / aarch64 */
  arch: string;
  /** /etc/os-release 的 PRETTY_NAME。空串表示该文件不存在（最小化镜像） */
  os_pretty: string;
  /** 虚拟化平台短名（KVM / VMware / …）。空串表示裸金属或识别不出 */
  virt: string;
  /** net.core.rmem_max / wmem_max。安装器不管这两项，界面上降一档色阶 */
  rmem_max: number;
  wmem_max: number;
  /** net.core.somaxconn */
  somaxconn: number;
}

export interface CpuCoreSample {
  cpu: number;
  user_pct: number;
  system_pct: number;
  softirq_pct: number;
  iowait_pct: number;
  steal_pct: number;
}

export interface CpuDetailSample {
  iowait_pct: number;
  load5: number;
  load15: number;
  pressure_some_pct: number | null;
  io_pressure_some_pct: number | null;
  io_pressure_full_pct: number | null;
  procs_running: number | null;
  procs_total: number | null;
  context_switches_per_sec: number | null;
  net_rx_softirqs_per_sec: number | null;
  net_tx_softirqs_per_sec: number | null;
  throttled_usec: number | null;
  frequency_mhz: number | null;
  cores: CpuCoreSample[];
}

export interface MemoryDetailSample {
  available_min_bytes: number;
  free_bytes: number;
  anon_bytes: number;
  file_cache_bytes: number;
  shmem_bytes: number;
  kernel_other_bytes: number;
  buffers_bytes: number;
  kernel_reclaimable_bytes: number;
  slab_unreclaimable_bytes: number;
  unevictable_bytes: number;
  mlocked_bytes: number;
  dirty_bytes: number;
  writeback_bytes: number;
  swap_total_bytes: number;
  swap_cached_bytes: number;
  zswap_bytes: number | null;
  zswapped_bytes: number | null;
  gup_pinned_bytes: number | null;
  swap_in_bytes: number;
  swap_out_bytes: number;
  pressure_some_pct: number | null;
  pressure_full_pct: number | null;
  major_faults: number;
  direct_reclaim_pages: number;
}

export interface DiskDetailSample {
  total_bytes: number | null;
  inode_total: number | null;
  inode_free: number | null;
  read_bps: number | null;
  write_bps: number | null;
  read_iops: number | null;
  write_iops: number | null;
  read_await_ms: number | null;
  write_await_ms: number | null;
  busy_pct: number | null;
  queue_depth: number | null;
  in_flight: number | null;
  pressure_some_pct: number | null;
  pressure_full_pct: number | null;
}

/** 新 Agent 上报的网络深度观测。连接/内存字段是窗口末快照，其余字段是该 30 秒窗口增量。 */
export interface NetworkDetailSample {
  tcp_curr_estab: number | null;
  tcp_inuse: number | null;
  tcp_time_wait: number | null;
  tcp_orphan: number | null;
  tcp_alloc: number | null;
  tcp_mem_bytes: number | null;
  udp_inuse: number | null;
  udp_mem_bytes: number | null;
  ephemeral_port_capacity?: number | null;
  tcp_ephemeral_inuse_v4?: number | null;
  tcp_ephemeral_inuse_v6?: number | null;
  tcp_ephemeral_time_wait_v4?: number | null;
  tcp_ephemeral_time_wait_v6?: number | null;
  tcp_ephemeral_top_target_v4?: number | null;
  tcp_ephemeral_top_target_v6?: number | null;
  tcp_active_opens: number | null;
  tcp_passive_opens: number | null;
  tcp_attempt_fails: number | null;
  tcp_estab_resets: number | null;
  tcp_retrans_segs: number | null;
  tcp_syn_retrans: number | null;
  tcp_in_errors: number | null;
  tcp_out_resets: number | null;
  tcp_timeouts: number | null;
  tcp_listen_overflows: number | null;
  tcp_listen_drops: number | null;
  udp_in_errors: number | null;
  udp_no_ports: number | null;
  udp_rcvbuf_errors: number | null;
  udp_sndbuf_errors: number | null;
}

/** 一台机器一个窗口的资源读数。agent 本地计算差值后上报的速率，不是累计值。 */
export interface LoadSample {
  window_start_unix_secs: number;
  window_end_unix_secs: number;
  /** 机器已重启，或中间存在窗口缺失。该条的速率不可比较，绘图时应断开而非连接 */
  has_gap: boolean;
  /* 三段分别统计而非合并为一个百分比：转发负载体现在 softirq 上，合并后
     网卡中断饱和与 xray 加密计算的显示相同，而两者的处理方式完全不同 */
  cpu_user_pct: number;
  cpu_sys_pct: number;
  cpu_softirq_pct: number;
  /** 窗口内 10 秒子采样的峰值。均值会平滑掉尖峰，而尖峰正是转发负载的特征 */
  cpu_peak_pct: number;
  /** /proc/stat 的 steal：虚拟机想要 CPU 但宿主机分给了别的虚拟机的时长占比。
      与三段分列不同，steal 不是这台机器在做功——持续非零说明宿主机超售 */
  cpu_steal_pct: number;
  load1: number;
  /** 缺失表示该窗口来自旧 Agent。 */
  cpu_detail?: CpuDetailSample | null;
  mem_available_bytes: number;
  swap_used_bytes: number;
  memory_detail?: MemoryDetailSample | null;
  /** 该窗口内确实有进程被内核终止。是实测结果，不是推断 */
  oom_kills: number;
  disk_free_bytes: number;
  disk_inode_free_pct: number;
  disk_detail?: DiskDetailSample | null;
  nic_rx_bps: number;
  nic_tx_bps: number;
  nic_rx_drop: number;
  nic_tx_drop: number;
  nic_err: number;
  conntrack_count: number | null;
  /** 缺失表示来自旧 Agent；不能把缺失解释成全部为零。 */
  network_detail?: NetworkDetailSample | null;
  uptime_secs: number;
}

/** 本系统部署在该机器上的进程。用于区分是机器整体负载高还是本系统进程负载高。 */
export interface ProcessSample {
  proc: 'xray' | 'wg' | 'phantun' | 'agent';
  /** null 表示该进程不存在，或它以内核模块形式运行（wg 内建时没有进程，开销体现在 softirq 中） */
  rss_bytes: number | null;
  cpu_pct: number | null;
  started_at_unix_secs: number | null;
  fds: number | null;
  fd_limit: number | null;
}

export interface NodeLoadView {
  node_id: string;
  /** null 表示该机器从未上报。与上报值为零是两种状态，界面上必须明确区分 */
  reported_at_unix_secs: number | null;
  /** 最近一次上报到达时 agent 时钟减控制面时钟（秒，带符号）。只能在接收时测量。
      超出 ±600 秒的整轮已被拒收，因此该值必在此区间内 */
  clock_skew_secs: number | null;
  host: HostFacts | null;
  /** 按时间从旧到新排列，可直接从左向右绘制 */
  series: LoadSample[];
  processes: ProcessSample[];
}

/** 一跳一个窗口的链路质量。键为 (chain_id, peer_node_id)，与 link_health 使用同一键。 */
export interface HopLinkSample {
  chain_id: string;
  peer_node_id: string;
  window_start_unix_secs: number;
  window_end_unix_secs: number;
  conns: number;
  /** 其中 delivery_rate 未带 app_limited 标志的样本数。只有这些样本的带宽估计有效 */
  conns_measured: number;
  /** null 表示该机器未使用 bbr，无法测量。cubic 不维护瓶颈带宽估计 */
  btlbw_p50_bps: number | null;
  btlbw_p90_bps: number | null;
  /** 所有连接中的最小值——传播时延对应最小值，取均值会将排队时延计入 */
  min_rtt_us: number;
  rtt_p50_us: number;
  rtt_p90_us: number;
  retrans_pct: number;
  /** 三个占比之和不必为 100：一条连接可能不受任何一种限制 */
  busy_pct: number;
  rwnd_limited_pct: number;
  sndbuf_limited_pct: number;
}

export interface HopLinkView {
  node_id: string;
  sample: HopLinkSample;
  /** 该机器的拥塞算法。返回该字段是为了在带宽为空的行上说明原因是使用 cubic 无法测量 */
  cc_algo: string;
}

/** 列表页需要 24 个窗口：单根柱子无法体现趋势，而趋势是该列的作用所在。 */
export const fetchNodeLoadList = (windows = 24, token = '') =>
  api<{ nodes: NodeLoadView[] }>(`/load/nodes?windows=${windows}`, token);

export const fetchNodeLoad = (nodeId: string, windows = 24, token = '') =>
  api<NodeLoadView>(`/load/nodes/${encodeURIComponent(nodeId)}?windows=${windows}`, token);

export interface PingProbePoint {
  probed_at_unix_secs: number;
  /** false 表示受能力或路由限制而未实际发包，不应计作丢包。 */
  attempted: boolean;
  /** 微秒；已尝试且为 null 表示在超时前没有响应。 */
  latency_us: number | null;
}

export interface PingProbeTargetSeries extends PingProbeTarget {
  samples: PingProbePoint[];
}

export interface NodePingProbeView {
  node_id: string;
  targets: PingProbeTargetSeries[];
}

export const fetchNodePingProbeList = (windowSecs = 3600, token = '') =>
  api<{ nodes: NodePingProbeView[] }>(`/ping-probe/nodes?window_secs=${windowSecs}`, token);

export const fetchNodePingProbe = (nodeId: string, windowSecs = 86_400, token = '') =>
  api<NodePingProbeView>(`/ping-probe/nodes/${encodeURIComponent(nodeId)}?window_secs=${windowSecs}`, token);

export const fetchLinkQuality = (chainId?: string, token = '') =>
  api<{ hops: HopLinkView[] }>(`/links/quality${chainId ? `?chain_id=${encodeURIComponent(chainId)}` : ''}`, token);
