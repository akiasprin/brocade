import { useState, type ReactNode } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  cancelXrayRelease,
  createXrayRelease,
  fetchNodes,
  fetchXrayReleases,
  retryXrayReleaseTarget,
  type NodeAgentStateItem,
  type XrayReleaseTarget,
  type XrayReleaseTargetStatus,
  type XrayReleaseView,
} from '../api';
import { Confirm, ErrorBox } from '../ui/bits';
import { randomKey } from '../ui/platform';
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

/* 一次升级里每台机器的状态，用与 Agent 页相同的词：待升级 / 升级中 / 本次已升级。 */
const TARGET_STATUS: Record<XrayReleaseTargetStatus, { tone: ReleaseTone; text: string }> = {
  pending: { tone: 'run', text: '待升级' },
  dispatched: { tone: 'run', text: '升级中' },
  succeeded: { tone: 'ok', text: '本次已升级' },
  unverified: { tone: 'err', text: '未通过验证' },
  'failed-recovered': { tone: 'err', text: '失败 · 已恢复' },
  'failed-dirty': { tone: 'err', text: '失败 · 需处理' },
  unsupported: { tone: 'none', text: '不支持' },
  canceled: { tone: 'none', text: '已取消' },
};

function compactRuntimeVersion(value: string) {
  const version = value.match(/(?:^|\b)(?:Xray\s+)?v?(\d+(?:\.\d+){1,3})(?:\b|$)/i)?.[1];
  return version ? `v${version}` : value;
}

function eligible(node: NodeAgentStateItem) {
  const installed = node.runtime_versions?.xray_installed_sha256;
  const running = node.runtime_versions?.xray_running_sha256;
  return (
    node.lifecycle_phase === 'active' &&
    node.desired_poll_fresh &&
    node.runtime_report_fresh &&
    !!installed &&
    running === installed
  );
}

function unavailableReason(node: NodeAgentStateItem) {
  if (node.lifecycle_phase !== 'active') return '机器不在 active 生命周期';
  if (!node.desired_poll_fresh) return '最近 90 秒没有领取期望状态';
  if (!node.runtime_report_fresh) return '最近 2 分钟没有运行时上报';
  const installed = node.runtime_versions?.xray_installed_sha256;
  const running = node.runtime_versions?.xray_running_sha256;
  if (!installed) return '还没上报 Xray 摘要；先发布支持该能力的 Agent';
  if (!running) return 'Xray 当前未运行';
  if (running !== installed) return '运行中 Xray 与受管路径不一致；先完成本地收敛';
  return '';
}

type XrayState = 'current' | 'behind' | 'blocked';

/** 发布页 Xray 页签的数据、勾选与三种操作（发起、取消、重试）。页头按钮与页签内容读同一份。 */
export function useXrayRelease() {
  const qc = useQueryClient();
  const releases = useQuery({
    queryKey: ['xray-releases'],
    queryFn: fetchXrayReleases,
    refetchInterval: query => (query.state.data?.releases.some(release => release.active) ? 5_000 : false),
  });
  const nodes = useQuery({ queryKey: ['nodes'], queryFn: () => fetchNodes() });
  const [picked, setSelected] = useState<string[]>([]);
  const [idempotencyKey, setIdempotencyKey] = useState(randomKey);
  const [askCancel, setAskCancel] = useState(false);

  const view = releases.data;
  const fleet = nodes.data?.nodes ?? [];
  const active = view?.releases.find(release => release.active);
  const latest = view?.releases[0];
  const onThisBuild = (node: NodeAgentStateItem) =>
    !!view && view.available_xrays.some(artifact => artifact.sha256 === node.runtime_versions?.xray_installed_sha256);
  const stateOf = (node: NodeAgentStateItem): XrayState =>
    onThisBuild(node) ? 'current' : eligible(node) ? 'behind' : 'blocked';
  const targets = new Map((active?.targets ?? []).map(target => [target.node_id, target]));
  const states = fleet.map(node => ({ node, state: stateOf(node), target: targets.get(node.node_id) }));
  const outside = states.filter(item => !item.target);
  const counts = {
    current: outside.filter(item => item.state === 'current').length,
    behind: outside.filter(item => item.state === 'behind').length,
    blocked: outside.filter(item => item.state === 'blocked').length,
  };
  // 勾选只对仍可升级的机器有效：编辑期间机器失联、已被别处升级时，已勾的那台不再计数，也不提交。
  const pickable = new Set(outside.filter(item => item.state === 'behind').map(item => item.node.node_id));
  const selected = picked.filter(id => pickable.has(id));

  const accept = async (view: XrayReleaseView) => {
    await qc.cancelQueries({ queryKey: ['xray-releases'] });
    qc.setQueryData(['xray-releases'], view);
  };
  const create = useMutation({
    mutationFn: () =>
      createXrayRelease({
        idempotency_key: idempotencyKey,
        release_id: releases.data!.available_release_id,
        nodes: selected,
        note: null,
      }),
    onSuccess: async view => {
      await accept(view);
      setSelected([]);
      setIdempotencyKey(randomKey());
    },
  });
  const cancel = useMutation({
    mutationFn: (releaseId: number) => cancelXrayRelease(releaseId),
    onSuccess: async view => {
      setAskCancel(false);
      await accept(view);
    },
  });
  const retry = useMutation({
    mutationFn: ({ releaseId, nodeId }: { releaseId: number; nodeId: string }) =>
      retryXrayReleaseTarget(releaseId, nodeId),
    onSuccess: accept,
  });

  return {
    releases,
    nodes,
    ready: !!view && !!nodes.data,
    error: releases.error ?? nodes.error,
    view,
    active,
    /** 最近一次已结束的升级；进行中时不算。 */
    last: latest && !latest.active ? latest : undefined,
    states,
    counts,
    selected,
    setSelected,
    askCancel,
    setAskCancel,
    create,
    cancel,
    retry,
    busy: create.isPending || cancel.isPending || retry.isPending,
  };
}

