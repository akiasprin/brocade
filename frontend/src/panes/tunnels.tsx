import { useMemo, useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  fetchNodes,
  fetchSnapshot,
  fetchTenants,
  registerWarpBinding,
  removeWarpBinding,
  updateWarpBinding,
  upsertExternalOutbound,
  upsertFront,
  type ExternalOutbound,
  type ExternalWarpBinding,
  type WarpBindingOverrides,
} from '../api';
import { draft } from '../draft';
import { can, useSession } from '../session';
import { Empty, ErrorBox, Loading } from '../ui/bits';
import { Icon, ListIcon } from '../ui/icons';
import { useCrumb } from '../wm/crumb';
import { wm, type CrumbSeg, type Win } from '../wm/store';
import { ExternalOutboundEditor } from './rules';

type Drill = { p: 'list' } | { p: 'tunnel'; tenant: string; id: string };

export type WarpIpStack = 'dual' | 'prefer_ipv4' | 'prefer_ipv6' | 'ipv4' | 'ipv6';

type WarpProtocol = Extract<ExternalOutbound['protocol'], { t: 'warp' }>;

/**
 * WARP keeps Xray's low-level routing fields in the model, but the operator-facing choice is
 * represented as one address policy. Routes remain authoritative for single-stack configurations;
 * domainStrategy then distinguishes automatic dual stack from either preferred family.
 */
export const warpIpStackOf = (protocol: WarpProtocol): WarpIpStack => {
  const hasIpv4 = protocol.v.allowed_ips.some(network => network.includes('.'));
  const hasIpv6 = protocol.v.allowed_ips.some(network => network.includes(':'));
  if (hasIpv4 && !hasIpv6) return 'ipv4';
  if (hasIpv6 && !hasIpv4) return 'ipv6';
  if (protocol.v.domain_strategy === 'ForceIPv4') return 'ipv4';
  if (protocol.v.domain_strategy === 'ForceIPv6') return 'ipv6';
  if (protocol.v.domain_strategy === 'ForceIPv4v6') return 'prefer_ipv4';
  if (protocol.v.domain_strategy === 'ForceIPv6v4') return 'prefer_ipv6';
  return 'dual';
};

export const warpIpRouting = (stack: WarpIpStack): Pick<WarpProtocol['v'], 'allowed_ips' | 'domain_strategy'> => {
  switch (stack) {
    case 'prefer_ipv4':
      return { allowed_ips: ['0.0.0.0/0', '::/0'], domain_strategy: 'ForceIPv4v6' };
    case 'prefer_ipv6':
      return { allowed_ips: ['0.0.0.0/0', '::/0'], domain_strategy: 'ForceIPv6v4' };
    case 'ipv4':
      return { allowed_ips: ['0.0.0.0/0'], domain_strategy: 'ForceIPv4' };
    case 'ipv6':
      return { allowed_ips: ['::/0'], domain_strategy: 'ForceIPv6' };
    default:
      return { allowed_ips: ['0.0.0.0/0', '::/0'], domain_strategy: 'ForceIP' };
  }
};

const warpIpStackLabel = (stack: WarpIpStack) =>
  ({
    dual: '自动双栈',
    prefer_ipv4: 'IPv4 优先',
    prefer_ipv6: 'IPv6 优先',
    ipv4: '仅 IPv4',
    ipv6: '仅 IPv6',
  })[stack];

function WarpIpStackControl({ value, onChange }: { value: WarpIpStack; onChange: (value: WarpIpStack) => void }) {
  return (
    <select
      className="f warp-stack-control"
      aria-label="WARP 出口协议栈"
      value={value}
      onChange={event => onChange(event.target.value as WarpIpStack)}
    >
      <option value="dual">自动双栈</option>
      <option value="prefer_ipv4">IPv4 优先</option>
      <option value="prefer_ipv6">IPv6 优先</option>
      <option value="ipv4">仅 IPv4</option>
      <option value="ipv6">仅 IPv6</option>
    </select>
  );
}

const protocolName = (tunnel: ExternalOutbound) =>
  ({
    vless: 'VLESS',
    shadowsocks2022: 'Shadowsocks 2022',
    socks5: 'SOCKS5',
    http_connect: 'HTTP CONNECT',
    wireguard: 'WireGuard',
    warp: 'Cloudflare WARP',
  })[tunnel.protocol.t];

const protocolMark = (tunnel: ExternalOutbound) =>
  ({
    vless: 'VL',
    shadowsocks2022: 'SS',
    socks5: 'S5',
    http_connect: 'HT',
    wireguard: 'WG',
    warp: 'CF',
  })[tunnel.protocol.t];

const endpoint = (tunnel: ExternalOutbound) =>
  tunnel.address.includes(':') && !tunnel.address.startsWith('[')
    ? `[${tunnel.address}]:${tunnel.port}`
    : `${tunnel.address}:${tunnel.port}`;

// An unused WARP tunnel needs no status: being available for later use is its normal state, not
// something the operator has to act on. Once rules reference it, keep the two useful outcomes —
// whether every referenced machine has an identity, or registration is still owed.
export const warpReferenceStatus = (referenceCount: number, missingBindings: number): '待注册' | '已引用' | null => {
  if (referenceCount === 0) return null;
  return missingBindings > 0 ? '待注册' : '已引用';
};

const slug = (value: string) =>
  value
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9_-]+/g, '-')
    .replace(/^-+|-+$/g, '')
    .slice(0, 63) || 'cloudflare-warp';

const crumbs = (drill: Drill, tunnel?: ExternalOutbound): CrumbSeg[] =>
  drill.p === 'tunnel' ? [{ label: tunnel?.name || drill.id }] : [];

export function TunnelsPane({ win }: { win: Win }) {
  const drill = (win.data.drill as Drill | undefined) ?? { p: 'list' };
  const go = (next: Drill) => wm.setData(win.id, { ...win.data, drill: next });
  const snapshot = useQuery({ queryKey: ['snapshot'], queryFn: fetchSnapshot });
  const tunnel =
    drill.p === 'tunnel'
      ? (snapshot.data?.snapshot.external_outbounds ?? []).find(
          candidate => candidate.tenant === drill.tenant && candidate.id === drill.id,
        )
      : undefined;
  useCrumb(win, crumbs(drill, tunnel));

  if (drill.p === 'tunnel') return <TunnelDetail tenantId={drill.tenant} tunnelId={drill.id} go={go} />;
  return <TunnelList go={go} />;
}

