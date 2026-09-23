import { useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  cancelXrayRelease,
  confirmXrayRelease,
  createXrayRelease,
  fetchNodes,
  fetchXrayRelease,
  fetchXrayReleaseHistory,
  fetchXrayReleases,
  retryXrayReleaseTarget,
  type NodeAgentStateItem,
  type XrayRelease,
  type XrayReleaseTargetStatus,
  type XrayReleaseView,
} from '../api';
import { Ago, Confirm, ErrorBox, Loading } from '../ui/bits';
import { PanelTitle } from '../ui/icons';
import { randomKey } from '../ui/platform';

type Ask =
  | { kind: 'create' }
  | { kind: 'confirm'; releaseId: number }
  | { kind: 'cancel'; releaseId: number }
  | { kind: 'retry'; releaseId: number; nodeId: string }
  | null;

const TARGET_TEXT: Record<XrayReleaseTargetStatus, string> = {
  pending: '等待本波',
  dispatched: '已下发',
  succeeded: '已完成',
  unverified: '灰度未验证',
  'failed-recovered': '失败 · 已恢复',
  'failed-dirty': '失败 · 需人工处理',
  unsupported: '不支持',
  canceled: '已取消',
};

const RELEASE_TEXT: Record<XrayRelease['status'], string> = {
  running: '推送中',
  halted: '已熔断',
  succeeded: '已成功',
  canceled: '已取消',
};

const targetClass = (status: XrayReleaseTargetStatus) => {
  if (status === 'succeeded') return 'st-succeeded';
  if (status === 'dispatched') return 'st-dispatched';
  if (status === 'failed-dirty') return 'st-failed-dirty';
  if (status === 'unverified' || status === 'failed-recovered' || status === 'unsupported') return 'st-halted';
  if (status === 'canceled') return 'st-canceled';
  return 'st-pending';
};

const releaseClass = (status: XrayRelease['status']) => {
  if (status === 'succeeded') return 'st-succeeded';
  if (status === 'halted') return 'st-halted';
  if (status === 'canceled') return 'st-canceled';
  return 'st-running';
};

const shortSha = (sha: string | null | undefined) => sha?.slice(0, 12) ?? '—';
const noop = () => undefined;

function compactRuntimeVersion(value: string) {
  // Agent 上报的是 Xray 完整 banner（含 slogan、commit、Go 版本和架构）。摘要卡只需
  // 展示可用来区分机队的语义版本；完整摘要仍在展开后的机器表中可查。
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
    (!running || running === installed)
  );
}

function unavailableReason(node: NodeAgentStateItem) {
  if (node.lifecycle_phase !== 'active') return '机器不在 active 生命周期';
  if (!node.desired_poll_fresh) return '最近 90 秒没有领取期望状态';
  if (!node.runtime_report_fresh) return '最近 2 分钟没有运行时上报';
  const installed = node.runtime_versions?.xray_installed_sha256;
  const running = node.runtime_versions?.xray_running_sha256;
  if (!installed) return '还没上报 Xray 摘要；先发布支持该能力的 Agent';
  if (running && running !== installed) return '运行中 Xray 与受管路径不一致；先完成一次本地收敛';
  return '';
}

function hasLaterWave(release: XrayRelease) {
  return release.targets.some(target => target.wave > release.confirmed_wave);
}

function confirmedWaveDone(release: XrayRelease) {
  return release.targets
    .filter(target => target.wave <= release.confirmed_wave)
    .every(target => target.status === 'succeeded');
}

export interface XraySummary {
  loading: boolean;
  version: string | null;
  /** 运行可发版本的机器数 / 有上报的机器数 */
  onVersion: number;
  reported: number;
  /** 其余版本，按台数倒序：v25.8.3 × 2 */
  others: string;
  activeId: number | null;
  activeStatus: XrayRelease['status'] | null;
  /** 发布流水里的「Xray 发布 #N」若干行，按时间倒序。 */
  events: {
    id: number;
    version: string;
    status: XrayRelease['status'];
    statusText: string;
    at: string;
    by: string;
    done: number;
    total: number;
  }[];
}

