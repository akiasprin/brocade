import { useMemo, useState, useSyncExternalStore } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  ApiError,
  cancelTunnelProbe,
  fetchTunnelProbe,
  fetchTunnelProbeCapability,
  fetchTunnelProbes,
  startTunnelProbe,
  updateTunnelProbePolicy,
  type TunnelProbeHealth,
  type TunnelProbeListItem,
  type TunnelProbeResultStatus,
  type TunnelProbeRun,
  type TunnelProbeSource,
} from './api';
import { draft } from './draft';
import { EmptyState, ErrorBox, Loading, SegmentedControl } from './ui/bits';
import { DialogClose, DialogLayer } from './ui/dialog';
import { PanelTitle } from './ui/icons';
import { confirmDiscardChanges, useUnsavedChanges } from './ui/navigation-guard';

const INTERVALS = [
  { value: 60, label: '每分钟' },
  { value: 300, label: '每 5 分钟' },
  { value: 900, label: '每 15 分钟' },
  { value: 1800, label: '每 30 分钟' },
  { value: 3600, label: '每小时' },
] as const;

const healthText: Record<TunnelProbeHealth, string> = {
  healthy: '正常',
  degraded: '变慢',
  down: '故障',
  paused: '未监测',
  unknown: '暂无结论',
};

const resultText: Record<TunnelProbeResultStatus, string> = {
  ok: '成功',
  timeout: '超时',
  'connect-failed': '连接失败',
  'target-failed': '落点失败',
  unsupported: '不支持',
  canceled: '已取消',
  interrupted: '执行中断',
};

const phaseOrder = ['queued', 'preparing', 'starting-xray', 'requesting'] as const;
const phaseText = ['排队', '冻结配置', '启动 Xray', '请求落点'] as const;

const isActive = (run: TunnelProbeRun | null | undefined) => run?.status === 'queued' || run?.status === 'running';
const intervalText = (seconds: number) => INTERVALS.find(option => option.value === seconds)?.label ?? `${seconds} 秒`;
const dateText = (seconds: number | null) =>
  seconds === null ? '—' : new Date(seconds * 1000).toLocaleString('zh-CN', { hour12: false });

function HealthBadge({ health }: { health: TunnelProbeHealth }) {
  return <span className={`tunnel-probe-health ${health}`}>{healthText[health]}</span>;
}

function ProbePhases({ run }: { run: TunnelProbeRun }) {
  const current = Math.max(0, phaseOrder.indexOf(run.phase as (typeof phaseOrder)[number]));
  return (
    <ol className="tunnel-probe-phases" aria-label="拨测进度">
      {phaseText.map((label, index) => (
        <li className={index < current ? 'done' : index === current ? 'current' : ''} key={label}>
          <span>{index + 1}</span>
          {label}
        </li>
      ))}
    </ol>
  );
}

