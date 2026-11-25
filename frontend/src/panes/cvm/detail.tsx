import { navigate } from '../../forge/route';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useEffect, useId, useRef, useState, type ReactNode } from 'react';
import { commandCvm, fetchCvmInstanceEntries, type CvmChange, type CvmInstance, type CvmWorkspace } from '../../api';
import { ErrorBox } from '../../ui/bits';
import { Icon } from '../../ui/icons';
import { ActionMenu } from '../../ui/choice-menu';
import { DialogClose, DialogLayer } from '../../ui/dialog';
import { AccessDialog, canEditInstanceAccess } from './access';
import { CvmOverview } from './overview';
import { CvmArtifacts } from './artifacts';
import { CvmMonitoringPanel, useCvmObservation } from './monitoring';

import { uncertainCvmResult } from './network-common';
import { CommandDialog } from './operations';
import { AppliedState, CvmPageHeader, CvmTabs, instanceLamp } from './presentation';
import { TitleActions } from '../../ui/title-actions';
import { CvmTerminalAccess } from './terminal-access';
import { dur } from '../telemetry-format';
import { QUERY, requestId, guestTerminalAccess, hasPendingCvmChange, invalidateCvmChanges } from './queries';

type DetailTab = 'overview' | 'network' | 'monitoring' | 'artifacts';

