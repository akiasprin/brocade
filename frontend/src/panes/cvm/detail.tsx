import { navigate } from '../../forge/route';
import { useMutation, useQueryClient } from '@tanstack/react-query';
import { useId, useRef, useState } from 'react';
import { ApiError, commandCvm, type CvmChange, type CvmInstance, type CvmWorkspace } from '../../api';
import { ErrorBox } from '../../ui/bits';
import { Icon } from '../../ui/icons';
import { ActionMenu } from '../../ui/choice-menu';
import { ManagedNetworking, ContainerAccessPanel } from './networking';
import { InstanceFlow } from './flow';
import { CvmArtifacts } from './artifacts';
import { CvmMonitoringPanel, CvmRecentEvents } from './monitoring';

import { CommandDialog } from './operations';
import { AppliedState, CvmPageHeader, CvmTabs, instanceLamp, Section } from './presentation';
import { TitleActions } from '../../ui/title-actions';
import { CvmTerminalAccess } from './terminal-access';
import { capacity, QUERY, requestId, statusText, guestTerminalAccess } from './queries';

export function InstanceDetail({ instance, workspace }: { instance: CvmInstance; workspace: CvmWorkspace }) {
  const qc = useQueryClient();
  const [detailTab, setDetailTab] = useState<'overview' | 'network' | 'monitoring' | 'artifacts'>('overview');
  const detailId = useId();
  const attempt = useRef<{ change: CvmChange; generation: number; id: string } | null>(null);
  const [dialog, setDialog] = useState<null | {
    action: 'start' | 'stop' | 'restart' | 'reinstall' | 'configure' | 'detach';
    interfaceId?: string;
  }>(null);
  const host = workspace.hosts.find(item => item.node_id === instance.node_id);
  const caps = host?.capabilities;
  const spec = instance.desired.spec;
  const native = spec.execution?.kind === 'baremetal';
  const deleted = instance.desired.power === 'deleted';
  const ready = instance.status === 'applied';
  const reviewedInput = useRef<string | undefined>(undefined);
  const command = useMutation({
    mutationFn: (change: CvmChange) => {
      if (attempt.current && JSON.stringify(attempt.current.change) !== JSON.stringify(change))
        throw new Error('上一项操作的结果尚未确认，请先核查或重试原操作。');
      attempt.current ??= { change, generation: instance.desired.generation, id: requestId() };
      return commandCvm(
        instance.desired.id,
        attempt.current.generation,
        change,
        attempt.current.id,
        reviewedInput.current,
      );
    },
    onError: error => {
      if (error instanceof ApiError && [400, 403, 409, 422].includes(error.status)) attempt.current = null;
      void qc.invalidateQueries({ queryKey: QUERY });
    },
    onSuccess: result => {
      attempt.current = null;
      reviewedInput.current = undefined;
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
  const terminal = guestTerminalAccess(instance, host, workspace.can_write);
  const canRecover = workspace.can_write && !command.isPending;
  const writable = canRecover && !!host?.fresh && !deleted;
  return (
    <>
      <CvmPageHeader
        title={spec.name}
        lamp={instanceLamp(instance)}
        // 运行与停止由图标角上的状态灯表达；处理中、失败等需要注意的状态仍用文字标出。
        state={instance.status !== 'applied' ? <AppliedState instance={instance} /> : undefined}
        meta={`${host?.name ?? instance.node_id} · ${
          spec.image ? `${spec.image.name} ${spec.image.version}` : (caps?.baremetal?.os ?? '裸机')
        } · ${instance.user_id}`}
        detail
        tabs={
          <CvmTabs
            label="实例详情页签"
            value={detailTab}
            items={[
              { key: 'overview', label: '概览' },
              { key: 'network', label: '网络' },
              { key: 'monitoring', label: '监控与诊断' },
              { key: 'artifacts', label: '产物' },
            ]}
            onChange={setDetailTab}
            panelId={`${detailId}-content`}
          />
        }
        tools={
          workspace.can_write && (
            <TitleActions>
              {!native && (
                <>
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
                </>
              )}
              <CvmTerminalAccess
                key={`${instance.desired.id}:${instance.desired.restart_epoch}:${instance.desired.reinstall_epoch}:${terminal.enabled}`}
                id={instance.desired.id}
                name={spec.name}
                enabled={terminal.enabled}
                reason={terminal.reason}
              />
              {!native && (
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
              )}
              <button
                className="btn title-action-compact"
                aria-label={native ? '解除纳管' : '删除'}
                title={native ? '撤销受管网络和终端访问，保留宿主系统和磁盘' : '删除实例、系统盘及关联网络资源'}
                disabled={!canRecover || deleted || (!native && !caps?.delete)}
                onClick={() =>
                  command.mutate(native ? { action: 'release_host' } : { action: 'delete', delete_data: true })
                }
              >
                <Icon of="trash" size={14} />
                <span className="title-action-label">{native ? '解除纳管' : '删除'}</span>
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
      {instance.revision_id != null && (instance.published_revision_id ?? 0) < instance.revision_id && (
        <div className="cgr-notice cvm-release-notice" role="status">
          <span>容器配置已保存至全局 R{instance.revision_id}，等待发布。</span>
          <button className="btn" disabled={!workspace.can_publish} onClick={() => navigate('deploy', { p: 'plan' })}>
            查看混合发布计划
          </button>
        </div>
      )}

      <div className="nd-paper-body cvm-detail">
        {native && (
          <p className="cvm-notice">
            裸机实例 · 整机独占，无容器隔离。终端和网络操作作用于宿主，解除纳管保留系统与磁盘。
          </p>
        )}
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
          className={`cvm-detail-tab${detailTab === 'overview' ? ' cvm-dashboard' : ''}`}
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
          {detailTab === 'artifacts' &&
            (workspace.redacted ? (
              <p className="note">当前只读角色不能读取产物内容。</p>
            ) : (
              <CvmArtifacts instance={instance} />
            ))}
          {detailTab === 'overview' && <InstanceFlow instance={instance} workspace={workspace} />}
          {(detailTab === 'overview' || detailTab === 'monitoring') && (
            <CvmMonitoringPanel
              key={instance.desired.id}
              instance={instance}
              compact={detailTab === 'overview'}
              onOpen={() => setDetailTab('monitoring')}
            />
          )}
          {detailTab === 'network' && (
            <ManagedNetworking
              instance={instance}
              workspace={workspace}
              writable={writable && ready}
              routesWritable={writable && (ready || instance.status === 'failed' || instance.status === 'unknown')}
              onDetach={interfaceId => setDialog({ action: 'detach', interfaceId })}
            />
          )}
          {detailTab === 'overview' && (
            <div className="cvm-overview-row">
              <Section title="连接" className="cvm-access">
                <ContainerAccessPanel
                  instance={instance}
                  redacted={workspace.redacted}
                  loginUser={caps?.baremetal?.terminal_user}
                />
              </Section>
              <Section title="规格" className="cvm-spec-widget">
                <div className="cvm-spec-figures">
                  <span>
                    <b>{spec.resources.vcpus}</b>
                    <small>{native ? 'CPU 核数' : 'vCPU'}</small>
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
                      {spec.image
                        ? `${spec.image.name} ${spec.image.version} · ${spec.image.architecture}`
                        : `${caps?.baremetal?.os ?? '现有系统'} · 裸机`}
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
                        {native
                          ? '宿主原有磁盘，不由实例分配或删除'
                          : instance.report?.volume_deleted
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
                      <dd>{native ? '使用宿主原有系统' : caps?.stable_storage ? '已启用' : '未启用'}</dd>
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
              <CvmRecentEvents native={native} id={instance.desired.id} onOpen={() => setDetailTab('monitoring')} />
            </div>
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
            onApply={(change, inputSha256) => {
              reviewedInput.current = inputSha256;
              command.mutate(change);
            }}
          />
        )}
      </div>
    </>
  );
}