/** 发布页的软件读数行。与 `XrayReleaseSection` 读同一组查询，不产生额外请求。 */
export function useXraySummary(): XraySummary {
  const releases = useQuery({ queryKey: ['xray-releases'], queryFn: fetchXrayReleases });
  const nodes = useQuery({ queryKey: ['nodes'], queryFn: () => fetchNodes() });
  const empty: XraySummary = {
    loading: releases.isPending || nodes.isPending,
    version: null,
    onVersion: 0,
    reported: 0,
    others: '',
    activeId: null,
    activeStatus: null,
    events: [],
  };
  if (!releases.data || !nodes.data) return empty;
  const view = releases.data;
  const target = compactRuntimeVersion(view.xray_version);
  const counts = new Map<string, number>();
  for (const node of nodes.data.nodes) {
    const version = node.runtime_versions?.xray;
    if (!version) continue;
    const label = compactRuntimeVersion(version);
    counts.set(label, (counts.get(label) ?? 0) + 1);
  }
  const active = view.releases.find(release => release.active);
  return {
    loading: false,
    version: view.xray_version,
    onVersion: counts.get(target) ?? 0,
    reported: [...counts.values()].reduce((sum, count) => sum + count, 0),
    others: [...counts]
      .filter(([label]) => label !== target)
      .sort(([, a], [, b]) => b - a)
      .map(([label, count]) => `${label} × ${count}`)
      .join(' · '),
    activeId: active?.id ?? null,
    activeStatus: active?.status ?? null,
    /* 当前账本与历史摘要在这几项上字段一致，合并后按时间倒序；两处出现同一条时以账本为准。 */
    events: dedupeById([
      ...view.releases.map(release => ({
        id: release.id,
        version: release.version,
        status: release.status,
        statusText: RELEASE_TEXT[release.status],
        at: release.created_at,
        by: release.created_by,
        done: release.targets.filter(target => target.status === 'succeeded').length,
        total: release.targets.length,
      })),
      ...view.history.map(release => ({
        id: release.id,
        version: release.version,
        status: release.status,
        statusText: RELEASE_TEXT[release.status],
        at: release.created_at,
        by: release.created_by,
        done: release.succeeded_count,
        total: release.target_count,
      })),
    ]).sort((left, right) => right.id - left.id),
  };
}

/* 保留先出现的那条：账本在前，历史摘要在后。 */
const dedupeById = <T extends { id: number }>(items: T[]) =>
  items.filter((item, index) => items.findIndex(other => other.id === item.id) === index);

