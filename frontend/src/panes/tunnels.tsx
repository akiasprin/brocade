import { lazy } from '../ui/lazy';
import { Fragment, Suspense, useMemo, useState, useSyncExternalStore } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  ApiError,
  fetchDeployments,
  fetchNodes,
  fetchSnapshot,
  fetchTunnelProbeCapability,
  removeWarpBinding,
  updateWarpBinding,
  upsertExternalOutbound,
  type DeploymentListItem,
  type ConsoleSnapshot,
  type ExternalOutbound,
  type ExternalVlessTransport,
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
import {
  TunnelProbePanel,
  tunnelExitRun,
  tunnelProbeStatus,
  useTunnelProbeChart,
  useTunnelProbeView,
  type TunnelProbeStatus,
  type TunnelProbeTone,
} from '../tunnel-probe';
import { CopyButton, copyStateIcon } from '../ui/copy-button';
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
  if (selectedId) return <TunnelDetail key={selectedId} tunnelId={selectedId} />;
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
      <section className="panel titled tunnel-list-panel" data-page-title="true">
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
  const [id] = useState(() => randomWarpTunnelId(existingIds));
  return <WarpMasqueEditor tenantId={tenantId} id={id} onClose={onClose} onSaved={onCreated} />;
}

function WarpMasqueEditor({
  tenantId,
  id,
  tunnel,
  onClose,
  onSaved,
}: {
  tenantId: string;
  id: string;
  tunnel?: ExternalOutbound;
  onClose: () => void;
  onSaved: (tunnel: ExternalOutbound) => void;
}) {
  const qc = useQueryClient();
  const initial = tunnel?.protocol.t === 'warp' ? tunnel.protocol.v : null;
  const [name, setName] = useState(tunnel?.name ?? 'Cloudflare WARP');
  const [address, setAddress] = useState(tunnel?.address ?? '162.159.198.2');
  const [port, setPort] = useState(String(tunnel?.port ?? 443));
  const [httpVersion, setHttpVersion] = useState<'h3' | 'h2'>(initial?.http_version ?? 'h3');
  const [dns, setDns] = useState((initial?.remote_dns ?? ['1.1.1.1', '1.0.0.1']).join(', '));
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const guardScope = `warp-masque:${tenantId}:${id}`;
  const dirty =
    name !== (tunnel?.name ?? 'Cloudflare WARP') ||
    address !== (tunnel?.address ?? '162.159.198.2') ||
    port !== String(tunnel?.port ?? 443) ||
    httpVersion !== (initial?.http_version ?? 'h3') ||
    dns !== (initial?.remote_dns ?? ['1.1.1.1', '1.0.0.1']).join(', ');
  const clearUnsavedChanges = useUnsavedChanges(dirty, 'WARP MASQUE 参数', guardScope);
  const valid =
    !!tenantId &&
    !!name.trim() &&
    !!address.trim() &&
    !/\s/.test(address) &&
    Number.isInteger(Number(port)) &&
    Number(port) > 0 &&
    Number(port) <= 65535 &&
    !!dns.trim();
  const save = async () => {
    if (!valid) return;
    setSaving(true);
    setError(null);
    const value: ExternalOutbound = {
      id,
      tenant: tenantId,
      name: name.trim(),
      address: address.trim(),
      port: Number(port),
      protocol: {
        t: 'warp',
        v: {
          http_version: httpVersion,
          remote_dns: dns.split(/[,\s]+/).filter(Boolean),
        },
      },
      security: { t: 'none' },
      bindings: tunnel?.bindings ?? [],
    };
    try {
      await upsertExternalOutbound({
        id,
        tenant_id: tenantId,
        name: value.name,
        address: value.address,
        port: value.port,
        protocol: value.protocol,
        security: value.security,
      });
      await Promise.all([
        qc.invalidateQueries({ queryKey: ['snapshot'] }),
        qc.invalidateQueries({ queryKey: ['compile'] }),
      ]);
      clearUnsavedChanges();
      onSaved(value);
    } catch (next) {
      setError(next);
    } finally {
      setSaving(false);
    }
  };
  return (
    <DialogLayer
      label={tunnel ? '编辑 WARP 默认参数' : '创建 Cloudflare WARP'}
      mode="drawer"
      onClose={onClose}
      canClose={() => confirmDiscardChanges(guardScope)}
    >
      <section className="dialog-surface dialog-drawer external-outbound-drawer warp-create-drawer">
        <header>
          <b>{tunnel ? '编辑 WARP 默认参数' : '创建 Cloudflare WARP'}</b>
          <small>MASQUE</small>
          <span className="sp" />
          <DialogClose className="btn">关闭</DialogClose>
        </header>
        <div className="external-outbound-body">
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
                  min={1}
                  max={65535}
                  aria-label="Endpoint 端口"
                  value={port}
                  onChange={event => setPort(event.target.value)}
                />
              </span>
            </div>
            <label className="row">
              <span className="k">HTTP 版本</span>
              <span className="v">
                <select
                  className="f"
                  value={httpVersion}
                  onChange={event => setHttpVersion(event.target.value as 'h3' | 'h2')}
                >
                  <option value="h3">HTTP/3 · QUIC / UDP</option>
                  <option value="h2">HTTP/2 · TLS / TCP</option>
                </select>
                <span className="sub">UDP 受限时可选 HTTP/2；不会自动切换。两种方式都承载 TCP 和 UDP 业务。</span>
              </span>
            </label>
            <label className="row">
              <span className="k">隧道内 DNS</span>
              <span className="v">
                <input className="f mono" value={dns} onChange={event => setDns(event.target.value)} />
                <span className="sub">填写 1–8 个 IP，以逗号分隔；通过 WARP 隧道查询。</span>
              </span>
            </label>
          </div>
          <p className="tunnel-provider-note">
            {tunnel
              ? '修改保存到草稿，发布后生效。每台机器可独立覆盖 Endpoint。'
              : '提交草稿后，为使用隧道的机器申请 P-256 设备身份，再发布配置。旧版 WARP 身份需要重新注册。'}
          </p>
          {error !== null && <ErrorBox error={error} />}
        </div>
        <footer>
          <span className="sp" />
          <DialogClose className="btn">取消</DialogClose>
          <button
            className="btn primary"
            disabled={saving || !valid || (!!tunnel && !dirty)}
            onClick={() => void save()}
          >
            {saving ? '保存中…' : tunnel ? '保存到草稿' : '创建隧道'}
          </button>
        </footer>
      </section>
    </DialogLayer>
  );
}

