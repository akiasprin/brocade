import { useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  fetchAgentRelease,
  fetchNodes,
  saveAgentRelease,
  type AgentReleaseScope,
  type AgentReleaseView,
} from '../api';
import { Ago, ErrorBox, Loading } from '../ui/bits';

const bareBuild = (raw: string | null) => raw?.replace(/^brocade-agent\//, '') ?? null;

const onThisBuild = (view: AgentReleaseView, raw: string | null) => {
  const bare = bareBuild(raw);
  return !!bare && view.available_agents.some(a => a.sha256 === bare);
};

export function useAgentDrift(): { pending: number; loading: boolean } {
  const rel = useQuery({ queryKey: ['agent-release'], queryFn: () => fetchAgentRelease() });
  const nodes = useQuery({ queryKey: ['nodes'], queryFn: () => fetchNodes() });
  if (!rel.data || !nodes.data) return { pending: 0, loading: rel.isPending || nodes.isPending };
  const view = rel.data;
  const scope = view.released.scope;
  if (scope === 'off') return { pending: 0, loading: false };
  const inScope = nodes.data.nodes.filter(n => scope === 'all' || view.released.nodes.includes(n.node_id));
  return { pending: inScope.filter(n => !onThisBuild(view, n.agent_version)).length, loading: false };
}


export function AgentReleaseSection({ editable, no }: { editable: boolean; no: string }) {
  const qc = useQueryClient();
  const rel = useQuery({ queryKey: ['agent-release'], queryFn: () => fetchAgentRelease() });
  const nodes = useQuery({ queryKey: ['nodes'], queryFn: () => fetchNodes() });
  const [form, setForm] = useState<{
    scope: AgentReleaseScope;
    nodes: string[];
    note: string;
  } | null>(null);

  const [syncedFrom, setSyncedFrom] = useState<typeof rel.data>(undefined);
  // 必须等 nodes.data 一并就绪再 seed：scope 为 all 时选中集要展开成整机队的 id，而它取自
  // nodes 查询。只等 rel.data 会有竞态——nodes 尚未返回则展开成空集，且 seed 只按 rel.data
  // 身份跑一次，nodes 后到也不再 seed，空选状态就此固定（刷新时表现为时而全选、时而全空）。
  if (rel.data && nodes.data && rel.data !== syncedFrom) {
    setSyncedFrom(rel.data);
    setForm({
      scope: rel.data.released.scope,
      nodes: rel.data.released.scope === 'all'
        ? (nodes.data?.nodes ?? []).map(n => n.node_id)
        : rel.data.released.nodes,
      note: '',
    });
  }

  const save = useMutation({
    mutationFn: () => {
      const allNodeIds = (nodes.data?.nodes ?? []).map(n => n.node_id);
      const scope: AgentReleaseScope =
        form!.nodes.length === 0 ? 'off' : form!.nodes.length === allNodeIds.length ? 'all' : 'nodes';
      return saveAgentRelease({
        release_id:
          scope === 'off'
            ? (rel.data!.released.release_id ?? rel.data!.available_release_id)
            : rel.data!.available_release_id,
        scope,
        nodes: form!.nodes,
        note: form!.note.trim() || null,
        version: null,
        commit: null,
        released_at: null,
        released_by: null,
      });
    },
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ['agent-release'] });
    },
  });

  if (rel.isPending || nodes.isPending) return <Loading />;
  if (rel.error) return <ErrorBox error={rel.error} />;
  const view = rel.data!;
  const rows = nodes.data?.nodes ?? [];
  const allIds = rows.map(n => n.node_id);
  const f = form ?? { scope: 'off' as AgentReleaseScope, nodes: [], note: '' };
  const effectiveScope: AgentReleaseScope =
    f.nodes.length === 0 ? 'off' : f.nodes.length === allIds.length ? 'all' : 'nodes';
  const dirty =
    effectiveScope !== view.released.scope ||
    (effectiveScope === 'nodes' && f.nodes.join() !== view.released.nodes.join()) ||
    f.note.trim() !== '' ||
    (effectiveScope !== 'off' && view.released.release_id !== view.available_release_id);
  const toggle = (id: string) =>
    setForm({ ...f, nodes: f.nodes.includes(id) ? f.nodes.filter(n => n !== id) : [...f.nodes, id] });
  const allSelected = f.nodes.length === allIds.length;
  const toggleAll = () => setForm({ ...f, nodes: allSelected ? [] : allIds });

  return (
    <section className="panel titled" id="dp-agent">
      <header>
        <span className="no">{no}</span>
        <h4>Agent 版本</h4>
        <span className="sp" />
      </header>

      {save.error && <ErrorBox error={save.error} />}

      <div className="guard" style={{ marginBottom: 10 }}>
        批准后 10 分钟内，范围内的机器自行替换并重启。没有回滚，建议先发一台确认。
      </div>

      <dl className="kv form2">
        <dt>可发版本</dt>
        <dd>
          <span className="mono">v{view.agent_version}</span>
          <span className="dim" style={{ marginLeft: 8 }}>
            {view.available_agents.map(a => `${a.arch} ${a.sha256.slice(0, 8)}`).join(' · ')}
          </span>
        </dd>

        <dt>已批准</dt>
        <dd>
          {view.released.release_id ? (
            <>
              <span className={view.released.release_id === view.available_release_id ? 'mono' : 'mono bad'}>
                {view.released.version ? `v${view.released.version}` : '—'}
              </span>
              <span className="dim" style={{ marginLeft: 8 }}>
                {[
                  view.released.released_at?.slice(0, 16),
                  view.released.released_by,
                ].filter(Boolean).join(' · ')}
              </span>
            </>
          ) : (
            <span className="dim">未批准，机队不会自行更新</span>
          )}
        </dd>

      </dl>

      <table className="tbl ag-t" style={{ marginTop: 10 }}>
        <thead>
          <tr>
            <th className="pick">
              <input
                type="checkbox"
                checked={allSelected}
                disabled={!editable}
                onChange={toggleAll}
              />
            </th>
            <th>机器</th>
            <th>当前构建</th>
            <th>状态</th>
            <th className="r">上次来拉</th>
          </tr>
        </thead>
        <tbody>
          {rows.map(n => {
            const on = onThisBuild(view, n.agent_version);
            const picked = f.nodes.includes(n.node_id);
            return (
              <tr key={n.node_id} className={picked ? undefined : 'out'}>
                <td className="pick">
                  <input
                    type="checkbox"
                    checked={picked}
                    disabled={!editable}
                    onChange={() => toggle(n.node_id)}
                  />
                </td>
                <td className="nm">{n.name || n.node_id}</td>
                <td className="bd">{bareBuild(n.agent_version)?.slice(0, 12) ?? '—'}</td>
                <td>
                  {!n.agent_version ? (
                    <span className="st st-canceled">没上报过</span>
                  ) : on ? (
                    <span className="st st-succeeded">已是这一批</span>
                  ) : picked ? (
                    <span className="st st-pending">待替换</span>
                  ) : (
                    <span className="st st-skipped">不在范围</span>
                  )}
                </td>
                <td className="r"><Ago at={n.last_poll_at} /></td>
              </tr>
            );
          })}
        </tbody>
      </table>

      <div className="guard-foot">
        <button
          className={dirty ? 'btn primary' : 'btn'}
          disabled={!editable || !dirty || save.isPending}
          onClick={() => save.mutate()}
        >
          {save.isPending ? '批准中…' : '批准'}
        </button>
      </div>

    </section>
  );
}
