import { useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  analyzeFrontRoutes,
  ApiError,
  deleteFront,
  cancelGrantProbe,
  fetchCommittedSnapshot,
  fetchFrontClientConfigState,
  fetchGrantProbeCapability,
  fetchGrantProbeJob,
  fetchTenants,
  fetchUsers,
  startFrontCombinationProbe,
  upsertFront,
  type ClientConfigCommitResult,
  type ExternalOutbound,
  type FrontClientConfigState,
  type FrontRouteAnalysisView,
  type FrontRouteDecision,
  type FrontRouteStatus,
  type GrantProbeJob,
  type SnapshotApp,
  type SnapshotFront,
  type SnapshotIngress,
  type UserListItem,
} from '../api';
import { can, useSession } from '../session';
import { Confirm, Empty, ErrorBox, Loading } from '../ui/bits';
import { ListIcon, PanelTitle } from '../ui/icons';
import { navigate } from '../forge/route';
import { useUnsavedChanges } from '../ui/navigation-guard';

export interface ChainProxyLocation {
  appId: string;
  frontId: string;
}

const targetIds = (app: SnapshotApp, frontId: string) =>
  app.ingresses.filter(ingress => ingress.front === frontId).map(ingress => ingress.id);

const tenantUnder = (child: string, parent: string) => child === parent || child.startsWith(`${parent}.`);

const userGrantSet = (app: SnapshotApp, user: UserListItem) =>
  new Set(
    app.grants.filter(grant => grant.tenant === user.tenant_id && grant.user === user.id).map(grant => grant.ingress),
  );

const userProjectionKey = (user: UserListItem) => `${user.tenant_id}/${user.id}`;

const affectedUsers = (app: SnapshotApp, front: SnapshotFront, users: UserListItem[]) => {
  const targets = targetIds(app, front.id);
  return users.filter(user => {
    if (!tenantUnder(user.tenant_id, front.tenant) || user.status !== 'active') return false;
    const grants = userGrantSet(app, user);
    const hasTarget = targets.some(id => grants.has(id));
    const hasMember = front.external_via.length > 0 || front.via.some(id => grants.has(id));
    return hasTarget && hasMember;
  }).length;
};

export function ChainProxyListSection({
  apps,
  users,
  editable,
  onOpen,
  onCreate,
}: {
  apps: SnapshotApp[];
  users: UserListItem[];
  editable: boolean;
  onOpen: (location: ChainProxyLocation) => void;
  onCreate: () => void;
}) {
  const fronts = apps.flatMap(app => app.fronts.map(front => ({ app, front })));
  return (
    <section className="panel titled chain-proxy-list-panel" data-page-title="true">
      <header>
        <ListIcon of="client" />
        <h4>链式代理</h4>
        <span className="hint">{fronts.length} 个前置组</span>
        <span className="rd">客户端订阅 · 无需发布机器</span>
        <button className="btn" disabled={!editable || apps.length === 0} onClick={onCreate}>
          ＋ 新建前置组
        </button>
      </header>
      {fronts.length === 0 ? (
        <div className="chain-proxy-empty">
          <p>还没有前置组。创建后，把入口或外部隧道作为成员，再选择需要经它拨号的目标入口。</p>
          <button className="btn primary" disabled={!editable || apps.length === 0} onClick={onCreate}>
            创建第一个前置组
          </button>
        </div>
      ) : (
        <div className="chain-proxy-card-grid">
          {fronts.map(({ app, front }) => {
            const targets = targetIds(app, front.id);
            const members = front.via.length + front.external_via.length;
            const impacted = affectedUsers(app, front, users);
            return (
              <button
                className="chain-proxy-card"
                key={`${app.id}/${front.id}`}
                data-route-focus={`front:${app.id}:${front.id}`}
                onClick={() => onOpen({ appId: app.id, frontId: front.id })}
              >
                <span className="chain-proxy-mark">⇢</span>
                <span className="chain-proxy-card-main">
                  <b>{front.name}</b>
                  <small>
                    {app.label || app.id} · {front.strategy}
                  </small>
                </span>
                <span className="chain-proxy-card-metrics">
                  <span>
                    <b>{members}</b> 成员
                  </span>
                  <span>
                    <b>{targets.length}</b> 目标
                  </span>
                  <span>
                    <b>{impacted}</b> 用户
                  </span>
                </span>
                <span className={`st ${targets.length > 0 && members === 0 ? 'st-warn' : ''}`}>
                  {targets.length === 0 ? '未启用' : members === 0 ? '缺少成员' : '已配置'}
                </span>
                <span className="tunnel-chevron">›</span>
              </button>
            );
          })}
        </div>
      )}
    </section>
  );
}

