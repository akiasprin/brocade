import type { ReactNode } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { fetchAgentRelease, fetchNodes, saveAgentRelease, type AgentReleaseScope, type AgentReleaseView } from '../api';
import { ErrorBox, SegmentedControl } from '../ui/bits';
import { useServerForm } from '../ui/server-form';
import { useUnsavedChanges } from '../ui/navigation-guard';
import {
  MachineTable,
  ReleaseLedger,
  ScopeToolbar,
  TabError,
  stamp,
  type LedgerPart,
  type MachineGroup,
  type MachineRow,
  type ReleaseTone,
} from './deploy-cockpit';

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

/* 逐台状态。「待升级」只给批准仍然有效、且在升级范围内的机器：控制面重新部署后携带的是另一批构建，
   原先批准的 id 不再对应可下发的内容，这些机器不会自行升级，要重新批准，因此算「可升级」。 */
type AgentState = 'current' | 'waiting' | 'behind' | 'unreported';

const AGENT_STATUS: Record<AgentState, { tone: ReleaseTone; text: string }> = {
  current: { tone: 'ok', text: '已是新版' },
  waiting: { tone: 'run', text: '待升级' },
  behind: { tone: 'warn', text: '可升级' },
  unreported: { tone: 'none', text: '不可升级' },
};

const SCOPE_NOTES: Record<AgentReleaseScope, string> = {
  all: '批准后 10 分钟内，所有机器自行下载校验、升级 Agent 并重启，以后新增的机器同样适用；没有回滚。',
  nodes: '批准后 10 分钟内，勾选的机器自行下载校验、升级 Agent 并重启；没有回滚，建议先选一台确认。',
  off: '所有机器保持当前版本，不会自行升级。',
};

const SCOPE_LABEL: Record<AgentReleaseScope, string> = { all: '全部机器', nodes: '选中的机器', off: '不升级' };

interface AgentForm {
  scope: AgentReleaseScope;
  nodes: string[];
  note: string;
}

