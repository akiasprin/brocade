import { lazy, Suspense, useMemo, useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  fetchDeployments,
  fetchNodes,
  fetchSnapshot,
  fetchTunnelProbe,
  fetchTunnelProbeCapability,
  removeWarpBinding,
  updateWarpBinding,
  upsertExternalOutbound,
  type DeploymentListItem,
  type ConsoleSnapshot,
  type ExternalOutbound,
  type ExternalWarpBinding,
  type NodeAgentStateItem,
  type SnapshotApp,
  type WarpBindingOverrides,
} from '../api';
import { draft } from '../draft';
import { warpTunnelId as randomWarpTunnelId } from '../model-id';
import { can, isVisitor, useSession } from '../session';
import { Empty, EmptyState, ErrorBox, Loading } from '../ui/bits';
import { Icon, ListIcon, PanelTitle } from '../ui/icons';
import { RegionFlag } from '../ui/region-flag';
import { useCrumb } from '../wm/crumb';
import { type CrumbSeg, type Win } from '../wm/store';
import { navigate, navigateInPlace, returnTo } from '../forge/route';
import { DialogClose, DialogLayer } from '../ui/dialog';
import { confirmDiscardChanges, useUnsavedChanges } from '../ui/navigation-guard';
import { WarpRegistrationAction, useWarpRegistrationAvailability } from '../warp-registration';
import { TunnelProbePanel } from '../tunnel-probe';
import { isVpngateOutbound } from '../vpngate-selection';
import { VpngateProviderSection } from './vpngate-provider';

// The list only needs the compact provider card. Loading the VPN Gate workspace here would also
// download its ECharts renderer before the list can make its first model request.
const VpngatePage = lazy(() => import('./vpngate').then(module => ({ default: module.VpngatePage })));
const loadTunnelEditor = () => import('./rules');
const ExternalOutboundEditor = lazy(() =>
  loadTunnelEditor().then(module => ({ default: module.ExternalOutboundEditor })),
);
const TunnelDeleteDialog = lazy(() => loadTunnelEditor().then(module => ({ default: module.TunnelDeleteDialog })));

type Drill =
  | { p: 'list' }
  | { p: 'vpngate'; id?: string; country?: string }
  | { p: 'warp'; id: string }
  | { p: 'custom'; id: string };

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

const protocolName = (tunnel: ExternalOutbound) => {
  if (tunnel.protocol.t === 'vless' && tunnel.protocol.v.encryption !== 'none') return 'VLESS Encryption';
  return {
    anytls: 'AnyTLS',
    vless: 'VLESS',
    shadowsocks2022: 'Shadowsocks',
    socks5: 'SOCKS5',
    http_connect: 'HTTP CONNECT',
    wireguard: 'WireGuard',
    warp: 'Cloudflare WARP',
    vpngate: 'VPN Gate',
  }[tunnel.protocol.t];
};

const protocolMark = (tunnel: ExternalOutbound) => {
  if (tunnel.protocol.t === 'vless' && tunnel.protocol.v.encryption !== 'none') return 'VE';
  return {
    anytls: 'AT',
    vless: 'VL',
    shadowsocks2022: 'SS',
    socks5: 'S5',
    http_connect: 'HT',
    wireguard: 'WG',
    warp: 'CF',
    vpngate: 'VG',
  }[tunnel.protocol.t];
};

const hostPort = (address: string, port: number) =>
  address.includes(':') && !address.startsWith('[') ? `[${address}]:${port}` : `${address}:${port}`;

const endpoint = (tunnel: ExternalOutbound) => hostPort(tunnel.address, tunnel.port);

// An unused WARP tunnel needs no status: being available for later use is its normal state, not
// something the operator has to act on. Once rules reference it, keep the two useful outcomes —
// whether every referenced machine has an identity, or registration is still owed.
export const warpReferenceStatus = (referenceCount: number, missingBindings: number): '待注册' | '已引用' | null => {
  if (referenceCount === 0) return null;
  return missingBindings > 0 ? '待注册' : '已引用';
};

const crumbs = (drill: Drill, label?: string): CrumbSeg[] => {
  if (drill.p === 'vpngate' && !drill.id) return [{ label: 'VPN Gate' }];
  if ('id' in drill && drill.id) return [{ label: label || drill.id }];
  return [];
};

/** The single-tenant workspace already carries ownership in its model snapshot. Never guess a
 * tenant from a display name or silently choose the first one if that invariant stops holding. */
export function singleTunnelTenant(snapshot: ConsoleSnapshot['snapshot'], tenantScope: string | null): string | null {
  const tenants = new Set<string>();
  const add = (tenant: string | undefined) => {
    if (tenant) tenants.add(tenant);
  };
  for (const node of snapshot.nodes ?? []) add(node.tenant);
  for (const user of snapshot.users ?? []) add(user.tenant);
  for (const outbound of snapshot.external_outbounds ?? []) add(outbound.tenant);
  for (const app of snapshot.apps) {
    for (const chain of app.chains) add(chain.tenant);
    for (const front of app.fronts) add(front.tenant);
    for (const grant of app.grants) add(grant.tenant);
  }
  if (tenants.size > 1) return null;
  return tenants.values().next().value ?? tenantScope;
}

export function TunnelsPane({ win }: { win: Win }) {
  const drill = (win.data.drill as Drill | undefined) ?? { p: 'list' };
  const go = (next: Drill) => navigate('tunnels', next);
  const selectedId = 'id' in drill ? drill.id : undefined;
  const selectedVpngateCountry = drill.p === 'vpngate' ? drill.country : undefined;
  const selectVpngateCountry = (country: string) => navigateInPlace('tunnels', { p: 'vpngate', country });
  const snapshot = useQuery({ queryKey: ['snapshot'], queryFn: fetchSnapshot, enabled: Boolean(selectedId) });
  const label = selectedId
    ? (snapshot.data?.snapshot.external_outbounds ?? []).find(candidate => candidate.id === selectedId)?.name
    : undefined;
  useCrumb(win, crumbs(drill, label));

  if (drill.p === 'vpngate' && !drill.id)
    return (
      <Suspense fallback={<Loading variant="vpngate" />}>
        <VpngatePage countryCode={selectedVpngateCountry} onCountryChange={selectVpngateCountry} />
      </Suspense>
    );
  if (selectedId) return <TunnelDetail tunnelId={selectedId} />;
  return <TunnelList go={go} />;
}

