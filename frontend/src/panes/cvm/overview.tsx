import './overview.css';
import { useState } from 'react';
import { useQuery, type UseQueryResult } from '@tanstack/react-query';
import {
  fetchCvmChainCatalog,
  fetchCvmNicCatalog,
  type CvmEntry,
  type CvmHost,
  type CvmInstance,
  type CvmWorkspace,
} from '../../api';
import { Ago, ErrorBox } from '../../ui/bits';
import { Icon, PanelTitle } from '../../ui/icons';
import { PanelLoading } from '../../ui/loading';
import { RuntimeBand, RuntimeCells } from '../../ui/runtime-band';
import { GuestPassword, InstanceAccess } from './access';
import { EventList, type useCvmObservation } from './monitoring';
import { ContainerAccessPanel } from './networking';
import { entryChip, exitChips, NetChipView } from './network-chips';
import { NIC_CATALOG } from './network-common';
import type { CvmOverviewMetric } from './monitoring-data';
import { CvmOverviewHistory, CvmOverviewMetrics } from './overview-metrics';
import { capacity, hasPendingCvmChange, operationText, statusText, supportsGuestTerminal } from './queries';

/* 概览沿用机器详情的观测页骨架：指标带 → 双列框架卡 → 分带运行状态卡。
   卡片与标题条复用 .chart-card / .load-network-cap / .panel.nd-runtime，不另起一套卡面。 */

const dim = (text = '—') => <span className="dim">{text}</span>;