export function ChainProxyDetail({
  appId,
  frontId,
  onOpen,
  onBack,
}: {
  appId?: string;
  frontId?: string;
  onOpen: (location: ChainProxyLocation) => void;
  onBack: () => void;
}) {
  const snapshot = useQuery({ queryKey: ['snapshot', 'committed'], queryFn: fetchCommittedSnapshot });
  const tenants = useQuery({ queryKey: ['tenants'], queryFn: () => fetchTenants() });
  const users = useQuery({ queryKey: ['users'], queryFn: () => fetchUsers(true) });
  const clientState = useQuery({
    queryKey: ['front-client-state', appId, frontId],
    queryFn: () => fetchFrontClientConfigState(appId!, frontId!),
    enabled: Boolean(appId && frontId),
  });
  if (snapshot.isPending || tenants.isPending || users.isPending) return <Loading variant="config-detail" sheeted />;
  if (snapshot.error) return <ErrorBox error={snapshot.error} />;
  if (tenants.error) return <ErrorBox error={tenants.error} />;
  if (users.error) return <ErrorBox error={users.error} />;

  const model = snapshot.data.snapshot;
  const initialApp = appId ? model.apps.find(app => app.id === appId) : model.apps[0];
  const initialFront = frontId ? initialApp?.fronts.find(front => front.id === frontId) : undefined;
  if ((appId && !initialApp) || (frontId && !initialFront)) {
    return (
      <Empty>
        这个前置组不存在，可能已被其他操作者删除。{' '}
        <button className="btn" onClick={onBack}>
          返回链式代理
        </button>
      </Empty>
    );
  }
  if (!initialApp) return <Empty>先创建项目，才能创建前置组。</Empty>;
  if (frontId && clientState.error) return <ErrorBox error={clientState.error} />;

  return (
    <ChainProxyEditor
      key={`${initialApp.id}/${initialFront?.id ?? 'new'}`}
      apps={model.apps}
      tunnels={model.external_outbounds ?? []}
      revision={model.revision}
      users={users.data.users}
      tenantOptions={tenants.data.tenants}
      initialApp={initialApp}
      initialFront={initialFront}
      clientState={clientState.data}
      onOpen={onOpen}
      onBack={onBack}
    />
  );
}

