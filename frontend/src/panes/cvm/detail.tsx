import { useMutation, useQueryClient } from '@tanstack/react-query';
import { lazy, Suspense, useId, useRef, useState } from 'react';
import { ApiError, commandCvm, type CvmChange, type CvmInstance, type CvmWorkspace } from '../../api';
import { ErrorBox, Loading } from '../../ui/bits';
import { Icon } from '../../ui/icons';
import { ActionMenu } from '../../ui/choice-menu';
import { ManagedNetworking, ContainerAccessPanel } from './networking';
import { InstanceEntries } from './entries';
import { CvmMonitoringPanel, CvmRecentEvents } from './monitoring';

import { CommandDialog } from './operations';
import { AppliedState, CvmPageHeader, CvmTabs, Section } from './presentation';
import { TitleActions } from '../../ui/title-actions';
import { capacity, QUERY, requestId, statusText, supportsGuestTerminal } from './queries';

const CvmTerminal = lazy(() => import('./terminal'));

export function CvmTerminalAccess({
  id,
  name,
  enabled,
  reason,
}: {
  id: string;
  name: string;
  enabled: boolean;
  reason: string;
}) {
  const [open, setOpen] = useState(false);
  return (
    <>
      <button
        className="btn title-action-compact"
        aria-label="终端"
        disabled={!enabled}
        onClick={() => setOpen(true)}
        title={reason}
      >
        <Icon of="nodes" size={14} />
        <span className="title-action-label">终端</span>
      </button>
      {open && enabled && (
        <Suspense fallback={<Loading />}>
          <CvmTerminal id={id} name={name} onClose={() => setOpen(false)} />
        </Suspense>
      )}
    </>
  );
}

