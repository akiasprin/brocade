import { useQuery } from '@tanstack/react-query';
import { hopWireLabel } from '../ui/format';
import { fetchCompileView, fetchRevisions } from '../api';
import { Empty, ErrorBox, Loading } from '../ui/bits';
import { useNodeNames } from '../ui/node-name';
import { artifactPanel } from '../ui/artifact-panel';
import type { AppIr, SystemIr } from '../topo/model';
import { hopKey, undirectedNodePairKey } from '../topo/model';
import { hopPathCopy } from './hop-inspect';

/* 检视窗：一个对象一个窗口，内容全部来自服务端的 IR，不包含内部导航。 */
export function InspectPane({ kind, id }: { kind: string; id: string }) {
  const revisions = useQuery({ queryKey: ['revisions'], queryFn: () => fetchRevisions() });
  const current = revisions.data?.current_revision;
  const compile = useQuery({
    queryKey: ['compile', current],
    queryFn: () => fetchCompileView(current!),
    enabled: !!current,
  });

  if (revisions.isPending || (current != null && compile.isPending)) return <Loading />;
  if (revisions.error || compile.error) return <ErrorBox error={revisions.error ?? compile.error} />;
  if (!current) return <Empty>还没有可检视的修订。</Empty>;
  if (!compile.data) return <ErrorBox error={new Error('编译结果没有返回内容')} />;

  const system = compile.data.system as SystemIr;
  const apps = (compile.data.apps as AppIr[]) ?? [];

  if (kind === 'node') return <NodeInspect id={id} system={system} apps={apps} />;
  if (kind === 'link') return <LinkInspect id={id} system={system} apps={apps} />;
  if (kind === 'ingress') return <IngressInspect id={id} apps={apps} />;
  if (kind === 'hop') return <HopInspect id={id} apps={apps} />;
  return <Empty>不认识的对象。</Empty>;
}

// 一跳的详情。画布上只显示端口——地址、是否走 overlay、加密方式都在此处显示。
// id 是 model.ts 的 hopKey：chain|from|to。
function HopInspect({ id, apps }: { id: string; apps: AppIr[] }) {
  const nameOf = useNodeNames();
  const [chainId, from, to] = id.split('|');
  const app = apps.find(a => a.hops.some(h => hopKey(h) === id));
  const hop = app?.hops.find(h => hopKey(h) === id);
  if (!hop || !app) return <Empty>这个修订里没有这一跳。</Empty>;
  const chain = app.chains.find(c => c.id === chainId);
  const pathCopy = hopPathCopy(hop.path);

  return (
    <>
      <dl className="kv" style={{ gridTemplateColumns: '92px minmax(0,1fr)' }}>
        <dt>转发</dt>
        <dd>
          <span title={from}>{nameOf(from)}</span> <span className="dim">→</span> <span title={to}>{nameOf(to)}</span>
        </dd>
        <dt>链</dt>
        <dd className="mono">{chain?.name ?? chainId}</dd>
        <dt>{pathCopy.endpointLabel}</dt>
        <dd className="mono">
          {hop.address}:{hop.port}
        </dd>
        <dt>路径</dt>
        <dd>
          <span className={`st${pathCopy.publicNetwork ? ' st-gold' : ''}`}>{pathCopy.badge}</span>{' '}
          <span className="dim">{pathCopy.detail}</span>
        </dd>
        <dt>overlay 链路</dt>
        <dd className="mono dim">{hop.link}</dd>
        <dt>协议</dt>
        <dd className="mono dim">
          {hopWireLabel(hop.security?.t)}
          {/* 走 overlay 时明文是合理的，wg 已对该跳加密；走公网时不是。
              因此该说明只在条件成立时显示，而不是作为所有非加密档的通用说明。 */}
          {!hop.security?.t || hop.security.t === 'none' ? ` — ${pathCopy.plaintextDetail}` : ''}
        </dd>
      </dl>
      <p className="note">这一跳的规则表在「链」页修改；在画布上从 {nameOf(from)} 拖出连线是同一入口。</p>
    </>
  );
}

/* 产物面板常驻右侧，此处只负责将其切换到该产物（面板收起时同时展开） */
const artifactWin = (targetKind: string, targetId: string, artifactKind: string) =>
  artifactPanel.show({ targetKind, targetId, artifactKind });