type TunnelReference = { app: string; project: string; node: string; chain: string; chainId: string };

type WarpPhase = 'draft' | 'missing' | 'unused' | 'configured' | 'ready' | 'rolling' | 'live';

const hasWarpBindingOverrides = (binding: ExternalWarpBinding) =>
  binding.endpoint_address != null || binding.endpoint_port != null;

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
                <dt>传输</dt>
                <dd>MASQUE / {warpProtocol.v.http_version === 'h3' ? 'HTTP/3' : 'HTTP/2'}</dd>
                <dt>隧道内 DNS</dt>
                <dd>{warpProtocol.v.remote_dns.join(', ')}</dd>
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

/** One row of a custom tunnel's connection table. Credentials arrive sealed and are only named. */
type ConnectionFact = { label: string; value: string; copy?: boolean; dim?: boolean };

const TRANSPORT_NAME: Record<ExternalVlessTransport['t'], string> = {
  raw: 'TCP',
  kcp: 'mKCP',
  websocket: 'WebSocket',
  http: 'HTTP/2',
  grpc: 'gRPC',
  http_upgrade: 'HTTPUpgrade',
  xhttp: 'XHTTP',
};

const sealedText = (credential: string) => (credential ? '已密封' : '未设置');

const usesVlessEncryption = (tunnel: ExternalOutbound) =>
  tunnel.protocol.t === 'vless' && tunnel.protocol.v.encryption !== 'none';

/** VLESS Encryption, Shadowsocks and WireGuard secure the payload themselves; the rest name the
 * TLS or REALITY layer they ride on. */
const carriesTransportSecurity = (tunnel: ExternalOutbound) =>
  !usesVlessEncryption(tunnel) && tunnel.protocol.t !== 'shadowsocks2022' && tunnel.protocol.t !== 'wireguard';

