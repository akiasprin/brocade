import { useState, type ReactNode } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  cancelBinaryRelease,
  createBinaryRelease,
  fetchNodes,
  fetchBinaryReleases,
  fetchDeploymentSettings,
  retryBinaryReleaseTarget,
  type NodeAgentStateItem,
  type BinaryReleaseTarget,
  type BinaryReleaseTargetStatus,
  type BinaryReleaseView,
  type BinaryComponent,
  type XrayReleaseStrategy,
  XRAY_HANDOFF_PROTOCOL_VERSION,
  XRAY_HANDOFF_RESTART_FALLBACK_PROTOCOL_VERSION,
} from '../api';
import { Confirm, ErrorBox } from '../ui/bits';
import { randomKey } from '../ui/platform';
import { BinaryReleaseHistory } from './binary-release-history';
import { BINARY_TARGET_STATUS } from '../ui/binary-release-status';
import { binaryReleaseVersion, xrayRunningCommit } from '../ui/binary-release-version';
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

/* 一次升级里每台机器的状态，用与 Agent 页相同的词：待升级 / 升级中 / 已升级。 */

const agentHash = (node: NodeAgentStateItem) => node.agent_version?.replace(/^brocade-agent\//, '') ?? null;
const runningHash = (node: NodeAgentStateItem, component: BinaryComponent) =>
  component === 'agent' ? agentHash(node) : node.runtime_versions?.xray_running_sha256;
function baseEligible(node: NodeAgentStateItem, component: BinaryComponent) {
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

function handoffStrategyEligible(node: NodeAgentStateItem) {
  return (
    node.agent_protocol_version != null &&
    node.agent_protocol_version >= XRAY_HANDOFF_PROTOCOL_VERSION &&
    (node.runtime_versions?.xray_handoff?.state === 'ready' ||
      node.agent_protocol_version >= XRAY_HANDOFF_RESTART_FALLBACK_PROTOCOL_VERSION)
  );
}

function eligible(node: NodeAgentStateItem, component: BinaryComponent, strategy: XrayReleaseStrategy) {
  return (
    baseEligible(node, component) && (component !== 'xray' || strategy === 'restart' || handoffStrategyEligible(node))
  );
}

function unavailableReason(node: NodeAgentStateItem, component: BinaryComponent, strategy: XrayReleaseStrategy) {
  if (node.lifecycle_phase !== 'active') return '机器不在 active 生命周期';
  if (!node.desired_poll_fresh) return '最近 90 秒没有领取期望状态';
  if (component === 'agent') return /^[0-9a-f]{64}$/.test(agentHash(node) ?? '') ? '' : '还没有上报有效的 Agent 摘要';
  if (!node.runtime_report_fresh) return '最近 2 分钟没有运行时上报';
  const installed = node.runtime_versions?.xray_installed_sha256;
  const running = node.runtime_versions?.xray_running_sha256;
  if (!installed) return '还没上报 Xray 摘要；先发布支持该能力的 Agent';
  if (!running) return 'Xray 当前未运行';
  if (running !== installed) return '运行中 Xray 与受管路径不一致；先完成本地收敛';
  if (strategy !== 'restart' && !handoffStrategyEligible(node)) {
    return node.runtime_versions?.xray_handoff?.reason || 'Agent 尚未确认运行中的 Xray 可平滑交接';
  }
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
  const settings = useQuery({
    queryKey: ['deployment-settings'],
    queryFn: fetchDeploymentSettings,
    enabled: component === 'xray',
  });
  const [picked, setSelected] = useState<string[]>([]);
  const [idempotencyKey, setIdempotencyKey] = useState(randomKey);
  const [askCancel, setAskCancel] = useState(false);
  const [askCreate, setAskCreate] = useState(false);
  const handoffBeta = settings.data?.xray_handoff_beta === true;
  const [selection, setSelection] = useState<{ beta: boolean; strategy: Exclude<XrayReleaseStrategy, 'handoff'> }>(
    () => ({ beta: handoffBeta, strategy: 'restart' }),
  );
  if (selection.beta !== handoffBeta) setSelection({ beta: handoffBeta, strategy: 'restart' });
  const xrayStrategy = handoffBeta && selection.beta ? selection.strategy : 'restart';
  const setXrayStrategy = (strategy: Exclude<XrayReleaseStrategy, 'handoff'>) =>
    setSelection({ beta: handoffBeta, strategy });
  const [xrayDrainTimeoutSecs, setXrayDrainTimeoutSecs] = useState('120');
  const drainSeconds = Number(xrayDrainTimeoutSecs);
  const drainTimeoutValid =
    /^\d+$/.test(xrayDrainTimeoutSecs) &&
    Number.isInteger(drainSeconds) &&
    drainSeconds >= 10 &&
    drainSeconds <= 86_400;

  const view = releases.data;
  const fleet = nodes.data?.nodes ?? [];
  const latest = view?.current;
  const active = latest?.active ? latest : undefined;
  const onThisBuild = (node: NodeAgentStateItem) =>
    !!view &&
    baseEligible(node, component) &&
    view.available.artifacts.some(artifact => artifact.sha256 === runningHash(node, component));
  const stateOf = (node: NodeAgentStateItem): BinaryState =>
    onThisBuild(node) ? 'current' : eligible(node, component, xrayStrategy) ? 'behind' : 'blocked';
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
    mutationFn: () => {
      if (component === 'xray' && xrayStrategy !== 'restart' && !drainTimeoutValid)
        throw new Error('排空等待须为 10–86400 秒的整数。');
      return createBinaryRelease(component, {
        idempotency_key: idempotencyKey,
        build_id: releases.data!.available.build_id,
        nodes: selected,
        note: null,
        ...(component === 'xray'
          ? {
              xray_strategy: xrayStrategy,
              ...(xrayStrategy === 'restart' ? {} : { xray_drain_timeout_secs: drainSeconds }),
            }
          : {}),
      });
    },
    onSuccess: async view => {
      setAskCreate(false);
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
    ready: !!view && !!nodes.data && (component !== 'xray' || !!settings.data),
    error: releases.error ?? nodes.error ?? (component === 'xray' ? settings.error : undefined),
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
    askCreate,
    setAskCreate,
    xrayStrategy,
    handoffBeta,
    setXrayStrategy,
    xrayDrainTimeoutSecs,
    setXrayDrainTimeoutSecs,
    drainTimeoutValid,
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
      : xrayRunningCommit(view, node.runtime_versions?.xray_running_sha256);
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
      reason: state === 'blocked' ? unavailableReason(node, release.component, release.xrayStrategy) || null : null,
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
    [release.component === 'xray' ? '可发 commit' : '可发版本', <span className="cgc-mono">{target}</span>],
    ...(release.component === 'xray' && active
      ? ([['执行策略', xrayStrategyLabel(active.xray_strategy)]] as [string, ReactNode][])
      : []),
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
    const recoveredAfterDirty = active.targets.filter(
      item =>
        item.status === 'succeeded' && item.recovered_after_dirty === true && item.reported_forced_retirement !== true,
    ).length;
    const upgraded = tally('succeeded') - recoveredAfterDirty;
    const failed = tally('unverified', 'failed-recovered', 'failed-dirty');
    const timedOut = active.targets.filter(item => item.status === 'dispatched' && item.execution_timed_out).length;
    const running = tally('dispatched') - timedOut;
    const pending = tally('pending');
    const skipped = tally('unsupported', 'canceled');
    const parts: LedgerPart[] = [
      ...(upgraded ? [{ tone: 'ok' as const, label: '已升级', count: upgraded }] : []),
      ...(recoveredAfterDirty ? [{ tone: 'ok' as const, label: '自动恢复完成', count: recoveredAfterDirty }] : []),
      { tone: 'run', label: '升级中', count: running },
      ...(timedOut ? [{ tone: 'warn' as const, label: '整体执行超时', count: timedOut }] : []),
      ...(pending ? [{ tone: 'run' as const, label: '待升级', count: pending }] : []),
      ...(failed ? [{ tone: 'err' as const, label: '失败', count: failed }] : []),
      ...(skipped ? [{ tone: 'none' as const, label: '未执行', count: skipped }] : []),
    ];
    ledger = (
      <ReleaseLedger
        label="已升级"
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
  const approve = () => {
    if (release.component === 'xray') {
      release.setAskCreate(true);
      return;
    }
    release.create.mutate(undefined, { onSuccess: () => onEditingChange(false) });
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
                  : xrayStrategyDescription(release.xrayStrategy)
              }
              error={release.create.error ? <ErrorBox error={release.create.error} /> : undefined}
            >
              {release.component === 'xray' && (
                <div className="cgc-strategy" role="radiogroup" aria-label="Xray 升级策略">
                  {(
                    [
                      ['restart', '直接重启'],
                      ['handoff-force', '双跑交接 Beta'],
                    ] as const
                  )
                    .filter(([value]) => value === 'restart' || release.handoffBeta)
                    .map(([value, text]) => (
                      <label key={value} title={xrayStrategyDescription(value)}>
                        <input
                          type="radio"
                          name="xray-release-strategy"
                          value={value}
                          checked={release.xrayStrategy === value}
                          disabled={busy}
                          onChange={() => release.setXrayStrategy(value)}
                        />
                        <span>{text}</span>
                      </label>
                    ))}
                  {!release.handoffBeta && <span className="hint">进程交接 Beta 未开启，仅支持直接重启。</span>}
                  {release.xrayStrategy !== 'restart' && (
                    <label className="cgc-drain-timeout">
                      <span>等待</span>
                      <input
                        type="number"
                        min={10}
                        max={86_400}
                        step={1}
                        aria-label="旧进程排空等待秒数"
                        value={release.xrayDrainTimeoutSecs}
                        disabled={busy}
                        aria-invalid={!release.drainTimeoutValid}
                        aria-describedby={!release.drainTimeoutValid ? 'xray-drain-error' : undefined}
                        onChange={event => release.setXrayDrainTimeoutSecs(event.currentTarget.value)}
                      />
                      <span>秒</span>
                      {!release.drainTimeoutValid && (
                        <span id="xray-drain-error" role="alert">
                          请输入 10–86400 秒的整数
                        </span>
                      )}
                    </label>
                  )}
                </div>
              )}
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
                disabled={
                  !editable ||
                  selected.length === 0 ||
                  busy ||
                  (release.component === 'xray' && release.xrayStrategy !== 'restart' && !release.drainTimeoutValid)
                }
                onClick={approve}
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
      {release.askCreate && release.component === 'xray' && (
        <Confirm
          title={release.xrayStrategy === 'restart' ? '确认直接重启 Xray？' : '确认双跑升级？'}
          body={
            release.xrayStrategy === 'restart' ? (
              <>直接重启会立即中断这批机器上的现有 Xray 连接；该选择会写入发布审计。</>
            ) : (
              <>
                新实例接管后，旧连接最多保留 {release.xrayDrainTimeoutSecs} 秒；到期结束旧进程，剩余旧连接断开。 Agent
                v40 起交接条件不足时会降级直接重启，并记录原因。
              </>
            )
          }
          confirmLabel={release.xrayStrategy === 'restart' ? '确认破坏性升级' : '确认升级'}
          danger={release.xrayStrategy === 'restart'}
          confirmDisabled={release.create.isPending}
          onConfirm={() => release.create.mutate(undefined, { onSuccess: () => onEditingChange(false) })}
          onCancel={() => release.setAskCreate(false)}
        />
      )}
    </>
  );
}

function xrayStrategyLabel(strategy: XrayReleaseStrategy | null | undefined) {
  // 历史发布仍按批准时冻结的策略显示；新建发布不再提供普通 handoff。
  if (strategy === 'handoff') return '平滑交接';
  if (strategy === 'handoff-force') return '双跑交接 Beta';
  return '直接重启';
}

function xrayStrategyDescription(strategy: Exclude<XrayReleaseStrategy, 'handoff'>) {
  if (strategy === 'handoff-force') return 'Beta：新实例先接管；旧连接到期后强制结束';
  return '停止旧进程再启动新版本，现有连接会中断';
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
  const overallTimedOut = target.status === 'dispatched' && target.execution_timed_out === true;
  const recoveredAfterDirty =
    target.status === 'succeeded' &&
    target.recovered_after_dirty === true &&
    target.reported_forced_retirement !== true;
  const status = recoveredAfterDirty
    ? { tone: 'ok' as const, text: '自动恢复完成' }
    : overallTimedOut
      ? { tone: 'warn' as const, text: '整体执行超时' }
      : BINARY_TARGET_STATUS[target.status];
  const reportedUpgrade = node.xray_upgrade;
  const runtime =
    release.component === 'xray' &&
    target.status !== 'succeeded' &&
    reportedUpgrade != null &&
    reportedUpgrade.release_id === release.active?.id &&
    reportedUpgrade.attempt === target.attempt
      ? reportedUpgrade
      : null;
  const runtimeText = runtime
    ? [
        `阶段 ${runtime.phase}`,
        `旧 PID ${runtime.old.pid}${runtime.old.alive ? '' : '（已退出）'}`,
        runtime.candidate
          ? `新 PID ${runtime.candidate.pid}${runtime.candidate.alive ? '' : '（已退出）'}`
          : '新 PID 尚未启动',
        runtime.old.active_connections == null ? null : `旧连接 ${runtime.old.active_connections}`,
      ]
        .filter(Boolean)
        .join(' · ')
    : null;
  const timeoutText = overallTimedOut
    ? `超过 1 小时仍未收到终态回执${
        target.execution_deadline_at ? `（截止 ${stamp(target.execution_deadline_at)}）` : ''
      }；不会自动终止 Xray`
    : null;
  const completionText = recoveredAfterDirty
    ? '本次曾进入需处理状态，Agent 随后完成本地恢复并确认目标版本正常运行'
    : null;
  const detail =
    [completionText, timeoutText, runtime?.blocked_reason ?? runtimeText].filter(Boolean).join(' · ') || null;
  return {
    node,
    tone: status.tone,
    status: status.text,
    current: target.status === 'succeeded' ? version : current,
    target: target.status === 'dispatched' || status.tone === 'err' ? version : null,
    reason:
      target.error ??
      detail ??
      (target.verification === 'observed' ? '兼容升级：已观测到目标进程恢复，旧 Agent 没有执行回执' : null),
    action:
      target.retryable && !runtime && release.active && release.active.build_id === release.view?.available.build_id ? (
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