function ChainProxyEditor({
  apps,
  tunnels,
  revision,
  users,
  tenantOptions,
  initialApp,
  initialFront,
  clientState,
  onOpen,
  onBack,
}: {
  apps: SnapshotApp[];
  tunnels: ExternalOutbound[];
  revision: number;
  users: UserListItem[];
  tenantOptions: Awaited<ReturnType<typeof fetchTenants>>['tenants'];
  initialApp: SnapshotApp;
  initialFront?: SnapshotFront;
  clientState?: FrontClientConfigState;
  onOpen: (location: ChainProxyLocation) => void;
  onBack: () => void;
}) {
  const { who } = useSession();
  const editable = can(who.role, 'edit');
  const qc = useQueryClient();
  const [selectedAppId, setSelectedAppId] = useState(initialApp.id);
  const selectedApp = apps.find(app => app.id === selectedAppId) ?? initialApp;
  const initialTenant =
    initialFront?.tenant ?? selectedApp.chains[0]?.tenant ?? tenantOptions[0]?.id ?? who.tenant_scope ?? '';
  const [tenantId, setTenantId] = useState(initialTenant);
  const [name, setName] = useState(initialFront?.name ?? '新前置组');
  const [strategy, setStrategy] = useState<SnapshotFront['strategy']>(initialFront?.strategy ?? 'select');
  const [via, setVia] = useState(initialFront?.via ?? []);
  const [externalVia, setExternalVia] = useState(initialFront?.external_via ?? []);
  const [targets, setTargets] = useState(initialFront ? targetIds(initialApp, initialFront.id) : []);
  const [expectedRevision, setExpectedRevision] = useState(revision);
  const [saving, setSaving] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [deletePending, setDeletePending] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [saved, setSaved] = useState<ClientConfigCommitResult | null>(null);
  const [previewUserKey, setPreviewUserKey] = useState('');
  const [frontId] = useState(
    () =>
      initialFront?.id ??
      `front-${Array.from(crypto.getRandomValues(new Uint32Array(2)), value =>
        value.toString(16).padStart(8, '0'),
      ).join('-')}`,
  );

  const chainById = new Map(selectedApp.chains.map(chain => [chain.id, chain]));
  const ingressName = (ingress: SnapshotIngress) => {
    const chain = chainById.get(ingress.chain);
    return `${chain?.name || ingress.chain} · ${ingress.id}`;
  };
  const eligibleIngresses = selectedApp.ingresses.filter(ingress => {
    const ingressTenant = chainById.get(ingress.chain)?.tenant;
    return ingressTenant !== undefined && tenantUnder(tenantId, ingressTenant);
  });
  const memberOf = new Map<string, SnapshotFront>();
  for (const front of selectedApp.fronts) {
    for (const ingressId of front.via) memberOf.set(ingressId, front);
  }
  const manualTunnels = tunnels.filter(tunnel => tunnel.tenant === tenantId && tunnel.protocol.t !== 'warp');
  const scopedUsers = users.filter(user => tenantUnder(user.tenant_id, tenantId) && user.status === 'active');
  const previewUser = scopedUsers.find(user => userProjectionKey(user) === previewUserKey) ?? scopedUsers[0];
  const previewGrants = previewUser ? userGrantSet(selectedApp, previewUser) : new Set<string>();
  const previewInternal = via.filter(id => previewGrants.has(id));
  const previewTargets = targets.filter(id => previewGrants.has(id));
  const previewMembers = [...previewInternal, ...externalVia];
  const previewActive = previewTargets.length > 0 && previewMembers.length > 0;
  const movedTargets = targets.filter(id => {
    const owner = selectedApp.ingresses.find(ingress => ingress.id === id)?.front;
    return owner && owner !== initialFront?.id;
  });
  const dirty =
    selectedAppId !== initialApp.id ||
    tenantId !== initialTenant ||
    name !== (initialFront?.name ?? '新前置组') ||
    strategy !== (initialFront?.strategy ?? 'select') ||
    JSON.stringify(via) !== JSON.stringify(initialFront?.via ?? []) ||
    JSON.stringify(externalVia) !== JSON.stringify(initialFront?.external_via ?? []) ||
    JSON.stringify(targets) !== JSON.stringify(initialFront ? targetIds(initialApp, initialFront.id) : []);
  const clearUnsavedChanges = useUnsavedChanges(
    dirty && !saving,
    initialFront ? `${initialFront.name} 的前置组` : '新前置组',
  );
  const valid = !!name.trim() && !!tenantId && (targets.length === 0 || via.length + externalVia.length > 0);
  const needsConnectivityAnalysis = targets.length > 0 && via.length + externalVia.length > 0;
  const routeAnalysis = useQuery({
    queryKey: [
      'front-route-analysis',
      selectedApp.id,
      frontId,
      tenantId,
      name.trim(),
      strategy,
      expectedRevision,
      via,
      externalVia,
      targets,
    ],
    queryFn: () =>
      analyzeFrontRoutes(selectedApp.id, {
        expected_revision: expectedRevision,
        id: frontId,
        tenant_id: tenantId,
        name: name.trim(),
        strategy,
        via,
        external_via: externalVia,
        targets,
      }),
    enabled: valid,
    retry: false,
  });
  const routeBlocked = routeAnalysis.data?.analysis.blocking ?? false;
  const routeAnalysisReady = !needsConnectivityAnalysis || routeAnalysis.data !== undefined;
  const routeAnalysisFailed = needsConnectivityAnalysis && routeAnalysis.isError;

  const toggle = (values: string[], id: string, enabled: boolean, set: (next: string[]) => void) => {
    set(enabled ? [...values, id] : values.filter(value => value !== id));
  };
  const move = (values: string[], index: number, offset: -1 | 1, set: (next: string[]) => void) => {
    const target = index + offset;
    if (target < 0 || target >= values.length) return;
    const next = [...values];
    [next[index], next[target]] = [next[target], next[index]];
    set(next);
  };
  const changeApp = (next: string) => {
    setSelectedAppId(next);
    const app = apps.find(candidate => candidate.id === next);
    setTenantId(app?.chains[0]?.tenant ?? tenantOptions[0]?.id ?? '');
    setVia([]);
    setExternalVia([]);
    setTargets([]);
  };
  const changeTenant = (next: string) => {
    setTenantId(next);
    setVia([]);
    setExternalVia([]);
    setTargets([]);
  };
  const save = async () => {
    if (!valid || saving || !routeAnalysisReady || routeBlocked) return;
    setSaving(true);
    setError(null);
    try {
      const result = await upsertFront(selectedApp.id, {
        expected_revision: expectedRevision,
        id: frontId,
        tenant_id: tenantId,
        name: name.trim(),
        strategy,
        via,
        external_via: externalVia,
        targets,
      });
      setExpectedRevision(result.revision_id);
      setSaved(result.client_config);
      await Promise.all([
        qc.invalidateQueries({ queryKey: ['snapshot'] }),
        qc.invalidateQueries({ queryKey: ['revisions'] }),
        qc.invalidateQueries({ queryKey: ['compile'] }),
        qc.invalidateQueries({ queryKey: ['front-client-state', selectedApp.id, frontId] }),
      ]);
      clearUnsavedChanges();
      if (!initialFront) onOpen({ appId: selectedApp.id, frontId });
    } catch (next) {
      if (next instanceof ApiError && next.status === 409) {
        try {
          const latest = await fetchCommittedSnapshot();
          setExpectedRevision(latest.snapshot.revision);
          qc.setQueryData(['snapshot', 'committed'], latest);
          setError(
            new ApiError(
              409,
              `保存期间配置已更新，现已刷新到修订 #${latest.snapshot.revision}。当前表单尚未覆盖，请核对后再次保存。`,
            ),
          );
        } catch {
          setError(next);
        }
      } else {
        setError(next);
      }
    } finally {
      setSaving(false);
    }
  };
  const remove = async () => {
    if (!initialFront || deletePending) return;
    setDeletePending(true);
    setError(null);
    try {
      await deleteFront(initialApp.id, initialFront.id, expectedRevision);
      await Promise.all([
        qc.invalidateQueries({ queryKey: ['snapshot'] }),
        qc.invalidateQueries({ queryKey: ['revisions'] }),
      ]);
      onBack();
    } catch (next) {
      setDeletePending(false);
      setError(next);
    }
  };

  return (
    <div className="nd-sheet nd-page chain-proxy-page">
      <div className="fg-sheet nd-paper">
        <header className="nd-page-head chain-proxy-head">
          <div className="nd-page-identity">
            <span className="chain-proxy-detail-mark">⇢</span>
            <h1 className="nd-id nd-name">{initialFront ? name : '新建前置组'}</h1>
          </div>
          <div className="chain-proxy-head-meta">
            <span>链式代理</span>
            <span>客户端路由</span>
          </div>
          <div className="nd-acts">
            <button className="btn" onClick={onBack}>
              返回
            </button>
            {initialFront && (
              <button className="btn danger" disabled={!editable} onClick={() => setConfirmDelete(true)}>
                删除
              </button>
            )}
            <button
              className="btn primary"
              disabled={!editable || !valid || saving || !routeAnalysisReady || routeBlocked}
              onClick={() => void save()}
            >
              {saving
                ? '保存中…'
                : routeBlocked
                  ? '存在阻断，不能保存'
                  : routeAnalysisFailed
                    ? '连通性不可用'
                    : !routeAnalysisReady
                      ? '正在判定连通性…'
                      : '保存前置组'}
            </button>
          </div>
        </header>

        <div className="nd-paper-body chain-proxy-body">
          <section className="chain-proxy-boundaries" aria-label="生效边界">
            <article>
              <small>客户端路由 · 本页</small>
              <b>保存即更新订阅</b>
              <p>只创建客户端配置快照和订阅 generation；不创建机器变更单。</p>
            </article>
            <article>
              <small>服务端路由 · 线路页</small>
              <b>草稿 → 发布 → Agent 收敛</b>
              <p>成员能否访问目标、目标如何出网，仍由各自所在链路的规则决定。</p>
            </article>
          </section>

          {initialFront && (
            <div
              className={`callout ${clientState?.active ? 'ok' : 'warn'} chain-proxy-serving-state`}
              aria-label="当前订阅生效状态"
            >
              {!clientState
                ? '正在读取当前订阅生效状态…'
                : `客户端头 #${clientState.head_snapshot_id} · ${
                    clientState.active
                      ? `正在下发${clientState.serving_generation === null ? '' : ` G${clientState.serving_generation}`}`
                      : clientState.topology_revision_id === null
                        ? '等待首次拓扑发布'
                        : '当前未被订阅 serving 引用'
                  } · 拓扑${
                    clientState.topology_revision_id === null ? '未发布' : ` R${clientState.topology_revision_id}`
                  } · 权限${
                    clientState.permissions_revision_id === null ? '未发布' : ` R${clientState.permissions_revision_id}`
                  }${
                    clientState.pending_topology.length === 0
                      ? ''
                      : ` · ${clientState.pending_topology.length} 项等待拓扑，当前不下发`
                  }`}
            </div>
          )}

          {saved && (
            <div className="callout ok chain-proxy-saved" role="status">
              客户端快照 #{saved.snapshot_id}
              {saved.status === 'unchanged'
                ? ' · 内容未变化'
                : saved.status === 'activated'
                  ? ` · 订阅${saved.serving_generation === null ? '' : ` G${saved.serving_generation}`} 已推进`
                  : ' · 等待首次拓扑发布'}
              {saved.pending_topology.length > 0 &&
                ` · ${saved.pending_topology.length} 项等待拓扑，当前不下发；空组目标不会改为直连`}
            </div>
          )}
          {error !== null && <ErrorBox error={error} />}
          <ConnectivityMatrix
            view={routeAnalysis.data}
            loading={valid && routeAnalysis.isPending}
            error={routeAnalysis.error}
            memberLabel={id => {
              const ingress = selectedApp.ingresses.find(candidate => candidate.id === id);
              if (ingress) return ingressName(ingress);
              const tunnel = tunnels.find(candidate => candidate.id === id);
              return tunnel ? `${tunnel.name} · ${tunnel.protocol.t}` : id;
            }}
            targetLabel={id => {
              const ingress = selectedApp.ingresses.find(candidate => candidate.id === id);
              return ingress ? ingressName(ingress) : id;
            }}
            probeContext={{
              appId: selectedApp.id,
              frontId: initialFront?.id,
              user: previewUser,
              authorizedIngresses: previewGrants,
              servingActive: clientState?.active === true,
              dirty,
            }}
          />
          <div className="chain-proxy-layout">
            <div className="chain-proxy-config">
              <section className="panel config-panel chain-proxy-panel">
                <header>
                  <PanelTitle of="tunnels">共享定义</PanelTitle>
                  <span className="hint">租户 / 项目保存一份</span>
                </header>
                <div className="fgrid one chain-proxy-fields">
                  <label className="row">
                    <span className="k">项目</span>
                    <span className="v">
                      <select
                        className="f"
                        value={selectedApp.id}
                        disabled={!editable || !!initialFront}
                        onChange={e => changeApp(e.target.value)}
                      >
                        {apps.map(app => (
                          <option key={app.id} value={app.id}>
                            {app.label || app.id}
                          </option>
                        ))}
                      </select>
                    </span>
                  </label>
                  <label className="row">
                    <span className="k">租户</span>
                    <span className="v">
                      <select
                        className="f"
                        value={tenantId}
                        disabled={!editable || !!initialFront}
                        onChange={e => changeTenant(e.target.value)}
                      >
                        {tenantOptions.map(tenant => (
                          <option key={tenant.id} value={tenant.id}>
                            {tenant.name || tenant.id}
                          </option>
                        ))}
                      </select>
                    </span>
                  </label>
                  <label className="row">
                    <span className="k">名称</span>
                    <span className="v">
                      <input className="f" value={name} disabled={!editable} onChange={e => setName(e.target.value)} />
                      <span className="sub">这个名称会成为 Mihomo 中的一张代理组卡片。</span>
                    </span>
                  </label>
                  <label className="row">
                    <span className="k">策略</span>
                    <span className="v">
                      <select
                        className="f"
                        value={strategy}
                        disabled={!editable}
                        onChange={e => setStrategy(e.target.value as SnapshotFront['strategy'])}
                      >
                        <option value="select">手动选择 · select</option>
                        <option value="url-test">自动测速 · url-test</option>
                        <option value="fallback">顺序故障转移 · fallback</option>
                      </select>
                      <span className="sub">所有目标入口共享这一个选择结果，不会按目标分别选择成员。</span>
                    </span>
                  </label>
                </div>
              </section>

              <SelectorPanel
                title="内部成员"
                hint="客户端先拨这些入口"
                editable={editable}
                selected={via}
                items={eligibleIngresses.map(ingress => ({
                  id: ingress.id,
                  label: ingressName(ingress),
                  disabled: targets.includes(ingress.id) || (!!ingress.front && ingress.front !== initialFront?.id),
                  note:
                    ingress.front && ingress.front !== initialFront?.id
                      ? `已是 ${ingress.front} 的目标`
                      : ingress.front === initialFront?.id
                        ? '当前目标，先从目标中移除'
                        : undefined,
                }))}
                onToggle={(id, enabled) => toggle(via, id, enabled, setVia)}
                onMove={(index, offset) => move(via, index, offset, setVia)}
              />

              <SelectorPanel
                title="外部成员"
                hint="凭据会进入命中用户的订阅"
                editable={editable}
                selected={externalVia}
                items={manualTunnels.map(tunnel => ({
                  id: tunnel.id,
                  label: `${tunnel.name} · ${tunnel.protocol.t}`,
                }))}
                empty="该租户没有可下发的外部隧道；WARP 使用机器独立身份，不能作为前置成员。"
                onToggle={(id, enabled) => toggle(externalVia, id, enabled, setExternalVia)}
                onMove={(index, offset) => move(externalVia, index, offset, setExternalVia)}
              />

              <SelectorPanel
                title="目标订阅入口"
                hint="这些入口写入 dialer-proxy"
                editable={editable}
                selected={targets}
                items={eligibleIngresses.map(ingress => {
                  const owner = memberOf.get(ingress.id);
                  return {
                    id: ingress.id,
                    label: ingressName(ingress),
                    disabled: via.includes(ingress.id) || (!!owner && owner.id !== initialFront?.id),
                    note:
                      owner && owner.id !== initialFront?.id
                        ? `已是前置组「${owner.name}」的成员`
                        : ingress.front && ingress.front !== initialFront?.id
                          ? `保存后从 ${ingress.front} 移到本组`
                          : undefined,
                  };
                })}
                onToggle={(id, enabled) => toggle(targets, id, enabled, setTargets)}
              />
              {movedTargets.length > 0 && (
                <div className="callout warn">
                  {movedTargets.length} 个目标当前属于其它前置组；保存会原子迁移到本组。
                </div>
              )}
            </div>

            <aside className="chain-proxy-preview">
              <section className="panel config-panel chain-proxy-panel">
                <header>
                  <PanelTitle of="client">用户投影预览</PanelTitle>
                  <span className="hint">结构预览 · 非完整 YAML</span>
                </header>
                <div className="chain-proxy-shared-flow">
                  <span>一份共享前置组</span>
                  <i>→</i>
                  <span>按用户授权过滤</span>
                  <i>→</i>
                  <span>Mihomo 卡片</span>
                </div>
                <label className="chain-proxy-user-select">
                  预览用户
                  <select
                    className="f"
                    value={previewUser ? userProjectionKey(previewUser) : ''}
                    onChange={e => setPreviewUserKey(e.target.value)}
                  >
                    {scopedUsers.map(user => (
                      <option key={userProjectionKey(user)} value={userProjectionKey(user)}>
                        {user.id} · {user.tenant_id}
                      </option>
                    ))}
                  </select>
                </label>
                {!previewUser ? (
                  <p className="tunnel-inline-empty">该租户没有可预览的启用用户。</p>
                ) : !previewActive ? (
                  <div className="chain-proxy-projection-empty">
                    <b>Mihomo 中不会出现空卡片</b>
                    <p>该用户没有同时获得“至少一个成员 + 至少一个目标”的授权组合。</p>
                  </div>
                ) : (
                  <>
                    <article className="mihomo-front-card">
                      <small>{strategy}</small>
                      <b>{name || frontId}</b>
                      <span>{previewMembers.length} 个可选前置成员</span>
                      <div>
                        {previewMembers.map(id => (
                          <em key={id}>{id}</em>
                        ))}
                      </div>
                    </article>
                    <div className="mihomo-target-list">
                      {previewTargets.map(id => (
                        <span key={id}>
                          {id} <code>dialer-proxy: {name || frontId}</code>
                        </span>
                      ))}
                    </div>
                  </>
                )}
                <p className="chain-proxy-preview-note">
                  控制面不会给每个用户复制一份前置组。这里只解释投影结构，不替代订阅原文；生成订阅时才按该用户的目标和内部成员授权投影，外部成员随命中的组写入。
                </p>
              </section>
              <section className="panel config-panel chain-proxy-panel">
                <header>
                  <PanelTitle of="chains">服务端联网路由</PanelTitle>
                  <span className="hint">本页只读边界</span>
                </header>
                <p className="chain-proxy-route-copy">
                  前置组不改链路规则。内部成员使用其入口所在链路去访问目标地址；目标接通后，再由目标入口所在链路决定如何出网。
                </p>
                <button className="btn" onClick={() => navigate('chains', { p: 'list' })}>
                  去线路页配置
                </button>
              </section>
            </aside>
          </div>
        </div>
      </div>
      {confirmDelete && initialFront && (
        <Confirm
          title={`删除前置组「${initialFront.name}」？`}
          body={
            <p>
              {targets.length} 个目标入口会立即移除 <code>dialer-proxy</code>。订阅 generation
              会推进，但不会创建机器变更单。
            </p>
          }
          confirmLabel={deletePending ? '删除中…' : '确认删除'}
          confirmDisabled={deletePending}
          onCancel={() => setConfirmDelete(false)}
          onConfirm={() => void remove()}
        />
      )}
    </div>
  );
}