/** Protocol, transport and security in the order a share link states them, for the page header. */
function tunnelStackText(tunnel: ExternalOutbound): string {
  const parts = [protocolName(tunnel)];
  if (tunnel.protocol.t === 'vless') parts.push(TRANSPORT_NAME[tunnel.protocol.v.transport.t]);
  if (carriesTransportSecurity(tunnel) && tunnel.security.t !== 'none') parts.push(tunnel.security.t.toUpperCase());
  return parts.join(' · ');
}

function connectionFacts(tunnel: ExternalOutbound): ConnectionFact[] {
  const facts: ConnectionFact[] = [
    { label: '地址', value: endpoint(tunnel), copy: true },
    { label: '协议', value: protocolName(tunnel) },
  ];
  const protocol = tunnel.protocol;
  switch (protocol.t) {
    case 'vless': {
      facts.push({ label: 'UUID', value: sealedText(protocol.v.credential), dim: true });
      if (protocol.v.encryption !== 'none') facts.push({ label: '加密', value: protocol.v.encryption.split('.')[0] });
      if (protocol.v.flow) facts.push({ label: '流控', value: protocol.v.flow });
      const transport = protocol.v.transport;
      facts.push({ label: '传输', value: TRANSPORT_NAME[transport.t] });
      if (transport.t === 'grpc') {
        facts.push({ label: '服务名', value: transport.v.service_name });
      } else if (transport.t !== 'raw' && transport.t !== 'kcp') {
        facts.push({ label: '路径', value: transport.v.path, copy: true });
        if (transport.v.host) facts.push({ label: 'Host', value: transport.v.host });
      }
      break;
    }
    case 'shadowsocks2022':
      facts.push(
        { label: '加密方式', value: protocol.v.method },
        { label: '密码', value: sealedText(protocol.v.credential), dim: true },
      );
      break;
    case 'socks5':
    case 'http_connect':
      if (protocol.v.username) facts.push({ label: '用户名', value: protocol.v.username });
      facts.push({ label: '密码', value: sealedText(protocol.v.credential), dim: true });
      break;
    case 'anytls':
      facts.push({ label: '密码', value: sealedText(protocol.v.credential), dim: true });
      break;
    case 'wireguard':
      facts.push(
        { label: '私钥', value: sealedText(protocol.v.credential), dim: true },
        { label: '对端公钥', value: protocol.v.peer_public_key, copy: true },
        { label: '本地地址', value: protocol.v.local_addresses.join(', ') || '—' },
        { label: 'MTU', value: String(protocol.v.mtu) },
      );
      break;
    case 'warp':
    case 'vpngate':
      // Both have their own detail pages.
      break;
  }
  if (carriesTransportSecurity(tunnel)) {
    const security = tunnel.security;
    facts.push({ label: '传输安全', value: security.t === 'none' ? '无' : security.t.toUpperCase() });
    if (security.t !== 'none') {
      facts.push({ label: 'SNI', value: security.v.server_name || '—' });
      facts.push({ label: '指纹', value: security.v.fingerprint || '—' });
    }
    if (security.t === 'tls' && security.v.alpn?.length)
      facts.push({ label: 'ALPN', value: security.v.alpn.join(', ') });
    if (security.t === 'reality') {
      facts.push({ label: '公钥', value: security.v.public_key, copy: true });
      if (security.v.short_id) facts.push({ label: 'Short ID', value: security.v.short_id });
    }
  }
  return facts;
}

const HEAD_CHIP: Partial<Record<TunnelProbeStatus['key'], string>> = {
  degraded: 'st-gold',
  down: 'st-halted',
  paused: 'st-pending',
  unknown: 'st-pending',
  unpublished: 'st-pending',
};
const NODE_LAMP: Record<TunnelProbeTone, string> = { ok: '', warn: 'warn', err: 'bad', idle: 'idle' };

