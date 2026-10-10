import { useId, useState } from 'react';
import { useInfiniteQuery, useQuery } from '@tanstack/react-query';
import {
  fetchBinaryRelease,
  fetchBinaryReleaseHistory,
  fetchBinaryReleaseAttempts,
  fetchBinaryReleaseEvents,
  type BinaryComponent,
  type BinaryReleaseEvent,
  type BinaryReleaseTarget,
} from '../api';
import { Empty, ErrorBox } from '../ui/bits';
import { PanelLoading } from '../ui/loading';
import { BINARY_RELEASE_STATUS, BINARY_TARGET_STATUS, BINARY_VERIFICATION } from '../ui/binary-release-status';
import { binaryReleaseVersion } from '../ui/binary-release-version';
import { useNodeNames } from '../ui/node-name';
import { stamp } from './deploy-cockpit';

const EVENTS: Record<string, string> = {
  created: '创建发布',
  canceled: '停止发布',
  succeeded: '发布完成',
  'target-retried': '重新尝试',
  'target-reported': '收到执行结果',
  'target-reported-after-cancel': '停止后收到执行结果',
  'target-unsupported': '目标不支持',
  'canceled-for-node-lifecycle': '机器生命周期变化，停止发布',
  'legacy-layout-imported': '历史记录迁移',
};

/** History and evidence load only after an explicit disclosure, never in overview polling. */
export function BinaryReleaseHistory({ component, latestId }: { component: BinaryComponent; latestId?: number }) {
  const [expanded, setExpanded] = useState(false);
  const [selected, setSelected] = useState<number | null>(null);
  const id = useId();
  const history = useInfiniteQuery({
    queryKey: ['binary-release-history', component, latestId],
    initialPageParam: undefined as number | undefined,
    queryFn: ({ pageParam }) => fetchBinaryReleaseHistory(component, pageParam),
    getNextPageParam: page => page.next_before_id ?? undefined,
    enabled: expanded,
  });
  return (
    <section className="cgc-main cgc-span binary-history">
      <button
        type="button"
        className="btn"
        aria-expanded={expanded}
        aria-controls={id}
        onClick={() => setExpanded(value => !value)}
      >
        {expanded ? '收起发布历史' : '发布历史'}
      </button>
      {expanded && (
        <div id={id}>
          {history.isPending ? (
            <PanelLoading />
          ) : history.error ? (
            <ErrorBox error={history.error} />
          ) : history.data.pages[0].items.length === 0 ? (
            <Empty>还没有发布记录。</Empty>
          ) : (
            <ol className="binary-history-list">
              {history.data.pages
                .flatMap(page => page.items)
                .map(item => (
                  <li key={item.id}>
                    <button
                      className="binary-history-row"
                      type="button"
                      aria-expanded={selected === item.id}
                      onClick={() => setSelected(current => (current === item.id ? null : item.id))}
                    >
                      <span>
                        <b>#{item.id}</b> · {binaryReleaseVersion(component, item.version)}{' '}
                        <span className="mono">{item.build_id.slice(0, 8)}</span>
                      </span>
                      <span>
                        {BINARY_RELEASE_STATUS[item.status]} · {item.succeeded_count} / {item.target_count} 台
                      </span>
                      <small>
                        {item.created_by} · {stamp(item.created_at)}
                        {item.problem_count ? ` · ${item.problem_count} 台需处理` : ''}
                      </small>
                    </button>
                    {selected === item.id && <ReleaseDetail component={component} id={item.id} />}
                  </li>
                ))}
            </ol>
          )}
          {history.hasNextPage && (
            <button
              className="btn"
              type="button"
              disabled={history.isFetchingNextPage}
              onClick={() => void history.fetchNextPage()}
            >
              {history.isFetchingNextPage ? '加载中…' : '更早的发布'}
            </button>
          )}
        </div>
      )}
    </section>
  );
}