export function InstanceDetail({ instance, workspace }: { instance: CvmInstance; workspace: CvmWorkspace }) {
  const qc = useQueryClient();
  const [detailTab, setDetailTab] = useState<'overview' | 'network' | 'monitoring'>('overview');
  const detailId = useId();
  const attempt = useRef<{ change: CvmChange; generation: number; id: string } | null>(null);
  const [dialog, setDialog] = useState<null | {
    action: 'start' | 'stop' | 'restart' | 'reinstall' | 'configure' | 'detach';
    interfaceId?: string;
  }>(null);
  const host = workspace.hosts.find(item => item.node_id === instance.node_id);
  const caps = host?.capabilities;
  const spec = instance.desired.spec;
  const deleted = instance.desired.power === 'deleted';
  const ready = instance.status === 'applied';
  const command = useMutation({
    mutationFn: (change: CvmChange) => {
      if (attempt.current && JSON.stringify(attempt.current.change) !== JSON.stringify(change))
        throw new Error('上一项操作的结果尚未确认，请先核查或重试原操作。');
      attempt.current ??= { change, generation: instance.desired.generation, id: requestId() };
      return commandCvm(instance.desired.id, attempt.current.generation, change, attempt.current.id);
    },
    onError: error => {
      if (error instanceof ApiError && [400, 403, 409, 422].includes(error.status)) attempt.current = null;
      void qc.invalidateQueries({ queryKey: QUERY });
    },
    onSuccess: result => {
      attempt.current = null;
      qc.setQueryData<CvmWorkspace>(QUERY, old =>
        old
          ? { ...old, instances: old.instances.map(item => (item.desired.id === result.desired.id ? result : item)) }
          : old,
      );
      void qc.invalidateQueries({ queryKey: QUERY });
      setDialog(null);
    },
  });
  const running = instance.report?.power === 'running';
  const terminalEnabled =
    workspace.can_write &&
    running &&
    !deleted &&
    instance.desired.power === 'running' &&
    supportsGuestTerminal(caps) &&
    !!host?.terminal_enabled;
  const canRecover = workspace.can_write && !command.isPending;
  const writable = canRecover && !!host?.fresh && !deleted;
  return (
    <>
      <CvmPageHeader
        title={spec.name}
        state={<AppliedState instance={instance} />}
        detail
        tabs={
          <CvmTabs
            label="实例详情页签"
            value={detailTab}
            items={[
              { key: 'overview', label: '概览' },
              { key: 'network', label: '网络' },
              { key: 'monitoring', label: '监控与诊断' },
            ]}
            onChange={setDetailTab}
            panelId={`${detailId}-content`}
          />
        }
        tools={
          workspace.can_write && (
            <TitleActions>
              <button
                className="btn title-action-compact"
                disabled={!writable || !ready || !caps?.stable_storage || (running ? !caps.stop : !caps?.start)}
                aria-label={running ? '关机' : '开机'}
                onClick={() => setDialog({ action: running ? 'stop' : 'start' })}
              >
                <Icon of={running ? 'pause' : 'play'} size={14} />
                <span className="title-action-label">{running ? '关机' : '开机'}</span>
              </button>
              <button
                className="btn title-action-compact"
                disabled={!writable || !ready || !running || !caps?.restart}
                aria-label="重启"
                onClick={() => setDialog({ action: 'restart' })}
              >
                <Icon of="refresh" size={14} />
                <span className="title-action-label">重启</span>
              </button>
              <CvmTerminalAccess
                key={`${instance.desired.id}:${instance.desired.restart_epoch}:${instance.desired.reinstall_epoch}:${terminalEnabled}`}
                id={instance.desired.id}
                name={spec.name}
                enabled={terminalEnabled}
                reason={
                  !host?.terminal_enabled
                    ? '该宿主的 WebShell 已关闭，可在宿主页签开启'
                    : !supportsGuestTerminal(caps)
                      ? '宿主运行栈尚不支持终端'
                      : !workspace.can_write
                        ? '当前账号无权连接终端'
                        : !running || instance.desired.power !== 'running'
                          ? '运行中的实例才能连接终端'
                          : '连接实例终端'
                }
              />
              <ActionMenu
                label="实例更多操作"
                items={[
                  {
                    label: '修改配置',
                    note: running ? '关机后修改 CPU、内存和名称' : '修改 CPU、内存和名称',
                    disabled: !writable || !ready || running || !caps?.configure,
                    onSelect: () => setDialog({ action: 'configure' }),
                  },
                  {
                    label: '重装系统',
                    note: '清除系统盘数据并重新安装',
                    disabled: !writable || !ready || !caps?.reinstall,
                    onSelect: () => setDialog({ action: 'reinstall' }),
                  },
                ]}
              />
              <button
                className="btn title-action-compact"
                aria-label="删除"
                title="删除实例、系统盘及关联网络资源"
                disabled={!canRecover || deleted || !caps?.delete}
                onClick={() => command.mutate({ action: 'delete', delete_data: true })}
              >
                <Icon of="trash" size={14} />
                <span className="title-action-label">删除</span>
              </button>
              {['failed', 'unknown', 'restart_required'].includes(instance.status) && (
                <button
                  className="btn primary"
                  disabled={!canRecover}
                  onClick={() => command.mutate({ action: 'retry' })}
                >
                  核查并重试原任务
                </button>
              )}
            </TitleActions>
          )
        }
      />
      <div className="nd-paper-body cvm-detail">
        {!ready && <p className="note">正在应用变更 · 版本 {instance.desired.generation}</p>}
        {instance.report?.error && (
          <p className="cvm-notice error" role="alert">
            {instance.report.error}
          </p>
        )}
        {command.error && <ErrorBox error={command.error} />}
        {host && !host.fresh && (
          <p className="cvm-notice">暂未获取到机器的最新能力。可以提交删除或核查原任务，恢复连接后会继续处理。</p>
        )}
        <div
          className={detailTab === 'overview' ? 'cvm-dashboard' : undefined}
          id={`${detailId}-content`}
          role="tabpanel"
          aria-label={detailTab === 'overview' ? '概览' : detailTab === 'network' ? '网络' : '监控与诊断'}
        >
          {detailTab !== 'network' && (
            <CvmMonitoringPanel
              key={instance.desired.id}
              instance={instance}
              compact={detailTab === 'overview'}
              onOpen={() => setDetailTab('monitoring')}
            />
          )}
          {detailTab === 'network' && <InstanceEntries instance={instance} workspace={workspace} />}
          {detailTab === 'network' && (
            <ManagedNetworking
              instance={instance}
              workspace={workspace}
              writable={writable && ready}
              onDetach={interfaceId => setDialog({ action: 'detach', interfaceId })}
            />
          )}
          {detailTab === 'overview' && (
            <>
              <Section title="连接" className="cvm-widget-5">
                <ContainerAccessPanel instance={instance} redacted={workspace.redacted} />
              </Section>
              <ManagedNetworking
                instance={instance}
                workspace={workspace}
                writable={writable && ready}
                compact
                onOpen={() => setDetailTab('network')}
                onDetach={interfaceId => setDialog({ action: 'detach', interfaceId })}
              />

              <Section title="实例规格" className="cvm-widget-5 cvm-spec-widget">
                <div className="cvm-spec-figures">
                  <span>
                    <b>{spec.resources.vcpus}</b>
                    <small>vCPU</small>
                  </span>
                  <span>
                    <b>{capacity(spec.resources.memory_bytes).split(' ')[0]}</b>
                    <small>{capacity(spec.resources.memory_bytes).split(' ')[1]} 内存</small>
                  </span>
                  <span>
                    <b>{capacity(spec.resources.root_disk_bytes).split(' ')[0]}</b>
                    <small>{capacity(spec.resources.root_disk_bytes).split(' ')[1]} 系统盘</small>
                  </span>
                </div>
                <dl className="cvm-facts cvm-spec-facts">
                  <div>
                    <dt>系统</dt>
                    <dd>
                      {spec.image.name} {spec.image.version} · {spec.image.architecture}
                    </dd>
                  </div>
                  <div>
                    <dt>宿主</dt>
                    <dd>{host?.name ?? instance.node_id}</dd>
                  </div>
                  <div>
                    <dt>用户</dt>
                    <dd>{instance.user_id}</dd>
                  </div>
                </dl>
                <details className="cvm-operation-detail">
                  <summary>存储与执行记录</summary>
                  <dl className="cvm-facts">
                    <div>
                      <dt>系统盘</dt>
                      <dd>
                        {instance.report?.volume_deleted
                          ? '已确认释放'
                          : instance.report?.volume_retained
                            ? '已保留'
                            : instance.report?.volume_id
                              ? '已分配'
                              : '等待分配'}
                      </dd>
                    </div>
                    <div>
                      <dt>原盘恢复</dt>
                      <dd>{caps?.stable_storage ? '已启用' : '未启用'}</dd>
                    </div>
                    <div>
                      <dt>最近更新</dt>
                      <dd>
                        <time dateTime={instance.updated_at}>
                          {new Date(instance.updated_at).toLocaleString('zh-CN')}
                        </time>
                      </dd>
                    </div>
                    <div>
                      <dt>结果</dt>
                      <dd>{statusText[instance.status]}</dd>
                    </div>
                  </dl>
                  <p className="mono">{instance.desired.operation_id}</p>
                  <p className="mono">{instance.desired.spec_sha256}</p>
                  <p className="note">重试沿用原任务身份。未知状态先核查原运行时和系统盘，不自动重建空盘。</p>
                </details>
              </Section>
              <CvmRecentEvents id={instance.desired.id} onOpen={() => setDetailTab('monitoring')} />
            </>
          )}
        </div>
        {dialog && host && (
          <CommandDialog
            key={`${dialog.action}:${dialog.interfaceId ?? ''}`}
            instance={instance}
            host={host}
            dialog={dialog}
            pending={command.isPending}
            error={command.error}
            onClose={() => setDialog(null)}
            onApply={change => command.mutate(change)}
          />
        )}
      </div>
    </>
  );
}
