// 草稿：编辑累积在浏览器中，点击提交后写库，一次提交产生一个修订。
// *
// * 该设计替代了原有的编辑即提交。原方案的问题在使用后显现：
// * 模型写接口是纯 upsert（`steps` 没有删除接口），因此文档中的「撤销即反向修改一次」
// * 对相当一部分改动无法实现；且一次界面操作通常涉及多次写入——修改一条转发规则需要
// * 同时写入对端的中转端口——修订号与实际的一次操作不对应。
// *
// * # 此处存储的是操作而非模型
// *
// * 草稿是一组 `ModelOp`，每条对应一个已有的写接口。浏览器侧不保存模型副本：
// * 修改后的结果由服务端计算（`POST /model/preview`：开启事务、正常执行、读取结果、回滚），
// * 因为 upsert 的语义只应有一份实现，且部分内容前端无法计算——`accept.uuid`、
// * REALITY 密钥对、`hop_in` 的配置继承规则都在 store 中生成。
// *
// * # 同一对象的多次修改只保留一条
// *
// * 每条操作有一个 `key`（如 `put_step:app-main/c-lan/cn-edge`）。再次修改同一目标时替换
// * 原有记录而非追加。否则改动条数会随操作次数增长，而回放结果完全相同。

import type {
  CreateRealityIngress,
  DestMatch,
  Dns,
  DomainStrategy,
  EgressDnsResolution,
  ExternalOutboundWrite,
  HopInRequest,
  IngressProjection,
  ModelSettings,
  NodeConnection,
  Rule,
} from './api';

export type ModelOp =
  | { op: 'create_app'; app: { id: string; label: string } }
  | { op: 'upsert_app'; app: { id: string; label: string } }
  | { op: 'reorder_apps'; ids: string[] }
  | { op: 'reorder_chains'; app_id: string; ids: string[] }
  | { op: 'upsert_external_outbound'; outbound: ExternalOutboundWrite }
  | {
      op: 'create_chain';
      app_id: string;
      chain: { id: string; tenant_id: string; name: string; subscription_country?: string | null };
    }
  | {
      op: 'upsert_chain';
      app_id: string;
      chain: { id: string; tenant_id: string; name: string; subscription_country?: string | null };
    }
  | {
      op: 'upsert_front';
      app_id: string;
      front: {
        id: string;
        tenant_id: string;
        name: string;
        strategy: 'url-test' | 'select' | 'fallback';
        via: string[];
        external_via: string[];
      };
    }
  | {
      op: 'create_ingress';
      app_id: string;
      ingress: {
        id: string;
        chain_id: string;
        node_id: string;
        bind: string;
        port: number;
        front_id?: string;
        reality: CreateRealityIngress;
        projection?: IngressProjection;
      };
    }
  | {
      op: 'upsert_ingress';
      app_id: string;
      ingress: {
        id: string;
        chain_id: string;
        node_id: string;
        bind: string;
        port: number;
        front_id?: string;
        reality: CreateRealityIngress;
        projection?: IngressProjection;
      };
    }
  | {
      op: 'put_step';
      app_id: string;
      chain_id: string;
      node_id: string;
      step: { rules: Rule[]; accept?: { uuid?: string; label?: string }; hop_in?: HopInRequest };
    }
  | {
      op: 'set_node_egress_dns';
      node_id: string;
      selector: DestMatch;
      resolution: EgressDnsResolution | null;
    }
  | { op: 'reorder_node_egress_dns'; node_id: string; selectors: DestMatch[] }
  | { op: 'delete_step'; app_id: string; chain_id: string; node_id: string }
  | { op: 'delete_chain'; app_id: string; chain_id: string }
  // 移除该链上从链头不可达的 step。保存整棵规则树时排在所有 put_step 之后——
  // 该判定需要整条链的规则表，逐条回放的中间状态都不完整（见 api.ts 的 pruneChain）。
  | { op: 'prune_chain'; app_id: string; chain_id: string }
  | {
      op: 'upsert_grant';
      grant: {
        app_id: string;
        tenant_id: string;
        user_id: string;
        ingress_id: string;
        enabled: boolean;
      };
    }
  | { op: 'create_tenant'; tenant: { id: string; name: string } }
  | { op: 'create_user'; user: { tenant_id: string; id: string } }
  | { op: 'rotate_user_uuid'; tenant_id: string; user_id: string }
  | { op: 'update_user_status'; tenant_id: string; user_id: string; status: { status: string } }
  | {
      op: 'update_node';
      node_id: string;
      node: {
        tenant_id?: string;
        name?: string;
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
        mtu?: number;
        connection?: NodeConnection;
        wg_transport?: { t: 'udp' } | { t: 'fake_tcp'; v: { port: number } };
      };
    }
  | { op: 'update_node_status'; node_id: string; status: { status: string } }
  | { op: 'update_settings'; settings: ModelSettings };

