import { useEffect, useMemo, useRef, useState, type RefObject } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  clearMachineEvents,
  fetchMachineEvents,
  markMachineEventsRead,
  type ActiveMachineIncident,
  type MachineEvent,
  type MachineEventList,
} from '../api';
import { Ago, ErrorBox, Loading } from './bits';
import { Icon } from './icons';
import { usePresence } from './presence';
import { CPU_STEAL_ALERT_PCT } from './telemetry-thresholds';

const INCIDENT_GROUP_WINDOW_MS = 60_000;
const DEFAULT_VISIBLE_NOTIFICATIONS = 10;

export interface MachineIncident {
  kind: 'control_plane_offline' | 'cpu_steal';
  nodeId: string;
  nodeName: string;
  openingEventId: number;
  lastEventId: number;
  startedAt: string;
  detectedAt: string;
  recoveredAt: string | null;
  currentValue: number | null;
  peakValue: number | null;
  threshold: number | null;
}

export interface MachineIncidentGroup {
  kind: MachineIncident['kind'];
  status: 'active' | 'recovered';
  incidents: MachineIncident[];
  lastEventId: number;
  unread: boolean;
}

const parseAt = (value: string) => {
  const parsed = Date.parse(value.endsWith('Z') || value.includes('+') ? value : `${value}Z`);
  return Number.isNaN(parsed) ? 0 : parsed;
};

const localTime = (value: string) => {
  const parsed = parseAt(value);
  return parsed ? new Date(parsed).toLocaleString('zh-CN', { hour12: false }) : value;
};