function ProbeChart({
  points,
  p95Ms,
}: {
  points: { result: TunnelProbeResultStatus; ttfb_ms: number | null }[];
  p95Ms: number | null;
}) {
  const geometry = useMemo(() => {
    const maximum = Math.max(
      1,
      p95Ms ?? 0,
      ...points.flatMap(point => (point.ttfb_ms === null ? [] : [point.ttfb_ms])),
    );
    const x = (index: number) => (points.length <= 1 ? 300 : (index / (points.length - 1)) * 580 + 10);
    const y = (value: number) => 104 - (value / maximum) * 88;
    const segments: string[] = [];
    let current = '';
    points.forEach((point, index) => {
      if (point.result !== 'ok' || point.ttfb_ms === null) {
        if (current) segments.push(current);
        current = '';
        return;
      }
      current += `${current ? ' L' : 'M'} ${x(index).toFixed(1)} ${y(point.ttfb_ms).toFixed(1)}`;
    });
    if (current) segments.push(current);
    return { segments, x, y };
  }, [points, p95Ms]);

  if (points.length === 0) return <p className="tunnel-probe-chart-empty">完成第一次拨测后，这里会出现首字节趋势。</p>;
  return (
    <svg className="tunnel-probe-chart" viewBox="0 0 600 116" preserveAspectRatio="none" aria-label="首字节趋势">
      <line x1="10" x2="590" y1="104" y2="104" className="axis" />
      {p95Ms !== null && (
        <line
          x1="10"
          x2="590"
          y1={geometry.y(p95Ms)}
          y2={geometry.y(p95Ms)}
          className="p95"
          aria-label={`P95 ${p95Ms} 毫秒`}
        />
      )}
      {geometry.segments.map((segment, index) => (
        <path d={segment} className="line" key={index} />
      ))}
      {points.map((point, index) =>
        point.result === 'ok' && point.ttfb_ms !== null ? (
          <circle cx={geometry.x(index)} cy={geometry.y(point.ttfb_ms)} r="2.5" className="reading" key={index} />
        ) : (
          <g key={index}>
            <line x1={geometry.x(index)} x2={geometry.x(index)} y1="86" y2="104" className="failure-drop" />
            <circle cx={geometry.x(index)} cy="104" r="3.5" className="failure" />
          </g>
        ),
      )}
    </svg>
  );
}

function RunRows({ runs }: { runs: TunnelProbeRun[] }) {
  if (runs.length === 0) return <p className="tunnel-probe-inline-empty">还没有拨测记录。</p>;
  return (
    <div className="tunnel-probe-runs">
      {runs.map(run => (
        <article className={`tunnel-probe-run ${run.result ?? run.status}`} key={run.id}>
          <span className="tunnel-probe-run-mark" aria-hidden />
          <span className="tunnel-probe-run-main">
            <b>{run.result ? resultText[run.result] : run.status === 'running' ? '运行中' : '等待执行'}</b>
            <small>
              {run.trigger === 'manual' ? '手动' : '定时'} · {run.source === 'draft' ? '草稿' : 'Serving'} ·{' '}
              {dateText(run.finished_at_unix_secs ?? run.queued_at_unix_secs)}
            </small>
          </span>
          <span className="tunnel-probe-run-reading">
            <b>{run.ttfb_ms === null ? '—' : `${run.ttfb_ms} ms`}</b>
            <small>{run.http_status === null ? (run.error_detail ?? '尚未完成') : `HTTP ${run.http_status}`}</small>
          </span>
          {run.source === 'draft' ? (
            <span
              className="tunnel-probe-run-revision draft"
              title={`点击时冻结的草稿，基于修订 r${run.topology_revision}`}
            >
              草稿 {run.draft_sha256?.slice(0, 8) ?? '—'}
            </span>
          ) : (
            <span className="tunnel-probe-run-revision" title={`Serving generation ${run.serving_generation ?? '—'}`}>
              G{run.serving_generation ?? '—'} · r{run.topology_revision}
            </span>
          )}
        </article>
      ))}
    </div>
  );
}