function ReleaseDetail({ component, id }: { component: BinaryComponent; id: number }) {
  const detail = useQuery({
    queryKey: ['binary-release-detail', component, id],
    queryFn: () => fetchBinaryRelease(component, id),
  });
  if (detail.isPending)
    return (
      <div className="binary-history-detail">
        <PanelLoading />
      </div>
    );
  if (detail.error) return <ErrorBox error={detail.error} />;
  const release = detail.data;
  return (
    <div className="binary-history-detail">
      {release.note && <p>{release.note}</p>}
      <p className="hint">发布记录仅保留摘要和执行结果，不保存历史二进制。</p>
      <ul className="binary-history-list">
        {release.targets.map(target => (
          <TargetAttempts key={target.node_id} component={component} id={id} target={target} />
        ))}
      </ul>
      <details className="cg-disclosure">
        <summary>操作事件 · 最近 {release.events.length} 条</summary>
        <EventList events={release.events} />
        {release.next_event_before_id != null && (
          <EarlierEvents component={component} id={id} before={release.next_event_before_id} />
        )}
      </details>
    </div>
  );
}

function TargetAttempts({
  component,
  id,
  target,
}: {
  component: BinaryComponent;
  id: number;
  target: BinaryReleaseTarget;
}) {
  const [expanded, setExpanded] = useState(false);
  const names = useNodeNames();
  const attempts = useQuery({
    queryKey: ['binary-release-detail', component, id, target.node_id, 'attempts'],
    queryFn: () => fetchBinaryReleaseAttempts(component, id, target.node_id),
    enabled: expanded,
  });
  return (
    <li>
      <button
        className="binary-history-row"
        type="button"
        aria-expanded={expanded}
        onClick={() => setExpanded(value => !value)}
      >
        <span>
          {names(target.node_id)} · {BINARY_TARGET_STATUS[target.status].text}
        </span>
        <span className="mono">
          {target.before_sha256.slice(0, 8)} → {target.desired_sha256?.slice(0, 8) ?? '未领取'}
        </span>
        <small>
          第 {target.attempt} 次尝试{target.verification ? ` · ${BINARY_VERIFICATION[target.verification]}` : ''}
        </small>
      </button>
      {target.error && <p className="hint">{target.error}</p>}
      {expanded &&
        (attempts.isPending ? (
          <PanelLoading />
        ) : attempts.error ? (
          <ErrorBox error={attempts.error} />
        ) : (
          <ol className="binary-attempts">
            {attempts.data.map(attempt => (
              <li key={attempt.attempt}>
                <span>
                  第 {attempt.attempt} 次 · {BINARY_TARGET_STATUS[attempt.status].text}
                </span>
                <small>
                  {attempt.verification ? BINARY_VERIFICATION[attempt.verification] : '尚无验证结果'} ·{' '}
                  {attempt.started_at ? stamp(attempt.started_at) : '未领取'}
                  {attempt.finished_at ? ` → ${stamp(attempt.finished_at)}` : ''}
                </small>
                {attempt.error && <p>{attempt.error}</p>}
              </li>
            ))}
          </ol>
        ))}
    </li>
  );
}

function EventList({ events }: { events: BinaryReleaseEvent[] }) {
  return (
    <ol className="binary-attempts">
      {events.map(event => (
        <li key={event.id}>
          <span>
            {EVENTS[event.kind] ?? event.kind}
            {event.node_id ? ` · ${event.node_id}` : ''}
          </span>
          <small>
            {event.actor ?? 'Agent / 系统'} · {stamp(event.created_at)}
          </small>
        </li>
      ))}
    </ol>
  );
}

function EarlierEvents({ component, id, before }: { component: BinaryComponent; id: number; before: number }) {
  const [expanded, setExpanded] = useState(false);
  const query = useQuery({
    queryKey: ['binary-release-detail', component, id, 'events', before],
    queryFn: () => fetchBinaryReleaseEvents(component, id, before),
    enabled: expanded,
  });
  if (!expanded)
    return (
      <button className="btn" type="button" onClick={() => setExpanded(true)}>
        更早的事件
      </button>
    );
  if (query.isPending) return <PanelLoading />;
  if (query.error) return <ErrorBox error={query.error} />;
  return (
    <>
      <EventList events={query.data.items} />
      {query.data.next_before_id != null && (
        <EarlierEvents component={component} id={id} before={query.data.next_before_id} />
      )}
    </>
  );
}