const durationLabel = (from: string, to: string) => {
  const seconds = Math.max(0, Math.round((parseAt(to) - parseAt(from)) / 1000));
  if (seconds < 60) return `${seconds} 秒`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)} 分 ${seconds % 60} 秒`;
  return `${Math.floor(seconds / 3600)} 小时 ${Math.floor((seconds % 3600) / 60)} 分`;
};

const incidentGroupAnchor = (incident: MachineIncident) =>
  incident.kind === 'cpu_steal' ? incident.startedAt : incident.detectedAt;

function activeIncident(active: ActiveMachineIncident): MachineIncident {
  return {
    kind: active.incident_kind,
    nodeId: active.node_id,
    nodeName: active.node_name,
    openingEventId: active.event_id,
    lastEventId: active.event_id,
    startedAt: active.started_at,
    detectedAt: active.detected_at,
    recoveredAt: null,
    currentValue: active.current_value,
    peakValue: active.peak_value,
    threshold: active.incident_kind === 'cpu_steal' ? CPU_STEAL_ALERT_PCT : null,
  };
}

export function machineIncidents(data: MachineEventList): MachineIncident[] {
  const open = new Map<string, MachineIncident>();
  const recovered: MachineIncident[] = [];
  const events = [...data.events].sort((left, right) => left.id - right.id);
  for (const event of events) {
    if (event.event_kind === 'node_offline') {
      open.set(`control_plane_offline:${event.node_id}`, {
        kind: 'control_plane_offline',
        nodeId: event.node_id,
        nodeName: event.node_name,
        openingEventId: event.id,
        lastEventId: event.id,
        startedAt: event.last_contact_at ?? event.occurred_at,
        detectedAt: event.occurred_at,
        recoveredAt: null,
        currentValue: null,
        peakValue: null,
        threshold: null,
      });
      continue;
    }
    if (event.event_kind === 'node_online') {
      const key = `control_plane_offline:${event.node_id}`;
      const incident = open.get(key);
      if (!incident) continue;
      recovered.push({ ...incident, lastEventId: event.id, recoveredAt: event.occurred_at });
      open.delete(key);
      continue;
    }
    if (event.event_kind === 'cpu_steal_started') {
      open.set(`cpu_steal:${event.node_id}`, {
        kind: 'cpu_steal',
        nodeId: event.node_id,
        nodeName: event.node_name,
        openingEventId: event.id,
        lastEventId: event.id,
        startedAt: event.incident_started_at ?? event.occurred_at,
        detectedAt: event.occurred_at,
        recoveredAt: null,
        currentValue: event.metric_value ?? null,
        peakValue: event.metric_peak_value ?? null,
        threshold: event.metric_threshold ?? null,
      });
      continue;
    }
    if (event.event_kind !== 'cpu_steal_recovered') continue;
    const key = `cpu_steal:${event.node_id}`;
    const incident = open.get(key) ?? {
      kind: 'cpu_steal' as const,
      nodeId: event.node_id,
      nodeName: event.node_name,
      openingEventId: event.id,
      lastEventId: event.id,
      startedAt: event.incident_started_at ?? event.occurred_at,
      detectedAt: event.incident_started_at ?? event.occurred_at,
      recoveredAt: null,
      currentValue: event.metric_value ?? null,
      peakValue: event.metric_peak_value ?? null,
      threshold: event.metric_threshold ?? null,
    };
    recovered.push({
      ...incident,
      lastEventId: event.id,
      recoveredAt: event.occurred_at,
      currentValue: event.metric_value ?? null,
      peakValue: event.metric_peak_value ?? incident.peakValue,
      threshold: event.metric_threshold ?? null,
    });
    open.delete(key);
  }

  // Current state comes from the server-side state projections rather than assuming the bounded
  // event page contains the opening event for a long-running incident.
  const active = data.active.map(item => {
    const key = `${item.incident_kind}:${item.node_id}`;
    const historical = open.get(key);
    const current = activeIncident(item);
    return historical
      ? {
          ...historical,
          currentValue: current.currentValue,
          peakValue: current.peakValue,
        }
      : current;
  });
  // The API can include an older opening event solely to explain a new recovery.
  return [...active, ...recovered].filter(incident => incident.lastEventId > data.cleared_through_event_id);
}

export function groupMachineIncidents(data: MachineEventList): MachineIncidentGroup[] {
  const incidents = machineIncidents(data).sort((left, right) => {
    const statusOrder = Number(left.recoveredAt !== null) - Number(right.recoveredAt !== null);
    return statusOrder || parseAt(right.detectedAt) - parseAt(left.detectedAt);
  });
  const groups: MachineIncidentGroup[] = [];
  for (const incident of incidents) {
    const status = incident.recoveredAt ? 'recovered' : 'active';
    const previous = groups.at(-1);
    const previousAnchor = previous?.incidents[0] ? incidentGroupAnchor(previous.incidents[0]) : null;
    if (
      previous &&
      previous.kind === incident.kind &&
      previous.status === status &&
      previousAnchor &&
      Math.abs(parseAt(previousAnchor) - parseAt(incidentGroupAnchor(incident))) <= INCIDENT_GROUP_WINDOW_MS
    ) {
      previous.incidents.push(incident);
      previous.lastEventId = Math.max(previous.lastEventId, incident.lastEventId);
      previous.unread ||= incident.lastEventId > data.last_seen_event_id;
      continue;
    }
    groups.push({
      kind: incident.kind,
      status,
      incidents: [incident],
      lastEventId: incident.lastEventId,
      unread: incident.lastEventId > data.last_seen_event_id,
    });
  }
  return groups;
}

function IncidentGroup({ group, onNode }: { group: MachineIncidentGroup; onNode: (nodeId: string) => void }) {
  const active = group.status === 'active';
  const start = group.incidents.reduce(
    (value, item) => (parseAt(item.startedAt) < parseAt(value) ? item.startedAt : value),
    group.incidents[0]!.startedAt,
  );
  const recoveredAt = active
    ? null
    : group.incidents.reduce(
        (value, item) => (item.recoveredAt && parseAt(item.recoveredAt) > parseAt(value) ? item.recoveredAt : value),
        group.incidents[0]!.recoveredAt!,
      );
  const count = group.incidents.length;
  const steal = group.kind === 'cpu_steal';
  const title = steal
    ? active
      ? '宿主机 CPU 抢占'
      : 'CPU 抢占已恢复'
    : active
      ? 'Console-Agent 链路已断开'
      : 'Console-Agent 链路已恢复';
  const pct = (value: number | null) => (value === null ? '—' : `${value.toFixed(1)}%`);
  return (
    <section className={`machine-notification-group ${active ? 'active' : 'recovered'}`}>
      <header>
        <span className="machine-notification-state" aria-hidden="true" />
        <span>
          <b>{title}</b>
          <small>{count > 1 ? `${count} 台机器同批发生` : group.incidents[0]!.nodeName}</small>
        </span>
        <span className="sp" />
        {group.unread && <span className="machine-notification-new">未读</span>}
      </header>
      <div className="machine-notification-window" title={`${steal ? '异常开始' : '最后联系'} ${localTime(start)}`}>
        {active && !steal ? (
          <>
            最后联系 <Ago at={start} />
          </>
        ) : active ? (
          <>
            持续 <Ago at={start} />
          </>
        ) : (
          <>
            {localTime(start)}–{localTime(recoveredAt!)} · {durationLabel(start, recoveredAt!)}
          </>
        )}
      </div>
      <div className="machine-notification-nodes">
        {group.incidents.map(incident => (
          <button key={incident.nodeId} type="button" onClick={() => onNode(incident.nodeId)}>
            <span>{incident.nodeName}</span>
            <small>
              {steal
                ? `${active ? '当前' : '恢复值'} ${pct(incident.currentValue)} · 峰值 ${pct(incident.peakValue)}`
                : active
                  ? `确认失联 ${localTime(incident.detectedAt)}`
                  : `恢复 ${localTime(incident.recoveredAt!)}`}
            </small>
          </button>
        ))}
      </div>
    </section>
  );
}

function PublicIpEvent({ event, onNode }: { event: MachineEvent; onNode: (nodeId: string) => void }) {
  return (
    <button className="machine-notification-ip" type="button" onClick={() => onNode(event.node_id)}>
      <span className="machine-notification-state" aria-hidden="true" />
      <span>
        <b>
          {event.node_name} 公网 IPv{event.family} 已变化
        </b>
        <small>
          {event.previous_value} → {event.current_value}
        </small>
      </span>
      <time title={localTime(event.occurred_at)}>
        <Ago at={event.occurred_at} />
      </time>
    </button>
  );
}

export function MachineNotifications({
  narrow,
  open,
  publicView,
  globalClear,
  buttonRef,
  onToggle,
  onNode,
}: {
  narrow: boolean;
  open: boolean;
  publicView: boolean;
  globalClear: boolean;
  buttonRef: RefObject<HTMLButtonElement | null>;
  onToggle: () => void;
  onNode: (nodeId: string) => void;
}) {
  const queryClient = useQueryClient();
  const readSubmittedForOpen = useRef(false);
  const [expanded, setExpanded] = useState(false);
  const presence = usePresence(open, 180);
  // Keep masked public data in a separate cache entry. A logout must never reuse an operator's
  // unmasked notification response while the public request is still in flight.
  const queryKey = ['machine-notifications', publicView ? 'public' : 'operator'] as const;
  const query = useQuery({
    queryKey,
    queryFn: ({ signal }) => fetchMachineEvents(100, signal),
    refetchInterval: 15_000,
    refetchIntervalInBackground: true,
  });
  const read = useMutation({
    mutationFn: markMachineEventsRead,
    onMutate: throughEventId => {
      queryClient.setQueryData<MachineEventList>(queryKey, current =>
        current
          ? {
              ...current,
              last_seen_event_id: Math.max(current.last_seen_event_id, throughEventId),
              unread_count: 0,
            }
          : current,
      );
    },
    onError: () => queryClient.invalidateQueries({ queryKey }),
  });
  const clear = useMutation({
    mutationFn: clearMachineEvents,
    onMutate: () => queryClient.cancelQueries({ queryKey }),
    onSuccess: async ({ cleared_through_event_id: cursor }) => {
      // A periodic GET may have started during the POST. Cancel it before updating the cache so
      // its old snapshot cannot resurrect dismissed notifications after success.
      await queryClient.cancelQueries({ queryKey });
      queryClient.setQueryData<MachineEventList>(queryKey, current =>
        current
          ? {
              ...current,
              cleared_through_event_id: Math.max(current.cleared_through_event_id, cursor),
              last_seen_event_id: Math.max(current.last_seen_event_id, cursor),
              unread_count: current.latest_event_id <= cursor ? 0 : current.unread_count,
              active: current.active.filter(incident => incident.event_id > cursor),
            }
          : current,
      );
      setExpanded(false);
      await queryClient.invalidateQueries({ queryKey });
    },
  });
  const submitReadCursor = read.mutate;
  const data = query.data;
  useEffect(() => {
    if (!open) {
      readSubmittedForOpen.current = false;
      return;
    }
    if (publicView || !data || readSubmittedForOpen.current) return;
    readSubmittedForOpen.current = true;
    if (data.latest_event_id <= data.last_seen_event_id) return;
    submitReadCursor(data.latest_event_id);
  }, [data, open, publicView, submitReadCursor]);

  const groups = useMemo(() => (data ? groupMachineIncidents(data) : []), [data]);
  const activeCount = data?.active.filter(incident => incident.event_id > data.cleared_through_event_id).length ?? 0;
  const unreadCount = data?.unread_count ?? 0;
  // The clear cursor dismisses incidents, not the retained public-IP change log returned by the server.
  const publicIpEvents = (data?.events ?? []).filter(event => event.event_kind === 'public_ip_changed');
  const totalNotifications = groups.length + publicIpEvents.length;
  const visibleGroupCount = expanded ? groups.length : Math.min(groups.length, DEFAULT_VISIBLE_NOTIFICATIONS);
  const visibleGroups = groups.slice(0, visibleGroupCount);
  const visibleIpCount = expanded
    ? publicIpEvents.length
    : Math.min(publicIpEvents.length, DEFAULT_VISIBLE_NOTIFICATIONS - visibleGroupCount);
  const visibleIpEvents = publicIpEvents.slice(0, visibleIpCount);
  const hiddenCount = totalNotifications - visibleGroups.length - visibleIpEvents.length;
  const hasCollapsedNotifications = totalNotifications > DEFAULT_VISIBLE_NOTIFICATIONS;
  const label = query.error
    ? '机器通知读取失败'
    : activeCount
      ? `${activeCount} 条进行中机器事故`
      : unreadCount
        ? `${unreadCount} 条未读机器通知`
        : '机器通知';

  return (
    <div className="fg-menuwrap machine-notification-wrap">
      <button
        ref={buttonRef}
        type="button"
        className={narrow ? 'fg-ico' : 'fg-tgl'}
        aria-expanded={open}
        aria-controls="machine-notification-popover"
        aria-label={label}
        title={label}
        onClick={event => {
          event.stopPropagation();
          if (open) setExpanded(false);
          onToggle();
        }}
      >
        <Icon of="bell" size={narrow ? 13 : 15} className="fg-tgl-ic" />
        {!narrow && <span className="fg-tgl-label">通知</span>}
        {(query.error || activeCount > 0 || unreadCount > 0) && (
          <span className={`${narrow ? 'fg-dot' : 'fg-badge'}${query.error || activeCount ? ' err' : ' chg'}`}>
            {!narrow && (query.error ? '!' : activeCount || Math.min(unreadCount, 99))}
          </span>
        )}
      </button>

      {presence.present && (
        <div
          id="machine-notification-popover"
          className="fg-pop machine-notification-pop"
          data-motion-state={presence.phase}
          aria-hidden={!open || undefined}
          inert={!open}
          onClick={event => event.stopPropagation()}
        >
          <header className="machine-notification-head">
            <span>
              <b>机器通知</b>
              <small>Console-Agent 链路与宿主机告警</small>
            </span>
            <span className="sp" />
            {data && <small>保留 {data.retention_days} 天</small>}
            {!publicView && (
              <button
                className="btn"
                type="button"
                disabled={!data || groups.length === 0 || clear.isPending || !!query.error}
                title={`${globalClear ? '清空所有账号' : '只清空当前账号'}的链路与宿主机告警；公网 IP 变化通知会保留`}
                onClick={() => data && clear.mutate(data.latest_event_id)}
              >
                {clear.isPending ? '清空中…' : '清空告警'}
              </button>
            )}
          </header>
          <div className="machine-notification-body">
            {clear.error && <ErrorBox error={clear.error} />}
            {query.error ? (
              <ErrorBox error={query.error} />
            ) : query.isPending ? (
              <Loading variant="table" />
            ) : groups.length === 0 && publicIpEvents.length === 0 ? (
              <div className="machine-notification-empty" role="status">
                {data && data.cleared_through_event_id > 0 ? '暂无新通知' : '最近没有机器状态事件'}
              </div>
            ) : (
              <>
                {visibleGroups.map(group => (
                  <IncidentGroup
                    key={`${group.kind}-${group.status}-${group.lastEventId}`}
                    group={group}
                    onNode={onNode}
                  />
                ))}
                {visibleIpEvents.length > 0 && (
                  <section className="machine-notification-history">
                    <h3>公网地址变化</h3>
                    {visibleIpEvents.map(event => (
                      <PublicIpEvent key={event.id} event={event} onNode={onNode} />
                    ))}
                  </section>
                )}
                {hasCollapsedNotifications && (
                  <div className="machine-notification-more">
                    <button type="button" aria-expanded={expanded} onClick={() => setExpanded(value => !value)}>
                      {expanded ? '收起' : `查看更多 ${hiddenCount} 条`}
                    </button>
                  </div>
                )}
              </>
            )}
          </div>
        </div>
      )}
    </div>
  );
}
