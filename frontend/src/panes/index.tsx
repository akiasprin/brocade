import { lazy, useMemo, type ReactNode } from 'react';
import { useQuery } from '@tanstack/react-query';
import {
  fetchCompileView,
  fetchNodes,
  fetchRevisions,
  fetchSnapshot,
  visibleDiagnostics,
  type DiagNames,
} from '../api';
import { Empty, ErrorBox, Loading, LoadingBoundary, type LoadingVariant } from '../ui/bits';
import { DiagTable } from '../ui/diag-table';
import { useSession } from '../session';
import type { Win } from '../wm/store';

const NodesPane = lazy(() => import('./nodes').then(module => ({ default: module.NodesPane })));
const DeployPane = lazy(() => import('./deploy').then(module => ({ default: module.DeployPane })));
const UsersPane = lazy(() => import('./users').then(module => ({ default: module.UsersPane })));
const PasswordPane = lazy(() => import('./password').then(module => ({ default: module.PasswordPane })));
const UsagePane = lazy(() => import('./usage').then(module => ({ default: module.UsagePane })));
const SettingsPane = lazy(() => import('./settings').then(module => ({ default: module.SettingsPane })));
const InspectPane = lazy(() => import('./inspect').then(module => ({ default: module.InspectPane })));
const ChainsPane = lazy(() => import('./chains').then(module => ({ default: module.ChainsPane })));
const TunnelsPane = lazy(() => import('./tunnels').then(module => ({ default: module.TunnelsPane })));

type PaneLoadingProps = {
  variant: LoadingVariant;
  sheeted?: boolean;
  userDetail?: boolean;
};

const drillPage = (data: Record<string, unknown>): string | undefined => {
  const drill = data.drill;
  if (!drill || typeof drill !== 'object' || !('p' in drill)) return undefined;
  return typeof drill.p === 'string' ? drill.p : undefined;
};

const ROUTE_IDENTITY_FIELDS = ['id', 'node', 'app', 'chain', 'revision'] as const;

/**
 * The loading latch belongs to one routed surface, not to the whole top-level pane. Steps, tabs and
 * generated idempotency keys deliberately stay out of this identity: changing those must preserve
 * the mounted surface, while entering another list/detail/form location starts a new cold read.
 */
export function paneLoadingRouteKey(paneKey: string, data: Record<string, unknown> = {}): string {
  // 用户页是常驻的主从双栏：列表、详情以及不同用户都复用同一批查询和同一张页面。
  // 选择用户只应替换详情区域；若把用户 ID 算进边界身份，路由更新会让整页短暂退回
  // loading guard，生产环境的空加载面就表现为一次全页闪烁。
  if (paneKey === 'tab:users') return JSON.stringify([paneKey, 'master-detail']);

  const drill = data.drill;
  if (!drill || typeof drill !== 'object') return JSON.stringify([paneKey, 'list']);
  const record = drill as Record<string, unknown>;
  const page = typeof record.p === 'string' && record.p !== 'list' ? record.p : 'list';
  const identity = ROUTE_IDENTITY_FIELDS.flatMap(field => {
    const value = record[field];
    return typeof value === 'string' || typeof value === 'number' ? [[field, value] as const] : [];
  });
  return JSON.stringify([paneKey, page, ...identity]);
}

function paneLoadingProps(paneKey: string, data: Record<string, unknown> = {}, bare = false): PaneLoadingProps {
  const page = drillPage(data);
  switch (paneKey) {
    case 'tab:nodes':
      if (page === 'node') return { variant: 'detail', sheeted: bare };
      if (page && page !== 'list') return { variant: 'form' };
      return { variant: 'nodes', sheeted: bare };
    case 'tab:chains':
      return { variant: page === 'chain' ? 'chain-detail' : page === 'new' ? 'form' : 'chains' };
    case 'tab:tunnels':
      if (page === 'vpngate') return { variant: 'vpngate', sheeted: true };
      return page && page !== 'list' ? { variant: 'config-detail', sheeted: true } : { variant: 'tunnels' };
    case 'tab:users':
      return { variant: 'users', sheeted: bare, userDetail: page === 'user' };
    case 'tab:usage':
      return { variant: 'usage' };
    case 'tab:settings':
      return { variant: 'settings' };
    case 'tab:deploy':
      return { variant: page === 'plan' ? 'plan' : page === 'detail' ? 'deployment' : 'deploy' };
    case 'diag':
      return { variant: 'table' };
    default:
      return { variant: paneKey.startsWith('insp:') ? 'table' : 'panel', sheeted: bare };
  }
}