function TunnelList({ go }: { go: (drill: Drill) => void }) {
  const { who } = useSession();
  const editable = can(who.role, 'edit');
  const snapshot = useQuery({ queryKey: ['snapshot'], queryFn: fetchSnapshot });
  const nodes = useQuery({ queryKey: ['nodes'], queryFn: () => fetchNodes() });
  const tenants = useQuery({ queryKey: ['tenants'], queryFn: () => fetchTenants() });
  const [create, setCreate] = useState<'scope' | 'manual' | 'warp' | null>(null);
  const apps = useMemo(() => snapshot.data?.snapshot.apps ?? [], [snapshot.data?.snapshot.apps]);
  const tunnels = snapshot.data?.snapshot.external_outbounds ?? [];
  const tenantOptions = tenants.data?.tenants ?? [];
  const [tenantId, setTenantId] = useState('');
  const selectedTenant = tenantId || tenantOptions[0]?.id || '';
  const tenantName = new Map(tenantOptions.map(tenant => [tenant.id, tenant.name || tenant.id]));
  const warpCount = tunnels.filter(tunnel => tunnel.protocol.t === 'warp').length;

  const references = useMemo(() => {
    const counts = new Map<string, Set<string>>();
    for (const app of apps) {
      for (const step of app.steps ?? []) {
        for (const rule of step.rules ?? []) {
          if (rule.a.t !== 'proxy') continue;
          const key = rule.a.outbound;
          const set = counts.get(key) ?? new Set<string>();
          set.add(step.node);
          counts.set(key, set);
        }
      }
    }
    return counts;
  }, [apps]);

  const warpTunnels = tunnels.filter(tunnel => tunnel.protocol.t === 'warp');
  const manualTunnels = tunnels.filter(tunnel => tunnel.protocol.t !== 'warp');
  const warpRefCount = warpTunnels.reduce((sum, tunnel) => sum + (references.get(tunnel.id)?.size ?? 0), 0);
  const manualUsedCount = manualTunnels.filter(tunnel => (references.get(tunnel.id)?.size ?? 0) > 0).length;

  if (snapshot.isPending || nodes.isPending || tenants.isPending) return <Loading />;
  if (snapshot.error) return <ErrorBox error={snapshot.error} />;
  if (nodes.error) return <ErrorBox error={nodes.error} />;
  if (tenants.error) return <ErrorBox error={tenants.error} />;

  return (
    <div className="cardpage tunnel-cardpage">
      <section className="panel titled tunnel-list-panel">
        {/* 与机器、线路列表共用页面级面板标题：标题、计数、右侧读数和主操作都落在
            同一条色带上，页面切换时不再从“面板标题”跳成一块悬浮页头。 */}
        <header>
          <ListIcon of="tunnels" />
          <h4>隧道</h4>
          <span className="hint">{tunnels.length} 条</span>
          <span className="rd">
            <b>{tunnels.length - warpCount}</b> 自定义 · <b>{warpCount}</b> WARP
          </span>
          <button
            className="btn primary"
            disabled={!editable || tenantOptions.length === 0}
            onClick={() => setCreate('scope')}
          >
            ＋ 新建隧道
          </button>
        </header>

        {tunnels.length === 0 ? (
          <div className="tunnel-empty">
            <span className="tunnel-empty-mark">
              <Icon of="tunnels" size={25} />
            </span>
            <b>还没有隧道</b>
            <p>导入 VLESS、SS2022、SOCKS5、HTTP CONNECT、WireGuard，或为每台出口机器申请独立 WARP 身份。</p>
            <button
              className="btn"
              disabled={!editable || tenantOptions.length === 0}
              onClick={() => setCreate('scope')}
            >
              创建第一条隧道
            </button>
          </div>
        ) : (
          <div className="tunnel-groups">
            {/* WARP 与自定义隧道生命周期不同，分族列出：WARP 以每机注册为主轴，
                自定义以协议 / Endpoint / 规则引用为主轴。 */}
            {warpTunnels.length > 0 && (
              <section className="tunnel-group">
                <header className="tunnel-group-head">
                  <h5>Cloudflare WARP</h5>
                  <span className="tunnel-group-agg">
                    {warpTunnels.length} 条 · <b>{warpRefCount}</b> 处引用
                  </span>
                </header>
                <div className="tunnel-list">
                  {warpTunnels.map(tunnel => {
                    const used = references.get(tunnel.id) ?? new Set<string>();
                    const boundNodes = new Set((tunnel.bindings ?? []).map(binding => binding.node));
                    const missingBindings = [...used].filter(node => !boundNodes.has(node)).length;
                    const status = warpReferenceStatus(used.size, missingBindings);
                    return (
                      <button
                        className="tunnel-row warp-row"
                        key={tunnel.id}
                        onClick={() => go({ p: 'tunnel', tenant: tunnel.tenant, id: tunnel.id })}
                      >
                        <span className="tunnel-proto managed">CF</span>
                        <span className="tunnel-row-main">
                          <span>
                            <b>{tunnel.name}</b>
                            <em>{tenantName.get(tunnel.tenant) || tunnel.tenant}</em>
                          </span>
                          <small className="mono">{endpoint(tunnel)}</small>
                        </span>
                        {/* 未使用是正常空闲状态，不渲染标签；只有发生引用后才显示结果。 */}
                        {status && <span className={`st ${missingBindings > 0 ? 'st-warn' : ''}`}>{status}</span>}
                        <span className="tunnel-chevron">›</span>
                      </button>
                    );
                  })}
                </div>
              </section>
            )}
            {manualTunnels.length > 0 && (
              <section className="tunnel-group">
                <header className="tunnel-group-head">
                  <h5>自定义隧道</h5>
                  <span className="tunnel-group-agg">
                    {manualTunnels.length} 条 · <b>{manualUsedCount}</b> 已引用
                  </span>
                </header>
                <div className="tunnel-list">
                  {manualTunnels.map(tunnel => {
                    const used = references.get(tunnel.id) ?? new Set<string>();
                    return (
                      <button
                        className="tunnel-row manual-row"
                        key={tunnel.id}
                        onClick={() => go({ p: 'tunnel', tenant: tunnel.tenant, id: tunnel.id })}
                      >
                        <span className="tunnel-proto">{protocolMark(tunnel)}</span>
                        <span className="tunnel-row-main">
                          <span>
                            <b>{tunnel.name}</b>
                            <em>{tenantName.get(tunnel.tenant) || tunnel.tenant}</em>
                          </span>
                          <small className="mono">
                            {protocolName(tunnel)} · {endpoint(tunnel)}
                          </small>
                        </span>
                        <span className="tunnel-row-kind">
                          <small>规则引用</small>
                          <b>{used.size === 0 ? '未被规则引用' : `${used.size} 台机器`}</b>
                        </span>
                        <span className="st">{used.size === 0 ? '未引用' : '已引用'}</span>
                        <span className="tunnel-chevron">›</span>
                      </button>
                    );
                  })}
                </div>
              </section>
            )}
          </div>
        )}
      </section>

      {create === 'scope' && (
        <div className="tunnel-dialog-wrap" role="dialog" aria-modal="true" aria-label="选择隧道类型">
          <button className="tunnel-dialog-scrim" aria-label="关闭" onClick={() => setCreate(null)} />
          <section className="tunnel-dialog">
            <header>
              <b>新建隧道</b>
              <span className="sp" />
              <button className="btn" onClick={() => setCreate(null)}>
                关闭
              </button>
            </header>
            <div className="tunnel-dialog-body">
              <div className="tunnel-scope-grid">
                <label>
                  租户
                  <select className="f" value={selectedTenant} onChange={event => setTenantId(event.target.value)}>
                    {tenantOptions.map(tenant => (
                      <option key={tenant.id} value={tenant.id}>
                        {tenant.name}
                      </option>
                    ))}
                  </select>
                </label>
              </div>
              <div className="tunnel-kind-grid">
                <button onClick={() => setCreate('warp')}>
                  <span className="tunnel-kind-icon cf">CF</span>
                  <span>
                    <b>Cloudflare WARP</b>
                    <small>自动申请免费 WARP；每台机器独立身份</small>
                  </span>
                  <i>推荐</i>
                </button>
                <button onClick={() => setCreate('manual')}>
                  <span className="tunnel-kind-icon">↗</span>
                  <span>
                    <b>导入或自定义配置</b>
                    <small>VLESS / SS2022 / SOCKS5 / HTTP / WireGuard</small>
                  </span>
                </button>
              </div>
              <p className="tunnel-provider-note">
                WARP WireGuard 注册使用非官方兼容接口；正式发布前仍会经过编译诊断与发布计划。
              </p>
            </div>
          </section>
        </div>
      )}
      {create === 'manual' && (
        <ExternalOutboundEditor
          tenantId={selectedTenant}
          existing={null}
          purpose="resource"
          onClose={() => setCreate(null)}
          onSaved={tunnel => {
            setCreate(null);
            go({ p: 'tunnel', tenant: tunnel.tenant, id: tunnel.id });
          }}
        />
      )}
      {create === 'warp' && (
        <WarpCreate
          tenantId={selectedTenant}
          existingIds={new Set(tunnels.map(tunnel => tunnel.id))}
          onClose={() => setCreate(null)}
          onCreated={tunnel => {
            setCreate(null);
            go({ p: 'tunnel', tenant: tunnel.tenant, id: tunnel.id });
          }}
        />
      )}
    </div>
  );
}