function TunnelDetail({ tunnelId }: { tunnelId: string }) {
  const { who } = useSession();
  const editable = can(who.role, 'edit');
  const snapshot = useQuery({ queryKey: ['snapshot'], queryFn: fetchSnapshot });
  const nodes = useQuery({ queryKey: ['nodes'], queryFn: () => fetchNodes() });
  const draftEntries = useSyncExternalStore(draft.subscribe, draft.snapshot);
  const [editing, setEditing] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const [windowSecs, setWindowSecs] = useState(86_400);
  const tunnel = (snapshot.data?.snapshot.external_outbounds ?? []).find(candidate => candidate.id === tunnelId);
  const probeEnabled = tunnel != null && !isVpngateOutbound(tunnel) && tunnel.protocol.t !== 'warp';
  const probe = useTunnelProbeView(tunnel?.tenant ?? '', tunnel?.id ?? '', windowSecs, probeEnabled);
  const probeCapability = useQuery({
    queryKey: ['tunnel-probe-capability'],
    queryFn: fetchTunnelProbeCapability,
    enabled: probeEnabled && editable,
    staleTime: 60_000,
    retry: false,
  });
  const chart = useTunnelProbeChart(probeEnabled);
  const apps = snapshot.data?.snapshot.apps ?? [];
  const nodeList = (nodes.data?.nodes ?? []).filter(node => node.tenant_id === tunnel?.tenant && !node.retired_at);
  const nodeById = new Map((nodes.data?.nodes ?? []).map(node => [node.node_id, node]));
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
    (probeEnabled && probe.isPending) ||
    (probeEnabled && editable && probeCapability.isPending) ||
    (probeEnabled && chart.status === 'pending')
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

  // The panel reports a failed read itself; the header then claims no status at all.
  const missing = probe.error instanceof ApiError && probe.error.status === 404;
  const view = probe.data ?? null;
  const status = probe.error && !missing ? null : tunnelProbeStatus(missing ? null : view);
  const exit = missing ? null : tunnelExitRun(view);
  const tone = status?.tone ?? 'idle';
  const chip = status ? HEAD_CHIP[status.key] : undefined;
  const draftChanged = draftEntries.some(
    entry =>
      entry.op.op === 'upsert_external_outbound' &&
      entry.op.outbound.tenant_id === tunnel.tenant &&
      entry.op.outbound.id === tunnel.id,
  );
  const referencedChains = new Set(references.map(reference => `${reference.app}\u0000${reference.chainId}`)).size;
  const referencedNodes = new Set(references.map(reference => reference.node)).size;

  return (
    <div className="nd-sheet nd-page tnd-page">
      {/* 自铺 sheet 对齐机器详情：连续纸 + 纸内页头 + 纸身；工作区不再额外包纸。 */}
      <div className="fg-sheet nd-paper">
        <header className="nd-page-head tnd-head">
          <div className="nd-page-identity tnd-identity">
            {/* 出口国旗与机器详情的地区旗同一底板；还没有出口读数时退回隧道图标。 */}
            {exit?.exit_loc ? (
              <span className="nd-idplate" title={`出口 ${exit.exit_loc}`}>
                <span className="nd-idplate-clip">
                  <RegionFlag code={exit.exit_loc} square />
                </span>
                <i className={`node-lamp ${NODE_LAMP[tone]}`} title={status?.word} aria-label={status?.word} />
              </span>
            ) : (
              <span className="cg-plate tnd-plate">
                <Icon of="tunnels" size={15} />
                <i className={`cg-lamp ${tone}`} title={status?.word} aria-label={status?.word} />
              </span>
            )}
            <div className="nd-ident-text">
              <div className="nd-ident-row">
                <h1 className="nd-id nd-name">{tunnel.name}</h1>
                {status && chip && <span className={`st ${chip}`}>{status.word}</span>}
              </div>
              <span className="nd-ident-meta">
                {tunnelStackText(tunnel)} · {endpoint(tunnel)}
              </span>
            </div>
          </div>
          <div className="nd-acts">
            <button className="btn" type="button" disabled={!editable} onClick={() => setEditing(true)}>
              编辑
            </button>
            <button className="btn danger" type="button" disabled={!editable} onClick={() => setDeleting(true)}>
              删除
            </button>
          </div>
        </header>

        <div className="nd-paper-body tnd-body">
          <div className="tnd-main">
            <TunnelProbePanel
              tenantId={tunnel.tenant}
              outboundId={tunnel.id}
              draftSupported
              editable={editable}
              windowSecs={windowSecs}
              onWindowChange={setWindowSecs}
            />

            {/* 规则引用：每项跳转到对应线路 */}
            <section className="panel config-panel cg-sec tnd-refs">
              <header>
                <PanelTitle of="chains">规则引用</PanelTitle>
                {references.length > 0 && (
                  <span className="cg-meta">
                    {status?.key === 'down' ? (
                      <span>
                        <b className="err">{referencedChains}</b> 条线路受影响 · <b>{referencedNodes}</b> 台机器
                      </span>
                    ) : (
                      <span>
                        <b>{referencedChains}</b> 条线路 · <b>{referencedNodes}</b> 台机器
                      </span>
                    )}
                  </span>
                )}
              </header>
              {references.length === 0 ? (
                <p className="tnd-none">尚未被任何规则使用；不会下发到机器。</p>
              ) : (
                <div className="tnd-refrows">
                  {references.map(reference => {
                    const node = nodeById.get(reference.node);
                    return (
                      <button
                        key={`${reference.app}/${reference.chainId}/${reference.node}`}
                        className="tnd-ref"
                        type="button"
                        onClick={() => openChain(reference.app, reference.chainId)}
                      >
                        <span className="cgo-main">
                          <b>{reference.chain}</b>
                          <small>{reference.project}</small>
                        </span>
                        <span className="tnd-refnode">
                          <RegionFlag code={node?.public_ipv4_country} />
                          <span>{node?.name || reference.node}</span>
                          <span className="mono">{reference.node}</span>
                        </span>
                        <span className="tnd-go" aria-hidden>
                          ›
                        </span>
                      </button>
                    );
                  })}
                </div>
              )}
            </section>
          </div>

          <aside className="tnd-aside">
            <section className="panel config-panel cg-sec tnd-conn">
              <header>
                <PanelTitle of="tunnels">连接</PanelTitle>
                {draftChanged ? (
                  <span className="cg-meta warn">草稿已修改</span>
                ) : status?.key === 'unpublished' ? (
                  <span className="cg-meta">仅在草稿</span>
                ) : null}
              </header>
              <dl className="cg-kv tnd-kv">
                {connectionFacts(tunnel).map(fact => (
                  <Fragment key={fact.label}>
                    <dt>{fact.label}</dt>
                    <dd className={fact.dim ? 'dim' : undefined}>
                      <span className="tnd-kv-value" title={fact.value}>
                        {fact.value}
                      </span>
                      {fact.copy && (
                        <CopyButton className="tnd-copy" text={fact.value} iconOnly label={`复制${fact.label}`}>
                          {state => <Icon of={copyStateIcon(state)} size={12} />}
                        </CopyButton>
                      )}
                    </dd>
                  </Fragment>
                ))}
              </dl>
            </section>
          </aside>
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
  editable: boolean;
  removalBlockedReason?: string;
}) {
  const qc = useQueryClient();
  const effectiveAddress = binding.endpoint_address ?? defaultAddress;
  const effectivePort = binding.endpoint_port ?? defaultPort;
  const customized = hasWarpBindingOverrides(binding);
  const overrideTags = customized ? [{ key: 'Endpoint', value: hostPort(effectiveAddress, effectivePort) }] : [];
  const [editing, setEditing] = useState(false);
  const [address, setAddress] = useState(effectiveAddress);
  const [port, setPort] = useState(String(effectivePort));
  const [formError, setFormError] = useState<unknown>(null);
  const [confirmingRemoval, setConfirmingRemoval] = useState(false);
  const dirty = address.trim() !== effectiveAddress || Number(port) !== effectivePort;
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
    setFormError(null);
    update.mutate({
      endpoint_address: nextAddress === defaultAddress ? null : nextAddress,
      endpoint_port: nextPort === defaultPort ? null : nextPort,
    });
  };
  const clear = () => {
    setFormError(null);
    update.mutate({
      endpoint_address: null,
      endpoint_port: null,
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
  return (
    <WarpMasqueEditor
      key={`${tunnel.tenant}/${tunnel.id}`}
      tenantId={tunnel.tenant}
      id={tunnel.id}
      tunnel={tunnel}
      onClose={onClose}
      onSaved={onClose}
    />
  );
}