export function TunnelProbePolicyDialog({
  item,
  onClose,
  onSaved,
}: {
  item: TunnelProbeListItem;
  onClose: () => void;
  onSaved?: () => void;
}) {
  const qc = useQueryClient();
  const [enabled, setEnabled] = useState(item.policy?.enabled ?? false);
  const [interval, setIntervalValue] = useState(item.policy?.interval_secs ?? 300);
  const [timeout, setTimeoutValue] = useState(String(item.policy?.timeout_secs ?? 10));
  const dirty =
    enabled !== (item.policy?.enabled ?? false) ||
    interval !== (item.policy?.interval_secs ?? 300) ||
    timeout !== String(item.policy?.timeout_secs ?? 10);
  const guardScope = `tunnel-probe:${item.tenant_id}:${item.outbound_id}`;
  useUnsavedChanges(dirty, `${item.name} 的定时监测`, guardScope);
  const validTimeout = Number.isInteger(Number(timeout)) && Number(timeout) >= 1 && Number(timeout) <= 120;
  const save = useMutation({
    mutationFn: () =>
      updateTunnelProbePolicy(item.tenant_id, item.outbound_id, {
        enabled,
        interval_secs: interval,
        timeout_secs: Number(timeout),
      }),
    onSuccess: async () => {
      await Promise.all([
        qc.invalidateQueries({ queryKey: ['tunnel-probes'] }),
        qc.invalidateQueries({ queryKey: ['tunnel-probe', item.tenant_id, item.outbound_id] }),
      ]);
      onSaved?.();
      onClose();
    },
  });

  return (
    <DialogLayer
      label={`设置 ${item.name} 定时监测`}
      onClose={onClose}
      canClose={() => confirmDiscardChanges(guardScope)}
    >
      <section className="dialog-surface tunnel-dialog narrow tunnel-probe-dialog">
        <header>
          <b>定时监测</b>
          <span className="sp" />
          <DialogClose className="btn">关闭</DialogClose>
        </header>
        <div className="tunnel-dialog-body">
          <p className="tunnel-probe-dialog-name">
            <b>{item.name}</b>
            <span className="mono">
              {item.tenant_id} / {item.outbound_id}
            </span>
          </p>
          <div className="fgrid one tunnel-probe-policy-fields">
            <div className="row">
              <span className="k">状态</span>
              <span className="v">
                <SegmentedControl
                  value={enabled}
                  options={[
                    { value: false, label: '暂停' },
                    { value: true, label: '启用' },
                  ]}
                  onChange={setEnabled}
                  ariaLabel="定时监测状态"
                />
              </span>
            </div>
            <label className="row">
              <span className="k">监测间隔</span>
              <span className="v">
                <select className="f" value={interval} onChange={event => setIntervalValue(Number(event.target.value))}>
                  {INTERVALS.map(option => (
                    <option value={option.value} key={option.value}>
                      {option.label}
                    </option>
                  ))}
                </select>
              </span>
            </label>
            <label className="row">
              <span className="k">首字节超时</span>
              <span className="v tunnel-probe-timeout-field">
                <input
                  className="f"
                  type="number"
                  min={1}
                  max={120}
                  value={timeout}
                  aria-invalid={!validTimeout || undefined}
                  onChange={event => setTimeoutValue(event.target.value)}
                />
                <span>秒</span>
              </span>
            </label>
          </div>
          <p className="guard">计划在 Console 机器上执行；采用当前 Serving 修订，不会改动节点配置。</p>
          {save.error && <ErrorBox error={save.error} />}
        </div>
        <footer>
          <span className="hint">间隔带稳定抖动，避免同一秒集中发起。</span>
          <span className="sp" />
          <DialogClose className="btn">取消</DialogClose>
          <button className="btn primary" disabled={!validTimeout || save.isPending} onClick={() => save.mutate()}>
            {save.isPending ? '保存中…' : '保存'}
          </button>
        </footer>
      </section>
    </DialogLayer>
  );
}