const routeStatusMeta: Record<FrontRouteStatus, { label: string; symbol: string }> = {
  reachable: { label: '可达', symbol: '✓' },
  blocked: { label: '阻断', symbol: '×' },
  conditional: { label: '条件', symbol: '◇' },
  unknown: { label: '未知', symbol: '?' },
  external: { label: '外部', symbol: '↗' },
  pending: { label: '等待', symbol: '…' },
};

const decisionTrace = (decision: FrontRouteDecision) =>
  [
    decision.chain_id && `链路 ${decision.chain_id}`,
    decision.node_id && `节点 ${decision.node_id}`,
    decision.rule_index !== null && `规则 #${decision.rule_index}`,
    decision.selector,
    decision.action,
  ]
    .filter(Boolean)
    .join(' · ');

function ConnectivityMatrix({
  view,
  loading,
  error,
  memberLabel,
  targetLabel,
  probeContext,
}: {
  view?: FrontRouteAnalysisView;
  loading: boolean;
  error: unknown;
  memberLabel: (id: string) => string;
  targetLabel: (id: string) => string;
  probeContext: {
    appId: string;
    frontId?: string;
    user?: UserListItem;
    authorizedIngresses: Set<string>;
    servingActive: boolean;
    dirty: boolean;
  };
}) {
  const [pinned, setPinned] = useState('');
  const cells = view?.analysis.cells ?? [];
  const selected = cells.find(cell => `${cell.member_id}/${cell.target_id}` === pinned) ?? cells[0];

  return (
    <section className="panel config-panel chain-proxy-connectivity" aria-label="成员与目标连通性" aria-busy={loading}>
      <header>
        <PanelTitle of="chains">端到端连通性</PanelTitle>
        <span className="hint">服务中规则静态判定 · 不含协议握手或实时在线 · 改动后自动重算</span>
      </header>
      {view && (
        <div className="chain-proxy-analysis-version">
          <span>客户端基线 #{view.base_client_snapshot_id}</span>
          <span>{view.topology_revision_id === null ? '拓扑未发布' : `拓扑 R${view.topology_revision_id}`}</span>
          <span>{view.permissions_revision_id === null ? '权限未发布' : `权限 R${view.permissions_revision_id}`}</span>
          <span>{view.serving_generation === null ? '订阅未服务' : `订阅 G${view.serving_generation}`}</span>
        </div>
      )}
      {loading && !view && <p className="tunnel-inline-empty">正在按当前成员、目标和服务中规则判定…</p>}
      {error !== null && error !== undefined && (
        <div className="chain-proxy-analysis-error">
          <ErrorBox error={error} />
          <p>连通性结果不可用时不会开放保存，请刷新配置后重试。</p>
        </div>
      )}
      {view?.analysis.blocking && (
        <div className="callout err" role="alert">
          当前组合存在明确阻断。后端会拒绝保存，不会把已知不可用的路径下发给用户。
        </div>
      )}
      {view && view.analysis.cells.length === 0 && (
        <p className="tunnel-inline-empty">至少选择一个成员和一个目标入口后，才会形成连通性矩阵。</p>
      )}
      {view && view.analysis.cells.length > 0 && (
        <>
          <div className="chain-proxy-matrix-scroll">
            <table className="chain-proxy-matrix">
              <thead>
                <tr>
                  <th scope="col">成员 ＼ 目标</th>
                  {view.analysis.targets.map(target => {
                    const status = routeStatusMeta[target.landing.status];
                    return (
                      <th scope="col" key={target.id}>
                        <b>{targetLabel(target.id)}</b>
                        <small>{target.endpoints.join(' / ') || '等待服务端拓扑'}</small>
                        <span className={`chain-route-status ${target.landing.status}`}>
                          {status.symbol} 落地{status.label}
                        </span>
                      </th>
                    );
                  })}
                </tr>
              </thead>
              <tbody>
                {view.analysis.members.map(member => (
                  <tr key={member.id}>
                    <th scope="row">
                      <b>{memberLabel(member.id)}</b>
                      <small>{member.kind === 'external' ? '外部未纳管' : member.chain_id || '等待服务端拓扑'}</small>
                    </th>
                    {view.analysis.targets.map(target => {
                      const cell = cells.find(
                        candidate => candidate.member_id === member.id && candidate.target_id === target.id,
                      );
                      if (!cell) return <td key={target.id}>—</td>;
                      const status = routeStatusMeta[cell.combined.status];
                      const active = selected === cell;
                      return (
                        <td key={target.id}>
                          <button
                            className={`chain-route-cell ${cell.combined.status}${active ? ' active' : ''}`}
                            aria-label={`${memberLabel(member.id)} 到 ${targetLabel(target.id)}：${status.label}`}
                            aria-pressed={active}
                            onClick={() => setPinned(`${member.id}/${target.id}`)}
                          >
                            <span>{status.symbol}</span>
                            {status.label}
                          </button>
                        </td>
                      );
                    })}
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          {selected && (
            <div className="chain-proxy-route-detail" aria-live="polite">
              <header>
                <b>
                  {memberLabel(selected.member_id)} → {targetLabel(selected.target_id)}
                </b>
                <span className={`chain-route-status ${selected.combined.status}`}>
                  {routeStatusMeta[selected.combined.status].symbol} {routeStatusMeta[selected.combined.status].label}
                </span>
              </header>
              <div>
                <article>
                  <small>中继链路</small>
                  <p>{selected.relay.reason}</p>
                  {decisionTrace(selected.relay) && <code>{decisionTrace(selected.relay)}</code>}
                </article>
                <article>
                  <small>落地链路</small>
                  <p>{selected.landing.reason}</p>
                  {decisionTrace(selected.landing) && <code>{decisionTrace(selected.landing)}</code>}
                </article>
                <article>
                  <small>端到端</small>
                  <p>{selected.combined.reason}</p>
                  {selected.endpoints.length > 0 && (
                    <ul>
                      {selected.endpoints.map(endpoint => (
                        <li key={endpoint.endpoint}>
                          {endpoint.endpoint} · {routeStatusMeta[endpoint.decision.status].label}
                        </li>
                      ))}
                    </ul>
                  )}
                </article>
              </div>
              <FrontLiveProbe
                cell={selected}
                view={view}
                memberLabel={memberLabel(selected.member_id)}
                targetLabel={targetLabel(selected.target_id)}
                context={probeContext}
              />
            </div>
          )}
        </>
      )}
    </section>
  );
}

const liveProbeStatus = (status: GrantProbeJob['items'][number]['status'], ttfb: number | null) => {
  if (status === 'passed') return ttfb === null ? '通过' : `通过 · ${ttfb}ms`;
  if (status === 'failed') return '失败';
  if (status === 'running') return '拨号中…';
  if (status === 'waiting') return '等待';
  return '已取消';
};

function FrontLiveProbe({
  cell,
  view,
  memberLabel,
  targetLabel,
  context,
}: {
  cell: FrontRouteAnalysisView['analysis']['cells'][number];
  view: FrontRouteAnalysisView;
  memberLabel: string;
  targetLabel: string;
  context: {
    appId: string;
    frontId?: string;
    user?: UserListItem;
    authorizedIngresses: Set<string>;
    servingActive: boolean;
    dirty: boolean;
  };
}) {
  const qc = useQueryClient();
  const capability = useQuery({
    queryKey: ['grant-probe-capability'],
    queryFn: fetchGrantProbeCapability,
    staleTime: 60_000,
    retry: false,
  });
  const [jobId, setJobId] = useState<string>();
  const [startedCell, setStartedCell] = useState('');
  const [reused, setReused] = useState(false);
  const jobQuery = useQuery({
    queryKey: ['grant-probe-job', jobId],
    queryFn: () => fetchGrantProbeJob(jobId!),
    enabled: Boolean(jobId),
    retry: false,
    refetchInterval: query => (query.state.data?.status === 'running' ? 1_000 : false),
  });
  const cellKey = `${cell.member_id}/${cell.target_id}`;
  const jobMissing = jobQuery.error instanceof ApiError && jobQuery.error.status === 404;
  const job = !jobMissing && startedCell === cellKey ? jobQuery.data : undefined;
  const start = useMutation({
    mutationFn: () => {
      if (!context.user || !context.frontId || view.serving_generation === null) {
        throw new Error('当前 Serving 尚不能执行组合拨测');
      }
      return startFrontCombinationProbe(context.user.tenant_id, context.user.id, {
        app_id: context.appId,
        front_id: context.frontId,
        member_id: cell.member_id,
        target_id: cell.target_id,
        expected_serving_generation: view.serving_generation,
        expected_client_snapshot_id: view.base_client_snapshot_id,
      });
    },
    onSuccess: response => {
      setStartedCell(cellKey);
      setJobId(response.job.id);
      setReused(response.reused);
      qc.setQueryData(['grant-probe-job', response.job.id], response.job);
    },
  });
  const cancel = useMutation({
    mutationFn: (id: string) => cancelGrantProbe(id),
    onSuccess: snapshot => qc.setQueryData(['grant-probe-job', snapshot.id], snapshot),
  });

  const member = view.analysis.members.find(item => item.id === cell.member_id);
  const isExternal = member?.kind === 'external';
  const authorized =
    context.authorizedIngresses.has(cell.target_id) && (isExternal || context.authorizedIngresses.has(cell.member_id));
  const unavailableReason = !context.frontId
    ? '先保存前置组，才能从 Serving 取得真实订阅参数。'
    : context.dirty
      ? '当前表单有未保存变更；保存后再测，结果才与用户收到的订阅一致。'
      : !context.servingActive || view.serving_generation === null
        ? '客户端快照尚未进入 Serving。'
        : !context.user
          ? '请选择一个启用用户。'
          : !authorized
            ? isExternal
              ? '所选用户没有获得这个目标入口。'
              : '所选用户没有同时获得这个成员和目标入口。'
            : capability.data && !capability.data.available
              ? capability.data.reason || 'Console 拨测组件不可用。'
              : null;
  const error = start.error ?? cancel.error ?? (jobMissing ? null : jobQuery.error) ?? capability.error;
  const passed = job?.items.filter(item => item.status === 'passed').length ?? 0;
  const failed = job?.items.filter(item => item.status === 'failed').length ?? 0;
  const finishedAt = job?.finished_at_unix_secs
    ? new Date(job.finished_at_unix_secs * 1_000).toLocaleString('zh-CN', { hour12: false })
    : null;

  return (
    <section className="chain-proxy-live-probe" aria-label="真实组合拨测">
      <header>
        <span>
          <small>真实组合拨测</small>
          <b>
            {memberLabel} → {targetLabel}
          </b>
        </span>
        <button
          className={`btn${job?.status === 'running' ? '' : ' primary'}`}
          disabled={
            start.isPending ||
            cancel.isPending ||
            capability.isPending ||
            ((!job || job.status !== 'running') && unavailableReason !== null)
          }
          onClick={() => {
            if (job?.status === 'running') cancel.mutate(job.id);
            else start.mutate();
          }}
        >
          {job?.status === 'running' ? '取消拨测' : start.isPending ? '创建中…' : '实测这条路径'}
        </button>
      </header>
      <p>
        {isExternal
          ? '使用当前 Serving 中的外部隧道参数和所选用户的目标凭据，实测外部成员 → 目标 → 互联网；会产生少量真实流量。'
          : '使用所选用户当前生效的真实凭据，把目标出站的拨号强制经过成员出站；会产生少量真实流量并计入该用户用量。'}
      </p>
      {unavailableReason && <div className="chain-proxy-live-note">{unavailableReason}</div>}
      {error && <ErrorBox error={error} />}
      {job && (
        <div className={`chain-proxy-live-result ${failed > 0 ? 'failed' : job.status}`} aria-live="polite">
          <div>
            <b>{job.message || '正在执行全部协议与地址族组合…'}</b>
            <span>
              Serving G{job.serving_generation}
              {job.client_snapshot_id === undefined || job.client_snapshot_id === null
                ? ''
                : ` · 客户端 #${job.client_snapshot_id}`}
              {reused ? ' · 复用 5 分钟内结果' : ''}
            </span>
            {finishedAt && (
              <span>
                完成于 {finishedAt}
                {job.timeout_secs ? ` · 单项超时 ${job.timeout_secs}s` : ''}
              </span>
            )}
            <span>
              通过 {passed}/{job.items.length} · 失败 {failed}
            </span>
          </div>
          <ul>
            {job.items.map(item => (
              <li key={item.id} className={item.status} title={item.detail ?? undefined}>
                <span>
                  {item.member_protocol} · {item.member_family} → {item.protocol} · {item.family}
                </span>
                <b>{liveProbeStatus(item.status, item.ttfb_ms)}</b>
                {item.detail && <small>{item.detail}</small>}
              </li>
            ))}
          </ul>
        </div>
      )}
    </section>
  );
}

function SelectorPanel({
  title,
  hint,
  editable,
  items,
  selected,
  empty = '没有可选入口。',
  onToggle,
  onMove,
}: {
  title: string;
  hint: string;
  editable: boolean;
  items: { id: string; label: string; disabled?: boolean; note?: string }[];
  selected: string[];
  empty?: string;
  onToggle: (id: string, enabled: boolean) => void;
  onMove?: (index: number, offset: -1 | 1) => void;
}) {
  const byId = new Map(items.map(item => [item.id, item]));
  return (
    <section className="panel config-panel chain-proxy-panel">
      <header>
        <PanelTitle of="tunnels">{title}</PanelTitle>
        <span className="hint">{hint}</span>
      </header>
      {selected.length > 0 && (
        <div className="chain-proxy-order">
          {selected.map((id, index) => (
            <div key={id}>
              <span className="mono">{index + 1}</span>
              <b>{byId.get(id)?.label || id}</b>
              <span>
                {onMove && (
                  <>
                    <button
                      className="btn icon"
                      aria-label={`${id} 上移`}
                      disabled={!editable || index === 0}
                      onClick={() => onMove(index, -1)}
                    >
                      ↑
                    </button>
                    <button
                      className="btn icon"
                      aria-label={`${id} 下移`}
                      disabled={!editable || index === selected.length - 1}
                      onClick={() => onMove(index, 1)}
                    >
                      ↓
                    </button>
                  </>
                )}
                <button
                  className="btn icon"
                  aria-label={`${id} 移除`}
                  disabled={!editable}
                  onClick={() => onToggle(id, false)}
                >
                  ×
                </button>
              </span>
            </div>
          ))}
        </div>
      )}
      <div className="chain-proxy-options">
        {items.length === 0 ? (
          <p className="tunnel-inline-empty">{empty}</p>
        ) : (
          items.map(item => (
            <label key={item.id} className={item.disabled ? 'disabled' : ''}>
              <input
                type="checkbox"
                checked={selected.includes(item.id)}
                disabled={!editable || (item.disabled && !selected.includes(item.id))}
                onChange={event => onToggle(item.id, event.target.checked)}
              />
              <span>
                <b>{item.label}</b>
                {item.note && <small>{item.note}</small>}
              </span>
            </label>
          ))
        )}
      </div>
    </section>
  );
}