export type XrayRelease = ReturnType<typeof useXrayRelease>;

/** 页签角标：升级进行中是本次进度，否则是可升级的台数。 */
export function xrayBadge(xray: XrayRelease): { tone: 'run' | 'gold'; text: string } | null {
  if (!xray.ready) return null;
  if (xray.active) {
    const done = xray.active.targets.filter(target => target.status === 'succeeded').length;
    return { tone: xray.active.status === 'halted' ? 'gold' : 'run', text: `${done}/${xray.active.targets.length}` };
  }
  return xray.counts.behind > 0 ? { tone: 'gold', text: String(xray.counts.behind) } : null;
}

export function XrayReleaseTab({
  xray,
  editable,
  editing,
  onEditingChange,
}: {
  xray: XrayRelease;
  editable: boolean;
  editing: boolean;
  onEditingChange: (editing: boolean) => void;
}) {
  if (!xray.ready) return xray.error ? <TabError error={xray.error} /> : null;
  const view = xray.view!;
  const { active, selected, setSelected, busy } = xray;
  const target = compactRuntimeVersion(view.xray_version);
  const versionOf = (node: NodeAgentStateItem) =>
    node.runtime_versions?.xray ? compactRuntimeVersion(node.runtime_versions.xray) : null;
  const picking = editing && !active;
  const actionable = xray.states.filter(item => !item.target && item.state === 'behind').map(item => item.node.node_id);
  const allPicked = actionable.length > 0 && actionable.every(id => selected.includes(id));
  const toggle = (id: string) =>
    setSelected(current => (current.includes(id) ? current.filter(nodeId => nodeId !== id) : [...current, id]));

  const rowOf = ({ node, state, target: releaseTarget }: (typeof xray.states)[number]): MachineRow => {
    if (releaseTarget) return targetRow(node, releaseTarget, target, versionOf(node), xray, editable);
    const status: Record<XrayState, { tone: ReleaseTone; text: string }> = {
      current: { tone: 'ok', text: '已是新版' },
      behind: { tone: 'warn', text: '可升级' },
      blocked: { tone: 'none', text: '不可升级' },
    };
    return {
      node,
      tone: status[state].tone,
      status: status[state].text,
      current: versionOf(node),
      target: state === 'behind' ? target : null,
      reason: state === 'blocked' ? unavailableReason(node) || null : null,
      pick: {
        checked: selected.includes(node.node_id),
        disabled: !editable || busy || state !== 'behind',
        onChange: () => toggle(node.node_id),
      },
    };
  };
  const outsideGroup = (key: XrayState, label: string): MachineGroup => ({
    key,
    label,
    rows: xray.states.filter(item => !item.target && item.state === key).map(rowOf),
  });
  const groups: MachineGroup[] = [
    ...(active
      ? [{ key: 'release', label: `升级 #${active.id}`, rows: xray.states.filter(item => item.target).map(rowOf) }]
      : []),
    outsideGroup('behind', '可升级'),
    outsideGroup('blocked', '不可升级'),
    outsideGroup('current', '已是新版'),
  ];

  const facts: [string, ReactNode][] = [
    ['可发版本', <span className="cgc-mono">{target}</span>],
    active
      ? ['不可升级', `${xray.counts.blocked} 台`]
      : ['上次批准', xray.last ? `${xray.last.created_by} · ${stamp(xray.last.created_at)} · #${xray.last.id}` : '—'],
    ...view.available_xrays.map(
      artifact =>
        [artifact.arch, <span className="cgc-mono">{artifact.sha256.slice(0, 8)}</span>] as [string, ReactNode],
    ),
  ];

  let ledger: ReactNode;
  if (active) {
    const tally = (...statuses: XrayReleaseTargetStatus[]) =>
      active.targets.filter(item => statuses.includes(item.status)).length;
    const failed = tally('unverified', 'failed-recovered', 'failed-dirty');
    const pending = tally('pending');
    const skipped = tally('unsupported', 'canceled');
    const parts: LedgerPart[] = [
      { tone: 'ok', label: '本次已升级', count: tally('succeeded') },
      { tone: 'run', label: '升级中', count: tally('dispatched') },
      ...(pending ? [{ tone: 'run' as const, label: '待升级', count: pending }] : []),
      ...(failed ? [{ tone: 'err' as const, label: '失败', count: failed }] : []),
      ...(skipped ? [{ tone: 'none' as const, label: '未执行', count: skipped }] : []),
    ];
    ledger = (
      <ReleaseLedger
        label="本次已升级"
        value={tally('succeeded')}
        unit={`/ ${active.targets.length} 台`}
        period={`升级 #${active.id} · ${active.created_by} ${stamp(active.created_at)} 发起${
          active.status === 'halted' ? ' · 已暂停' : ''
        }`}
        parts={parts}
        facts={facts}
      />
    );
  } else {
    const { current, behind, blocked } = xray.counts;
    ledger = (
      <ReleaseLedger
        label="已是新版"
        value={current}
        unit={`/ ${current + behind} 台`}
        period={`可发 ${target} · 升级范围：逐台选择`}
        parts={[
          { tone: 'ok', label: '已是新版', count: current },
          ...(behind ? [{ tone: 'warn' as const, label: '可升级', count: behind }] : []),
          { tone: 'none', label: '不可升级', count: blocked },
        ]}
        facts={facts}
      />
    );
  }

  const cancelEditing = () => {
    setSelected([]);
    xray.create.reset();
    onEditingChange(false);
  };
  const mutationError = xray.create.error ?? xray.cancel.error ?? xray.retry.error;

  return (
    <>
      {ledger}
      <section className="cgc-main">
        {!picking && mutationError && <ErrorBox error={mutationError} />}
        <MachineTable
          label="逐台 Xray 升级状态"
          editing={picking}
          groups={groups}
          pickAll={
            <input
              type="checkbox"
              aria-label="选择全部可升级的机器"
              checked={allPicked}
              disabled={!editable || busy || actionable.length === 0}
              onChange={() => setSelected(allPicked ? [] : actionable)}
            />
          }
          toolbar={
            <ScopeToolbar
              note="批准后，勾选的机器自行下载校验、升级 Xray 并重启；启动检查失败时恢复原版本。"
              error={xray.create.error ? <ErrorBox error={xray.create.error} /> : undefined}
            >
              <span className="cgc-picked">
                {selected.length ? `已选 ${selected.length} 台` : '勾选下方可升级的机器'}
              </span>
              <span className="sp" />
              <button className="btn" type="button" disabled={xray.create.isPending} onClick={cancelEditing}>
                取消
              </button>
              <button
                className={selected.length ? 'btn primary' : 'btn'}
                type="button"
                disabled={!editable || selected.length === 0 || busy}
                onClick={() => xray.create.mutate(undefined, { onSuccess: () => onEditingChange(false) })}
              >
                {xray.create.isPending ? '批准中…' : '批准'}
              </button>
            </ScopeToolbar>
          }
        />
      </section>

      {xray.askCancel && active && (
        <Confirm
          title="取消 Xray 升级？"
          body={<>尚未领取的机器将停止升级；已完成的机器保持当前版本。</>}
          confirmLabel="取消升级"
          confirmDisabled={xray.cancel.isPending}
          onConfirm={() => xray.cancel.mutate(active.id)}
          onCancel={() => xray.setAskCancel(false)}
        />
      )}
    </>
  );
}

/** 本次升级里的一台：状态取自升级记录，失败原因写在机器名下，可重试的给重试按钮。 */
function targetRow(
  node: NodeAgentStateItem,
  target: XrayReleaseTarget,
  version: string,
  current: string | null,
  xray: XrayRelease,
  editable: boolean,
): MachineRow {
  const status = TARGET_STATUS[target.status];
  return {
    node,
    tone: status.tone,
    status: status.text,
    current: target.status === 'succeeded' ? version : current,
    target: status.tone === 'run' || status.tone === 'err' ? version : null,
    reason: target.error,
    action:
      target.retryable && xray.active ? (
        <button
          className="btn cgc-retry"
          type="button"
          disabled={!editable || xray.busy}
          onClick={() => xray.retry.mutate({ releaseId: xray.active!.id, nodeId: node.node_id })}
        >
          重试
        </button>
      ) : undefined,
  };
}