export function Pane({ win, bare = false }: { win: Win; bare?: boolean }) {
  const { initial } = useSession();
  let content: ReactNode;
  switch (win.key) {
    case 'tab:nodes':
      content = <NodesPane win={win} bare={bare} />;
      break;
    case 'tab:chains':
      content = <ChainsPane win={win} />;
      break;
    case 'tab:tunnels':
      content = <TunnelsPane win={win} />;
      break;
    case 'tab:deploy':
      content = <DeployPane win={win} />;
      break;
    case 'tab:users':
      content = <UsersPane win={win} bare={bare} />;
      break;
    case 'tab:usage':
      content = <UsagePane />;
      break;
    case 'tab:settings':
      content = <SettingsPane />;
      break;
    case 'tab:password':
      content = <PasswordPane />;
      break;
    case 'diag':
      content = <DiagPane />;
      break;
    default: {
      /* 检视窗的 key 形如 insp:node:hk-01 */
      const [tag, kind, ...rest] = win.key.split(':');
      content = tag === 'insp' ? <InspectPane kind={kind} id={rest.join(':')} /> : <Empty>这个面还没做。</Empty>;
    }
  }
  const loading = paneLoadingProps(win.key, win.data, bare);
  const fallback = <Loading {...loading} initial={initial} />;
  return (
    <LoadingBoundary fallback={fallback} variant={loading.variant} routeKey={paneLoadingRouteKey(win.key, win.data)}>
      {content}
    </LoadingBoundary>
  );
}

export function DiagPane() {
  const revisions = useQuery({ queryKey: ['revisions'], queryFn: () => fetchRevisions() });
  const current = revisions.data?.current_revision;
  const compile = useQuery({
    queryKey: ['compile', current],
    queryFn: () => fetchCompileView(current!),
    enabled: !!current,
  });
  // 位置列需要显示名称，因此本页也需要这两份数据。与顶栏共享 tanstack 缓存，
  // 不产生额外请求。
  const nodeList = useQuery({ queryKey: ['nodes'], queryFn: () => fetchNodes() });
  const snapshot = useQuery({ queryKey: ['snapshot'], queryFn: () => fetchSnapshot() });
  const names = useMemo<DiagNames>(() => {
    const nodes = new Map((nodeList.data?.nodes ?? []).map(n => [n.node_id, n.name]));
    const chains = new Map(
      (snapshot.data?.snapshot.apps ?? []).flatMap(a => a.chains.map(c => [c.id, c.name] as const)),
    );
    return { node: id => nodes.get(id), chain: id => chains.get(id) };
  }, [nodeList.data, snapshot.data]);

  if (revisions.isPending || (current != null && compile.isPending)) return <Loading variant="table" />;
  if (revisions.error || compile.error) return <ErrorBox error={revisions.error ?? compile.error} />;
  if (!current) return <Empty>还没有可诊断的修订。</Empty>;
  if (!compile.data) return <ErrorBox error={new Error('编译结果没有返回内容')} />;

  const { summary, revision } = compile.data;
  const diagnostics = visibleDiagnostics(compile.data.diagnostics);

  // 与顶栏气泡共用同一组件：上一版本在两处分别实现计数和渲染，修改时遗漏其中一处，
  // 导致服务端返回 `warnings: 0` 而界面显示「2 警」。
  return (
    <div className="dg-pane">
      <div className="chipline" style={{ marginBottom: 10 }}>
        <span className={`st ${summary.can_publish ? 'st-succeeded' : 'st-halted'}`}>
          {summary.can_publish ? '可发布' : '禁发布'}
        </span>
        <span className="dim mono">修订 {revision}</span>
      </div>
      <div className="dg-box">
        {(nodeList.error || snapshot.error) && <ErrorBox error={nodeList.error ?? snapshot.error} />}
        <DiagTable diagnostics={diagnostics} names={names} />
      </div>
    </div>
  );
}