export interface DraftEntry {
  /* 同一目标的重复编辑依据它合并 */
  key: string;
  label: string;
  op: ModelOp;
}

type IngressWrite = Extract<ModelOp, { op: 'upsert_ingress' }>['ingress'];

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function sameValue(left: unknown, right: unknown): boolean {
  if (Object.is(left, right)) return true;
  if (Array.isArray(left) || Array.isArray(right)) {
    return (
      Array.isArray(left) &&
      Array.isArray(right) &&
      left.length === right.length &&
      left.every((value, index) => sameValue(value, right[index]))
    );
  }
  if (!isRecord(left) || !isRecord(right)) return false;
  const keys = new Set([...Object.keys(left), ...Object.keys(right)]);
  return [...keys].every(key => sameValue(left[key], right[key]));
}

/* Each ingress editor builds a complete write body from the same snapshot, but different panels
 * edit different nested fields. Merge only the fields changed from that snapshot so a later panel
 * save cannot erase an earlier panel's pending change. Arrays remain atomic values. */
function mergeChanged(base: unknown, next: unknown, pending: unknown): unknown {
  if (sameValue(base, next)) return pending;
  if (!isRecord(base) || !isRecord(next)) return next;

  const result: Record<string, unknown> = isRecord(pending) ? { ...pending } : { ...base };
  const keys = new Set([...Object.keys(base), ...Object.keys(next)]);
  const changedKeys = [...keys].filter(key => !sameValue(base[key], next[key]));
  // A projection endpoint being removed is an explicit user action. An XHTTP-only edit made
  // from the old snapshot changes only that endpoint's `download` child; it must not resurrect
  // the endpoint while the pending deletion is still in the draft.
  if (pending === null && changedKeys.length === 1 && changedKeys[0] === 'download') return pending;
  for (const key of keys) {
    const baseHas = Object.prototype.hasOwnProperty.call(base, key);
    const nextHas = Object.prototype.hasOwnProperty.call(next, key);
    if (!nextHas) {
      if (baseHas) delete result[key];
      continue;
    }
    if (!baseHas || !sameValue(base[key], next[key])) {
      const baseValue = base[key];
      const nextValue = next[key];
      const pendingValue = result[key];
      result[key] =
        isRecord(baseValue) && isRecord(nextValue) ? mergeChanged(baseValue, nextValue, pendingValue) : nextValue;
    }
  }
  return result;
}

function mergeIngressChanges(base: IngressWrite, next: IngressWrite, pending: IngressWrite): IngressWrite {
  return mergeChanged(base, next, pending) as IngressWrite;
}