function NodeInspect({ id, system, apps }: { id: string; system: SystemIr; apps: AppIr[] }) {
  const n = system.nodes.find(x => x.id === id);
  if (!n) return <Empty>这个修订里没有 {id}。</Empty>;
  const appNode = apps.flatMap(a => a.nodes).find(x => x.id === id);
  const links = system.links.filter(l => l.a === id || l.b === id);
  const ingresses = apps.flatMap(a => a.ingresses.filter(i => i.node === id));

  return (
    <>
      <dl className="kv" style={{ gridTemplateColumns: '92px minmax(0,1fr)' }}>
        <dt>节点</dt>
        <dd>
          {appNode?.name || n.id} {appNode?.name && <span className="dim mono">{n.id}</span>}
        </dd>
        <dt>租户</dt>
        <dd className="mono dim">{n.tenant}</dd>
        <dt>overlay</dt>
        <dd className="mono">{n.overlay_addr}/32</dd>
        <dt>overlay 链路</dt>
        <dd className="mono dim">{links.length} 条</dd>
        <dt>接入面</dt>
        <dd className="mono dim">{ingresses.map(i => `${i.id}:${i.port}`).join('、') || '—'}</dd>
      </dl>
      <div className="toolbar">
        <button className="btn" onClick={() => artifactWin('node', id, 'wireguard')}>
          wg0.conf
        </button>
        <button className="btn" onClick={() => artifactWin('node', id, 'xray')}>
          xray.json
        </button>
      </div>
      <p className="note">私钥不会进入订阅；非 system-admin 看到的是打码后的内容。</p>
    </>
  );
}

function LinkInspect({ id, system, apps }: { id: string; system: SystemIr; apps: AppIr[] }) {
  const l = system.links.find(x => x.id === id);
  if (!l) return <Empty>这个修订里没有这条链路。</Empty>;
  const linkKey = undirectedNodePairKey(l.a, l.b);
  const used = apps.flatMap(a =>
    a.hops.filter(h => h.path === 'overlay' && undirectedNodePairKey(h.from, h.to) === linkKey),
  );
  return (
    <>
      <dl className="kv" style={{ gridTemplateColumns: '92px minmax(0,1fr)' }}>
        <dt>链路</dt>
        <dd className="mono">{l.id}</dd>
        <dt>两端</dt>
        <dd className="mono">
          {l.a} ↔ {l.b}
        </dd>
        <dt>被谁用</dt>
        <dd className="mono dim">
          {used.length ? used.map(h => `${h.chain}: ${h.from}→${h.to}`).join('、') : '没有业务跑在上面'}
        </dd>
      </dl>
      <p className="note">全互联 overlay：绝大多数 peer 之间没有实际流量。</p>
    </>
  );
}

// 主干是派生概念：从链头沿 hops 遍历得出的路径。hops 的顺序与规则表一致，
// 取第一条即兜底规则对应的那条；分叉边不在该主路径上。
function irChainPath(app: AppIr, chainId: string): string[] {
  const head = app.ingresses.find(i => i.chain === chainId)?.node;
  if (!head) return [];
  const path = [head];
  const seen = new Set([head]);
  let cur = head;
  while (true) {
    const hop = app.hops.find(h => h.chain === chainId && h.from === cur);
    if (!hop || seen.has(hop.to)) break;
    path.push(hop.to);
    seen.add(hop.to);
    cur = hop.to;
  }
  return path;
}

function IngressInspect({ id, apps }: { id: string; apps: AppIr[] }) {
  const nameOf = useNodeNames();
  const app = apps.find(a => a.ingresses.some(i => i.id === id));
  const ing = app?.ingresses.find(i => i.id === id);
  if (!ing || !app) return <Empty>这个修订里没有 {id}。</Empty>;
  const chain = app.chains.find(c => c.id === ing.chain);
  const users = app.grants.filter(g => g.ingress === id);

  return (
    <>
      <dl className="kv" style={{ gridTemplateColumns: '92px minmax(0,1fr)' }}>
        <dt>接入面</dt>
        <dd className="mono">{ing.id}</dd>
        <dt>落在</dt>
        <dd>
          <span title={ing.node}>{nameOf(ing.node)}</span>
          <span className="dim mono">:{ing.port}</span>
        </dd>
        <dt>链</dt>
        <dd className="mono">
          {chain?.name ?? ing.chain}
          {chain && <span className="dim"> · {irChainPath(app, chain.id).join(' → ')}</span>}
        </dd>
        <dt>前置组</dt>
        <dd className="mono dim">{ing.front ?? '—'}</dd>
        <dt>授权用户</dt>
        <dd className="mono dim">{users.map(u => `${u.tenant}/${u.user}`).join('、') || '还没人'}</dd>
      </dl>
      {users.length > 0 && (
        <div className="toolbar">
          {users.slice(0, 4).map(u => (
            <button
              key={`${u.tenant}/${u.user}`}
              className="btn"
              /* target_id 必须是 tenant:user 形式——服务端 split_user_target 按冒号分割（console.rs） */
              onClick={() => artifactWin('user', `${u.tenant}:${u.user}`, 'uri')}
            >
              {u.user} 的订阅
            </button>
          ))}
        </div>
      )}
      <p className="note">同一用户在所有入口共用一个 UUID，授权只决定加入哪个 inbound。</p>
    </>
  );
}