/** 发布页 Agent 页签的数据与编辑状态。页头的操作按钮、页签角标和页签内容读同一份。 */
export function useAgentRelease() {
  const qc = useQueryClient();
  const rel = useQuery({ queryKey: ['agent-release'], queryFn: () => fetchAgentRelease() });
  const nodes = useQuery({ queryKey: ['nodes'], queryFn: () => fetchNodes() });
  const source: AgentForm = {
    scope: rel.data?.released.scope ?? 'off',
    nodes: rel.data?.released.nodes ?? [],
    note: '',
  };
  const { form, setForm, accept } = useServerForm(source);

  const fleet = nodes.data?.nodes ?? [];
  const allIds = fleet.map(n => n.node_id);
  const selected = form.scope === 'all' ? allIds : form.nodes.filter(id => allIds.includes(id));

  const save = useMutation({
    mutationFn: (submitted: AgentForm) => {
      const known = new Set(allIds);
      const picked = submitted.nodes.filter(id => known.has(id));
      const scope: AgentReleaseScope =
        submitted.scope === 'all' ? 'all' : submitted.scope === 'nodes' && picked.length ? 'nodes' : 'off';
      return saveAgentRelease({
        release_id:
          scope === 'off'
            ? (rel.data!.released.release_id ?? rel.data!.available_release_id)
            : rel.data!.available_release_id,
        scope,
        nodes: scope === 'nodes' ? picked : [],
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

  const view = rel.data;
  // 机器列表决定升级范围和「全部机器」的含义；读取失败时不能把空列表当成空机队，
  // 否则一次保存会把原有范围误算成 off。
  const ready = !!view && !!nodes.data;
  const released = view?.released;
  const approvalCurrent = !!view && view.released.release_id === view.available_release_id;
  const inScope = (id: string) =>
    released?.scope === 'all' || (released?.scope === 'nodes' && released.nodes.includes(id));
  const stateOf = (raw: string | null, id: string): AgentState => {
    if (!raw) return 'unreported';
    if (view && onThisBuild(view, raw)) return 'current';
    return approvalCurrent && inScope(id) ? 'waiting' : 'behind';
  };
  const states = fleet.map(node => ({ node, state: stateOf(node.agent_version, node.node_id) }));
  const count = (state: AgentState) => states.filter(item => item.state === state).length;
  const counts = {
    current: count('current'),
    waiting: count('waiting'),
    behind: count('behind'),
    unreported: count('unreported'),
  };

  const effectiveScope: AgentReleaseScope = form.scope === 'nodes' ? (selected.length ? 'nodes' : 'off') : form.scope;
  const dirty =
    !!view &&
    (effectiveScope !== view.released.scope ||
      (effectiveScope === 'nodes' && !sameIds(selected, view.released.nodes)) ||
      form.note.trim() !== '' ||
      (effectiveScope !== 'off' && !approvalCurrent));
  // 「选中的机器」一台都没勾时不能提交：要停用请明确选「不升级」。
  const submittable = dirty && !(form.scope === 'nodes' && selected.length === 0);
  const edited = JSON.stringify(form) !== JSON.stringify(source);

  return {
    rel,
    nodes,
    ready,
    error: rel.error ?? nodes.error,
    view,
    states,
    counts,
    approvalCurrent,
    form,
    setForm,
    selected,
    allIds,
    save,
    submittable,
    edited,
    /** 放弃未提交的范围改动，回到服务端当前的批准。 */
    reset: () => setForm(source),
  };
}

export type AgentRelease = ReturnType<typeof useAgentRelease>;

/** 页签角标：机器正在自行升级时是进度，有机器可升级时是待处理数。 */
export function agentBadge(agent: AgentRelease): { tone: 'run' | 'gold'; text: string } | null {
  if (!agent.ready) return null;
  const { current, waiting, behind } = agent.counts;
  if (waiting > 0) return { tone: 'run', text: `${current}/${current + waiting}` };
  if (behind > 0) return { tone: 'gold', text: String(behind) };
  return null;
}

export function AgentReleaseTab({
  agent,
  editable,
  editing,
  onEditingChange,
}: {
  agent: AgentRelease;
  editable: boolean;
  editing: boolean;
  onEditingChange: (editing: boolean) => void;
}) {
  const { view, form, setForm, selected, save } = agent;
  useUnsavedChanges(editing && agent.edited, 'Agent 升级范围');

  if (!agent.ready) return agent.error ? <TabError error={agent.error} /> : null;
  const loaded = view!;
  const released = loaded.released;
  const { current, waiting, behind, unreported } = agent.counts;
  const reported = current + waiting + behind;
  const scopeLabel = released.scope === 'nodes' ? `${released.nodes.length} 台机器` : SCOPE_LABEL[released.scope];
  const stale = released.scope !== 'off' && !agent.approvalCurrent;
  const target = `v${loaded.agent_version}`;

  const parts: LedgerPart[] = [
    { tone: 'ok', label: '已是新版', count: current },
    ...(waiting ? [{ tone: 'run' as const, label: '待升级', count: waiting }] : []),
    ...(behind ? [{ tone: 'warn' as const, label: '可升级', count: behind }] : []),
    { tone: 'none', label: '不可升级', count: unreported },
  ];

  const toggle = (id: string) => {
    const next = selected.includes(id) ? selected.filter(n => n !== id) : [...selected, id];
    setForm({ ...form, scope: 'nodes', nodes: next });
  };
  const busy = !editable || save.isPending;
  const rowOf = ({ node, state }: (typeof agent.states)[number]): MachineRow => ({
    node,
    tone: AGENT_STATUS[state].tone,
    status: AGENT_STATUS[state].text,
    current: bareBuild(node.agent_version)?.slice(0, 8) ?? null,
    target: state === 'waiting' || state === 'behind' ? target : null,
    reason: state === 'unreported' ? '没上报过 Agent 版本' : null,
    pick: {
      checked: form.scope === 'all' || (form.scope === 'nodes' && selected.includes(node.node_id)),
      disabled: busy || form.scope !== 'nodes',
      onChange: () => toggle(node.node_id),
    },
  });
  const group = (key: AgentState, label: string): MachineGroup => ({
    key,
    label,
    rows: agent.states.filter(item => item.state === key).map(rowOf),
  });
  const cancel = () => {
    agent.reset();
    save.reset();
    onEditingChange(false);
  };

  return (
    <>
      <ReleaseLedger
        label="已是新版"
        value={current}
        unit={`/ ${reported} 台`}
        period={`可发 ${target} · ${stale ? '这一版尚未批准' : `升级范围：${scopeLabel}`}`}
        parts={parts}
        facts={[
          ['可发版本', <span className="cgc-mono">{target}</span>],
          ['上次批准', released.released_at ? `${released.released_by ?? '—'} · ${stamp(released.released_at)}` : '—'],
          ...loaded.available_agents.map(
            build => [build.arch, <span className="cgc-mono">{build.sha256.slice(0, 8)}</span>] as [string, ReactNode],
          ),
        ]}
      />
      <section className="cgc-main">
        <MachineTable
          label="逐台 Agent 升级状态"
          editing={editing}
          groups={[
            group('waiting', '待升级'),
            group('behind', '可升级'),
            group('unreported', '不可升级'),
            group('current', '已是新版'),
          ]}
          toolbar={
            <ScopeToolbar
              note={SCOPE_NOTES[form.scope]}
              error={save.error ? <ErrorBox error={save.error} /> : undefined}
            >
              <SegmentedControl
                value={form.scope}
                options={(['all', 'nodes', 'off'] as const).map(scope => ({ value: scope, label: SCOPE_LABEL[scope] }))}
                disabled={busy}
                ariaLabel="升级范围选项"
                onChange={scope => setForm({ ...form, scope })}
              />
              <span className="sp" />
              <button className="btn" type="button" disabled={save.isPending} onClick={cancel}>
                取消
              </button>
              <button
                className={agent.submittable ? 'btn primary' : 'btn'}
                type="button"
                disabled={busy || !agent.submittable}
                onClick={() => save.mutate(form, { onSuccess: () => onEditingChange(false) })}
              >
                {save.isPending ? '批准中…' : '批准'}
              </button>
            </ScopeToolbar>
          }
        />
      </section>
    </>
  );
}