export function InstanceDetail({
  instance,
  workspace,
  initialTab = 'overview',
  onTabChange,
  networkEditor,
  networkDraftCount = 0,
  topBar,
}: {
  instance: CvmInstance;
  workspace: CvmWorkspace;
  initialTab?: DetailTab;
  onTabChange?: (tab: DetailTab) => void;
  networkEditor: ReactNode;
  networkDraftCount?: number;
  topBar?: ReactNode;
}) {
  const qc = useQueryClient();
  const [localTab, setLocalTab] = useState<DetailTab>(initialTab);
  const selectedTab = onTabChange ? initialTab : localTab;
  const setDetailTab = onTabChange ?? setLocalTab;
  const detailId = useId();
  const body = useRef<HTMLDivElement>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [editAccess, setEditAccess] = useState(false);
  const [unconfirmedChange, setUnconfirmedChange] = useState<CvmChange['action']>();
  const attempt = useRef<{ change: CvmChange; generation: number; id: string; inputSha256?: string } | null>(null);
  const [dialog, setDialog] = useState<null | {
    action: 'start' | 'stop' | 'restart' | 'reinstall' | 'configure';
  }>(null);
  const canWrite = workspace.can_write && !workspace.redacted;
  const canUse = workspace.can_use && !workspace.redacted;
  const canReadArtifacts = !workspace.redacted && (canWrite || !!workspace.can_publish);
  const detailTab = selectedTab === 'artifacts' && !canReadArtifacts ? 'overview' : selectedTab;
  const host = workspace.hosts.find(item => item.node_id === instance.node_id);
  const caps = host?.capabilities;
  const spec = instance.desired.spec;
  const native = spec.execution.kind === 'baremetal';
  const deleted = instance.desired.power === 'deleted';
  const staged = hasPendingCvmChange(instance);
  const ready = instance.status === 'applied' && !staged;
  const command = useMutation({
    mutationFn: ({
      change,
      generation = instance.desired.generation,
      inputSha256,
    }: {
      change: CvmChange;
      generation?: number;
      inputSha256?: string;
    }) => {
      if (attempt.current && JSON.stringify(attempt.current.change) !== JSON.stringify(change))
        throw new Error('上一项操作的结果尚未确认，请先核查或重试原操作。');
      attempt.current ??= { change, generation, id: requestId(), inputSha256 };
      return commandCvm(
        instance.desired.id,
        attempt.current.generation,
        change,
        attempt.current.id,
        change.action === 'configure' || change.action === 'detach' ? attempt.current.inputSha256 : undefined,
      );
    },
    onError: error => {
      if (!uncertainCvmResult(error)) attempt.current = null;
      setUnconfirmedChange(attempt.current?.change.action);
      void qc.invalidateQueries({ queryKey: QUERY });
    },
    onSuccess: result => {
      attempt.current = null;
      setUnconfirmedChange(undefined);
      qc.setQueryData<CvmWorkspace>(QUERY, old =>
        old
          ? { ...old, instances: old.instances.map(item => (item.desired.id === result.desired.id ? result : item)) }
          : old,
      );
      invalidateCvmChanges(qc);
      setConfirmDelete(false);
      setDialog(null);
    },
  });
  const running = instance.report?.power === 'running';
  const observation = useCvmObservation(instance.desired.id, 3600, workspace.redacted);
  const entries = useQuery({
    queryKey: ['cvm-instance-entries', instance.desired.id],
    queryFn: () => fetchCvmInstanceEntries(instance.desired.id),
    enabled: !workspace.redacted,
    staleTime: 15_000,
    refetchInterval: 15_000,
  });
  const networkCount =
    spec.interfaces.length + spec.routing.routes.length + (workspace.redacted ? 0 : (entries.data?.length ?? 0));
  const uptime = running && observation.current ? observation.latest?.uptime_millis : undefined;
  const terminal = guestTerminalAccess(instance, host, canUse && !deleted);
  const canRecover = canWrite && !command.isPending;
  const writable = canRecover && !!host?.fresh && !deleted;
  const usable = canUse && !command.isPending && !!host?.fresh && !deleted;
  const hostNames = new Map(workspace.hosts.map(item => [item.node_id, item.name]));
  // The unfiltered list groups instances by host; preserve workspace order within each host.
  const instances = workspace.instances
    .filter(item => item.reserved)
    .sort(
      (a, b) =>
        (hostNames.get(a.node_id) ?? a.node_id).localeCompare(hostNames.get(b.node_id) ?? b.node_id) ||
        a.node_id.localeCompare(b.node_id),
    );
  const index = instances.findIndex(item => item.desired.id === instance.desired.id);
  const previous = index < 0 ? undefined : instances[index - 1]?.desired.id;
  const next = index < 0 ? undefined : instances[index + 1]?.desired.id;
  const routeTab = detailTab === 'overview' ? undefined : detailTab;
  const step = (id: string) => navigate('cvm', { p: 'instance', id, ...(routeTab ? { tab: routeTab } : {}) });
  const canStep = !dialog && !confirmDelete && !editAccess && !command.isPending;
  useEffect(() => {
    if (!canStep || (!previous && !next)) return;
    const onKeyDown = (event: KeyboardEvent) => {
      const id = event.key === 'ArrowUp' ? previous : event.key === 'ArrowDown' ? next : undefined;
      if (
        !id ||
        event.defaultPrevented ||
        event.altKey ||
        event.ctrlKey ||
        event.metaKey ||
        event.shiftKey ||
        event.isComposing
      )
        return;
      const target = event.target instanceof Element ? event.target : document.activeElement;
      const scope = body.current?.parentElement;
      if (!target || !scope) return;
      // Shell navigation restores focus to the workspace itself, not the nested detail header.
      if (target !== document.body && target !== scope.closest('.fg-view') && !scope.contains(target)) return;
      if (document.querySelector('.dialog-layer:not([inert])')) return;
      const control = target.closest(
        'input, textarea, select, button, a, summary, [contenteditable], [role="tablist"], [role="menu"], [role="listbox"], [role="slider"], [tabindex]',
      );
      if (control && scope.contains(control) && !control.hasAttribute('data-cvm-step')) return;
      event.preventDefault();
      navigate('cvm', { p: 'instance', id, ...(routeTab ? { tab: routeTab } : {}) });
    };
    document.addEventListener('keydown', onKeyDown);
    return () => document.removeEventListener('keydown', onKeyDown);
  }, [canStep, next, previous, routeTab]);
  const networkBadge =
    networkDraftCount > 0 && canWrite
      ? { count: networkDraftCount, label: `${networkDraftCount} 处未保存的网络变更`, tone: 'gold' as const }
      : { count: networkCount, label: `${networkCount} 个入口、网卡与路由`, tone: 'dim' as const };
  return (
    <>
      {topBar}
      <CvmPageHeader
        title={spec.name}
        lamp={instanceLamp(instance)}
        // 运行与停止由图标角上的状态灯表达；处理中、失败等需要注意的状态仍用文字标出。
        state={
          staged ||
          instance.status !== 'applied' ||
          (!instance.reserved && instance.operation_kind === 'undo_pending') ? (
            <AppliedState instance={instance} />
          ) : undefined
        }
        // 裸机就是宿主本身，再写宿主名是重复信息；只留运行时长。
        meta={
          [native ? null : (host?.name ?? instance.node_id), uptime == null ? null : `已运行 ${dur(uptime / 1000)}`]
            .filter(Boolean)
            .join(' · ') || undefined
        }
        detail
        tabs={
          <CvmTabs
            label="实例详情页签"
            value={detailTab}
            items={[
              { key: 'overview', label: '概览', icon: 'observe' },
              {
                key: 'network',
                label: '网络',
                icon: 'dns',
                badge: networkBadge,
              },
              { key: 'monitoring', label: '监控与诊断', shortLabel: '监控', icon: 'diag' },
              ...(canReadArtifacts ? [{ key: 'artifacts', label: '产物', icon: 'artifacts' } as const] : []),
            ]}
            onChange={setDetailTab}
            panelId={`${detailId}-content`}
          />
        }
        tools={
          (canUse || canWrite || previous || next) && (
            <TitleActions
              status={
                (previous || next) && (
                  <div className="title-action-group cvm-detail-nav" role="group" aria-label="切换容器">
                    <div className="title-action-item">
                      <button
                        className="btn icon"
                        type="button"
                        data-cvm-step=""
                        aria-label="上一个容器"
                        aria-keyshortcuts="ArrowUp"
                        title="上一个容器 (↑)"
                        disabled={!canStep || !previous}
                        onClick={() => previous && step(previous)}
                      >
                        <Icon of="arrowUp" size={14} />
                      </button>
                    </div>
                    <div className="title-action-item">
                      <button
                        className="btn icon"
                        type="button"
                        data-cvm-step=""
                        aria-label="下一个容器"
                        aria-keyshortcuts="ArrowDown"
                        title="下一个容器 (↓)"
                        disabled={!canStep || !next}
                        onClick={() => next && step(next)}
                      >
                        <Icon of="arrowDown" size={14} />
                      </button>
                    </div>
                  </div>
                )
              }
            >
              {canUse && !native && !deleted && (
                <>
                  <button
                    className="btn title-action-compact"
                    disabled={!usable || !ready || !caps?.stable_storage || (running ? !caps.stop : !caps?.start)}
                    aria-label={running ? '关机' : '开机'}
                    onClick={() => setDialog({ action: running ? 'stop' : 'start' })}
                  >
                    <Icon of={running ? 'pause' : 'play'} size={14} />
                    <span className="title-action-label">{running ? '关机' : '开机'}</span>
                  </button>
                  <button
                    className="btn title-action-compact"
                    disabled={!usable || !ready || !running || !caps?.restart}
                    aria-label="重启"
                    onClick={() => setDialog({ action: 'restart' })}
                  >
                    <Icon of="refresh" size={14} />
                    <span className="title-action-label">重启</span>
                  </button>
                </>
              )}
              {canUse && !deleted && (
                <CvmTerminalAccess
                  key={`${instance.desired.id}:${instance.desired.restart_epoch}:${instance.desired.reinstall_epoch}:${terminal.enabled}`}
                  id={instance.desired.id}
                  name={spec.name}
                  enabled={terminal.enabled}
                  reason={terminal.reason}
                />
              )}
              {canWrite && !deleted && (
                <ActionMenu
                  label="实例更多操作"
                  items={[
                    ...(native
                      ? []
                      : [
                          {
                            label: '使用授权',
                            note: '授权或收回同租户用户的容器访问',
                            disabled: !canEditInstanceAccess(instance, workspace) || command.isPending,
                            onSelect: () => setEditAccess(true),
                          },
                          {
                            label: '修改配置',
                            note: running ? '关机后修改 CPU、内存和名称' : '修改 CPU、内存和名称',
                            disabled: !writable || !ready || running || !caps?.configure,
                            onSelect: () => setDialog({ action: 'configure' }),
                          },
                          {
                            label: '重置登录密码',
                            note: '自动生成新密码，保留系统盘数据',
                            disabled: !writable || !ready || !caps?.stable_storage,
                            onSelect: () => command.mutate({ change: { action: 'reset_password' } }),
                          },
                          {
                            label: '重装系统',
                            note: '清除系统盘数据并重新安装',
                            disabled: !writable || !ready || !caps?.reinstall,
                            onSelect: () => setDialog({ action: 'reinstall' }),
                          },
                          {
                            label: '复制为新容器',
                            note: '用此容器的规格填写创建表单，不复制系统盘数据',
                            disabled: command.isPending,
                            onSelect: () => navigate('cvm', { p: 'new', copy: instance.desired.id }),
                          },
                        ]),
                    {
                      label: native ? '解除纳管' : '删除',
                      note: native ? '撤销受管网络和终端访问，保留宿主系统和磁盘' : '删除实例、系统盘及关联网络资源',
                      danger: true,
                      disabled: !canRecover || !!instance.blocking_deployment_id || (!native && !caps?.delete),
                      onSelect: () => setConfirmDelete(true),
                    },
                  ]}
                />
              )}
              {canWrite && ['failed', 'unknown'].includes(instance.status) && (
                <button
                  className="btn primary"
                  disabled={!canRecover}
                  onClick={() => command.mutate({ change: { action: 'retry' } })}
                >
                  核查并重试原任务
                </button>
              )}
            </TitleActions>
          )
        }
      />
      {confirmDelete && canWrite && (
        <RemoveInstanceDialog
          instance={instance}
          pending={command.isPending}
          error={command.error}
          replay={unconfirmedChange === (native ? 'release_host' : 'delete')}
          onConfirm={generation =>
            command.mutate({
              generation,
              change: native ? { action: 'release_host' } : { action: 'delete', delete_data: true },
            })
          }
          onCancel={() => setConfirmDelete(false)}
        />
      )}
      {editAccess && <AccessDialog instance={instance} workspace={workspace} onClose={() => setEditAccess(false)} />}
      <div className="nd-paper-body cvm-detail" ref={body}>
        {/* 待发布时标题旁已有「待发布」标记，运行状态由图标角上的状态灯表达，这里只说明正在执行的变更。 */}
        {!ready && !staged && <p className="note">正在应用变更 · 版本 {instance.desired.generation}</p>}
        {instance.report?.error && (
          <p className="cvm-notice error" role="alert">
            {workspace.redacted ? '容器返回了执行错误。' : instance.report.error}
          </p>
        )}
        {command.error && <ErrorBox error={command.error} />}
        {host && !host.fresh && (
          <p className="cvm-notice">
            暂未获取到宿主的最新状态。
            {canWrite ? '可以提交清理或核查原任务，恢复连接后会继续处理。' : '请等待宿主恢复连接。'}
          </p>
        )}
        <div
          className="cvm-detail-tab"
          id={`${detailId}-content`}
          role="tabpanel"
          aria-label={
            detailTab === 'overview'
              ? '概览'
              : detailTab === 'network'
                ? '网络'
                : detailTab === 'artifacts'
                  ? '产物'
                  : '监控与诊断'
          }
        >
          {detailTab === 'artifacts' && canReadArtifacts && <CvmArtifacts instance={instance} />}
          {detailTab === 'overview' && (
            <CvmOverview
              instance={instance}
              workspace={workspace}
              host={host}
              observation={observation}
              entries={entries}
              onNetwork={() => setDetailTab('network')}
              onMonitoring={() => setDetailTab('monitoring')}
              onArtifacts={canReadArtifacts ? () => setDetailTab('artifacts') : undefined}
              onResetPassword={
                canWrite && !native ? () => command.mutate({ change: { action: 'reset_password' } }) : undefined
              }
              resetPasswordDisabled={!writable || !ready || !caps?.stable_storage}
            />
          )}
          {detailTab === 'monitoring' && (
            <CvmMonitoringPanel instance={instance} redacted={workspace.redacted} showHostLink={canReadArtifacts} />
          )}
          {detailTab === 'network' && networkEditor}
        </div>
        {dialog && host && canUse && (
          <CommandDialog
            key={dialog.action}
            instance={instance}
            host={host}
            dialog={dialog}
            pending={command.isPending}
            error={command.error}
            onClose={() => setDialog(null)}
            onApply={(change, inputSha256, generation) => command.mutateAsync({ change, inputSha256, generation })}
            onRetry={() => command.mutateAsync(attempt.current!)}
          />
        )}
      </div>
    </>
  );
}