export function TunnelProbePanel({
  tenantId,
  outboundId,
  draftSupported,
  editable,
}: {
  tenantId: string;
  outboundId: string;
  draftSupported: boolean;
  editable: boolean;
}) {
  const qc = useQueryClient();
  const draftEntries = useSyncExternalStore(draft.subscribe, draft.snapshot);
  const [windowSecs, setWindowSecs] = useState(86_400);
  const [policyOpen, setPolicyOpen] = useState(false);
  const [requestedSource, setRequestedSource] = useState<TunnelProbeSource>('serving');
  const capability = useQuery({
    queryKey: ['tunnel-probe-capability'],
    queryFn: fetchTunnelProbeCapability,
    staleTime: 60_000,
    enabled: editable,
    retry: false,
    retryOnMount: false,
    refetchOnMount: false,
  });
  const probe = useQuery({
    queryKey: ['tunnel-probe', tenantId, outboundId, windowSecs],
    queryFn: () => fetchTunnelProbe(tenantId, outboundId, windowSecs),
    staleTime: 15_000,
    retry: false,
    retryOnMount: false,
    refetchOnMount: false,
    refetchInterval: query => (isActive(query.state.data?.item.latest_run) ? 2_000 : 15_000),
  });
  const refresh = () => qc.invalidateQueries({ queryKey: ['tunnel-probe', tenantId, outboundId] });
  const start = useMutation({
    mutationFn: (source: TunnelProbeSource) => startTunnelProbe(tenantId, outboundId, source),
    onSuccess: refresh,
  });
  const cancel = useMutation({ mutationFn: (id: number) => cancelTunnelProbe(id), onSuccess: refresh });
  const available = capability.data?.available ?? false;

  const startControls = (servingSupported: boolean, servingReason?: string | null) => {
    const selectedSource =
      requestedSource === 'serving' && !servingSupported && draftSupported
        ? 'draft'
        : requestedSource === 'draft' && !draftSupported && servingSupported
          ? 'serving'
          : requestedSource;
    const sourceAvailable = selectedSource === 'serving' ? servingSupported : draftSupported;
    const unavailableReason =
      selectedSource === 'serving'
        ? (servingReason ?? '这条隧道尚未进入 Serving')
        : '当前草稿是 WARP；Console 没有可用于拨测的独立身份';
    return (
      <div className="tunnel-probe-start">
        <select
          className="f"
          value={selectedSource}
          disabled={!editable || start.isPending}
          aria-label="拨测目标"
          onChange={event => setRequestedSource(event.target.value as TunnelProbeSource)}
        >
          <option value="serving" disabled={!servingSupported}>
            Serving{servingSupported ? '' : '（不可用）'}
          </option>
          <option value="draft" disabled={!draftSupported}>
            草稿{draftEntries.length > 0 ? `（${draftEntries.length} 项修改）` : ''}
            {draftSupported ? '' : '（不支持）'}
          </option>
        </select>
        <button
          className="btn primary"
          disabled={!editable || !available || !sourceAvailable || start.isPending}
          title={
            !sourceAvailable
              ? unavailableReason
              : !available
                ? (capability.data?.reason ?? 'Console 拨测组件不可用')
                : undefined
          }
          onClick={() => start.mutate(selectedSource)}
        >
          {start.isPending ? '排队中…' : selectedSource === 'serving' ? '拨测 Serving' : '拨测草稿'}
        </button>
      </div>
    );
  };

  if (probe.isPending) return <Loading variant="panel" />;
  if (probe.error instanceof ApiError && probe.error.status === 404) {
    return (
      <section className="panel config-panel tunnel-panel tunnel-probe-panel">
        <header>
          <PanelTitle of="tunnels">线路拨测</PanelTitle>
          <div className="tunnel-probe-actions">{startControls(false, '这条隧道尚未进入 Serving')}</div>
        </header>
        <EmptyState icon="tunnels" title={draftSupported ? '可拨测当前草稿' : 'WARP 暂不支持 Console 拨测'}>
          {draftSupported
            ? '这条隧道还未进入 Serving；可以先冻结并拨测当前草稿。定时监测仍需在发布后启用。'
            : 'WARP 的身份按机器分配，Console 没有可复用的独立身份；这里不会借用任一节点的私钥。'}
        </EmptyState>
        {!available && capability.data && (
          <p className="tunnel-probe-warning">{capability.data.reason ?? 'Console 拨测组件不可用'}</p>
        )}
        {capability.error && <ErrorBox error={capability.error} />}
        {start.error && <ErrorBox error={start.error} />}
      </section>
    );
  }
  if (probe.error) return <ErrorBox error={probe.error} />;
  const view = probe.data!;
  const active = view.recent_runs.find(isActive) ?? (isActive(view.item.latest_run) ? view.item.latest_run : null);
  return (
    <section className="panel config-panel tunnel-panel tunnel-probe-panel">
      <header>
        <PanelTitle of="tunnels">线路拨测</PanelTitle>
        <HealthBadge health={view.item.health} />
        <div className="tunnel-probe-actions">
          <SegmentedControl
            value={windowSecs}
            options={[
              { value: 86_400, label: '24 小时' },
              { value: 604_800, label: '7 天' },
            ]}
            onChange={setWindowSecs}
            ariaLabel="拨测统计范围"
          />
          <button
            className="btn"
            disabled={!editable || !view.item.supported}
            title={!view.item.supported ? (view.item.unsupported_reason ?? undefined) : undefined}
            onClick={() => setPolicyOpen(true)}
          >
            设置
          </button>
          {active ? (
            <button
              className="btn danger"
              disabled={!editable || cancel.isPending}
              onClick={() => cancel.mutate(active.id)}
            >
              {active.cancel_requested || cancel.isPending ? '取消中…' : '取消'}
            </button>
          ) : (
            startControls(view.item.supported, view.item.unsupported_reason)
          )}
        </div>
      </header>
      <p className="cardsub tunnel-probe-source">
        起点 <b>Console</b> · 落点{' '}
        <span className="mono">{view.item.latest_run ? '使用运行时冻结地址' : '全局探测地址'}</span> · 健康与趋势只统计
        Serving
      </p>
      {!view.item.supported && (
        <p className="tunnel-probe-warning">Serving：{view.item.unsupported_reason ?? '当前不可拨测'}</p>
      )}
      {!available && capability.data && (
        <p className="tunnel-probe-warning">{capability.data.reason ?? 'Console 拨测组件不可用'}</p>
      )}
      {capability.error && <ErrorBox error={capability.error} />}
      {(start.error || cancel.error) && <ErrorBox error={start.error ?? cancel.error} />}
      {active && <ProbePhases run={active} />}
      <div className="tunnel-probe-dashboard">
        <div className="tunnel-probe-plot">
          <ProbeChart points={view.points} p95Ms={view.summary.p95_ms} />
        </div>
        <dl className="tunnel-probe-metrics">
          <div>
            <dt>成功率</dt>
            <dd>{view.summary.success_rate === null ? '—' : `${Math.round(view.summary.success_rate * 100)}%`}</dd>
            <small>
              {view.summary.succeeded}/{view.summary.total} 次
            </small>
          </div>
          <div>
            <dt>P50</dt>
            <dd>{view.summary.p50_ms === null ? '—' : `${view.summary.p50_ms} ms`}</dd>
            <small>首字节中位数</small>
          </div>
          <div>
            <dt>P95</dt>
            <dd>{view.summary.p95_ms === null ? '—' : `${view.summary.p95_ms} ms`}</dd>
            <small>首字节慢尾</small>
          </div>
          <div className={view.summary.failures > 0 ? 'bad' : ''}>
            <dt>失败</dt>
            <dd>{view.summary.failures}</dd>
            <small>超时、连接与落点错误</small>
          </div>
        </dl>
      </div>
      <div className="tunnel-probe-record-head">
        <b>最近记录</b>
        <span>保留 {view.retention_days} 天</span>
      </div>
      <RunRows runs={view.recent_runs} />
      {policyOpen && <TunnelProbePolicyDialog item={view.item} onClose={() => setPolicyOpen(false)} />}
    </section>
  );
}