/* 一条操作的作用对象。合并和界面上列出草稿都依据它。 */
function entryOf(op: ModelOp): DraftEntry {
  switch (op.op) {
    case 'create_app':
    case 'upsert_app':
      return { key: `app:${op.app.id}`, label: `线路 ${op.app.id}`, op };
    case 'reorder_apps':
      return {
        key: 'app-order',
        label: '调整线路顺序',
        op,
      };
    case 'reorder_chains':
      return {
        key: `chain-order:${op.app_id}`,
        label: `调整链顺序 ${op.app_id}`,
        op,
      };
    case 'upsert_external_outbound':
      return {
        key: `external-outbound:${op.outbound.tenant_id}/${op.outbound.id}`,
        label: `隧道 ${op.outbound.name || op.outbound.id}`,
        op,
      };
    case 'upsert_front':
      return {
        key: `front:${op.app_id}/${op.front.id}`,
        label: `订阅前置 ${op.front.name || op.front.id}`,
        op,
      };
    case 'create_chain':
    case 'upsert_chain':
      return { key: `chain:${op.app_id}/${op.chain.id}`, label: `链 ${op.chain.id}`, op };
    case 'create_ingress':
    case 'upsert_ingress':
      return {
        key: `ingress:${op.app_id}/${op.ingress.id}`,
        label: `接入面 ${op.ingress.id}`,
        op,
      };
    case 'put_step':
      return {
        key: `step:${op.app_id}/${op.chain_id}/${op.node_id}`,
        label: `规则 ${op.chain_id}/${op.node_id}`,
        op,
      };
    case 'set_node_egress_dns': {
      const selector = canonicalDnsSelector(op.selector);
      return {
        key: `node-egress-dns:${op.node_id}/${JSON.stringify(selector)}`,
        label: `机器 DNS 策略 ${op.node_id}`,
        op: { ...op, selector },
      };
    }
    case 'reorder_node_egress_dns':
      return {
        key: `node-egress-dns-order:${op.node_id}`,
        label: `机器 DNS 策略顺序 ${op.node_id}`,
        op: { ...op, selectors: op.selectors.map(canonicalDnsSelector) },
      };
    // 与 put_step 使用同一个 key：先修改后删除和先删除后修改都收敛为后写入的一条，
    // 不会在草稿中留下相互冲突的两条记录。
    case 'delete_step':
      return {
        key: `step:${op.app_id}/${op.chain_id}/${op.node_id}`,
        label: `删除 ${op.chain_id}/${op.node_id}`,
        op,
      };
    case 'delete_chain':
      return {
        key: `chain:${op.app_id}/${op.chain_id}`,
        label: `删除链 ${op.chain_id}`,
        op,
      };
    // 使用独立的键，不与链的增删改合并：它不改变链的结构，只移除已无引用的悬空记录。
    // 同一条链多次清理也只保留一条（见 push 中的位置调整规则）。
    case 'prune_chain':
      return {
        key: `prune:${op.app_id}/${op.chain_id}`,
        label: `清理 ${op.chain_id} 落单节点`,
        op,
      };
    case 'upsert_grant':
      return {
        key: `grant:${op.grant.app_id}/${op.grant.tenant_id}/${op.grant.user_id}/${op.grant.ingress_id}`,
        label: `${op.grant.enabled ? '开' : '撤'}授权 ${op.grant.user_id} → ${op.grant.ingress_id}`,
        op,
      };
    case 'create_tenant':
      return { key: `tenant:${op.tenant.id}`, label: `租户 ${op.tenant.id}`, op };
    case 'create_user':
      return { key: `user:${op.user.tenant_id}/${op.user.id}`, label: `用户 ${op.user.id}`, op };
    case 'rotate_user_uuid':
      return {
        key: `user-uuid:${op.tenant_id}/${op.user_id}`,
        label: `轮换 uuid ${op.user_id}`,
        op,
      };
    case 'update_user_status':
      return {
        key: `user-status:${op.tenant_id}/${op.user_id}`,
        label: `用户 ${op.user_id} 置为 ${op.status.status}`,
        op,
      };
    case 'update_node':
      return { key: `node:${op.node_id}`, label: `机器 ${op.node_id}`, op };
    case 'update_node_status':
      return {
        key: `node-status:${op.node_id}`,
        label: `机器 ${op.node_id} 置为 ${op.status.status}`,
        op,
      };
    case 'update_settings':
      return { key: 'settings', label: '全局设置', op };
  }
}

function canonicalDnsSelector(selector: DestMatch): DestMatch {
  if (selector.t === 'domain_suffix' || selector.t === 'domain_keyword' || selector.t === 'geosite') {
    return { ...selector, v: [...new Set(selector.v)].sort() };
  }
  return selector;
}

