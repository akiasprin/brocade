import { useQuery } from '@tanstack/react-query';
import { useState } from 'react';
import { fetchCvmNetworkLogs, type CvmNetworkLogEvent } from '../../api';
import { ErrorBox, Loading } from '../../ui/bits';
import { CopyButton } from '../../ui/copy-button';
import { DialogClose, DialogLayer } from '../../ui/dialog';
import { Icon } from '../../ui/icons';

export interface NetworkLogMachine {
  node_id: string;
  name?: string;
  role?: string;
}
const sourceName = { agent: 'Agent 执行', xray: 'Xray 进程', phantun: 'Phantun 进程' } as const;
const levelName = { info: '信息', warning: '警告', error: '错误' } as const;
const phaseName = { preparing: '配置中', waiting: '等待连接', connected: '已确认', failed: '执行失败' } as const;
const time = (at: number) => new Date(at * 1000).toLocaleString('zh-CN', { hour12: false });

export function NetworkLogsButton({
  machines,
  resources = [],
  title = '网络配置日志',
  label = '日志',
  allowed,
  iconOnly = false,
}: {
  machines: NetworkLogMachine[];
  resources?: string[];
  title?: string;
  label?: string;
  allowed: boolean;
  /** 卡片标题里只放图标，悬停给出完整说明，避免每张卡都重复一个「日志」字样。 */
  iconOnly?: boolean;
}) {
  const [open, setOpen] = useState(false);
  if (!allowed || !machines.length) return null;
  return (
    <>
      <button
        type="button"
        className={`cvm-text-link cvm-log-button${iconOnly ? ' icon' : ''}`}
        aria-label={`查看${title}`}
        title={iconOnly ? `查看${title}` : undefined}
        onPointerDown={event => event.stopPropagation()}
        onClick={event => {
          event.stopPropagation();
          setOpen(true);
        }}
      >
        <Icon of="artifacts" size={iconOnly ? 13 : 12} />
        {!iconOnly && label}
      </button>
      {open && (
        <NetworkLogsDialog machines={machines} resources={resources} title={title} onClose={() => setOpen(false)} />
      )}
    </>
  );
}
export function NetworkLogsDialog({
  machines,
  resources = [],
  title,
  onClose,
}: {
  machines: NetworkLogMachine[];
  resources?: string[];
  title: string;
  onClose: () => void;
}) {
  const unique = machines.filter((machine, index) => machines.findIndex(m => m.node_id === machine.node_id) === index);
  const [selected, setSelected] = useState(unique[0]?.node_id ?? '');
  const [source, setSource] = useState<'all' | CvmNetworkLogEvent['source']>('all');
  const [history, setHistory] = useState(!resources.length);
  const query = useQuery({
    queryKey: ['cvm-network-logs', selected],
    queryFn: () => fetchCvmNetworkLogs([selected]),
    enabled: !!selected,
    refetchInterval: 5000,
    retry: false,
  });
  const node = query.data?.nodes.find(n => n.node_id === selected);
  const diagnostics = node?.diagnostics;
  const inScope = (id: string | null) => history || id === null || resources.includes(id);
  const events =
    diagnostics?.events.filter(e => inScope(e.resource_id) && (source === 'all' || e.source === source)).reverse() ??
    [];
  const progress = diagnostics?.resources.filter(p => history || resources.includes(p.id)).slice(-24) ?? [];
  const copy = events
    .map(
      e => `${time(e.at)} [${sourceName[e.source]}] [${levelName[e.level]}] [${e.resource_id ?? '机器'}] ${e.message}`,
    )
    .join('\n');
  return (
    <DialogLayer label={title} onClose={onClose}>
      <section
        className="dialog-surface tunnel-dialog cvm-dialog cvm-network-log-dialog"
        onClick={event => event.stopPropagation()}
        onPointerDown={event => event.stopPropagation()}
      >
        <header>
          <b>{title}</b>
          <DialogClose className="btn icon" aria-label="关闭日志">
            <Icon of="close" size={14} />
          </DialogClose>
        </header>
        <div className="tunnel-dialog-body cvm-dialog-body cvm-network-log-body">
          <div className="cvm-log-machines" aria-label="日志机器">
            {unique.map(machine => (
              <button
                className={`btn small ${selected === machine.node_id ? 'on' : ''}`}
                key={machine.node_id}
                aria-pressed={selected === machine.node_id}
                onClick={() => setSelected(machine.node_id)}
              >
                {machine.role && <small>{machine.role} · </small>}
                {machine.name ?? (machine.node_id === node?.node_id ? node.name : machine.node_id)}
              </button>
            ))}
          </div>
          <div className="cvm-log-toolbar">
            <small className="note">最新记录在前</small>
            <label>
              来源
              <select aria-label="日志来源" value={source} onChange={e => setSource(e.target.value as typeof source)}>
                <option value="all">全部</option>
                {Object.entries(sourceName).map(([key, value]) => (
                  <option key={key} value={key}>
                    {value}
                  </option>
                ))}
              </select>
            </label>
            {!!resources.length && (
              <label className="cvm-log-history">
                <input type="checkbox" checked={history} onChange={e => setHistory(e.target.checked)} />
                包含本机其他网络记录
              </label>
            )}
            <button className="btn small" disabled={query.isFetching} onClick={() => void query.refetch()}>
              {query.isFetching ? '正在刷新…' : '刷新'}
            </button>
            <CopyButton
              text={node && diagnostics ? `${node.name} · ${time(diagnostics.collected_at)}\n${copy}` : ''}
              label="复制日志"
              disabled={!events.length}
            />
          </div>
          {query.error && <ErrorBox error={query.error} />}
          {query.isPending && <Loading />}
          {node && (
            <>
              <p className="note cvm-log-freshness">
                {diagnostics ? (
                  <>
                    最近回报 {time(diagnostics.collected_at)}
                    {!node.fresh && <strong> · 回报已过期，当前显示历史日志</strong>}
                  </>
                ) : (
                  '这台机器尚未回报网络日志，请确认 Agent 已升级并在线。'
                )}
                <span>页面每 5 秒刷新，节点约每 15 秒回报。</span>
              </p>
              <div className="cvm-log-domains">
                {node.domains.map(domain => (
                  <span key={domain.domain}>
                    {domain.domain === 'entry' ? '入站' : '出站'} · 期望 v{domain.desired_generation}／回报{' '}
                    {domain.reported_generation === null ? '尚无' : `v${domain.reported_generation}`}
                    {domain.error && <strong>{domain.error}</strong>}
                  </span>
                ))}
              </div>
              {progress.length > 0 && (
                <ul className="cvm-log-progress" aria-label="网络配置进度">
                  {progress.map(p => (
                    <li key={p.id}>
                      <span
                        className={`cz-state ${p.phase === 'failed' ? 'failed' : p.phase === 'connected' ? 'active' : 'pending'}`}
                      >
                        {phaseName[p.phase]}
                      </span>
                      <span>{p.message}</span>
                      <small className="mono" title={p.id}>
                        {p.id} · 本轮开始 {time(p.since)}
                      </small>
                      {p.waiting_seconds > 0 && <small>已等待 {p.waiting_seconds} 秒</small>}
                    </li>
                  ))}
                </ul>
              )}
              {diagnostics && (
                <ol className="cvm-network-log-events" aria-label="网络执行日志">
                  {events.map((event, index) => (
                    <li className={event.level} key={`${event.at}-${index}`}>
                      <div>
                        <time>{time(event.at)}</time>
                        <span>{sourceName[event.source]}</span>
                        <span className="mono" title={event.resource_id ?? undefined}>
                          {event.resource_id ?? '机器'}
                        </span>
                        <b>{levelName[event.level]}</b>
                      </div>
                      <pre>{event.message}</pre>
                    </li>
                  ))}
                  {events.length === 0 && (
                    <li className="note">当前筛选下没有日志。可切换来源或查看本机其他网络记录。</li>
                  )}
                </ol>
              )}
              {!!diagnostics?.dropped_events && (
                <p className="note">
                  当前保留最近 {diagnostics.events.length} 条记录，已有 {diagnostics.dropped_events} 条较早记录被轮换。
                </p>
              )}
            </>
          )}
        </div>
        <footer>
          <DialogClose className="btn">关闭</DialogClose>
        </footer>
      </section>
    </DialogLayer>
  );
}