export function XrayReleaseSection({
  editable,
  compact = false,
  open,
  onClose,
}: {
  editable: boolean;
  compact?: boolean;
  /** 由调用方控制展开时（发布页的软件读数行即摘要），收起状态不渲染任何东西。 */
  open?: boolean;
  onClose?: () => void;
}) {
  const qc = useQueryClient();
  const controlled = open !== undefined;
  const [internalExpanded, setExpanded] = useState(!compact);
  const expanded = controlled ? open : internalExpanded;
  const collapse = onClose ?? (() => setExpanded(false));
  const releases = useQuery({
    queryKey: ['xray-releases'],
    queryFn: fetchXrayReleases,
    // Poll only while a rollout can change. The response carries compact history plus one current
    // ledger; older target/event detail is loaded only when an operator opens it.
    refetchInterval: query => (query.state.data?.releases.some(release => release.active) ? 5_000 : false),
  });
  const nodes = useQuery({ queryKey: ['nodes'], queryFn: () => fetchNodes() });
  const [selected, setSelected] = useState<string[]>([]);
  const [canary, setCanary] = useState<string | null>(null);
  const [note, setNote] = useState('');
  const [batchSize, setBatchSize] = useState(10);
  const [idempotencyKey, setIdempotencyKey] = useState(randomKey);
  const [ask, setAsk] = useState<Ask>(null);
  const [auditReleaseId, setAuditReleaseId] = useState<number | null>(null);
  const audit = useQuery({
    queryKey: ['xray-release', auditReleaseId],
    queryFn: () => fetchXrayRelease(auditReleaseId!),
    enabled: auditReleaseId !== null,
  });

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
        canary_node: canary!,
        batch_size: batchSize,
        note: note.trim() || null,
      }),
    onSuccess: async view => {
      await accept(view);
      setSelected([]);
      setCanary(null);
      setNote('');
      setBatchSize(10);
      setIdempotencyKey(randomKey());
    },
  });
  const confirm = useMutation({ mutationFn: confirmXrayRelease, onSuccess: accept });
  const cancel = useMutation({ mutationFn: cancelXrayRelease, onSuccess: accept });
  const retry = useMutation({
    mutationFn: ({ releaseId, nodeId }: { releaseId: number; nodeId: string }) =>
      retryXrayReleaseTarget(releaseId, nodeId),
    onSuccess: accept,
  });
  const loadHistory = useMutation({
    mutationFn: () => fetchXrayReleaseHistory(releases.data!.next_history_before_id!),
    onSuccess: page => {
      qc.setQueryData<XrayReleaseView>(['xray-releases'], current => {
        if (!current) return current;
        const known = new Set(current.history.map(release => release.id));
        return {
          ...current,
          history: [...current.history, ...page.history.filter(release => !known.has(release.id))],
          next_history_before_id: page.next_history_before_id,
        };
      });
    },
  });

  if (releases.isPending || nodes.isPending) return <Loading variant="panel" />;
  if (releases.error || nodes.error) return <ErrorBox error={releases.error ?? nodes.error} />;

  const view = releases.data!;
  const rows = nodes.data?.nodes ?? [];
  const active = view.releases.find(release => release.active);
  const latest = active ?? view.releases[0];
  const mutationError = create.error ?? confirm.error ?? cancel.error ?? retry.error ?? loadHistory.error;
  const busy = create.isPending || confirm.isPending || cancel.isPending || retry.isPending;
  const buildChanged = !!active && active.release_id !== view.available_release_id;
  const alreadyOnThisBuild = (node: NodeAgentStateItem) =>
    view.available_xrays.some(artifact => artifact.sha256 === node.runtime_versions?.xray_installed_sha256);
  const canExerciseCanary = (node: NodeAgentStateItem) => {
    const installed = node.runtime_versions?.xray_installed_sha256;
    const running = node.runtime_versions?.xray_running_sha256;
    return eligible(node) && !!running && running === installed && !alreadyOnThisBuild(node);
  };

  const toggle = (id: string) => {
    const next = selected.includes(id) ? selected.filter(nodeId => nodeId !== id) : [...selected, id];
    setSelected(next);
    if (!canary || !next.includes(canary)) {
      setCanary(rows.find(node => next.includes(node.node_id) && canExerciseCanary(node))?.node_id ?? null);
    }
  };
  const actionable = rows.filter(eligible);
  const allSelected = actionable.length > 0 && actionable.every(node => selected.includes(node.node_id));
  const toggleAll = () => {
    const next = allSelected ? [] : actionable.map(node => node.node_id);
    setSelected(next);
    if (!canary || !next.includes(canary)) {
      setCanary(actionable.find(node => next.includes(node.node_id) && canExerciseCanary(node))?.node_id ?? null);
    }
  };

  if (!expanded) {
    if (controlled) return null;
    const versions = new Map<string, number>();
    for (const node of rows) {
      const version = node.runtime_versions?.xray;
      if (version) {
        const label = compactRuntimeVersion(version);
        versions.set(label, (versions.get(label) ?? 0) + 1);
      }
    }
    const running = [...versions]
      .sort(([left], [right]) => left.localeCompare(right))
      .map(([version, count]) => `${version} × ${count}`)
      .join(' · ');
    return (
      <section className="panel titled cg-software" id="cg-xray">
        <header>
          <PanelTitle of="xray">Xray 版本</PanelTitle>
        </header>
        <div className="cg-soft">
          <div className="cg-soft-row cg-soft-version">
            <span>可发版本</span>
            <b>{view.xray_version}</b>
          </div>
          <div className="cg-soft-row">
            <span>运行中</span>
            <b>{running || '尚未上报'}</b>
          </div>
          <div className="cg-soft-foot">
            <p className="note">先在 1 台机器上验证新 Xray，成功后再逐步发布到其余机器。</p>
            <button className="btn" type="button" onClick={() => setExpanded(true)}>
              {editable ? '选择机器' : '查看'}
            </button>
          </div>
        </div>
      </section>
    );
  }

  return (
    <section className={`panel titled cg-software${compact || controlled ? ' is-expanded' : ''}`} id="cg-xray">
      <header>
        <PanelTitle of="xray">Xray 版本</PanelTitle>
        <span className="sp" />
        {active && (
          <span className={`st ${active.status === 'halted' ? 'st-halted' : 'st-running'}`}>#{active.id}</span>
        )}
        {(compact || controlled) && (
          <button className="btn" type="button" onClick={collapse}>
            收起
          </button>
        )}
      </header>

      {mutationError && <ErrorBox error={mutationError} />}
      <div className="guard" style={{ marginBottom: 10 }}>
        节点最长约 12 分钟领取发布并先下载校验；每台机器带稳定抖动，一次收敛尝试结束后才原子替换并重启
        Xray。新进程检查失败时会恢复本次发布冻结的旧二进制。
      </div>

      <dl className="kv form2">
        <dt>Console 携带</dt>
        <dd>
          <span className="mono">{view.xray_version}</span>
          <span className="dim" style={{ marginLeft: 8 }}>
            {view.available_xrays.map(artifact => `${artifact.arch} ${shortSha(artifact.sha256)}`).join(' · ')}
          </span>
        </dd>
        <dt>最近发布</dt>
        <dd>
          {latest ? (
            <>
              <span className="mono">
                #{latest.id} · {latest.version}
              </span>
              <span className={`st ${releaseClass(latest.status)}`} style={{ marginLeft: 8 }}>
                {RELEASE_TEXT[latest.status]}
              </span>
              <span className="dim" style={{ marginLeft: 8 }}>
                <Ago at={latest.created_at} />
              </span>
            </>
          ) : (
            <span className="dim">还没有 Xray 发布记录</span>
          )}
        </dd>
      </dl>

      {buildChanged && (
        <div className="callout warn">
          这条发布冻结的是上一批 Console 字节，当前 Console 不再提供它。取消该发布后，才能用当前构建创建新发布。
        </div>
      )}

      {active ? (
        <ActiveRelease
          release={active}
          names={new Map(rows.map(node => [node.node_id, node.name || node.node_id]))}
          editable={editable && !buildChanged}
          cancelable={editable}
          busy={busy}
          onConfirm={() => setAsk({ kind: 'confirm', releaseId: active.id })}
          onCancel={() => setAsk({ kind: 'cancel', releaseId: active.id })}
          onRetry={nodeId => setAsk({ kind: 'retry', releaseId: active.id, nodeId })}
        />
      ) : (
        <>
          <table className="tbl ag-t" style={{ marginTop: 10 }}>
            <thead>
              <tr>
                <th className="pick">
                  <input
                    type="checkbox"
                    aria-label="选择全部可发布机器"
                    checked={allSelected}
                    disabled={!editable || busy || actionable.length === 0}
                    onChange={toggleAll}
                  />
                </th>
                <th className="pick">灰度</th>
                <th>机器</th>
                <th>当前 Xray</th>
                <th>资格</th>
              </tr>
            </thead>
            <tbody>
              {rows.map(node => {
                const available = eligible(node);
                const picked = selected.includes(node.node_id);
                const reason = unavailableReason(node);
                const onThisBuild = alreadyOnThisBuild(node);
                const canaryReason = onThisBuild
                  ? '已经是本批字节，不能验证一次真实升级'
                  : !node.runtime_versions?.xray_running_sha256
                    ? 'Xray 当前未运行，不能验证真实启动'
                    : undefined;
                return (
                  <tr key={node.node_id} className={available ? undefined : 'out'} title={reason || undefined}>
                    <td className="pick">
                      <input
                        type="checkbox"
                        aria-label={`发布到 ${node.name || node.node_id}`}
                        checked={picked}
                        disabled={!editable || busy || !available}
                        onChange={() => toggle(node.node_id)}
                      />
                    </td>
                    <td className="pick">
                      <input
                        type="radio"
                        name="xray-canary"
                        aria-label={`设 ${node.name || node.node_id} 为灰度机器`}
                        checked={canary === node.node_id}
                        disabled={!editable || busy || !picked || !canExerciseCanary(node)}
                        title={canaryReason}
                        onChange={() => setCanary(node.node_id)}
                      />
                    </td>
                    <td className="nm">{node.name || node.node_id}</td>
                    <td className="bd">{shortSha(node.runtime_versions?.xray_installed_sha256)}</td>
                    <td>
                      {available ? (
                        onThisBuild ? (
                          <span className="st st-succeeded">已是本批</span>
                        ) : (
                          <span className="st st-pending">可发布</span>
                        )
                      ) : (
                        <span className="st st-skipped">不可发布</span>
                      )}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>

          <dl className="kv form2" style={{ marginTop: 10 }}>
            <dt>每批机器数</dt>
            <dd>
              <input
                className="f short"
                type="number"
                min={1}
                max={500}
                value={batchSize}
                disabled={!editable || busy}
                aria-label="Xray 每批机器数"
                onChange={event => setBatchSize(Math.min(500, Math.max(1, Number(event.target.value) || 1)))}
              />
              <span className="dim" style={{ marginLeft: 8 }}>
                灰度后每次确认最多开放这么多台
              </span>
            </dd>
            <dt>发布说明</dt>
            <dd>
              <input
                className="f words"
                value={note}
                maxLength={2_000}
                disabled={!editable || busy}
                placeholder="可选：升级原因或变更单号"
                aria-label="Xray 发布说明"
                onChange={event => setNote(event.target.value)}
              />
            </dd>
          </dl>
          <div className="guard-foot">
            <span className="note">第 1 波必须让一台未升级机器真实启动新 Xray；后续每批都需人工确认。</span>
            <span className="sp" />
            <button
              className="btn primary"
              disabled={!editable || busy || selected.length === 0 || !canary}
              onClick={() => setAsk({ kind: 'create' })}
            >
              {create.isPending ? '创建中…' : '创建 Xray 发布'}
            </button>
          </div>
        </>
      )}

      {!active && latest && (
        <details className="raw" style={{ marginTop: 12 }}>
          <summary>
            <span>
              最近发布 #{latest.id} · {RELEASE_TEXT[latest.status]}
            </span>
            <span className="sp" />
            <span className="src">
              {latest.targets.filter(target => target.status === 'succeeded').length}/{latest.targets.length} 台 ·{' '}
              <Ago at={latest.finished_at ?? latest.created_at} />
            </span>
          </summary>
          <ActiveRelease
            release={latest}
            names={new Map(rows.map(node => [node.node_id, node.name || node.node_id]))}
            editable={false}
            cancelable={false}
            busy={false}
            onConfirm={noop}
            onCancel={noop}
            onRetry={noop}
          />
        </details>
      )}

      {view.history.length > 0 && (
        <details className="raw" style={{ marginTop: 12 }}>
          <summary>
            <span>历史发布与审计</span>
            <span className="sp" />
            <span className="src">{view.history.length} 条</span>
          </summary>
          <table className="tbl ag-t" style={{ marginTop: 10 }}>
            <thead>
              <tr>
                <th>发布</th>
                <th>结果</th>
                <th>完成</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {view.history.map(summary => (
                <tr key={summary.id}>
                  <td className="nm">
                    #{summary.id} · {summary.version}
                  </td>
                  <td>
                    {summary.succeeded_count}/{summary.target_count} 台
                    {summary.problem_count > 0 && <span className="dim"> · {summary.problem_count} 个问题</span>}
                  </td>
                  <td>
                    <span className={`st ${releaseClass(summary.status)}`}>{RELEASE_TEXT[summary.status]}</span>
                  </td>
                  <td className="r">
                    <button className="btn sm" onClick={() => setAuditReleaseId(summary.id)}>
                      查看审计
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
          {audit.isPending && auditReleaseId !== null && <Loading variant="rows" rows={2} />}
          {audit.error && <ErrorBox error={audit.error} />}
          {audit.data && (
            <div style={{ marginTop: 10 }}>
              <ActiveRelease
                release={audit.data}
                names={new Map(rows.map(node => [node.node_id, node.name || node.node_id]))}
                editable={false}
                cancelable={false}
                busy={false}
                onConfirm={noop}
                onCancel={noop}
                onRetry={noop}
              />
              <table className="tbl ag-t" aria-label={`Xray 发布 ${audit.data.id} 审计事件`}>
                <thead>
                  <tr>
                    <th>时间</th>
                    <th>事件</th>
                    <th>机器 / 操作者</th>
                  </tr>
                </thead>
                <tbody>
                  {audit.data.events.map(event => (
                    <tr key={event.id}>
                      <td>
                        <Ago at={event.created_at} />
                      </td>
                      <td className="mono">{event.kind}</td>
                      <td>{event.node_id ?? event.actor ?? '系统'}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
          {view.next_history_before_id !== null && (
            <div className="guard-foot">
              <span className="sp" />
              <button className="btn sm" disabled={loadHistory.isPending} onClick={() => loadHistory.mutate()}>
                {loadHistory.isPending ? '加载中…' : '加载更早记录'}
              </button>
            </div>
          )}
        </details>
      )}

      {ask?.kind === 'create' && (
        <Confirm
          title="创建 Xray 灰度发布？"
          body={
            <>
              将先更新灰度机器；替换会重启 Xray 并中断它当前承载的连接。其余 {Math.max(0, selected.length - 1)}{' '}
              台会按每批最多 {batchSize} 台逐次确认，在灰度成功前不会收到更新。
            </>
          }
          confirmLabel="创建发布"
          onConfirm={() => {
            setAsk(null);
            create.mutate();
          }}
          onCancel={() => setAsk(null)}
        />
      )}
      {ask?.kind === 'confirm' && (
        <Confirm
          title="确认扩大 Xray 发布？"
          body={<>当前波次已成功。确认后，下一批机器会在各自带抖动的轮询时下载、替换并重启 Xray。</>}
          confirmLabel={active ? `确认第 ${active.confirmed_wave + 1} 波` : '确认下一批'}
          onConfirm={() => {
            const releaseId = ask.releaseId;
            setAsk(null);
            confirm.mutate(releaseId);
          }}
          onCancel={() => setAsk(null)}
        />
      )}
      {ask?.kind === 'cancel' && (
        <Confirm
          title="取消 Xray 发布？"
          body={
            <>
              尚未领取的机器不会再收到更新；已成功替换的机器保持当前版本。正在替换的机器仍会补记最终结果，历史不会被改写。
            </>
          }
          confirmLabel="取消发布"
          onConfirm={() => {
            const releaseId = ask.releaseId;
            setAsk(null);
            cancel.mutate(releaseId);
          }}
          onCancel={() => setAsk(null)}
        />
      )}
      {ask?.kind === 'retry' && (
        <Confirm
          title={`重试 ${rows.find(node => node.node_id === ask.nodeId)?.name || ask.nodeId}？`}
          body={<>这会重新开放该发布并再次尝试替换。先确认导致上次失败的问题已经处理。</>}
          confirmLabel="重试"
          onConfirm={() => {
            const request = { releaseId: ask.releaseId, nodeId: ask.nodeId };
            setAsk(null);
            retry.mutate(request);
          }}
          onCancel={() => setAsk(null)}
        />
      )}
    </section>
  );
}

function ActiveRelease({
  release,
  names,
  editable,
  cancelable,
  busy,
  onConfirm,
  onCancel,
  onRetry,
}: {
  release: XrayRelease;
  names: ReadonlyMap<string, string>;
  editable: boolean;
  cancelable: boolean;
  busy: boolean;
  onConfirm: () => void;
  onCancel: () => void;
  onRetry: (nodeId: string) => void;
}) {
  const failed = release.targets.filter(target =>
    ['unverified', 'failed-recovered', 'failed-dirty', 'unsupported'].includes(target.status),
  );
  const confirmable = release.status === 'running' && hasLaterWave(release) && confirmedWaveDone(release);
  return (
    <>
      <table className="tbl ag-t" style={{ marginTop: 10 }}>
        <thead>
          <tr>
            <th>机器</th>
            <th>波次 · 尝试</th>
            <th>状态</th>
            <th />
          </tr>
        </thead>
        <tbody>
          {release.targets.map(target => {
            const retryable = release.active && target.retryable;
            return (
              <tr key={target.node_id}>
                <td className="nm">{names.get(target.node_id) ?? target.node_id}</td>
                <td>
                  <div>{target.wave === 1 ? '灰度' : `第 ${target.wave} 波`}</div>
                  <div className="dim mono">
                    尝试 {target.attempt} · {shortSha(target.desired_sha256)}
                  </div>
                </td>
                <td>
                  <span
                    className={`st ${targetClass(target.status)}`}
                    title={`${target.error ? `${target.error} · ` : ''}目标 ${shortSha(target.desired_sha256)}`}
                  >
                    {TARGET_TEXT[target.status]}
                  </span>
                </td>
                <td className="r">
                  {retryable && (
                    <button className="btn sm" disabled={!editable || busy} onClick={() => onRetry(target.node_id)}>
                      重试
                    </button>
                  )}
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
      {failed.map(target => (
        <div key={target.node_id} className={target.status === 'failed-dirty' ? 'callout err' : 'callout warn'}>
          <b>{names.get(target.node_id) ?? target.node_id}</b>：{target.error ?? TARGET_TEXT[target.status]}
        </div>
      ))}
      <div className="guard-foot">
        <span className="note">
          已确认到第 {release.confirmed_wave} 波 ·{' '}
          {release.targets.filter(target => target.status === 'succeeded').length}/{release.targets.length} 台完成
        </span>
        <span className="sp" />
        {confirmable && (
          <button className="btn primary" disabled={!editable || busy} onClick={onConfirm}>
            扩大到第 {release.confirmed_wave + 1} 波
          </button>
        )}
        {release.active && (
          <button className="btn danger" disabled={!cancelable || busy} onClick={onCancel}>
            取消
          </button>
        )}
      </div>
    </>
  );
}
