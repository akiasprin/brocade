import { useMemo } from 'react';
import { useQuery } from '@tanstack/react-query';
import {
  fetchCompileView,
  fetchNodes,
  fetchRevisions,
  fetchSnapshot,
  visibleDiagnostics,
  type DiagNames,
} from '../api';
import { Empty, ErrorBox, Loading } from '../ui/bits';
import { DiagTable } from '../ui/diag-table';
import type { Win } from '../wm/store';
import { NodesPane } from './nodes';
import { DeployPane } from './deploy';
import { TenantsPane } from './tenants';
import { UsersPane } from './users';
import { PasswordPane } from './password';
import { UsagePane } from './usage';
import { SettingsPane } from './settings';
import { InspectPane } from './inspect';
import { ChainsPane } from './chains';

export function Pane({ win, bare = false }: { win: Win; bare?: boolean }) {
  switch (win.key) {
    case 'tab:nodes':
      return <NodesPane win={win} bare={bare} />;
    case 'tab:chains':
      return <ChainsPane win={win} />;
    case 'tab:deploy':
      return <DeployPane win={win} />;
    case 'tab:tenants':
      return <TenantsPane />;
    case 'tab:users':
      return <UsersPane win={win} bare={bare} />;
    case 'tab:usage':
      return <UsagePane />;
    case 'tab:settings':
      return <SettingsPane />;
    case 'tab:password':
      return <PasswordPane />;
    case 'diag':
      return <DiagPane />;
    default: {
      /* 检视窗的 key 形如 insp:node:hk-01 */
      const [tag, kind, ...rest] = win.key.split(':');
      if (tag === 'insp') return <InspectPane kind={kind} id={rest.join(':')} />;
      return <Empty>这个面还没做。</Empty>;
    }
  }
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

  if (compile.isPending) return <Loading />;
  if (compile.error) return <ErrorBox error={compile.error} />;

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
        <DiagTable diagnostics={diagnostics} names={names} />
      </div>
    </div>
  );
}