function RemoveInstanceDialog({
  instance,
  pending,
  error,
  replay,
  onConfirm,
  onCancel,
}: {
  instance: CvmInstance;
  pending: boolean;
  error: unknown;
  replay: boolean;
  onConfirm: (generation: number) => void;
  onCancel: () => void;
}) {
  const { generation, spec, power } = instance.desired;
  const [opened] = useState(() => ({ generation, name: spec.name }));
  const [name, setName] = useState('');
  const native = spec.execution.kind === 'baremetal';
  const title = native ? '解除纳管？' : '删除容器？';
  const changed = generation !== opened.generation || power === 'deleted';
  const disabled =
    pending || (!replay && (changed || !!instance.blocking_deployment_id || (!native && name !== opened.name)));
  return (
    <DialogLayer label={title} onClose={onCancel} canClose={() => !pending}>
      <section className="dialog-surface confirm-card cvm-remove-dialog">
        <div className="confirm-title">{title}</div>
        <div className="confirm-body cvm-remove-body">
          <p>
            发布后，对 <strong>{opened.name}</strong> 执行以下操作：
          </p>
          <ul className="cvm-impact-list">
            {native ? (
              <>
                <li>撤下受管访问入口、网卡与路由，关闭受管终端访问。</li>
                <li>保留宿主操作系统、本地磁盘和数据，不对宿主关机。</li>
              </>
            ) : (
              <>
                <li>停止实例并永久销毁系统盘；清盘后数据无法恢复。</li>
                <li>撤销访问入口，卸载网卡并清理关联路由。</li>
                <li>释放该容器在宿主上的资源分配。</li>
              </>
            )}
          </ul>
          <p className="note">这会保存一项待发布变更，发布前可以撤回。</p>
          {!native && (
            <label className="cvm-field">
              输入完整容器名称以确认删除
              <input
                className="f"
                value={name}
                placeholder={opened.name}
                autoComplete="off"
                spellCheck={false}
                disabled={pending || replay}
                onChange={event => setName(event.target.value)}
              />
            </label>
          )}
          {replay ? (
            <p className="cvm-notice" role="status">
              上次保存结果尚未确认，可重试同一请求。
            </p>
          ) : changed || instance.blocking_deployment_id ? (
            <p className="cvm-notice" role="status">
              容器状态已变化或正在发布，请关闭窗口并重新核查。
            </p>
          ) : null}
          {error != null && <ErrorBox error={error} />}
        </div>
        <div className="confirm-actions">
          <DialogClose className="btn" disabled={pending}>
            取消
          </DialogClose>
          <button
            className={`btn${disabled ? '' : ' danger'}`}
            type="button"
            disabled={disabled}
            onClick={() => onConfirm(opened.generation)}
          >
            {pending ? '保存中…' : replay ? '重试原请求' : native ? '保存解除纳管' : '保存删除变更'}
          </button>
        </div>
      </section>
    </DialogLayer>
  );
}