export function TunnelProbeSettingsSection({ editable }: { editable: boolean }) {
  const qc = useQueryClient();
  const [editing, setEditing] = useState<TunnelProbeListItem | null>(null);
  const probes = useQuery({
    queryKey: ['tunnel-probes'],
    queryFn: fetchTunnelProbes,
    staleTime: 30_000,
    refetchInterval: 30_000,
  });
  const capability = useQuery({
    queryKey: ['tunnel-probe-capability'],
    queryFn: fetchTunnelProbeCapability,
    staleTime: 60_000,
  });
  const toggle = useMutation({
    mutationFn: ({ item, enabled }: { item: TunnelProbeListItem; enabled: boolean }) =>
      updateTunnelProbePolicy(item.tenant_id, item.outbound_id, {
        enabled,
        interval_secs: item.policy?.interval_secs ?? 300,
        timeout_secs: item.policy?.timeout_secs ?? 10,
      }),
    onSuccess: () => qc.invalidateQueries({ queryKey: ['tunnel-probes'] }),
  });

  // Both reads are required to describe this section. On the initial mount, extend the settings
  // page's existing loading boundary instead of replacing this one section with a second skeleton.
  if (probes.isPending || capability.isPending) return <Loading variant="settings" />;
  const items = probes.data?.items.filter(item => item.supported) ?? [];

  return (
    <section className="panel config-panel tunnel-probe-settings" id="set-tunnel-probes">
      <header>
        <PanelTitle of="tunnels">隧道定时监测</PanelTitle>
        <span className="tunnel-probe-setting-count">{items.length} 条隧道</span>
      </header>
      <p className="cardsub">由 Console 定时验证隧道可用性</p>
      {probes.error || capability.error ? (
        <ErrorBox error={probes.error ?? capability.error} />
      ) : (
        <div className="setgrp settings-block tunnel-probe-setting-content">
          <p className="eyebrow">执行环境</p>
          <div className="tunnel-probe-setting-facts">
            <span>Console</span>
            <code>{probes.data!.endpoint_url}</code>
            <b>{capability.data?.available ? `Xray ${capability.data.version ?? ''}` : '不可用'}</b>
          </div>
          {capability.data && !capability.data.available && (
            <p className="tunnel-probe-warning">{capability.data.reason ?? 'Console 拨测组件不可用'}</p>
          )}
          {toggle.error && <ErrorBox error={toggle.error} />}
          <p className="eyebrow tunnel-probe-setting-list-title">监测隧道</p>
          <div className="tunnel-probe-setting-list">
            {items.map(item => {
              const busy = toggle.isPending && toggle.variables.item.outbound_id === item.outbound_id;
              return (
                <article className="tunnel-probe-setting-row" key={`${item.tenant_id}/${item.outbound_id}`}>
                  <span className="tunnel-probe-setting-name">
                    <b>{item.name}</b>
                    <small>
                      {item.tenant_id} · {item.protocol}
                    </small>
                  </span>
                  <HealthBadge health={item.health} />
                  <span className="tunnel-probe-setting-schedule">
                    <b>{item.policy?.enabled ? intervalText(item.policy.interval_secs) : '未启用'}</b>
                    <small>
                      {item.policy?.enabled
                        ? `下次 ${dateText(item.policy.next_run_at_unix_secs)}`
                        : (item.unsupported_reason ?? '可随时手动拨测')}
                    </small>
                  </span>
                  <label className={`switch${!item.supported || !editable ? ' disabled' : ''}`}>
                    <input
                      type="checkbox"
                      checked={item.policy?.enabled ?? false}
                      disabled={!item.supported || !editable || busy}
                      aria-label={`${item.name} 定时监测`}
                      onChange={event => toggle.mutate({ item, enabled: event.target.checked })}
                    />
                    <span className="switch-ui">
                      <span />
                    </span>
                    <span className="switch-label">{busy ? '保存中' : item.policy?.enabled ? '已启用' : '已暂停'}</span>
                  </label>
                  <button className="btn" disabled={!item.supported || !editable} onClick={() => setEditing(item)}>
                    设置
                  </button>
                </article>
              );
            })}
            {items.length === 0 && <p className="tunnel-probe-inline-empty">还没有可监测的隧道。</p>}
          </div>
        </div>
      )}
      {editing && <TunnelProbePolicyDialog item={editing} onClose={() => setEditing(null)} />}
    </section>
  );
}