function WarpCreate({
  tenantId,
  existingIds,
  onClose,
  onCreated,
}: {
  tenantId: string;
  existingIds: Set<string>;
  onClose: () => void;
  onCreated: (tunnel: ExternalOutbound) => void;
}) {
  const qc = useQueryClient();
  const [name, setName] = useState('Cloudflare WARP');
  const suggested = slug(name);
  const [idOverride, setIdOverride] = useState('');
  const id = idOverride || suggested;
  const [address, setAddress] = useState('engage.cloudflareclient.com');
  const [port, setPort] = useState('2408');
  const [advanced, setAdvanced] = useState(false);
  const [mtu, setMtu] = useState('1280');
  const [keepAlive, setKeepAlive] = useState('25');
  const [ipStack, setIpStack] = useState<WarpIpStack>('dual');
  const [noKernelTun, setNoKernelTun] = useState(false);
  const [workers, setWorkers] = useState('0');
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const valid =
    !!tenantId &&
    !!name.trim() &&
    /^[a-z0-9][a-z0-9_-]{0,63}$/.test(id) &&
    !existingIds.has(id) &&
    !!address.trim() &&
    Number.isInteger(Number(port)) &&
    Number(port) > 0 &&
    Number(port) <= 65535 &&
    Number.isInteger(Number(mtu)) &&
    Number(mtu) >= 576 &&
    Number(mtu) <= 9000 &&
    Number.isInteger(Number(keepAlive)) &&
    Number(keepAlive) >= 0 &&
    Number(keepAlive) <= 65535 &&
    Number.isInteger(Number(workers)) &&
    Number(workers) >= 0 &&
    Number(workers) <= 256;

  const save = async () => {
    setSaving(true);
    setError(null);
    const tunnel: ExternalOutbound = {
      id,
      tenant: tenantId,
      name: name.trim(),
      address: address.trim(),
      port: Number(port),
      protocol: {
        t: 'warp',
        v: {
          mtu: Number(mtu),
          keep_alive: Number(keepAlive),
          ...warpIpRouting(ipStack),
          no_kernel_tun: noKernelTun,
          workers: Number(workers),
        },
      },
      security: { t: 'none' },
      bindings: [],
    };
    try {
      await upsertExternalOutbound({
        id,
        tenant_id: tenantId,
        name: tunnel.name,
        address: tunnel.address,
        port: tunnel.port,
        protocol: tunnel.protocol,
        security: tunnel.security,
      });
      await Promise.all([
        qc.invalidateQueries({ queryKey: ['snapshot'] }),
        qc.invalidateQueries({ queryKey: ['compile'] }),
      ]);
      onCreated(tunnel);
    } catch (next) {
      setError(next);
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="external-outbound-wrap" role="dialog" aria-modal="true" aria-label="创建 Cloudflare WARP">
      <button className="external-outbound-scrim" aria-label="关闭" onClick={onClose} />
      <section className="external-outbound-drawer warp-create-drawer">
        <header>
          <b>创建 Cloudflare WARP</b>
          <small>免费 WARP · WireGuard 兼容</small>
          <span className="sp" />
          <button className="btn" onClick={onClose}>
            关闭
          </button>
        </header>
        <div className="external-outbound-body">
          <section className="warp-identity-card">
            <span>CF</span>
            <div>
              <b>逻辑隧道与机器身份分离</b>
              <p>这里先创建可引用的隧道；提交草稿后，再为实际使用它的机器逐台申请身份。不会在预览时联系 Cloudflare。</p>
            </div>
          </section>
          <div className="fgrid one warp-create-fields">
            <label className="row">
              <span className="k">名称</span>
              <span className="v">
                <input className="f" value={name} onChange={event => setName(event.target.value)} />
              </span>
            </label>
            <label className="row">
              <span className="k">资源 ID</span>
              <span className="v">
                <input className="f mono" value={id} onChange={event => setIdOverride(event.target.value)} />
                <span className="sub">全局唯一；名称变化时默认自动生成。</span>
                {existingIds.has(id) && <span className="sub err">这个 ID 已存在。</span>}
              </span>
            </label>
            <label className="row">
              <span className="k">Endpoint</span>
              <span className="v tunnel-endpoint-fields">
                <input className="f mono" value={address} onChange={event => setAddress(event.target.value)} />
                <input className="f mono" type="number" value={port} onChange={event => setPort(event.target.value)} />
              </span>
            </label>
            <div className="row">
              <span className="k">出口协议栈</span>
              <span className="v">
                <WarpIpStackControl value={ipStack} onChange={setIpStack} />
                <span className="sub">优先模式会先解析首选地址族，无结果时再回退；外层 Endpoint 可独立使用 IPv4。</span>
              </span>
            </div>
          </div>
          <button className="warp-advanced-toggle" onClick={() => setAdvanced(value => !value)}>
            <span>{advanced ? '−' : '+'}</span>高级 WireGuard 参数
          </button>
          {advanced && (
            <div className="fgrid one warp-create-fields compact-fields">
              <label className="row">
                <span className="k">MTU</span>
                <span className="v">
                  <input className="f mono" type="number" value={mtu} onChange={event => setMtu(event.target.value)} />
                  <span className="sub">Cloudflare 客户端默认 1280。</span>
                </span>
              </label>
              <label className="row">
                <span className="k">Keepalive</span>
                <span className="v">
                  <input
                    className="f mono"
                    type="number"
                    min={0}
                    max={65535}
                    aria-label="Keepalive"
                    value={keepAlive}
                    onChange={event => setKeepAlive(event.target.value)}
                  />
                </span>
              </label>
              <label className="row">
                <span className="k">TUN 实现</span>
                <span className="v">
                  <select
                    className="f"
                    aria-label="TUN 实现"
                    value={noKernelTun ? 'userspace' : 'auto'}
                    onChange={event => setNoKernelTun(event.target.value === 'userspace')}
                  >
                    <option value="auto">系统优先（默认）</option>
                    <option value="userspace">仅用户态</option>
                  </select>
                  <span className="sub">仅用户态会设置 Xray noKernelTun，适合内核 WireGuard 不可用的环境。</span>
                </span>
              </label>
              <label className="row">
                <span className="k">Workers</span>
                <span className="v">
                  <input
                    className="f mono"
                    type="number"
                    min={0}
                    max={256}
                    aria-label="Workers"
                    value={workers}
                    onChange={event => setWorkers(event.target.value)}
                  />
                  <span className="sub">wireguard-go 并行 Worker 数；0 表示自动。</span>
                </span>
              </label>
            </div>
          )}
          <div className="warp-safety-list">
            <span>1</span>
            <p>
              <b>提交草稿</b>
              <small>资源进入正式修订后才允许申请。</small>
            </p>
            <span>2</span>
            <p>
              <b>逐机申请</b>
              <small>每台机器一对本地密钥与一个 Cloudflare 设备。</small>
            </p>
            <span>3</span>
            <p>
              <b>发布验证</b>
              <small>规则引用后按发布计划下发 Xray。</small>
            </p>
          </div>
          {error !== null && <ErrorBox error={error} />}
        </div>
        <footer>
          <span className="note">本步不调用 Cloudflare，也不会产生孤儿设备。</span>
          <span className="sp" />
          <button className="btn" onClick={onClose}>
            取消
          </button>
          <button className="btn primary" disabled={!valid || saving} onClick={() => void save()}>
            {saving ? '创建中…' : '创建到草稿'}
          </button>
        </footer>
      </section>
    </div>
  );
}

function TunnelDetail({ tenantId, tunnelId, go }: { tenantId: string; tunnelId: string; go: (drill: Drill) => void }) {
  const { who } = useSession();
  const editable = can(who.role, 'edit');
  const qc = useQueryClient();
  const snapshot = useQuery({ queryKey: ['snapshot'], queryFn: fetchSnapshot });
  const nodes = useQuery({ queryKey: ['nodes'], queryFn: () => fetchNodes() });
  const tenants = useQuery({ queryKey: ['tenants'], queryFn: () => fetchTenants() });
  const [editing, setEditing] = useState(false);
  const [bindingNode, setBindingNode] = useState('');
  const [acceptTerms, setAcceptTerms] = useState(false);
  const [bindError, setBindError] = useState<unknown>(null);
  const tunnel = (snapshot.data?.snapshot.external_outbounds ?? []).find(
    candidate => candidate.tenant === tenantId && candidate.id === tunnelId,
  );
  const apps = snapshot.data?.snapshot.apps ?? [];
  const nodeList = (nodes.data?.nodes ?? []).filter(node => node.tenant_id === tunnel?.tenant && !node.retired_at);
  const nodeName = new Map((nodes.data?.nodes ?? []).map(node => [node.node_id, node.name]));
  const tenantName = tenants.data?.tenants.find(tenant => tenant.id === tunnel?.tenant)?.name || tunnel?.tenant;
  const references = apps.flatMap(app =>
    app.steps
      .filter(step => step.rules.some(rule => rule.a.t === 'proxy' && rule.a.outbound === tunnelId))
      .map(step => ({
        app: app.id,
        project: app.label || app.id,
        node: step.node,
        chain: app.chains.find(chain => chain.id === step.chain)?.name || step.chain,
        chainId: step.chain,
      })),
  );
  /* 规则引用跳转到对应线路：打开（或聚焦）线路 tab 并下钻到该链。 */
  const openChain = (appId: string, chainId: string) => {
    const target = wm.open('tab:chains', '线路');
    wm.patchData(target.id, { drill: { p: 'chain', app: appId, chain: chainId } });
  };
  const boundNodes = new Set((tunnel?.bindings ?? []).map(binding => binding.node));
  const missing = [...new Set(references.map(reference => reference.node))].filter(node => !boundNodes.has(node));
  const availableNodes = nodeList.filter(node => !boundNodes.has(node.node_id));
  const selectedNode = bindingNode || missing[0] || availableNodes[0]?.node_id || '';

  const bind = useMutation({
    mutationFn: () => registerWarpBinding(tenantId, tunnelId, selectedNode),
    onSuccess: async () => {
      setAcceptTerms(false);
      setBindingNode('');
      setBindError(null);
      await Promise.all([
        qc.invalidateQueries({ queryKey: ['snapshot'] }),
        qc.invalidateQueries({ queryKey: ['revisions'] }),
        qc.invalidateQueries({ queryKey: ['compile'] }),
      ]);
    },
    onError: setBindError,
  });

  if (snapshot.isPending || nodes.isPending || tenants.isPending) return <Loading />;
  if (snapshot.error) return <ErrorBox error={snapshot.error} />;
  if (nodes.error) return <ErrorBox error={nodes.error} />;
  if (tenants.error) return <ErrorBox error={tenants.error} />;
  if (!tunnel)
    return (
      <Empty>
        这条隧道不存在，可能已在草稿中移除。
        <button className="btn" onClick={() => go({ p: 'list' })}>
          返回列表
        </button>
      </Empty>
    );

  const toggleFront = async (appId: string, frontId: string, enabled: boolean) => {
    const app = apps.find(candidate => candidate.id === appId);
    if (!app) return;
    const front = app.fronts.find(candidate => candidate.id === frontId);
    if (!front) return;
    const external = enabled
      ? [...new Set([...front.external_via, tunnel.id])]
      : front.external_via.filter(id => id !== tunnel.id);
    await upsertFront(app.id, {
      id: front.id,
      tenant_id: front.tenant,
      name: front.name,
      strategy: front.strategy,
      via: front.via,
      external_via: external,
    });
    await qc.invalidateQueries({ queryKey: ['snapshot'] });
  };

  const fronts = apps.flatMap(app =>
    app.fronts.filter(front => front.tenant === tunnel.tenant).map(front => ({ app, front })),
  );
  const isWarp = tunnel.protocol.t === 'warp';
  const warpDefaults = tunnel.protocol.t === 'warp' ? tunnel.protocol.v : null;
  return (
    <div className="nd-sheet nd-page tunnel-detail-page">
      {/* 铺 sheet 对齐机器详情：连续纸 + 纸内 52px 页头横梁 + 纸身。外层 fg-sheet 由
          `:has(> .nd-sheet)` 脱纸，避免纸中纸。 */}
      <div className="fg-sheet nd-paper">
        <header className="nd-page-head tunnel-detail-head">
          <div className="nd-page-identity">
            <span className={`tunnel-detail-icon ${isWarp ? 'managed' : ''}`}>
              <Icon of="tunnels" size={15} />
              {isWarp && <i className={`node-lamp ${missing.length ? 'warn' : 'ok'}`} aria-hidden />}
            </span>
            <h1 className="nd-id nd-name">{tunnel.name}</h1>
          </div>
          <div className="tunnel-head-meta">
            <span>{protocolName(tunnel)}</span>
            <span>租户 {tenantName}</span>
            <span>
              <b>{references.length}</b> 处引用
            </span>
          </div>
          <div className="nd-acts">
            <button className="btn" disabled={!editable} onClick={() => setEditing(true)}>
              编辑
            </button>
          </div>
        </header>

        <div className="nd-paper-body tunnel-detail-body">
          {/* 连接事实条：把散落的连接参数收成一行 */}
          <dl className="tunnel-strip">
            <div>
              <dt>{isWarp ? '默认 Endpoint' : 'Endpoint'}</dt>
              <dd className="mono">{endpoint(tunnel)}</dd>
            </div>
            {isWarp ? (
              <>
                <div>
                  <dt>出口策略</dt>
                  <dd>{warpDefaults ? warpIpStackLabel(warpIpStackOf({ t: 'warp', v: warpDefaults })) : '自动双栈'}</dd>
                </div>
                <div>
                  <dt>MTU / Keepalive</dt>
                  <dd className="mono">
                    {warpDefaults?.mtu ?? 1280} · {warpDefaults?.keep_alive ?? 25}s
                  </dd>
                </div>
              </>
            ) : (
              <div>
                <dt>协议</dt>
                <dd>{protocolName(tunnel)}</dd>
              </div>
            )}
            <div>
              <dt>{isWarp ? 'TUN / Workers' : '安全层'}</dt>
              <dd>
                {isWarp && warpDefaults
                  ? `${warpDefaults.no_kernel_tun ? '仅用户态' : '系统优先'} · ${warpDefaults.workers || '自动'}`
                  : tunnel.security.t.toUpperCase()}
              </dd>
            </div>
          </dl>

          {/* 机器注册（WARP）/ 订阅前置（自定义）排在规则引用之前 */}
          {isWarp ? (
            <section className="panel config-panel tunnel-panel">
              <header>
                <h4>机器注册</h4>
                <span className="hint">{tunnel.bindings.length} 已注册</span>
              </header>
              <div className="warp-bindings">
                {tunnel.bindings.length === 0 && (
                  <p className="tunnel-inline-empty">
                    还没有注册身份。提交隧道后，为实际使用它的机器逐台向 Cloudflare 注册。
                  </p>
                )}
                {tunnel.bindings.map(binding => (
                  <WarpBindingCard
                    key={binding.node}
                    tenantId={tunnel.tenant}
                    outboundId={tunnel.id}
                    binding={binding}
                    nodeName={nodeName.get(binding.node) || binding.node}
                    defaultAddress={tunnel.address}
                    defaultPort={tunnel.port}
                    defaults={{ t: 'warp', v: warpDefaults! }}
                    editable={editable}
                    removalBlockedReason={
                      references.some(reference => reference.node === binding.node)
                        ? '这台机器仍被规则引用。请先解除引用并完成发布，再注销身份。'
                        : !draft.isEmpty()
                          ? '先提交或丢弃当前草稿，再注销身份。'
                          : undefined
                    }
                  />
                ))}
                <div className="warp-bind-box">
                  <label>
                    注册机器
                    <select
                      className="f"
                      value={selectedNode}
                      disabled={!editable || availableNodes.length === 0 || !draft.isEmpty()}
                      onChange={event => setBindingNode(event.target.value)}
                    >
                      {availableNodes.map(node => (
                        <option key={node.node_id} value={node.node_id}>
                          {node.name}
                          {missing.includes(node.node_id) ? ' · 规则待注册' : ''}
                        </option>
                      ))}
                    </select>
                  </label>
                  <label className="warp-terms">
                    <input
                      type="checkbox"
                      checked={acceptTerms}
                      disabled={!editable || !draft.isEmpty()}
                      onChange={event => setAcceptTerms(event.target.checked)}
                    />
                    <span>我同意 Cloudflare Application Terms，并知悉 WireGuard 注册接口为非官方兼容能力。</span>
                  </label>
                  {!draft.isEmpty() && (
                    <p className="tunnel-warn">先提交或丢弃当前草稿。Cloudflare 注册不能附着在尚未提交的资源上。</p>
                  )}
                  {missing.length > 0 && draft.isEmpty() && (
                    <p className="tunnel-warn">
                      规则已使用，但仍有 {missing.length} 台机器未注册 WARP 身份，当前修订不可发布。
                    </p>
                  )}
                  {bindError !== null && <ErrorBox error={bindError} />}
                  <button
                    className="btn primary"
                    disabled={!editable || !selectedNode || !acceptTerms || !draft.isEmpty() || bind.isPending}
                    onClick={() => bind.mutate()}
                  >
                    {bind.isPending ? '正在注册…' : '向 Cloudflare 注册'}
                  </button>
                </div>
              </div>
            </section>
          ) : (
            <section className="panel config-panel tunnel-panel">
              <header>
                <h4>订阅前置</h4>
                <span className="hint">Clash dialer-proxy</span>
              </header>
              <div className="tunnel-fronts">
                <p>显式加入后，这条隧道才会带着凭据写入用户的 Clash 订阅，并作为所选前置组的成员。</p>
                {fronts.length === 0 ? (
                  <p className="tunnel-inline-empty">该租户在所有项目中都没有可用的前置组。</p>
                ) : (
                  fronts.map(({ app, front }) => (
                    <label key={`${app.id}/${front.id}`}>
                      <input
                        type="checkbox"
                        disabled={!editable}
                        checked={front.external_via.includes(tunnel.id)}
                        onChange={event => void toggleFront(app.id, front.id, event.target.checked)}
                      />
                      <span>
                        <b>{front.name}</b>
                        <small>
                          {app.label || app.id} · {front.strategy} · {front.via.length} 个内部成员
                        </small>
                      </span>
                    </label>
                  ))
                )}
                <p className="tunnel-secret-warning">
                  加入前置组会把该隧道的连接凭据动态写入获得相应入口授权的用户订阅；WARP 不提供此选项。
                </p>
              </div>
            </section>
          )}

          {/* 规则引用：每项跳转到对应线路 */}
          <section className="panel config-panel tunnel-panel">
            <header>
              <h4>规则引用</h4>
              <span className="hint">{new Set(references.map(reference => reference.node)).size} 台机器</span>
            </header>
            <div className="tunnel-ref-list">
              {references.length === 0 ? (
                <p className="dim">尚未被任何规则使用；不会下发到机器。</p>
              ) : (
                references.map(reference => (
                  <button
                    key={`${reference.app}/${reference.chainId}/${reference.node}`}
                    className="tunnel-ref-row"
                    onClick={() => openChain(reference.app, reference.chainId)}
                  >
                    <b>{reference.chain}</b>
                    <small>
                      {reference.project} · {nodeName.get(reference.node) || reference.node}
                    </small>
                    <span className="tunnel-ref-go">›</span>
                  </button>
                ))
              )}
            </div>
          </section>
        </div>
      </div>

      {editing && !isWarp && (
        <ExternalOutboundEditor
          tenantId={tunnel.tenant}
          existing={tunnel}
          purpose="resource"
          onClose={() => setEditing(false)}
          onSaved={() => setEditing(false)}
        />
      )}
      {editing && isWarp && <WarpEdit tunnel={tunnel} onClose={() => setEditing(false)} />}
    </div>
  );
}

export function WarpBindingCard({
  tenantId,
  outboundId,
  binding,
  nodeName,
  defaultAddress,
  defaultPort,
  defaults,
  editable,
  removalBlockedReason,
}: {
  tenantId: string;
  outboundId: string;
  binding: ExternalWarpBinding;
  nodeName: string;
  defaultAddress: string;
  defaultPort: number;
  defaults: WarpProtocol;
  editable: boolean;
  removalBlockedReason?: string;
}) {
  const qc = useQueryClient();
  const effectiveAddress = binding.endpoint_address ?? defaultAddress;
  const effectivePort = binding.endpoint_port ?? defaultPort;
  const effectiveMtu = binding.mtu ?? defaults.v.mtu;
  const effectiveKeepAlive = binding.keep_alive ?? defaults.v.keep_alive;
  const effectiveIpStack = warpIpStackOf({
    t: 'warp',
    v: {
      ...defaults.v,
      allowed_ips: binding.allowed_ips ?? defaults.v.allowed_ips,
      domain_strategy: binding.domain_strategy ?? defaults.v.domain_strategy,
    },
  });
  const defaultIpStack = warpIpStackOf(defaults);
  const effectiveNoKernelTun = binding.no_kernel_tun ?? defaults.v.no_kernel_tun;
  const effectiveWorkers = binding.workers ?? defaults.v.workers;
  const customized =
    binding.endpoint_address != null ||
    binding.endpoint_port != null ||
    binding.mtu != null ||
    binding.keep_alive != null ||
    binding.allowed_ips != null ||
    binding.domain_strategy != null ||
    binding.no_kernel_tun != null ||
    binding.workers != null;
  const [editing, setEditing] = useState(false);
  const [address, setAddress] = useState(effectiveAddress);
  const [port, setPort] = useState(String(effectivePort));
  const [mtu, setMtu] = useState(String(effectiveMtu));
  const [keepAlive, setKeepAlive] = useState(String(effectiveKeepAlive));
  const [ipStack, setIpStack] = useState<WarpIpStack>(effectiveIpStack);
  const [noKernelTun, setNoKernelTun] = useState(effectiveNoKernelTun);
  const [workers, setWorkers] = useState(String(effectiveWorkers));
  const [formError, setFormError] = useState<unknown>(null);
  const [confirmingRemoval, setConfirmingRemoval] = useState(false);

  const update = useMutation({
    mutationFn: (overrides: WarpBindingOverrides) => updateWarpBinding(tenantId, outboundId, binding.node, overrides),
    onSuccess: async () => {
      setEditing(false);
      setFormError(null);
      await Promise.all([
        qc.invalidateQueries({ queryKey: ['snapshot'] }),
        qc.invalidateQueries({ queryKey: ['revisions'] }),
        qc.invalidateQueries({ queryKey: ['compile'] }),
      ]);
    },
  });
  const remove = useMutation({
    mutationFn: () => removeWarpBinding(tenantId, outboundId, binding.node),
    onSuccess: async () => {
      setConfirmingRemoval(false);
      setEditing(false);
      setFormError(null);
      await Promise.all([
        qc.invalidateQueries({ queryKey: ['snapshot'] }),
        qc.invalidateQueries({ queryKey: ['revisions'] }),
        qc.invalidateQueries({ queryKey: ['compile'] }),
      ]);
    },
  });

  const begin = () => {
    setAddress(effectiveAddress);
    setPort(String(effectivePort));
    setMtu(String(effectiveMtu));
    setKeepAlive(String(effectiveKeepAlive));
    setIpStack(effectiveIpStack);
    setNoKernelTun(effectiveNoKernelTun);
    setWorkers(String(effectiveWorkers));
    setFormError(null);
    update.reset();
    remove.reset();
    setConfirmingRemoval(false);
    setEditing(true);
  };
  const save = () => {
    const nextAddress = address.trim();
    const nextPort = Number(port);
    const nextMtu = Number(mtu);
    const nextKeepAlive = Number(keepAlive);
    const nextWorkers = Number(workers);
    if (!nextAddress) {
      setFormError(new Error('Endpoint 地址不能为空。'));
      return;
    }
    if (/\s/.test(nextAddress)) {
      setFormError(new Error('Endpoint 地址不能包含空白字符。'));
      return;
    }
    if (!Number.isInteger(nextPort) || nextPort < 1 || nextPort > 65535) {
      setFormError(new Error('Endpoint 端口必须是 1–65535 的整数。'));
      return;
    }
    if (!Number.isInteger(nextMtu) || nextMtu < 576 || nextMtu > 9000) {
      setFormError(new Error('MTU 必须是 576–9000 的整数。'));
      return;
    }
    if (!Number.isInteger(nextKeepAlive) || nextKeepAlive < 0 || nextKeepAlive > 65535) {
      setFormError(new Error('Keepalive 必须是 0–65535 的整数。'));
      return;
    }
    if (!Number.isInteger(nextWorkers) || nextWorkers < 0 || nextWorkers > 256) {
      setFormError(new Error('Workers 必须是 0–256 的整数；0 表示自动。'));
      return;
    }
    const routing = warpIpRouting(ipStack);
    const inheritsRouting = ipStack === defaultIpStack;
    setFormError(null);
    update.mutate({
      endpoint_address: nextAddress === defaultAddress ? null : nextAddress,
      endpoint_port: nextPort === defaultPort ? null : nextPort,
      mtu: nextMtu === defaults.v.mtu ? null : nextMtu,
      keep_alive: nextKeepAlive === defaults.v.keep_alive ? null : nextKeepAlive,
      allowed_ips: inheritsRouting ? null : routing.allowed_ips,
      domain_strategy: inheritsRouting ? null : routing.domain_strategy,
      no_kernel_tun: noKernelTun === defaults.v.no_kernel_tun ? null : noKernelTun,
      workers: nextWorkers === defaults.v.workers ? null : nextWorkers,
    });
  };
  const clear = () => {
    setFormError(null);
    update.mutate({
      endpoint_address: null,
      endpoint_port: null,
      mtu: null,
      keep_alive: null,
      allowed_ips: null,
      no_kernel_tun: null,
      domain_strategy: null,
      workers: null,
    });
  };
  const beginRemoval = () => {
    remove.reset();
    setConfirmingRemoval(true);
  };
  const cancelRemoval = () => {
    remove.reset();
    setConfirmingRemoval(false);
  };

  return (
    <article className={`warp-binding-card ${editing ? 'editing' : ''}`}>
      <span className="warp-binding-state">✓</span>
      <span className="warp-binding-machine">
        <b>{nodeName}</b>
        <small className="mono" title={`${binding.local_addresses.join(' · ')} · 设备 ${binding.device_id}`}>
          {binding.local_addresses.join(' · ')} · {binding.device_id.slice(0, 8)}…
        </small>
      </span>
      <span className="warp-binding-route">
        <b className="mono">
          {effectiveAddress.includes(':') && !effectiveAddress.startsWith('[')
            ? `[${effectiveAddress}]:${effectivePort}`
            : `${effectiveAddress}:${effectivePort}`}
        </b>
        <small>
          {warpIpStackLabel(effectiveIpStack)} · MTU {effectiveMtu} · Keepalive {effectiveKeepAlive}s
        </small>
        <small>
          {effectiveNoKernelTun ? '仅用户态 TUN' : '系统优先 TUN'} · Workers {effectiveWorkers || '自动'} ·{' '}
          {customized ? '机器自定义' : '跟随隧道默认'}
        </small>
      </span>
      <button
        className="btn warp-binding-edit"
        disabled={!editable || update.isPending || remove.isPending}
        onClick={begin}
      >
        设置
      </button>
      {editing && (
        <div className="warp-binding-editor">
          <div className="warp-binding-editor-fields">
            <label className="warp-binding-endpoint-address">
              <span>Endpoint 地址</span>
              <input className="f mono" value={address} onChange={event => setAddress(event.target.value)} />
            </label>
            <label>
              <span>端口</span>
              <input
                className="f mono"
                type="number"
                min={1}
                max={65535}
                value={port}
                onChange={event => setPort(event.target.value)}
              />
            </label>
            <label>
              <span>出口地址策略</span>
              <WarpIpStackControl value={ipStack} onChange={setIpStack} />
            </label>
            <label>
              <span>MTU</span>
              <input
                className="f mono"
                type="number"
                min={576}
                max={9000}
                value={mtu}
                onChange={event => setMtu(event.target.value)}
              />
            </label>
            <label>
              <span>Keepalive（秒）</span>
              <input
                className="f mono"
                type="number"
                min={0}
                max={65535}
                value={keepAlive}
                onChange={event => setKeepAlive(event.target.value)}
              />
            </label>
            <label>
              <span>TUN 实现</span>
              <select
                className="f"
                value={noKernelTun ? 'userspace' : 'auto'}
                onChange={event => setNoKernelTun(event.target.value === 'userspace')}
              >
                <option value="auto">系统优先</option>
                <option value="userspace">仅用户态</option>
              </select>
            </label>
            <label>
              <span>Workers</span>
              <input
                className="f mono"
                type="number"
                min={0}
                max={256}
                value={workers}
                onChange={event => setWorkers(event.target.value)}
              />
            </label>
          </div>
          {confirmingRemoval ? (
            <div className="warp-binding-remove-confirm" role="alert">
              <div>
                <b>注销这台机器的 Cloudflare 身份？</b>
                <p>
                  将先注销设备 <span className="mono">{binding.device_id.slice(0, 8)}…</span>，再移除 Brocade
                  保存的密钥和令牌。操作不可撤销，历史修订也不能恢复这个身份。
                </p>
              </div>
              {remove.error && <ErrorBox error={remove.error} />}
              <div className="warp-binding-remove-actions">
                <button className="btn" disabled={remove.isPending} onClick={cancelRemoval}>
                  保留身份
                </button>
                <button className="btn danger" disabled={remove.isPending} onClick={() => remove.mutate()}>
                  {remove.isPending ? '正在注销…' : '确认注销并移除'}
                </button>
              </div>
            </div>
          ) : (
            <>
              <p>所有字段都可逐机覆盖；与隧道默认值相同时自动恢复为“跟随默认”，修改在下一次发布时下发。</p>
              {(formError !== null || update.error) && <ErrorBox error={formError ?? update.error} />}
              <div className="warp-binding-editor-actions">
                <button
                  className="btn danger"
                  disabled={!editable || update.isPending || Boolean(removalBlockedReason)}
                  title={removalBlockedReason}
                  onClick={beginRemoval}
                >
                  注销并移除
                </button>
                {customized && (
                  <button className="btn" disabled={update.isPending} onClick={clear}>
                    取消覆盖
                  </button>
                )}
                <span className="sp" />
                <button className="btn" disabled={update.isPending} onClick={() => setEditing(false)}>
                  取消
                </button>
                <button className="btn primary" disabled={update.isPending} onClick={save}>
                  {update.isPending ? '保存中…' : '保存'}
                </button>
              </div>
              {removalBlockedReason && <p className="warp-binding-remove-blocked">{removalBlockedReason}</p>}
            </>
          )}
        </div>
      )}
    </article>
  );
}

export function WarpEdit({ tunnel, onClose }: { tunnel: ExternalOutbound; onClose: () => void }) {
  const qc = useQueryClient();
  const protocol = tunnel.protocol.t === 'warp' ? tunnel.protocol : null;
  const [name, setName] = useState(tunnel.name);
  const [address, setAddress] = useState(tunnel.address);
  const [port, setPort] = useState(String(tunnel.port));
  const [mtu, setMtu] = useState(String(protocol?.v.mtu ?? 1280));
  const [keepAlive, setKeepAlive] = useState(String(protocol?.v.keep_alive ?? 25));
  const [ipStack, setIpStack] = useState<WarpIpStack>(() => (protocol ? warpIpStackOf(protocol) : 'dual'));
  const [noKernelTun, setNoKernelTun] = useState(protocol?.v.no_kernel_tun ?? false);
  const [workers, setWorkers] = useState(String(protocol?.v.workers ?? 0));
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<unknown>(null);
  if (!protocol) return null;
  const valid =
    !!name.trim() &&
    !!address.trim() &&
    Number.isInteger(Number(port)) &&
    Number(port) >= 1 &&
    Number(port) <= 65535 &&
    Number.isInteger(Number(mtu)) &&
    Number(mtu) >= 576 &&
    Number(mtu) <= 9000 &&
    Number.isInteger(Number(keepAlive)) &&
    Number(keepAlive) >= 0 &&
    Number(keepAlive) <= 65535 &&
    Number.isInteger(Number(workers)) &&
    Number(workers) >= 0 &&
    Number(workers) <= 256;
  const save = async () => {
    if (!valid) return;
    setSaving(true);
    setError(null);
    try {
      await upsertExternalOutbound({
        id: tunnel.id,
        tenant_id: tunnel.tenant,
        name: name.trim(),
        address: address.trim(),
        port: Number(port),
        protocol: {
          ...protocol,
          v: {
            ...protocol.v,
            mtu: Number(mtu),
            keep_alive: Number(keepAlive),
            ...warpIpRouting(ipStack),
            no_kernel_tun: noKernelTun,
            workers: Number(workers),
          },
        },
        security: { t: 'none' },
      });
      await Promise.all([
        qc.invalidateQueries({ queryKey: ['snapshot'] }),
        qc.invalidateQueries({ queryKey: ['compile'] }),
      ]);
      onClose();
    } catch (next) {
      setError(next);
    } finally {
      setSaving(false);
    }
  };
  return (
    <div className="tunnel-dialog-wrap" role="dialog" aria-modal="true" aria-label="编辑 WARP 默认参数">
      <button className="tunnel-dialog-scrim" aria-label="关闭" onClick={onClose} />
      <section className="tunnel-dialog narrow">
        <header>
          <b>编辑 WARP 默认参数</b>
          <span className="sp" />
          <button className="btn" onClick={onClose}>
            关闭
          </button>
        </header>
        <div className="tunnel-dialog-body">
          <div className="fgrid one warp-create-fields">
            <label className="row">
              <span className="k">名称</span>
              <span className="v">
                <input className="f" value={name} onChange={event => setName(event.target.value)} />
              </span>
            </label>
            <label className="row">
              <span className="k">默认 Endpoint</span>
              <span className="v tunnel-endpoint-fields">
                <input className="f mono" value={address} onChange={event => setAddress(event.target.value)} />
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
            <div className="row">
              <span className="k">出口协议栈</span>
              <span className="v">
                <WarpIpStackControl value={ipStack} onChange={setIpStack} />
              </span>
            </div>
            <label className="row">
              <span className="k">默认 MTU</span>
              <span className="v">
                <input
                  className="f mono"
                  type="number"
                  min={576}
                  max={9000}
                  value={mtu}
                  onChange={event => setMtu(event.target.value)}
                />
              </span>
            </label>
            <label className="row">
              <span className="k">Keepalive</span>
              <span className="v">
                <input
                  className="f mono"
                  type="number"
                  min={0}
                  max={65535}
                  aria-label="Keepalive"
                  value={keepAlive}
                  onChange={event => setKeepAlive(event.target.value)}
                />
                <span className="sub">单位为秒；0 表示关闭心跳，NAT 环境建议保留 25。</span>
              </span>
            </label>
            <label className="row">
              <span className="k">TUN 实现</span>
              <span className="v">
                <select
                  className="f"
                  aria-label="TUN 实现"
                  value={noKernelTun ? 'userspace' : 'auto'}
                  onChange={event => setNoKernelTun(event.target.value === 'userspace')}
                >
                  <option value="auto">系统优先（默认）</option>
                  <option value="userspace">仅用户态</option>
                </select>
                <span className="sub">仅用户态会设置 noKernelTun，适合内核 WireGuard 不可用的机器。</span>
              </span>
            </label>
            <label className="row">
              <span className="k">Workers</span>
              <span className="v">
                <input
                  className="f mono"
                  type="number"
                  min={0}
                  max={256}
                  aria-label="Workers"
                  value={workers}
                  onChange={event => setWorkers(event.target.value)}
                />
                <span className="sub">wireguard-go 并行 Worker 数；0 表示自动。</span>
              </span>
            </label>
          </div>
          <p className="tunnel-provider-note">
            这些参数不会重新注册机器身份；保存到草稿并发布后生效。每台机器可以分别覆盖 Endpoint、端口、
            MTU、Keepalive、出口地址策略、TUN 实现和 Workers。
          </p>
          {error !== null && <ErrorBox error={error} />}
        </div>
        <footer>
          <span className="sp" />
          <button className="btn" onClick={onClose}>
            取消
          </button>
          <button className="btn primary" disabled={saving || !valid} onClick={() => void save()}>
            {saving ? '保存中…' : '保存到草稿'}
          </button>
        </footer>
      </section>
    </div>
  );
}

/**
 * The rule editor owns the WARP lifecycle for its current machine. Keeping this component next to
 * the existing binding/default editors means the hidden resource page and the rule path cannot
 * drift into two implementations of registration, overrides, or destructive removal.
 */
export function WarpRuleManager({
  tunnel,
  nodeId,
  nodeName,
  editable,
  removalBlockedReason,
  onClose,
}: {
  tunnel: ExternalOutbound;
  nodeId: string;
  nodeName: string;
  editable: boolean;
  removalBlockedReason?: string;
  onClose: () => void;
}) {
  const qc = useQueryClient();
  const protocol = tunnel.protocol.t === 'warp' ? tunnel.protocol : null;
  const binding = tunnel.bindings.find(candidate => candidate.node === nodeId);
  const [acceptTerms, setAcceptTerms] = useState(false);
  const [editingDefaults, setEditingDefaults] = useState(false);
  const [suggestedEndpoint, setSuggestedEndpoint] = useState<string | null>(null);

  const bind = useMutation({
    mutationFn: () => registerWarpBinding(tunnel.tenant, tunnel.id, nodeId),
    onSuccess: async result => {
      setAcceptTerms(false);
      setSuggestedEndpoint(result.suggested_endpoint ?? null);
      await Promise.all([
        qc.invalidateQueries({ queryKey: ['snapshot'] }),
        qc.invalidateQueries({ queryKey: ['revisions'] }),
        qc.invalidateQueries({ queryKey: ['compile'] }),
      ]);
    },
  });

  if (!protocol) return null;
  const stack = warpIpStackOf(protocol);
  return (
    <div
      className="external-outbound-wrap warp-rule-manager-wrap"
      role="dialog"
      aria-modal="true"
      aria-label="管理当前机器的 WARP 出口"
    >
      <button className="external-outbound-scrim" aria-label="关闭" onClick={onClose} />
      <section className="external-outbound-drawer warp-rule-manager">
        <header>
          <b>Cloudflare WARP</b>
          <small>{nodeName} · 当前机器</small>
          <span className="sp" />
          <button className="btn" onClick={onClose}>
            关闭
          </button>
        </header>
        <div className="external-outbound-body">
          <section className="warp-rule-defaults">
            <header>
              <span>
                <small>默认 Endpoint</small>
                <b className="mono">{endpoint(tunnel)}</b>
              </span>
              <span>
                <small>出口策略</small>
                <b>{warpIpStackLabel(stack)}</b>
              </span>
              <span>
                <small>MTU / Keepalive</small>
                <b className="mono">
                  {protocol.v.mtu} / {protocol.v.keep_alive}s
                </b>
              </span>
              <span>
                <small>TUN / Workers</small>
                <b>
                  {protocol.v.no_kernel_tun ? '仅用户态' : '系统优先'} · {protocol.v.workers || '自动'}
                </b>
              </span>
              <button className="btn" disabled={!editable} onClick={() => setEditingDefaults(true)}>
                编辑默认
              </button>
            </header>
            <p>默认参数属于租户 WARP 资源；当前机器有覆盖时，以机器参数为准。</p>
          </section>

          <section className="panel config-panel tunnel-panel warp-rule-machine">
            <header>
              <h4>机器身份</h4>
              <span className={`st ${binding ? '' : 'st-warn'}`}>{binding ? '已注册' : '待注册'}</span>
            </header>
            {binding ? (
              <div className="warp-bindings">
                <WarpBindingCard
                  tenantId={tunnel.tenant}
                  outboundId={tunnel.id}
                  binding={binding}
                  nodeName={nodeName}
                  defaultAddress={tunnel.address}
                  defaultPort={tunnel.port}
                  defaults={protocol}
                  editable={editable}
                  removalBlockedReason={removalBlockedReason}
                />
              </div>
            ) : (
              <div className="warp-bind-box warp-rule-register">
                <p>
                  为 <b>{nodeName}</b> 申请一套独立的 WireGuard 密钥、地址与 Cloudflare 设备身份。打开面板不会发起申请。
                </p>
                <label className="warp-terms">
                  <input
                    type="checkbox"
                    checked={acceptTerms}
                    disabled={!editable || bind.isPending}
                    onChange={event => setAcceptTerms(event.target.checked)}
                  />
                  <span>我同意 Cloudflare Application Terms，并知悉 WireGuard 注册接口为非官方兼容能力。</span>
                </label>
                {bind.error && <ErrorBox error={bind.error} />}
                {suggestedEndpoint && (
                  <p className="tunnel-warn">Cloudflare 返回的建议 Endpoint：{suggestedEndpoint}</p>
                )}
                <button
                  className="btn primary"
                  disabled={!editable || !acceptTerms || bind.isPending}
                  onClick={() => bind.mutate()}
                >
                  {bind.isPending ? '正在注册…' : '注册并绑定当前机器'}
                </button>
              </div>
            )}
          </section>
        </div>
      </section>
      {editingDefaults && <WarpEdit tunnel={tunnel} onClose={() => setEditingDefaults(false)} />}
    </div>
  );
}