class DraftStore {
  private entries: DraftEntry[] = [];
  private listeners = new Set<() => void>();
  private storageKey: string | null = null;
  // 快照需要保持稳定引用：useSyncExternalStore 每次接收到新数组都会判定为已变更，
  // 导致持续重渲染。只有实际发生修改时才创建新数组。
  private snap: readonly DraftEntry[] = [];
  /* 每次变更递增 1。api.ts 的预览缓存将其作为键的一部分——草稿变化时缓存立即失效。 */
  private ver = 0;

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };

  snapshot = (): readonly DraftEntry[] => this.snap;

  version = (): number => this.ver;

  /* 按操作者分键恢复。切换用户时重新开始——草稿表示该用户未提交的内容，随用户区分。 */
  init(operator: string) {
    // v2 is an intentional hard boundary: v1 drafts can contain chain/ingress IDs replaced by the
    // friendly-ID migration. Replaying one after that migration could target a different object
    // graph, so it is safer to start a clean draft than to guess an old-to-new mapping in the
    // browser. App slugs stay unchanged; server-side tombstones protect the two migrated kinds.
    const key = `brocade-console:draft:v2:${operator}`;
    if (this.storageKey === key) return;
    this.storageKey = key;
    this.entries = [];
    try {
      const raw = localStorage.getItem(key);
      if (raw) {
        // The former arrow-button UI stored one `swap_*` entry per click. A complete final order
        // cannot be reconstructed from those pairs without the old base snapshot, so retain every
        // unrelated edit and discard only these obsolete ordering operations during the upgrade.
        const restored = JSON.parse(raw) as { op?: { op?: unknown } }[];
        this.entries = restored.filter(
          entry => entry.op?.op !== 'swap_apps' && entry.op?.op !== 'swap_chains',
        ) as DraftEntry[];
      }
    } catch {
      /* 存储的草稿数据损坏时重新开始，优于整个页面无法打开 */
    }
    this.commit();
  }

  push(op: ModelOp) {
    let entry = entryOf(op);
    const at = this.entries.findIndex(e => e.key === entry.key);
    // Editing an object created earlier in the same draft must retain create-only collision
    // semantics. Replacing it with an upsert would let a coincidentally occupied random ID update
    // an unrelated object at commit time.
    const previous = at >= 0 ? this.entries[at].op : null;
    if (previous?.op === 'create_app' && op.op === 'upsert_app') {
      entry = entryOf({ op: 'create_app', app: op.app });
    } else if (previous?.op === 'create_chain' && op.op === 'upsert_chain') {
      entry = entryOf({ op: 'create_chain', app_id: op.app_id, chain: op.chain });
    } else if (previous?.op === 'create_ingress' && op.op === 'upsert_ingress') {
      entry = entryOf({ op: 'create_ingress', app_id: op.app_id, ingress: op.ingress });
    }
    if (
      at >= 0 &&
      op.op !== 'prune_chain' &&
      op.op !== 'reorder_node_egress_dns' &&
      op.op !== 'reorder_apps' &&
      op.op !== 'reorder_chains'
    ) {
      // 重复编辑保持原有位置。顺序存在依赖关系——链需要先创建才能写入其规则表，
      // 将后写入的记录移到末尾会使前置条件排在其后。
      this.entries[at] = entry;
    } else {
      // 清理和完整顺序都依赖此前的写入结果，因此重复操作也移到末尾。排序必须排在本轮
      // 新增/删除之后，否则它校验的“完整列表”仍是修改前的集合。拖动期间同一范围只留下
      // 最终顺序，而不是记录每次穿过相邻项的中间状态。
      if (at >= 0) this.entries.splice(at, 1);
      this.entries.push(entry);
    }
    this.commit();
  }

  pushIngress(appId: string, base: IngressWrite, next: IngressWrite) {
    const key = `ingress:${appId}/${next.id}`;
    const previous = this.entries.find(entry => entry.key === key)?.op;
    const pendingIngress =
      previous?.op === 'upsert_ingress' || previous?.op === 'create_ingress' ? previous.ingress : next;
    const ingress = previous ? mergeIngressChanges(base, next, pendingIngress) : next;
    const op: ModelOp =
      previous?.op === 'create_ingress'
        ? { op: 'create_ingress', app_id: appId, ingress }
        : { op: 'upsert_ingress', app_id: appId, ingress };
    this.push(op);
  }

  drop(key: string) {
    this.entries = this.entries.filter(e => e.key !== key);
    this.commit();
  }

  clear() {
    this.entries = [];
    this.commit();
  }

  ops(): ModelOp[] {
    return this.entries.map(e => e.op);
  }

  isEmpty(): boolean {
    return this.entries.length === 0;
  }

  private commit() {
    if (this.storageKey) {
      try {
        localStorage.setItem(this.storageKey, JSON.stringify(this.entries));
      } catch {
        /* 写入失败时不做处理，草稿丢失不影响已提交的内容 */
      }
    }
    this.ver += 1;
    this.snap = [...this.entries];
    for (const listener of this.listeners) listener();
  }
}

export const draft = new DraftStore();

/* 供冒烟脚本访问（与 wm 的做法一致） */
if (typeof window !== 'undefined') {
  (window as unknown as { __draft?: DraftStore }).__draft = draft;
}
