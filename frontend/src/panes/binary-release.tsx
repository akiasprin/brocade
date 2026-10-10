import { useState, type ReactNode } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  cancelBinaryRelease,
  createBinaryRelease,
  fetchNodes,
  fetchBinaryReleases,
  retryBinaryReleaseTarget,
  type NodeAgentStateItem,
  type BinaryReleaseTarget,
  type BinaryReleaseTargetStatus,
  type BinaryReleaseView,
  type BinaryComponent,
} from '../api';
import { Confirm, ErrorBox } from '../ui/bits';
import { randomKey } from '../ui/platform';
import { BinaryReleaseHistory } from './binary-release-history';
import { BINARY_TARGET_STATUS } from '../ui/binary-release-status';
import { binaryReleaseVersion } from '../ui/binary-release-version';
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

const agentHash = (node: NodeAgentStateItem) => node.agent_version?.replace(/^brocade-agent\//, '') ?? null;
const runningHash = (node: NodeAgentStateItem, component: BinaryComponent) =>
  component === 'agent' ? agentHash(node) : node.runtime_versions?.xray_running_sha256;
function eligible(node: NodeAgentStateItem, component: BinaryComponent) {
  if (component === 'agent')
    return node.lifecycle_phase === 'active' && node.desired_poll_fresh && /^[0-9a-f]{64}$/.test(agentHash(node) ?? '');
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

function unavailableReason(node: NodeAgentStateItem, component: BinaryComponent) {
  if (node.lifecycle_phase !== 'active') return '机器不在 active 生命周期';
  if (!node.desired_poll_fresh) return '最近 90 秒没有领取期望状态';
  if (component === 'agent') return /^[0-9a-f]{64}$/.test(agentHash(node) ?? '') ? '' : '还没有上报有效的 Agent 摘要';
  if (!node.runtime_report_fresh) return '最近 2 分钟没有运行时上报';
  const installed = node.runtime_versions?.xray_installed_sha256;
  const running = node.runtime_versions?.xray_running_sha256;
  if (!installed) return '还没上报 Xray 摘要；先发布支持该能力的 Agent';
  if (!running) return 'Xray 当前未运行';
  if (running !== installed) return '运行中 Xray 与受管路径不一致；先完成本地收敛';
  return '';
}

type BinaryState = 'current' | 'behind' | 'blocked';

/** 二进制发布共用数据、勾选和操作；页头与页签内容读取同一份状态。 */
export function useBinaryRelease(component: BinaryComponent) {
  const qc = useQueryClient();
  const releases = useQuery({
    queryKey: ['binary-releases', component],
    queryFn: async () => {
      const previous = qc.getQueryData<BinaryReleaseView>(['binary-releases', component]);
      const next = await fetchBinaryReleases(component);
      // Do not keep pre-upgrade node hashes after a completed attempt. Refresh only when a
      // result arrives, not on every progress poll; closed history queries stay dormant.
      if (
        previous?.current?.active &&
        (!next.current?.active ||
          next.current.targets.some(
            target =>
              target.finished_at &&
              target.finished_at !== previous.current?.targets.find(old => old.node_id === target.node_id)?.finished_at,
          ))
      ) {
        await Promise.all([
          qc.invalidateQueries({ queryKey: ['nodes'] }),
          qc.invalidateQueries({ queryKey: ['binary-release-history', component] }),
          qc.invalidateQueries({ queryKey: ['binary-release-detail', component] }),
        ]);
      }
      return next;
    },
    refetchInterval: query => (query.state.data?.current?.active ? 5_000 : false),
  });
  const nodes = useQuery({ queryKey: ['nodes'], queryFn: () => fetchNodes() });
  const [picked, setSelected] = useState<string[]>([]);
  const [idempotencyKey, setIdempotencyKey] = useState(randomKey);
  const [askCancel, setAskCancel] = useState(false);

  const view = releases.data;
  const fleet = nodes.data?.nodes ?? [];
  const latest = view?.current;
  const active = latest?.active ? latest : undefined;
  const onThisBuild = (node: NodeAgentStateItem) =>
    !!view &&
    eligible(node, component) &&
    view.available.artifacts.some(artifact => artifact.sha256 === runningHash(node, component));
  const stateOf = (node: NodeAgentStateItem): BinaryState =>
    onThisBuild(node) ? 'current' : eligible(node, component) ? 'behind' : 'blocked';
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

  const accept = async (view: BinaryReleaseView) => {
    await qc.cancelQueries({ queryKey: ['binary-releases', component] });
    qc.setQueryData(['binary-releases', component], view);
    await qc.invalidateQueries({ queryKey: ['binary-release-history', component] });
    await qc.invalidateQueries({ queryKey: ['binary-release-detail', component] });
    await qc.invalidateQueries({ queryKey: ['nodes'] });
  };
  const create = useMutation({
    mutationFn: () =>
      createBinaryRelease(component, {
        idempotency_key: idempotencyKey,
        build_id: releases.data!.available.build_id,
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
    mutationFn: (releaseId: number) => cancelBinaryRelease(component, releaseId),
    onSuccess: async view => {
      setAskCancel(false);
      await accept(view);
    },
  });
  const retry = useMutation({
    mutationFn: ({ releaseId, nodeId }: { releaseId: number; nodeId: string }) =>
      retryBinaryReleaseTarget(component, releaseId, nodeId),
    onSuccess: accept,
  });

  return {
    component,
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

export type BinaryRelease = ReturnType<typeof useBinaryRelease>;

/** 页签角标：升级进行中是本次进度，否则是可升级的台数。 */
export function binaryBadge(release: BinaryRelease): { tone: 'run' | 'gold'; text: string } | null {
  if (!release.ready) return null;
  if (release.active) {
    const done = release.active.targets.filter(target => target.status === 'succeeded').length;
    return {
      tone: release.active.status === 'halted' ? 'gold' : 'run',
      text: `${done}/${release.active.targets.length}`,
    };
  }
  return release.counts.behind > 0 ? { tone: 'gold', text: String(release.counts.behind) } : null;
}

export function BinaryReleaseTab({
  release,
  editable,
  editing,
  onEditingChange,
}: {
  release: BinaryRelease;
  editable: boolean;
  editing: boolean;
  onEditingChange: (editing: boolean) => void;
}) {
  if (!release.ready) return release.error ? <TabError error={release.error} /> : null;
  const view = release.view!;
  const { active, selected, setSelected, busy } = release;
  const target = binaryReleaseVersion(release.component, active?.version ?? view.available.version);
  const label = release.component === 'agent' ? 'Agent' : 'Xray';
  const unavailableBuild = !!active && active.build_id !== view.available.build_id;
  const versionOf = (node: NodeAgentStateItem) =>
    release.component === 'agent'
      ? (agentHash(node)?.slice(0, 8) ?? null)
      : node.runtime_versions?.xray
        ? binaryReleaseVersion('xray', node.runtime_versions.xray)
        : null;
  const picking = editing && !active;
  const actionable = release.states
    .filter(item => !item.target && item.state === 'behind')
    .map(item => item.node.node_id);
  const allPicked = actionable.length > 0 && actionable.every(id => selected.includes(id));
  const toggle = (id: string) =>
    setSelected(current => (current.includes(id) ? current.filter(nodeId => nodeId !== id) : [...current, id]));

  const rowOf = ({ node, state, target: releaseTarget }: (typeof release.states)[number]): MachineRow => {
    if (releaseTarget) return targetRow(node, releaseTarget, target, versionOf(node), release, editable);
    const status: Record<BinaryState, { tone: ReleaseTone; text: string }> = {
      current: { tone: 'ok', text: '已一致' },
      behind: { tone: 'warn', text: '可升级' },
      blocked: { tone: 'none', text: '不可升级' },
    };
    return {
      node,
      tone: status[state].tone,
      status: status[state].text,
      current: versionOf(node),
      target: state === 'behind' ? target : null,
      reason: state === 'blocked' ? unavailableReason(node, release.component) || null : null,
      pick: {
        checked: selected.includes(node.node_id),
        disabled: !editable || busy || state !== 'behind',
        onChange: () => toggle(node.node_id),
      },
    };
  };
  const outsideGroup = (key: BinaryState, label: string): MachineGroup => ({
    key,
    label,
    rows: release.states.filter(item => !item.target && item.state === key).map(rowOf),
  });
  const groups: MachineGroup[] = [
    ...(active
      ? [{ key: 'release', label: `升级 #${active.id}`, rows: release.states.filter(item => item.target).map(rowOf) }]
      : []),
    outsideGroup('behind', '可升级'),
    outsideGroup('blocked', '不可升级'),
    outsideGroup('current', '已一致'),
  ];

  const facts: [string, ReactNode][] = [
    ['可发版本', <span className="cgc-mono">{target}</span>],
    active
      ? ['不可升级', `${release.counts.blocked} 台`]
      : [
          '上次批准',
          release.last ? `${release.last.created_by} · ${stamp(release.last.created_at)} · #${release.last.id}` : '—',
        ],
    ...view.available.artifacts.map(
      artifact =>
        [artifact.arch, <span className="cgc-mono">{artifact.sha256.slice(0, 8)}</span>] as [string, ReactNode],
    ),
  ];

  let ledger: ReactNode;
  if (active) {
    const tally = (...statuses: BinaryReleaseTargetStatus[]) =>
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
    const { current, behind, blocked } = release.counts;
    ledger = (
      <ReleaseLedger
        label="已一致"
        value={current}
        unit={`/ ${current + behind} 台`}
        period={`可发 ${target} · 升级范围：逐台选择`}
        parts={[
          { tone: 'ok', label: '已一致', count: current },
          ...(behind ? [{ tone: 'warn' as const, label: '可升级', count: behind }] : []),
          { tone: 'none', label: '不可升级', count: blocked },
        ]}
        facts={facts}
      />
    );
  }

  const cancelEditing = () => {
    setSelected([]);
    release.create.reset();
    onEditingChange(false);
  };
  const mutationError = release.create.error ?? release.cancel.error ?? release.retry.error;

  return (
    <>
      {ledger}
      {unavailableBuild && (
        <section className="cgc-main cgc-span">
          <p className="hint">本次发布的产物已不可用。请停止本次发布后，按当前可发布版本重新创建。</p>
        </section>
      )}
      <section className="cgc-main">
        {!picking && mutationError && <ErrorBox error={mutationError} />}
        <MachineTable
          label={`逐台 ${label} 升级状态`}
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
              note={
                release.component === 'agent'
                  ? '批准时冻结本次机器名单。Agent 下载校验、重启后恢复轮询和运行上报，才确认成功。'
                  : '批准时冻结本次机器名单。Xray 替换后验证监听与运行摘要；启动失败时恢复原版本。'
              }
              error={release.create.error ? <ErrorBox error={release.create.error} /> : undefined}
            >
              <span className="cgc-picked">
                {selected.length ? `已选 ${selected.length} 台` : '勾选下方可升级的机器'}
              </span>
              <span className="sp" />
              <button className="btn" type="button" disabled={release.create.isPending} onClick={cancelEditing}>
                取消
              </button>
              <button
                className={selected.length ? 'btn primary' : 'btn'}
                type="button"
                disabled={!editable || selected.length === 0 || busy}
                onClick={() => release.create.mutate(undefined, { onSuccess: () => onEditingChange(false) })}
              >
                {release.create.isPending ? '批准中…' : '批准'}
              </button>
            </ScopeToolbar>
          }
        />
      </section>

      {view.legacy_approval?.released_at && !view.current && (
        <section className="cgc-main cgc-span">
          <p className="hint">
            迁移前批准：{view.legacy_approval.released_by ?? '—'} · {stamp(view.legacy_approval.released_at)}。
            仅保留原批准信息，不作为新发布单或持续升级授权。
          </p>
        </section>
      )}
      <BinaryReleaseHistory component={release.component} latestId={view.current?.id} />
      {release.askCancel && active && (
        <Confirm
          title={`停止 ${label} 发布？`}
          body={<>停止后不再下发任务；已经执行的替换不会撤销，迟到的执行结果仍会记录。</>}
          confirmLabel="停止发布"
          confirmDisabled={release.cancel.isPending}
          onConfirm={() => release.cancel.mutate(active.id)}
          onCancel={() => release.setAskCancel(false)}
        />
      )}
    </>
  );
}

/** 本次升级里的一台：状态取自升级记录，失败原因写在机器名下，可重试的给重试按钮。 */
function targetRow(
  node: NodeAgentStateItem,
  target: BinaryReleaseTarget,
  version: string,
  current: string | null,
  release: BinaryRelease,
  editable: boolean,
): MachineRow {
  const status = BINARY_TARGET_STATUS[target.status];
  return {
    node,
    tone: status.tone,
    status: status.text,
    current: target.status === 'succeeded' ? version : current,
    target: status.tone === 'run' || status.tone === 'err' ? version : null,
    reason:
      target.error ??
      (target.verification === 'observed' ? '兼容升级：已观测到目标进程恢复，旧 Agent 没有执行回执' : null),
    action:
      target.retryable && release.active && release.active.build_id === release.view?.available.build_id ? (
        <button
          className="btn cgc-retry"
          type="button"
          disabled={!editable || release.busy}
          onClick={() => release.retry.mutate({ releaseId: release.active!.id, nodeId: node.node_id })}
        >
          重试
        </button>
      ) : undefined,
  };
}