function TunnelList({ go }: { go: (drill: Drill) => void }) {
  const { who } = useSession();
  const editable = can(who.role, 'edit');
  const snapshot = useQuery({ queryKey: ['snapshot'], queryFn: fetchSnapshot });
  const [create, setCreate] = useState<'scope' | 'manual' | 'warp' | null>(null);
  const apps = useMemo(() => snapshot.data?.snapshot.apps ?? [], [snapshot.data?.snapshot.apps]);
  const tunnels = snapshot.data?.snapshot.external_outbounds ?? [];
  const tenantId = snapshot.data ? singleTunnelTenant(snapshot.data.snapshot, who.tenant_scope) : null;
  const modelState = snapshot.data ? 'ready' : snapshot.error ? 'error' : 'pending';

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
  const vpngateTunnels = tunnels.filter(isVpngateOutbound);
  const manualTunnels = tunnels.filter(tunnel => tunnel.protocol.t !== 'warp' && tunnel.protocol.t !== 'vpngate');
  const warpIds = new Set(warpTunnels.map(tunnel => tunnel.id));
  const warpNodes = new Set(warpTunnels.flatMap(tunnel => (tunnel.bindings ?? []).map(binding => binding.node)));
  const vpngateIds = new Set(vpngateTunnels.map(tunnel => tunnel.id));
  const warpChains = new Set<string>();
  const vpngateChains = new Set<string>();
  for (const app of apps) {
    for (const step of app.steps ?? []) {
      if (step.rules.some(rule => rule.a.t === 'proxy' && warpIds.has(rule.a.outbound))) {
        warpChains.add(`${app.id}\u0000${step.chain}`);
      }
      if (step.rules.some(rule => rule.a.t === 'proxy' && vpngateIds.has(rule.a.outbound))) {
        vpngateChains.add(`${app.id}\u0000${step.chain}`);
      }
    }
  }
  const manualUsedCount = manualTunnels.filter(tunnel => (references.get(tunnel.id)?.size ?? 0) > 0).length;

  return (
    <div className="cardpage tunnel-cardpage">
      <section className="panel titled tunnel-list-panel">
        {/* 与机器、线路列表共用页面级面板标题和主操作布局。 */}
        <header>
          <ListIcon of="tunnels" />
          <h4>隧道</h4>
          <span className="sp" />
          <button className="btn primary" disabled={!editable || !tenantId} onClick={() => setCreate('scope')}>
            ＋ 新建隧道
          </button>
        </header>

        <div className="chain-sections">
          <VpngateProviderSection
            pools={vpngateTunnels}
            chainCount={vpngateChains.size}
            modelState={modelState}
            onOpen={() => go({ p: 'vpngate' })}
          />

          {/* Bootstrap 只反映已提交库存；可选分组必须等含草稿的模型确认，不能预画。 */}
          {modelState === 'pending' && (
            <p className="tunnel-model-read-state" role="status">
              正在读取其他隧道…
            </p>
          )}
          {modelState === 'error' && <ErrorBox error={snapshot.error} />}
          {modelState === 'ready' && warpTunnels.length === 0 && manualTunnels.length === 0 ? (
            <EmptyState
              icon="tunnels"
              title="还没有自定义隧道"
              action={
                <button className="btn primary" disabled={!editable || !tenantId} onClick={() => setCreate('scope')}>
                  创建第一条隧道
                </button>
              }
            >
              可导入 VLESS、Shadowsocks、SOCKS5、HTTP CONNECT、WireGuard，或为每个出口节点申请独立 WARP 身份；VPN Gate
              在上方统一管理。
            </EmptyState>
          ) : modelState === 'ready' ? (
            <>
              {/* WARP 与自定义隧道生命周期不同，分族列出：WARP 以每机注册为主轴，
                自定义以协议 / Endpoint / 规则引用为主轴。 */}
              {warpTunnels.length > 0 && (
                <section className="chain-section tunnel-group">
                  <header className="chain-section-head">
                    <span className="no" aria-hidden="true">
                      02
                    </span>
                    <h5>Cloudflare WARP</h5>
                    <span className="tunnel-group-agg">
                      {warpNodes.size} 个机器 · {warpChains.size} 条链路
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
                          data-route-focus={`tunnel:${tunnel.tenant}:${tunnel.id}`}
                          onClick={() => go({ p: 'warp', id: tunnel.id })}
                        >
                          <span className="tunnel-proto managed">CF</span>
                          <span className="tunnel-row-main">
                            <span>
                              <b>{tunnel.name}</b>
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
                <section className="chain-section tunnel-group">
                  <header className="chain-section-head">
                    <span className="no" aria-hidden="true">
                      {warpTunnels.length > 0 ? '03' : '02'}
                    </span>
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
                          data-route-focus={`tunnel:${tunnel.tenant}:${tunnel.id}`}
                          onClick={() => go({ p: 'custom', id: tunnel.id })}
                        >
                          <span className="tunnel-proto">{protocolMark(tunnel)}</span>
                          <span className="tunnel-row-main">
                            <span>
                              <b>{tunnel.name}</b>
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
            </>
          ) : null}
        </div>
      </section>

      {create === 'scope' && (
        <DialogLayer label="选择隧道类型" onClose={() => setCreate(null)}>
          <section className="dialog-surface tunnel-dialog">
            <header>
              <b>新建隧道</b>
              <span className="sp" />
              <DialogClose className="btn">关闭</DialogClose>
            </header>
            <div className="tunnel-dialog-body">
              <div className="tunnel-kind-grid">
                <button onClick={() => setCreate('warp')}>
                  <span className="tunnel-kind-icon cf">CF</span>
                  <span>
                    <b>Cloudflare WARP</b>
                    <small>自动申请免费 WARP；每台机器独立身份</small>
                  </span>
                  <i>推荐</i>
                </button>
                <button
                  onClick={() => {
                    setCreate(null);
                    go({ p: 'vpngate' });
                  }}
                >
                  <span className="tunnel-kind-icon cf">VG</span>
                  <span>
                    <b>VPN Gate 地区出口</b>
                    <small>从统一目录选择地区，由使用它的机器真实拨测并自动择优</small>
                  </span>
                </button>
                <button onClick={() => setCreate('manual')}>
                  <span className="tunnel-kind-icon">↗</span>
                  <span>
                    <b>导入或自定义配置</b>
                    <small>VLESS / Shadowsocks / SOCKS5 / HTTP / WireGuard</small>
                  </span>
                </button>
              </div>
              <p className="tunnel-provider-note">
                WARP WireGuard 注册使用非官方兼容接口；正式发布前仍会经过编译诊断与发布计划。
              </p>
            </div>
          </section>
        </DialogLayer>
      )}
      {create === 'manual' && tenantId && (
        <Suspense fallback={null}>
          <ExternalOutboundEditor
            tenantId={tenantId}
            existing={null}
            existingIds={new Set(tunnels.map(tunnel => tunnel.id))}
            purpose="resource"
            onClose={() => setCreate(null)}
            onSaved={tunnel => {
              setCreate(null);
              go({ p: 'custom', id: tunnel.id });
            }}
          />
        </Suspense>
      )}
      {create === 'warp' && tenantId && (
        <WarpCreate
          tenantId={tenantId}
          existingIds={new Set(tunnels.map(tunnel => tunnel.id))}
          onClose={() => setCreate(null)}
          onCreated={tunnel => {
            setCreate(null);
            go({ p: 'warp', id: tunnel.id });
          }}
        />
      )}
    </div>
  );
}

export function WarpCreate({
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
  const [id] = useState(() => randomWarpTunnelId(existingIds));
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
  const guardScope = `warp-create:${id}`;
  const dirty =
    name !== 'Cloudflare WARP' ||
    address !== 'engage.cloudflareclient.com' ||
    port !== '2408' ||
    mtu !== '1280' ||
    keepAlive !== '25' ||
    ipStack !== 'dual' ||
    noKernelTun ||
    workers !== '0';
  const clearUnsavedChanges = useUnsavedChanges(dirty, '新建 WARP 隧道', guardScope);
  const valid =
    !!tenantId &&
    !!name.trim() &&
    /^[a-z0-9][a-z0-9_-]{0,63}$/.test(id) &&
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
    if (!valid) return;
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
      clearUnsavedChanges();
      onCreated(tunnel);
    } catch (next) {
      setError(next);
    } finally {
      setSaving(false);
    }
  };

  return (
    <DialogLayer
      label="创建 Cloudflare WARP"
      mode="drawer"
      onClose={onClose}
      canClose={() => confirmDiscardChanges(guardScope)}
    >
      <section className="dialog-surface dialog-drawer external-outbound-drawer warp-create-drawer">
        <header>
          <b>创建 Cloudflare WARP</b>
          <small>免费 WARP · WireGuard 兼容</small>
          <span className="sp" />
          <DialogClose className="btn">关闭</DialogClose>
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
            <div className="row">
              <span className="k">Endpoint</span>
              <span className="v tunnel-endpoint-fields">
                <input
                  className="f mono"
                  aria-label="Endpoint 地址"
                  value={address}
                  onChange={event => setAddress(event.target.value)}
                />
                <input
                  className="f mono"
                  type="number"
                  aria-label="Endpoint 端口"
                  value={port}
                  onChange={event => setPort(event.target.value)}
                />
              </span>
            </div>
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
              <b>在线路中选择</b>
              <small>由规则确定实际需要 WARP 身份的机器。</small>
            </p>
            <span>3</span>
            <p>
              <b>就地申请身份</b>
              <small>规则旁直接申请当前机器的一对密钥与 Cloudflare 设备。</small>
            </p>
            <span>4</span>
            <p>
              <b>发布并收敛</b>
              <small>发布页校验后下发 Xray，以 Agent 收敛结果为准。</small>
            </p>
          </div>
          {error !== null && <ErrorBox error={error} />}
        </div>
        <footer>
          <span className="note">本步不调用 Cloudflare，也不会产生孤儿设备。</span>
          <span className="sp" />
          <DialogClose className="btn">取消</DialogClose>
          <button className="btn primary" disabled={!valid || saving} onClick={() => void save()}>
            {saving ? '创建中…' : '创建到草稿'}
          </button>
        </footer>
      </section>
    </DialogLayer>
  );
}

type TunnelReference = {
  app: string;
  project: string;
  node: string;
  chain: string;
  chainId: string;
};

type WarpPhase = 'draft' | 'missing' | 'unused' | 'configured' | 'ready' | 'rolling' | 'live';

const hasWarpBindingOverrides = (binding: ExternalWarpBinding) =>
  binding.endpoint_address != null ||
  binding.endpoint_port != null ||
  binding.mtu != null ||
  binding.keep_alive != null ||
  binding.allowed_ips != null ||
  binding.domain_strategy != null ||
  binding.no_kernel_tun != null ||
  binding.workers != null;

function WarpTunnelDetail({
  tunnel,
  nodes,
  apps,
  references,
  editable,
  deploymentsReadable,
  editing,
  deleting,
  onEdit,
  onDelete,
  onCloseEdit,
  onCloseDelete,
}: {
  tunnel: ExternalOutbound;
  nodes: NodeAgentStateItem[];
  apps: SnapshotApp[];
  references: TunnelReference[];
  editable: boolean;
  deploymentsReadable: boolean;
  editing: boolean;
  deleting: boolean;
  onEdit: () => void;
  onDelete: () => void;
  onCloseEdit: () => void;
  onCloseDelete: () => void;
}) {
  const availability = useWarpRegistrationAvailability(tunnel);
  const deployments = useQuery({
    queryKey: ['deployments', 'config'],
    queryFn: () => fetchDeployments('config'),
    enabled: deploymentsReadable,
  });
  const [registrationNode, setRegistrationNode] = useState<string | null>(null);
  const [registrationChoice, setRegistrationChoice] = useState('');
  if (tunnel.protocol.t !== 'warp') return null;
  const warpProtocol = tunnel.protocol;
  const nodeById = new Map(nodes.map(node => [node.node_id, node]));
  const nodeNameOf = (nodeId: string) => nodeById.get(nodeId)?.name || nodeId;
  const referencesByNode = (nodeId: string) => references.filter(reference => reference.node === nodeId);
  const referenceNodes = [...new Set(references.map(reference => reference.node))];
  const boundNodes = new Set(tunnel.bindings.map(binding => binding.node));
  const missing = referenceNodes.filter(node => !boundNodes.has(node));
  const using = tunnel.bindings.filter(binding => referencesByNode(binding.node).length > 0);
  const idle = tunnel.bindings.filter(binding => referencesByNode(binding.node).length === 0);
  const spare = nodes.filter(
    node =>
      node.tenant_id === tunnel.tenant &&
      !node.retired_at &&
      !boundNodes.has(node.node_id) &&
      !missing.includes(node.node_id),
  );
  const release = (deployments.data?.deployments ?? []).reduce<DeploymentListItem | null>(
    (latest, candidate) =>
      candidate.revision_id === availability.committedRevision && (!latest || candidate.id > latest.id)
        ? candidate
        : latest,
    null,
  );
  const readyForRelease = availability.committed && references.length > 0 && missing.length === 0;
  const converged =
    readyForRelease &&
    release?.status === 'succeeded' &&
    release.activation_status === 'activated' &&
    release.settlement_status === 'converged';
  const releaseInProgress =
    release != null &&
    !converged &&
    release.status !== 'canceled' &&
    release.status !== 'halted' &&
    release.activation_status !== 'rejected';
  const phase: WarpPhase = !availability.committed
    ? 'draft'
    : missing.length > 0
      ? 'missing'
      : references.length === 0
        ? 'unused'
        : !deploymentsReadable
          ? 'configured'
          : converged
            ? 'live'
            : releaseInProgress
              ? 'rolling'
              : 'ready';
  const overriddenUsing = using.filter(hasWarpBindingOverrides).length;
  const following = tunnel.bindings.filter(binding => !hasWarpBindingOverrides(binding)).length;

  const phaseChip: Record<WarpPhase, { className: string; text: string }> = {
    draft: { className: 'st-pending', text: availability.checking ? '确认中' : '未提交' },
    missing: { className: 'st-gold', text: `${missing.length} 台待注册` },
    unused: { className: 'st-pending', text: '未引用' },
    configured: { className: 'st-pending', text: '已配置' },
    ready: { className: 'st-gold', text: '待发布' },
    rolling: { className: 'st-running', text: '发布中' },
    live: { className: 'st-succeeded', text: '已发布' },
  };
  const releaseText: Record<WarpPhase, { className: string; text: string }> = {
    draft: { className: 'dim', text: '—' },
    missing: { className: 'warn', text: '当前修订不可发布' },
    unused: { className: 'dim', text: '—' },
    configured: { className: 'dim', text: '—' },
    ready: { className: 'warn', text: '待发布' },
    rolling: { className: 'act', text: release ? `#${release.id} 发布中` : '发布中' },
    live: { className: 'ok', text: release ? `#${release.id} 已收敛` : '已收敛' },
  };

  const openRegistration = (nodeId: string) => {
    setRegistrationNode(nodeId);
    document.getElementById('warp-machine-registration')?.scrollIntoView({ behavior: 'smooth', block: 'start' });
  };
  const primaryAction = (() => {
    if (phase === 'draft')
      return {
        label: '查看变更集',
        onClick: () => document.querySelector<HTMLElement>('.fg-draft')?.scrollIntoView({ behavior: 'smooth' }),
      };
    if (phase === 'missing')
      return { label: `为 ${nodeNameOf(missing[0])} 申请身份`, onClick: () => openRegistration(missing[0]) };
    if (phase === 'unused') return { label: '前往线路', onClick: () => navigate('chains') };
    if (phase === 'configured') return null;
    if (phase === 'ready') return { label: '前往发布', onClick: () => navigate('deploy', { p: 'plan' }) };
    if (phase === 'rolling' && release)
      return { label: '查看变更单', onClick: () => navigate('deploy', { p: 'detail', id: release.id }) };
    return null;
  })();

  const selectedSpare =
    registrationChoice && spare.some(node => node.node_id === registrationChoice)
      ? registrationChoice
      : spare[0]?.node_id || '';

  return (
    <div className="nd-sheet nd-page cg-page cg-detail wp-page">
      <div className="fg-sheet nd-paper">
        <header className="nd-page-head cg-head">
          <div className="nd-page-identity">
            <div className="nd-ident-text">
              <div className="nd-ident-row wp-title-row">
                <span className="cg-plate wp-title-icon" aria-hidden>
                  <Icon of="tunnels" size={18} className="detail-title-glyph" />
                </span>
                <h1 className="nd-id nd-name">{tunnel.name}</h1>
                <span className={`st ${phaseChip[phase].className}`}>{phaseChip[phase].text}</span>
              </div>
            </div>
          </div>

          <div className="nd-acts">
            {primaryAction && (
              <button className="btn primary" type="button" onClick={primaryAction.onClick}>
                {primaryAction.label}
              </button>
            )}
            <button className="btn" type="button" disabled={!editable} onClick={onEdit}>
              编辑
            </button>
            <button className="btn danger" type="button" disabled={!editable} onClick={onDelete}>
              删除
            </button>
          </div>
        </header>

        <div className="nd-paper-body cg-body">
          <div className="cg-main">
            <section className="panel config-panel cg-sec wp-machines" id="warp-machine-registration">
              <header>
                <PanelTitle of="nodes">机器身份</PanelTitle>
                <span className="cg-meta">
                  <span>
                    <b>{tunnel.bindings.length}</b> 台
                    {missing.length > 0 && (
                      <>
                        {' · '}
                        <b className="warn">{missing.length}</b> 台待注册
                      </>
                    )}
                  </span>
                </span>
              </header>
              <div className="wp-register-bar">
                <span className="wp-register-label">新增身份</span>
                <div className="wp-register-controls">
                  <select
                    className="f wp-pick"
                    aria-label="待注册机器"
                    value={selectedSpare}
                    disabled={spare.length === 0 || registrationNode != null}
                    onChange={event => setRegistrationChoice(event.target.value)}
                  >
                    {spare.length === 0 ? (
                      <option value="">没有可注册机器</option>
                    ) : (
                      spare.map(node => (
                        <option key={node.node_id} value={node.node_id}>{`${node.name} · ${node.node_id}`}</option>
                      ))
                    )}
                  </select>
                  <button
                    className="btn primary"
                    type="button"
                    aria-label="注册机器"
                    title={availability.blockedReason ?? undefined}
                    disabled={!editable || !availability.committed || !selectedSpare || registrationNode != null}
                    onClick={() => setRegistrationNode('__new')}
                  >
                    注册
                  </button>
                </div>
              </div>

              {registrationNode === '__new' && selectedSpare && (
                <div className="wp-reg-row wp-reg-detail-row">
                  <WarpRegistrationAction
                    tunnel={tunnel}
                    nodeId={selectedSpare}
                    nodeName={nodeNameOf(selectedSpare)}
                    editable={editable}
                    context="detail"
                    onRegistered={() => {
                      setRegistrationChoice('');
                      setRegistrationNode(null);
                    }}
                  />
                  <button className="btn wp-reg-cancel" type="button" onClick={() => setRegistrationNode(null)}>
                    取消
                  </button>
                </div>
              )}

              {(missing.length > 0 || using.length > 0 || idle.length > 0) && (
                <div className="cgr-waves">
                  {missing.length > 0 && (
                    <div className="cgr-group is-open">
                      <div className="cgr-head">
                        <b>待注册</b>
                        <span className="lbl">规则已引用，没有 WARP 身份</span>
                        <span className="cgr-facts">{missing.length} 台</span>
                        <span className="cgr-state warn">当前修订不可发布</span>
                      </div>
                      {missing.map(nodeId => {
                        const open = registrationNode === nodeId;
                        const nodeReferences = referencesByNode(nodeId);
                        return (
                          <div
                            className={`cgr-row is-run wp-mrow wp-warn${open ? ' wp-open' : ''}`}
                            key={nodeId}
                            role="button"
                            tabIndex={0}
                            aria-expanded={open}
                            onClick={event => {
                              if ((event.target as HTMLElement).closest('.wp-detail')) return;
                              setRegistrationNode(open ? null : nodeId);
                            }}
                            onKeyDown={event => {
                              if (
                                (event.key === 'Enter' || event.key === ' ') &&
                                event.target === event.currentTarget
                              ) {
                                event.preventDefault();
                                setRegistrationNode(open ? null : nodeId);
                              }
                            }}
                          >
                            <span className="cg-lamp warn" aria-hidden />
                            <span className="cgo-main">
                              <b>
                                <RegionFlag code={nodeById.get(nodeId)?.public_ipv4_country} />
                                {nodeNameOf(nodeId)}
                              </b>
                              <small>{`${nodeId} · ${nodeReferences.map(reference => reference.chain).join('、')}`}</small>
                            </span>
                            <span className="cgo-state">
                              {!open && (
                                <button
                                  className="btn primary wp-act"
                                  type="button"
                                  onClick={() => setRegistrationNode(nodeId)}
                                >
                                  申请身份
                                </button>
                              )}
                            </span>
                            <span className="cgo-when">—</span>
                            {open && (
                              <div className="wp-detail">
                                <WarpRegistrationAction
                                  tunnel={tunnel}
                                  nodeId={nodeId}
                                  nodeName={nodeNameOf(nodeId)}
                                  editable={editable}
                                  context="detail"
                                  onRegistered={() => setRegistrationNode(null)}
                                />
                                <button
                                  className="btn wp-reg-cancel"
                                  type="button"
                                  onClick={() => setRegistrationNode(null)}
                                >
                                  取消
                                </button>
                              </div>
                            )}
                          </div>
                        );
                      })}
                    </div>
                  )}

                  {using.length > 0 && (
                    <div className="cgr-group">
                      <div className="cgr-head">
                        <b>使用中</b>
                        <span className="lbl">被规则引用</span>
                        <span className="cgr-facts">
                          {using.length} 台 ·{' '}
                          {overriddenUsing ? `${overriddenUsing} 台覆盖默认参数` : '全部跟随默认参数'}
                        </span>
                      </div>
                      {using.map(binding => (
                        <WarpBindingCard
                          key={binding.node}
                          tenantId={tunnel.tenant}
                          outboundId={tunnel.id}
                          binding={binding}
                          nodeName={nodeNameOf(binding.node)}
                          countryCode={nodeById.get(binding.node)?.public_ipv4_country}
                          referenceLabels={referencesByNode(binding.node).map(reference => reference.chain)}
                          defaultAddress={tunnel.address}
                          defaultPort={tunnel.port}
                          defaults={warpProtocol}
                          editable={editable}
                          removalBlockedReason="这台机器仍被规则引用。请先解除引用并完成发布，再注销身份。"
                        />
                      ))}
                    </div>
                  )}

                  {idle.length > 0 && (
                    <div className="cgr-group">
                      <div className="cgr-head">
                        <b>未引用</b>
                        <span className="lbl">已注册身份，未被规则使用</span>
                        <span className="cgr-facts">{idle.length} 台</span>
                      </div>
                      {idle.map(binding => (
                        <WarpBindingCard
                          key={binding.node}
                          tenantId={tunnel.tenant}
                          outboundId={tunnel.id}
                          binding={binding}
                          nodeName={nodeNameOf(binding.node)}
                          countryCode={nodeById.get(binding.node)?.public_ipv4_country}
                          defaultAddress={tunnel.address}
                          defaultPort={tunnel.port}
                          defaults={warpProtocol}
                          editable={editable}
                          removalBlockedReason={!draft.isEmpty() ? '先提交或丢弃当前草稿，再注销身份。' : undefined}
                        />
                      ))}
                    </div>
                  )}
                </div>
              )}
            </section>

            <section className="panel config-panel cg-sec wp-refs">
              <header>
                <PanelTitle of="chains">规则引用</PanelTitle>
                {references.length > 0 && (
                  <span className="cg-meta">
                    <span>
                      <b>{references.length}</b> 条规则 · <b>{referenceNodes.length}</b> 台机器
                    </span>
                  </span>
                )}
              </header>
              {references.length > 0 ? (
                <div className="wp-refrows">
                  {references.map(reference => {
                    const ready = boundNodes.has(reference.node);
                    return (
                      <button
                        className="cgr-row wp-ref"
                        type="button"
                        key={`${reference.app}/${reference.chainId}/${reference.node}`}
                        onClick={() => navigate('chains', { p: 'chain', app: reference.app, chain: reference.chainId })}
                      >
                        <span className={`cg-lamp ${ready ? 'ok' : 'warn'}`} aria-hidden />
                        <span className="cgo-main">
                          <b>{reference.chain}</b>
                          <small>{reference.project}</small>
                        </span>
                        <span
                          className={`cgr-effect wp-refnode${ready ? '' : ' risk'}`}
                          title={ready ? undefined : '没有 WARP 身份'}
                        >
                          <RegionFlag code={nodeById.get(reference.node)?.public_ipv4_country} />
                          {nodeNameOf(reference.node)}
                          <span className="mono">{reference.node}</span>
                          <span className="wp-go" aria-hidden>
                            ›
                          </span>
                        </span>
                      </button>
                    );
                  })}
                </div>
              ) : (
                <div className="cgr-names wp-empty">
                  暂无规则引用
                  <button className="btn" type="button" onClick={() => navigate('chains')}>
                    前往线路
                  </button>
                </div>
              )}
            </section>
          </div>

          <aside className="cg-aside">
            <section className="panel config-panel cg-sec wp-defaults">
              <header>
                <PanelTitle of="tunnels">隧道</PanelTitle>
              </header>
              <dl className="cg-kv">
                <dt>协议</dt>
                <dd>Cloudflare WARP</dd>
                <dt>修订</dt>
                <dd className={availability.committed ? '' : 'dim'}>
                  {availability.committed ? `r${availability.committedRevision ?? '—'}` : '仅在变更集'}
                </dd>
                <dt>发布</dt>
                <dd className={releaseText[phase].className}>{releaseText[phase].text}</dd>
                <dt>机器身份</dt>
                <dd className={missing.length > 0 ? 'warn' : ''}>
                  {tunnel.bindings.length} 台{missing.length > 0 ? ` · ${missing.length} 台待注册` : ''}
                </dd>
              </dl>
              <div className="cgo-label">
                <b>默认参数</b>
                {tunnel.bindings.length > 0 && (
                  <span className="wp-rd">
                    <b>{following}</b> 台跟随 · <b>{tunnel.bindings.length - following}</b> 台覆盖
                  </span>
                )}
              </div>
              <dl className="cg-kv wp-spec">
                <dt>Endpoint</dt>
                <dd>
                  {tunnel.address}
                  <span className="port">:{tunnel.port}</span>
                </dd>
                <dt>地址策略</dt>
                <dd className="t">{warpIpStackLabel(warpIpStackOf(warpProtocol))}</dd>
                <dt>MTU</dt>
                <dd>{warpProtocol.v.mtu}</dd>
                <dt>Keepalive</dt>
                <dd>
                  {warpProtocol.v.keep_alive}
                  <span className="u"> s</span>
                </dd>
                <dt>TUN</dt>
                <dd className="t">{warpProtocol.v.no_kernel_tun ? '仅用户态' : '系统优先'}</dd>
                <dt>Workers</dt>
                <dd className="t">{warpProtocol.v.workers || '自动'}</dd>
              </dl>
            </section>
          </aside>
        </div>
      </div>

      {editing && <WarpEdit tunnel={tunnel} onClose={onCloseEdit} />}
      {deleting && (
        <Suspense fallback={null}>
          <TunnelDeleteDialog
            outbound={tunnel}
            apps={apps}
            onClose={onCloseDelete}
            onDeleted={() => returnTo('tunnels')}
          />
        </Suspense>
      )}
    </div>
  );
}

function TunnelDetail({ tunnelId }: { tunnelId: string }) {
  const { who } = useSession();
  const editable = can(who.role, 'edit');
  const snapshot = useQuery({ queryKey: ['snapshot'], queryFn: fetchSnapshot });
  const nodes = useQuery({ queryKey: ['nodes'], queryFn: () => fetchNodes() });
  const [editing, setEditing] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const tunnel = (snapshot.data?.snapshot.external_outbounds ?? []).find(candidate => candidate.id === tunnelId);
  const probeEnabled = tunnel != null && !isVpngateOutbound(tunnel) && tunnel.protocol.t !== 'warp';
  const initialProbe = useQuery({
    queryKey: ['tunnel-probe', tunnel?.tenant, tunnel?.id, 86_400],
    queryFn: () => fetchTunnelProbe(tunnel!.tenant, tunnel!.id, 86_400),
    enabled: probeEnabled,
    staleTime: 15_000,
    retry: false,
  });
  const initialProbeCapability = useQuery({
    queryKey: ['tunnel-probe-capability'],
    queryFn: fetchTunnelProbeCapability,
    enabled: probeEnabled && editable,
    staleTime: 60_000,
    retry: false,
  });
  const apps = snapshot.data?.snapshot.apps ?? [];
  const nodeList = (nodes.data?.nodes ?? []).filter(node => node.tenant_id === tunnel?.tenant && !node.retired_at);
  const nodeName = new Map((nodes.data?.nodes ?? []).map(node => [node.node_id, node.name]));
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
  /* 规则引用跳转到对应线路，并把位置写入地址栏以支持刷新和浏览器后退。 */
  const openChain = (appId: string, chainId: string) => {
    navigate('chains', { p: 'chain', app: appId, chain: chainId });
  };
  if (
    snapshot.isPending ||
    nodes.isPending ||
    (probeEnabled && initialProbe.isPending) ||
    (probeEnabled && editable && initialProbeCapability.isPending)
  )
    return <Loading variant="config-detail" sheeted />;
  if (snapshot.error) return <ErrorBox error={snapshot.error} />;
  if (nodes.error) return <ErrorBox error={nodes.error} />;
  if (!tunnel)
    return (
      <Empty>
        这条隧道不存在，可能已在草稿中移除。
        <button className="btn" onClick={() => returnTo('tunnels')}>
          返回列表
        </button>
      </Empty>
    );

  if (isVpngateOutbound(tunnel)) {
    return (
      <Suspense fallback={<Loading variant="vpngate" />}>
        <VpngatePage
          initialPool={tunnel}
          onCountryChange={country => navigateInPlace('tunnels', { p: 'vpngate', country })}
        />
      </Suspense>
    );
  }

  const isWarp = tunnel.protocol.t === 'warp';
  const usesVlessEncryption = tunnel.protocol.t === 'vless' && tunnel.protocol.v.encryption !== 'none';
  if (isWarp) {
    return (
      <WarpTunnelDetail
        tunnel={tunnel}
        nodes={nodeList}
        apps={apps}
        references={references}
        editable={editable}
        deploymentsReadable={!isVisitor(who)}
        editing={editing}
        deleting={deleting}
        onEdit={() => setEditing(true)}
        onDelete={() => setDeleting(true)}
        onCloseEdit={() => setEditing(false)}
        onCloseDelete={() => setDeleting(false)}
      />
    );
  }
  return (
    <div className="nd-sheet nd-page tunnel-detail-page">
      {/* 自铺 sheet 对齐机器详情：连续纸 + 纸内页头横梁 + 纸身；工作区不再额外包纸。 */}
      <div className="fg-sheet nd-paper">
        <header className="nd-page-head tunnel-detail-head">
          <div className="nd-page-identity">
            <span className="tunnel-detail-icon">
              <Icon of="tunnels" size={15} />
            </span>
            <h1 className="nd-id nd-name">{tunnel.name}</h1>
          </div>
          <div className="tunnel-head-meta">
            <span>{protocolName(tunnel)}</span>
            <span>
              <b>{references.length}</b> 处引用
            </span>
          </div>
          <div className="nd-acts">
            <button className="btn danger" disabled={!editable} onClick={() => setDeleting(true)}>
              删除
            </button>
            <button className="btn" disabled={!editable} onClick={() => setEditing(true)}>
              编辑
            </button>
          </div>
        </header>

        <div className="nd-paper-body tunnel-detail-body">
          {/* 连接事实条：把散落的连接参数收成一行 */}
          <dl className="tunnel-strip">
            <div>
              <dt>Endpoint</dt>
              <dd className="mono">{endpoint(tunnel)}</dd>
            </div>
            <div>
              <dt>协议</dt>
              <dd>{protocolName(tunnel)}</dd>
            </div>
            {usesVlessEncryption ? null : tunnel.protocol.t === 'shadowsocks2022' ? (
              <div>
                <dt>加密方式</dt>
                <dd className="mono">{tunnel.protocol.v.method}</dd>
              </div>
            ) : (
              <div>
                <dt>传输安全</dt>
                <dd>{tunnel.security.t.toUpperCase()}</dd>
              </div>
            )}
          </dl>

          <TunnelProbePanel tenantId={tunnel.tenant} outboundId={tunnel.id} draftSupported editable={editable} />

          {/* 规则引用：每项跳转到对应线路 */}
          <section className="panel config-panel tunnel-panel">
            <header>
              <PanelTitle of="chains">规则引用</PanelTitle>
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

      {editing && (
        <Suspense fallback={null}>
          <ExternalOutboundEditor
            tenantId={tunnel.tenant}
            existing={tunnel}
            purpose="resource"
            onClose={() => setEditing(false)}
            onSaved={() => setEditing(false)}
          />
        </Suspense>
      )}
      {deleting && (
        <Suspense fallback={null}>
          <TunnelDeleteDialog
            outbound={tunnel}
            apps={apps}
            onClose={() => setDeleting(false)}
            onDeleted={() => returnTo('tunnels')}
          />
        </Suspense>
      )}
    </div>
  );
}

export function WarpBindingCard({
  tenantId,
  outboundId,
  binding,
  nodeName,
  countryCode,
  referenceLabels = [],
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
  countryCode?: string | null;
  referenceLabels?: string[];
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
  const customized = hasWarpBindingOverrides(binding);
  const overrideTags: { key?: string; value: string }[] = [];
  if (binding.endpoint_address != null || binding.endpoint_port != null) {
    overrideTags.push({ key: 'Endpoint', value: hostPort(effectiveAddress, effectivePort) });
  }
  if (binding.allowed_ips != null || binding.domain_strategy != null) {
    overrideTags.push({ value: warpIpStackLabel(effectiveIpStack) });
  }
  if (binding.mtu != null) overrideTags.push({ key: 'MTU', value: String(effectiveMtu) });
  if (binding.keep_alive != null) overrideTags.push({ key: 'Keepalive', value: `${effectiveKeepAlive}s` });
  if (binding.no_kernel_tun != null) {
    overrideTags.push({ key: 'TUN', value: effectiveNoKernelTun ? '仅用户态' : '系统优先' });
  }
  if (binding.workers != null) overrideTags.push({ key: 'Workers', value: String(effectiveWorkers || '自动') });
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
  const dirty =
    address.trim() !== effectiveAddress ||
    Number(port) !== effectivePort ||
    Number(mtu) !== effectiveMtu ||
    Number(keepAlive) !== effectiveKeepAlive ||
    ipStack !== effectiveIpStack ||
    noKernelTun !== effectiveNoKernelTun ||
    Number(workers) !== effectiveWorkers;
  const guardScope = `warp-binding:${tenantId}:${outboundId}:${binding.node}`;
  useUnsavedChanges(editing && dirty, `${nodeName} 的 WARP 参数`, guardScope);

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
    if (!dirty) return;
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
    <article
      className={`warp-binding-card cgr-row is-run wp-mrow${editing ? ' editing wp-open' : ''}`}
      role="button"
      tabIndex={0}
      aria-expanded={editing}
      onClick={event => {
        if ((event.target as HTMLElement).closest('.wp-detail')) return;
        if (!editing && editable && !update.isPending && !remove.isPending) begin();
      }}
      onKeyDown={event => {
        if (
          !editing &&
          editable &&
          (event.key === 'Enter' || event.key === ' ') &&
          event.target === event.currentTarget
        ) {
          event.preventDefault();
          begin();
        }
      }}
    >
      <span className={`cg-lamp ${referenceLabels.length > 0 ? 'ok' : 'idle'}`} aria-hidden />
      <span className="cgo-main">
        <b>
          <RegionFlag code={countryCode} />
          {nodeName}
        </b>
        <small title={`${binding.local_addresses.join(' · ')} · 设备 ${binding.device_id}`}>
          {binding.node} · 设备 {binding.device_id.slice(0, 8)}
          {referenceLabels.length > 0 ? ` · ${[...new Set(referenceLabels)].join('、')}` : ''}
        </small>
      </span>
      {overrideTags.length > 0 ? (
        <span className="cgo-state wp-ov">
          {overrideTags.map((tag, index) => (
            <span className="cg-tag" key={`${tag.key ?? 'value'}-${index}`}>
              {tag.key && <i>{tag.key}</i>}
              {tag.value}
            </span>
          ))}
        </span>
      ) : (
        <span className="cgo-state wp-ov idle">跟随默认</span>
      )}
      <span className="cgo-when">{binding.registered_at.slice(5, 10)}</span>
      {editing && (
        <div className="wp-detail warp-binding-editor">
          <div className="wp-fields warp-binding-editor-fields">
            <label className="warp-binding-endpoint-address">
              <span className="k">Endpoint</span>
              <span className="wp-ep">
                <input
                  className="f mono"
                  aria-label="Endpoint 地址"
                  value={address}
                  onChange={event => setAddress(event.target.value)}
                />
                <input
                  className="f mono"
                  aria-label="Endpoint 端口"
                  type="number"
                  min={1}
                  max={65535}
                  value={port}
                  onChange={event => setPort(event.target.value)}
                />
              </span>
            </label>
            <label>
              <span className="k">地址策略</span>
              <WarpIpStackControl value={ipStack} onChange={setIpStack} />
            </label>
            <label>
              <span className="k">MTU</span>
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
              <span className="k">Keepalive（秒）</span>
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
              <span className="k">TUN</span>
              <select
                className="f"
                aria-label="TUN 实现"
                value={noKernelTun ? 'userspace' : 'auto'}
                onChange={event => setNoKernelTun(event.target.value === 'userspace')}
              >
                <option value="auto">系统优先（默认）</option>
                <option value="userspace">仅用户态</option>
              </select>
            </label>
            <label>
              <span className="k">Workers</span>
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
          <p className="wp-id">
            设备 {binding.device_id} · {binding.local_addresses.join(' · ')}
          </p>
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
              {(formError !== null || update.error) && <ErrorBox error={formError ?? update.error} />}
              <div className="wp-ops warp-binding-editor-actions">
                <span className="wp-ops-l">
                  <button
                    className="btn danger"
                    disabled={!editable || update.isPending || Boolean(removalBlockedReason)}
                    title={removalBlockedReason}
                    onClick={beginRemoval}
                  >
                    注销身份
                  </button>
                  {removalBlockedReason && <span className="wp-note warn">{removalBlockedReason}</span>}
                </span>
                <span className="wp-ops-r">
                  <span className="wp-note">{customized ? `覆盖 ${overrideTags.length} 项` : '跟随默认'}</span>
                  {customized && (
                    <button className="btn" disabled={update.isPending} onClick={clear}>
                      取消覆盖
                    </button>
                  )}
                  <button
                    className="btn"
                    disabled={update.isPending}
                    onClick={() => confirmDiscardChanges(guardScope) && setEditing(false)}
                  >
                    取消
                  </button>
                  <button
                    className="btn primary"
                    disabled={update.isPending || !dirty}
                    title={dirty ? undefined : '没有修改'}
                    onClick={save}
                  >
                    {update.isPending ? '保存中…' : '保存'}
                  </button>
                </span>
              </div>
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
  const guardScope = `warp-edit:${tunnel.tenant}:${tunnel.id}`;
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
  const dirty =
    protocol !== null &&
    (name.trim() !== tunnel.name ||
      address.trim() !== tunnel.address ||
      Number(port) !== tunnel.port ||
      Number(mtu) !== protocol.v.mtu ||
      Number(keepAlive) !== protocol.v.keep_alive ||
      ipStack !== warpIpStackOf(protocol) ||
      noKernelTun !== protocol.v.no_kernel_tun ||
      Number(workers) !== protocol.v.workers);
  useUnsavedChanges(dirty, `${tunnel.name} 的 WARP 默认参数`, guardScope);
  if (!protocol) return null;
  const save = async () => {
    if (!valid || !dirty) return;
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
    <DialogLayer label="编辑 WARP 默认参数" onClose={onClose} canClose={() => confirmDiscardChanges(guardScope)}>
      <section className="dialog-surface tunnel-dialog narrow">
        <header>
          <b>编辑 WARP 默认参数</b>
          <span className="sp" />
          <DialogClose className="btn">关闭</DialogClose>
        </header>
        <div className="tunnel-dialog-body">
          <div className="fgrid one warp-create-fields">
            <label className="row">
              <span className="k">名称</span>
              <span className="v">
                <input className="f" value={name} onChange={event => setName(event.target.value)} />
              </span>
            </label>
            <div className="row">
              <span className="k">默认 Endpoint</span>
              <span className="v tunnel-endpoint-fields">
                <input
                  className="f mono"
                  aria-label="默认 Endpoint 地址"
                  value={address}
                  onChange={event => setAddress(event.target.value)}
                />
                <input
                  className="f mono"
                  type="number"
                  min={1}
                  max={65535}
                  aria-label="默认 Endpoint 端口"
                  value={port}
                  onChange={event => setPort(event.target.value)}
                />
              </span>
            </div>
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
          <DialogClose className="btn">取消</DialogClose>
          <button
            className="btn primary"
            disabled={saving || !valid || !dirty}
            title={dirty ? undefined : '没有修改'}
            onClick={() => void save()}
          >
            {saving ? '保存中…' : '保存到草稿'}
          </button>
        </footer>
      </section>
    </DialogLayer>
  );
}