export function CvmOverview({
  instance,
  workspace,
  host,
  observation,
  entries,
  onNetwork,
  onMonitoring,
  onArtifacts,
  onResetPassword,
  resetPasswordDisabled = true,
}: {
  instance: CvmInstance;
  workspace: CvmWorkspace;
  host: CvmHost | undefined;
  observation: ReturnType<typeof useCvmObservation>;
  entries: UseQueryResult<CvmEntry[]>;
  onNetwork: () => void;
  onMonitoring: () => void;
  onArtifacts?: () => void;
  onResetPassword?: () => void;
  resetPasswordDisabled?: boolean;
}) {
  const [expanded, setExpanded] = useState<CvmOverviewMetric | null>(null);
  const spec = instance.desired.spec;
  const native = spec.execution.kind === 'baremetal';
  const redacted = workspace.redacted;
  const report = instance.report;
  const contact = spec.interfaces.find(nic => nic.purpose === 'contact');
  const observedContact = report?.interfaces.find(nic => nic.id === contact?.id);
  const pending = hasPendingCvmChange(instance);
  const canConnect = workspace.can_use && !redacted && instance.desired.power !== 'deleted';
  const receipt =
    report?.generation === instance.desired.generation && report.operation_id === instance.desired.operation_id
      ? report.artifacts
      : undefined;
  const capabilities = host?.fresh ? host.capabilities : undefined;
  const owner = workspace.owners.find(
    item => item.tenant_id === instance.tenant_id && item.user_id === instance.user_id,
  );
  const business = spec.interfaces.filter(nic => nic.allocated);
  const catalog = useQuery({
    queryKey: NIC_CATALOG,
    queryFn: fetchCvmNicCatalog,
    enabled: !redacted && business.length > 0,
    refetchInterval: 60_000,
  });
  const chains = useQuery({
    queryKey: ['cvm-chain-egress', instance.desired.id],
    queryFn: () => fetchCvmChainCatalog(instance.desired.id),
    enabled: !redacted && spec.routing.routes.some(route => route.chain_id),
    refetchInterval: 15_000,
  });
  const publicationName = (id: string) => catalog.data?.publications.find(item => item.id === id)?.spec.name;
  const chainName = (id: string) => chains.data?.chains.find(item => item.id === id)?.name;
  const { query, health, healthStale, latest, label } = observation;
  const events = query.data?.events;
  const networkLabel = workspace.can_write && !redacted ? '编辑网络' : '查看网络';
  const sampling = (
    <>
      {label}
      {latest && (
        <>
          {' · '}
          <Ago at={new Date(latest.observed_at_unix_millis).toISOString()} />
        </>
      )}
    </>
  );
  const runtimeState =
    query.error || healthStale
      ? '读数已过期'
      : !health
        ? '待确认'
        : health.runtime_state === 'running'
          ? '运行中'
          : health.runtime_state === 'stopped'
            ? '已关机'
            : health.runtime_state === 'missing'
              ? '未发现'
              : '变更中';
  const ssh =
    query.error || healthStale
      ? null
      : health?.ssh_state === 'reachable'
        ? 'ok'
        : health?.ssh_state === 'unreachable'
          ? 'bad'
          : null;
  const volume = native
    ? '宿主原有磁盘'
    : report?.volume_deleted
      ? '已确认释放'
      : report?.volume_retained
        ? '已保留'
        : redacted
          ? '已脱敏'
          : report?.volume_id
            ? '已分配'
            : '等待分配';
  const revision =
    instance.published_revision_id == null ? null : (
      <>
        R{instance.published_revision_id}
        <span className="nd-rt-q">
          <time dateTime={instance.updated_at}>
            {new Date(instance.updated_at).toLocaleString('zh-CN', { dateStyle: 'short', timeStyle: 'short' })}
          </time>
        </span>
      </>
    );
  const capabilityChip = (label: string, supported: boolean | undefined) => (
    <span
      className={`st nd-rt-chip ${supported ? 'st-succeeded' : 'st-skipped'}`}
      aria-label={`${supported ? '支持' : '不支持'}${label}`}
    >
      {label}
    </span>
  );
  return (
    <div className="nd-tab-observed cvm-overview">
      {query.error && <ErrorBox error={query.error} />}
      {query.data?.error && <p className="cvm-notice">{query.data.error}</p>}
      {!!query.data?.issues.length && (
        <ul className="cvm-monitoring-issues" aria-label="需要关注">
          {query.data.issues.map((issue, index) => (
            <li key={`${issue.kind}:${index}`} className={issue.severity === 'bad' ? 'error' : 'warn'}>
              {issue.message}
            </li>
          ))}
        </ul>
      )}
      <CvmOverviewMetrics instance={instance} observation={observation} expanded={expanded} onExpand={setExpanded} />
      <div className="cvm-overview-pair">
        <section className="chart-card cvm-overview-card" aria-label="接入与网络">
          <div className="cvm-card-block">
            <div className="load-network-cap">
              <b>
                <Icon of="access" size={13} className="chart-title-icon" />
                接入与凭证
              </b>
              <span>{native ? '终端与网络作用于宿主' : '优先使用已生效入口'}</span>
            </div>
            <div className="cvm-card-body">
              {canConnect ? (
                <ContainerAccessPanel
                  instance={instance}
                  redacted={false}
                  loginUser={host?.capabilities.host_system?.terminal_user}
                />
              ) : (
                <p className="note cvm-ssh-empty">{redacted ? '连接地址已脱敏。' : '当前不可使用连接凭证。'}</p>
              )}
              {canConnect && !native && (
                <GuestPassword
                  key={`${instance.desired.id}:${spec.login_revision}:${instance.user_id}`}
                  instance={instance}
                  onReset={onResetPassword}
                  resetDisabled={resetPasswordDisabled}
                />
              )}
              {canConnect && native && (
                <div className="cvm-password-row">
                  <Icon of="access" size={13} />
                  <span className="cvm-password-who">
                    裸机登录用户 <b className="mono">{host?.capabilities.host_system?.terminal_user ?? '待确认'}</b> ·
                    宿主认证
                  </span>
                </div>
              )}
            </div>
          </div>
          <div className="cvm-card-block cvm-card-grow">
            <div className="load-network-cap">
              <b>
                <Icon of="dns" size={13} className="chart-title-icon" />
                网络概况
              </b>
              {!redacted && (
                <span>
                  入口 {entries.data?.length ?? '—'} · 网卡 {spec.interfaces.length} · 路由 {spec.routing.routes.length}
                </span>
              )}
              <button className="cvm-cap-link" type="button" onClick={onNetwork}>
                {networkLabel}
                <span aria-hidden="true">›</span>
              </button>
            </div>
            <div className="cvm-card-body">
              {redacted ? (
                <p className="note">网络详情已脱敏。</p>
              ) : (
                <dl className="cvm-net-summary">
                  <div>
                    <dt>入口</dt>
                    <dd>
                      {entries.isPending ? (
                        <PanelLoading label="读取访问入口" />
                      ) : entries.error ? (
                        dim('读取失败')
                      ) : entries.data?.length ? (
                        <>
                          {entries.data.slice(0, 4).map(entry => (
                            <NetChipView key={entry.id} chip={entryChip(entry)} onClick={onNetwork} />
                          ))}
                          {entries.data.length > 4 && (
                            <button className="cvm-chip local" type="button" onClick={onNetwork}>
                              +{entries.data.length - 4}
                            </button>
                          )}
                        </>
                      ) : (
                        dim('未配置')
                      )}
                    </dd>
                  </div>
                  <div>
                    <dt>网卡</dt>
                    <dd>
                      {spec.interfaces.map(nic => (
                        <NetChipView
                          key={nic.id}
                          onClick={onNetwork}
                          chip={{
                            key: nic.id,
                            text: nic.allocated
                              ? (publicationName(nic.allocated.publication_id) ?? '业务网卡')
                              : native
                                ? '宿主网络'
                                : '管理主网卡',
                            tail: report?.interfaces.find(item => item.id === nic.id)?.device,
                            kind: 'via',
                          }}
                        />
                      ))}
                    </dd>
                  </div>
                  <div>
                    <dt>出口</dt>
                    <dd>
                      {exitChips(instance, publicationName, chainName).map(chip => (
                        <NetChipView key={chip.key} chip={chip} onClick={onNetwork} />
                      ))}
                    </dd>
                  </div>
                </dl>
              )}
              {!redacted && entries.error && <ErrorBox error={entries.error} />}
            </div>
          </div>
        </section>
        <section className="chart-card cvm-overview-card" aria-label="流量与事件">
          <div className="cvm-card-block cvm-chart-flush">
            <CvmOverviewHistory observation={observation} metric="network" />
          </div>
          <div className="cvm-card-block cvm-card-grow">
            <div className="load-network-cap">
              <b>
                <Icon of="clock" size={13} className="chart-title-icon" />
                最近事件
              </b>
              <span>近 1 小时</span>
              <button className="cvm-cap-link" type="button" onClick={onMonitoring}>
                监控与诊断
                <span aria-hidden="true">›</span>
              </button>
            </div>
            <div className="cvm-card-body cvm-overview-events">
              {query.isPending ? (
                <PanelLoading label="读取事件" />
              ) : events?.length ? (
                <EventList events={events.slice(0, 4)} native={native} />
              ) : (
                <p className="note">近 1 小时没有事件记录。</p>
              )}
            </div>
          </div>
        </section>
      </div>
      <div className="panel nd-runtime cvm-runtime">
        <header>
          <PanelTitle of="observe">运行状态</PanelTitle>
          <span className="hint">{sampling}</span>
          {onArtifacts && (
            <button className="cvm-cap-link" type="button" onClick={onArtifacts}>
              产物
              <span aria-hidden="true">›</span>
            </button>
          )}
        </header>
        <RuntimeBand name="SPEC" icon="servers" meta={pending ? '待发布' : '已生效'}>
          <RuntimeCells
            items={[
              [
                '规格',
                <>
                  {spec.resources.vcpus} {native ? '核' : 'vCPU'}
                  <span className="nd-rt-q">{capacity(spec.resources.memory_bytes)}</span>
                  <span className="nd-rt-q">{capacity(spec.resources.root_disk_bytes)}</span>
                </>,
                'w2',
              ],
              [
                '系统',
                spec.image ? (
                  <>
                    {spec.image.name} {spec.image.version}
                    <span className="nd-rt-q">{spec.image.architecture}</span>
                  </>
                ) : (
                  <>
                    {host?.capabilities.host_system?.os ?? '现有系统'}
                    <span className="nd-rt-q">裸机</span>
                  </>
                ),
                'w2',
              ],
              [
                '宿主机',
                <>
                  {host?.name ?? instance.node_id}
                  {host?.capabilities.backend && <span className="nd-rt-q">{host.capabilities.backend}</span>}
                </>,
                'w2',
              ],
              [
                '系统盘',
                <>
                  {volume}
                  {!native && (
                    <span className="nd-rt-q">{host?.capabilities.stable_storage ? '原盘恢复' : '不可恢复'}</span>
                  )}
                </>,
                'w2',
              ],
            ]}
          />
        </RuntimeBand>
        <RuntimeBand name="ACCESS" icon="users" meta="实时生效">
          <RuntimeCells
            items={[
              [
                '使用者',
                <span className="cvm-rt-owner">
                  {owner?.name ?? instance.user_id ?? dim('未授权')}
                  <InstanceAccess instance={instance} workspace={workspace} />
                </span>,
                'w2',
              ],
              [
                '主网卡',
                redacted ? (
                  dim('已脱敏')
                ) : (
                  <>
                    {observedContact?.device ?? dim('待确认')}
                    {!!observedContact?.addresses.length && (
                      <span className="nd-rt-q">{observedContact.addresses.join(' / ')}</span>
                    )}
                  </>
                ),
                'w2',
              ],
              [
                '登录凭证',
                redacted ? (
                  dim('已脱敏')
                ) : native ? (
                  <>
                    {host?.capabilities.host_system?.terminal_user ?? 'root'}
                    <span className="nd-rt-q">宿主认证</span>
                  </>
                ) : (
                  <>
                    cube<span className="nd-rt-q">自动密码 v{spec.login_revision}</span>
                  </>
                ),
                'w2',
              ],
              [
                '宿主能力',
                capabilities ? (
                  <span className="nd-rt-chips">
                    {capabilityChip('稳定存储', capabilities.stable_storage)}
                    {capabilityChip(
                      '持久终端',
                      native ? capabilities.host_system?.terminal_ready === true : supportsGuestTerminal(capabilities),
                    )}
                    {capabilityChip('线路出口', capabilities.chain_egress)}
                  </span>
                ) : (
                  dim('待确认')
                ),
                'w2',
              ],
            ]}
          />
        </RuntimeBand>
        <RuntimeBand
          name="CONFIG"
          icon="artifacts"
          meta={instance.status === 'applied' ? (pending ? '待发布' : '已收敛') : statusText[instance.status]}
        >
          <RuntimeCells
            items={[
              ['生效修订', <span title={instance.desired.operation_id}>{revision ?? dim()}</span>, 'w2'],
              [
                '待发布变更',
                pending ? (
                  <span className="hot">{operationText[instance.operation_kind ?? ''] ?? '容器配置'}</span>
                ) : (
                  dim('无')
                ),
                'w2',
              ],
              [
                '运行探测',
                <>
                  {runtimeState}
                  <span className="nd-rt-q">SSH {ssh === 'ok' ? '可连接' : ssh === 'bad' ? '无响应' : '待确认'}</span>
                </>,
                'w2',
              ],
              [
                '产物校验',
                native ? (
                  dim('不适用')
                ) : !receipt ? (
                  dim('尚无当前版本回执')
                ) : receipt.mismatches.length ? (
                  <span className="hot">校验不一致</span>
                ) : (
                  '与运行态一致'
                ),
                'w2',
              ],
            ]}
          />
        </RuntimeBand>
      </div>
    </div>
  );
}
