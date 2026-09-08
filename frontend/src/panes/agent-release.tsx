import { useServerForm } from '../ui/server-form';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { fetchAgentRelease, fetchNodes, saveAgentRelease, type AgentReleaseScope, type AgentReleaseView } from '../api';
import { Ago, ErrorBox, Loading } from '../ui/bits';
import { PanelTitle } from '../ui/icons';

const bareBuild = (raw: string | null) => raw?.replace(/^brocade-agent\//, '') ?? null;

const onThisBuild = (view: AgentReleaseView, raw: string | null) => {
  const bare = bareBuild(raw);
  return !!bare && view.available_agents.some(a => a.sha256 === bare);
};

const sameIds = (a: string[], b: string[]) => {
  const left = new Set(a);
  const right = new Set(b);
  return left.size === right.size && [...left].every(id => right.has(id));
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

export function AgentReleaseSection({ editable }: { editable: boolean }) {
  const qc = useQueryClient();
  const rel = useQuery({ queryKey: ['agent-release'], queryFn: () => fetchAgentRelease() });
  const nodes = useQuery({ queryKey: ['nodes'], queryFn: () => fetchNodes() });
  const { form, setForm, accept } = useServerForm({
    scope: rel.data?.released.scope ?? ('off' as AgentReleaseScope),
    nodes: rel.data?.released.nodes ?? [],
    note: '',
  });

  const save = useMutation({
    mutationFn: (submitted: typeof form) => {
      const allNodeIds = new Set((nodes.data?.nodes ?? []).map(n => n.node_id));
      const selected = submitted.nodes.filter(id => allNodeIds.has(id));
      const scope: AgentReleaseScope = submitted.scope === 'all' ? 'all' : selected.length ? 'nodes' : 'off';
      return saveAgentRelease({
        release_id:
          scope === 'off'
            ? (rel.data!.released.release_id ?? rel.data!.available_release_id)
            : rel.data!.available_release_id,
        scope,
        nodes: scope === 'nodes' ? selected : [],
        note: submitted.note.trim() || null,
        version: null,
        commit: null,
        released_at: null,
        released_by: null,
      });
    },
    onMutate: () => qc.cancelQueries({ queryKey: ['agent-release'] }),
    onSuccess: async (view, submitted) => {
      await qc.cancelQueries({ queryKey: ['agent-release'] });
      accept({ scope: view.released.scope, nodes: view.released.nodes, note: '' }, submitted);
      qc.setQueryData(['agent-release'], view);
    },
  });

  if (rel.isPending || nodes.isPending) return <Loading />;
  // 机器列表决定批准范围和“全选”的含义；读取失败时不能把空列表当成空机队，
  // 否则一次保存会把原有范围误算成 off。
  if (rel.error || nodes.error) return <ErrorBox error={rel.error ?? nodes.error} />;
  const view = rel.data!;
  const rows = nodes.data?.nodes ?? [];
  const allIds = rows.map(n => n.node_id);
  const f = form ?? { scope: 'off' as AgentReleaseScope, nodes: [], note: '' };
  const selected = f.scope === 'all' ? allIds : f.nodes.filter(id => allIds.includes(id));
  const effectiveScope: AgentReleaseScope = f.scope === 'all' ? 'all' : selected.length ? 'nodes' : 'off';
  const dirty =
    effectiveScope !== view.released.scope ||
    (effectiveScope === 'nodes' && !sameIds(selected, view.released.nodes)) ||
    f.note.trim() !== '' ||
    (effectiveScope !== 'off' && view.released.release_id !== view.available_release_id);
  const toggle = (id: string) => {
    const next = selected.includes(id) ? selected.filter(n => n !== id) : [...selected, id];
    setForm({ ...f, scope: next.length ? 'nodes' : 'off', nodes: next });
  };
  const allSelected = allIds.length > 0 && sameIds(selected, allIds);
  const toggleAll = () => setForm({ ...f, scope: allSelected ? 'off' : 'all', nodes: allSelected ? [] : allIds });

  return (
    <section className="panel titled" id="dp-agent">
      <header>
        <PanelTitle of="agent">Agent 版本</PanelTitle>
        <span className="sp" />
      </header>

      {save.error && <ErrorBox error={save.error} />}

      <div className="guard" style={{ marginBottom: 10 }}>
        批准后 10 分钟内，范围内的机器自行替换并重启。没有回滚，建议先发一台确认。
        {f.scope === 'all' ? '当前范围为全部机器，包含以后新增的机器。' : '当前范围仅包含勾选的机器。'}
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
                {[view.released.released_at?.slice(0, 16), view.released.released_by].filter(Boolean).join(' · ')}
              </span>
            </>
          ) : (
            <span className="dim">未批准，所有机器不会自行更新</span>
          )}
        </dd>
      </dl>

      <table className="tbl ag-t" style={{ marginTop: 10 }}>
        <thead>
          <tr>
            <th className="pick">
              <input
                type="checkbox"
                aria-label="批准全部机器（包含以后新增的机器）"
                checked={allSelected}
                disabled={!editable || save.isPending || allIds.length === 0}
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
            const picked = selected.includes(n.node_id);
            return (
              <tr key={n.node_id} className={picked ? undefined : 'out'}>
                <td className="pick">
                  <input
                    type="checkbox"
                    aria-label={`批准 ${n.name || n.node_id}`}
                    checked={picked}
                    disabled={!editable || save.isPending}
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
                <td className="r">
                  <Ago at={n.last_poll_at} />
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>

      <div className="guard-foot">
        <button
          className={dirty ? 'btn primary' : 'btn'}
          disabled={!editable || !dirty || save.isPending}
          onClick={() => save.mutate(form)}
        >
          {save.isPending ? '批准中…' : '批准'}
        </button>
      </div>
    </section>
  );
}
