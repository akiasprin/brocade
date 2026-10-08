import { useRef, useState } from 'react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import type { MachineEventList } from '../src/api';
import { groupMachineIncidents, MachineNotifications } from '../src/ui/machine-notifications';

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const recoveredFleetEvent: MachineEventList = {
  retention_days: 90,
  latest_event_id: 6,
  last_seen_event_id: 0,
  cleared_through_event_id: 0,
  unread_count: 6,
  active: [],
  events: [
    {
      id: 6,
      node_id: 'mo-1',
      node_name: '澳门 AKILE',
      event_kind: 'node_online',
      family: null,
      previous_value: 'offline',
      current_value: 'online',
      last_contact_at: null,
      occurred_at: '2026-10-06T16:00:36+08:00',
    },
    {
      id: 5,
      node_id: 'hk-1',
      node_name: '香港 AKILE',
      event_kind: 'node_online',
      family: null,
      previous_value: 'offline',
      current_value: 'online',
      last_contact_at: null,
      occurred_at: '2026-10-06T16:00:34+08:00',
    },
    {
      id: 4,
      node_id: 'mo-2',
      node_name: '澳门 AKILE II',
      event_kind: 'node_online',
      family: null,
      previous_value: 'offline',
      current_value: 'online',
      last_contact_at: null,
      occurred_at: '2026-10-06T16:00:33+08:00',
    },
    {
      id: 3,
      node_id: 'mo-1',
      node_name: '澳门 AKILE',
      event_kind: 'node_offline',
      family: null,
      previous_value: 'online',
      current_value: 'offline',
      last_contact_at: '2026-10-06T15:57:50+08:00',
      occurred_at: '2026-10-06T15:59:21+08:00',
    },
    {
      id: 2,
      node_id: 'mo-2',
      node_name: '澳门 AKILE II',
      event_kind: 'node_offline',
      family: null,
      previous_value: 'online',
      current_value: 'offline',
      last_contact_at: '2026-10-06T15:57:35+08:00',
      occurred_at: '2026-10-06T15:59:06+08:00',
    },
    {
      id: 1,
      node_id: 'hk-1',
      node_name: '香港 AKILE',
      event_kind: 'node_offline',
      family: null,
      previous_value: 'online',
      current_value: 'offline',
      last_contact_at: '2026-10-06T15:57:34+08:00',
      occurred_at: '2026-10-06T15:59:06+08:00',
    },
  ],
};

const activeStealEvent: MachineEventList = {
  retention_days: 90,
  latest_event_id: 8,
  last_seen_event_id: 7,
  cleared_through_event_id: 0,
  unread_count: 1,
  active: [
    {
      event_id: 8,
      node_id: 'mo-1',
      node_name: '澳门 AKILE',
      incident_kind: 'cpu_steal',
      started_at: '2026-10-06T17:15:00+08:00',
      detected_at: '2026-10-06T17:16:02+08:00',
      last_observed_at: '2026-10-06T17:20:00+08:00',
      current_value: 59.4,
      peak_value: 63.1,
    },
  ],
  events: [
    {
      id: 8,
      node_id: 'mo-1',
      node_name: '澳门 AKILE',
      event_kind: 'cpu_steal_started',
      family: null,
      previous_value: 'normal',
      current_value: 'active',
      last_contact_at: null,
      incident_started_at: '2026-10-06T17:15:00+08:00',
      metric_value: 59.4,
      metric_peak_value: 63.1,
      metric_threshold: 10,
      occurred_at: '2026-10-06T17:16:02+08:00',
    },
  ],
};

const activeControlObservationIncident: MachineEventList = {
  retention_days: 90,
  latest_event_id: 7,
  last_seen_event_id: 6,
  cleared_through_event_id: 0,
  unread_count: 1,
  active: [
    {
      event_id: 7,
      node_id: 'mo-1',
      node_name: '澳门 AKILE',
      incident_kind: 'control_plane_offline',
      started_at: '2026-10-06T18:18:22+08:00',
      detected_at: '2026-10-06T18:20:05+08:00',
      last_observed_at: null,
      current_value: null,
      peak_value: null,
    },
  ],
  events: [
    {
      id: 7,
      node_id: 'mo-1',
      node_name: '澳门 AKILE',
      event_kind: 'node_offline',
      family: null,
      previous_value: 'online',
      current_value: 'offline',
      last_contact_at: '2026-10-06T18:18:22+08:00',
      occurred_at: '2026-10-06T18:20:05+08:00',
    },
  ],
};

function Harness({
  onNode,
  publicView = false,
  globalClear = false,
}: {
  onNode: (nodeId: string) => void;
  publicView?: boolean;
  globalClear?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const buttonRef = useRef<HTMLButtonElement>(null);
  return (
    <MachineNotifications
      narrow={false}
      open={open}
      publicView={publicView}
      globalClear={globalClear}
      buttonRef={buttonRef}
      onToggle={() => setOpen(value => !value)}
      onNode={onNode}
    />
  );
}

it('把同一时间窗内的多节点恢复聚合为一次 Console-Agent 链路事故', () => {
  const groups = groupMachineIncidents(recoveredFleetEvent);
  expect(groups).toHaveLength(1);
  expect(groups[0]?.status).toBe('recovered');
  expect(groups[0]?.incidents.map(item => item.nodeName)).toEqual(['澳门 AKILE', '澳门 AKILE II', '香港 AKILE']);
  expect(groups[0]?.unread).toBe(true);
});

it('使用 Console-Agent 链路文案区分进行中事故', () => {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  client.setQueryData(['machine-notifications', 'operator'], activeControlObservationIncident);
  vi.stubGlobal('fetch', vi.fn());
  render(
    <QueryClientProvider client={client}>
      <Harness onNode={vi.fn()} />
    </QueryClientProvider>,
  );

  fireEvent.click(screen.getByRole('button', { name: '1 条进行中机器事故' }));
  expect(screen.getByText('Console-Agent 链路已断开')).toBeTruthy();
});

it('把持续 CPU steal 保持为一条进行中事故并显示当前值与峰值', () => {
  const groups = groupMachineIncidents(activeStealEvent);
  expect(groups).toHaveLength(1);
  expect(groups[0]?.kind).toBe('cpu_steal');
  expect(groups[0]?.status).toBe('active');
  expect(groups[0]?.incidents[0]).toMatchObject({ currentValue: 59.4, peakValue: 63.1 });

  const client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  client.setQueryData(['machine-notifications', 'operator'], activeStealEvent);
  vi.stubGlobal('fetch', vi.fn());
  render(
    <QueryClientProvider client={client}>
      <Harness onNode={vi.fn()} />
    </QueryClientProvider>,
  );

  fireEvent.click(screen.getByRole('button', { name: '1 条进行中机器事故' }));
  expect(screen.getByText('宿主机 CPU 抢占')).toBeTruthy();
  expect(screen.getByText('当前 59.4% · 峰值 63.1%')).toBeTruthy();
});

it('按异常开始时间合并一分钟内采样窗口错开的多机 CPU steal 事故', () => {
  const staggered: MachineEventList = {
    ...activeStealEvent,
    latest_event_id: 9,
    unread_count: 2,
    active: [
      activeStealEvent.active[0]!,
      {
        event_id: 9,
        node_id: 'mo-2',
        node_name: '澳门 AKILE II',
        incident_kind: 'cpu_steal',
        started_at: '2026-10-06T17:15:59+08:00',
        detected_at: '2026-10-06T17:17:03+08:00',
        last_observed_at: '2026-10-06T17:20:21+08:00',
        current_value: 24.4,
        peak_value: 24.4,
      },
    ],
    events: [],
  };

  const groups = groupMachineIncidents(staggered);
  expect(groups).toHaveLength(1);
  expect(groups[0]?.incidents.map(item => item.nodeName)).toEqual(['澳门 AKILE II', '澳门 AKILE']);
});

it('在全局通知气泡显示事故，打开后持久标记已读并可进入机器', async () => {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity }, mutations: { retry: false } },
  });
  client.setQueryData(['machine-notifications', 'operator'], recoveredFleetEvent);
  const fetcher = vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) => {
    if (init?.method === 'POST') {
      return new Response(JSON.stringify({ last_seen_event_id: 6 }), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      });
    }
    return new Response(JSON.stringify(recoveredFleetEvent), {
      status: 200,
      headers: { 'content-type': 'application/json' },
    });
  });
  vi.stubGlobal('fetch', fetcher);
  const onNode = vi.fn();

  render(
    <QueryClientProvider client={client}>
      <Harness onNode={onNode} globalClear />
    </QueryClientProvider>,
  );

  fireEvent.click(screen.getByRole('button', { name: '6 条未读机器通知' }));
  expect(screen.getByText('Console-Agent 链路与宿主机告警')).toBeTruthy();
  expect(screen.getByRole('button', { name: '清空告警' }).getAttribute('title')).toBe(
    '清空所有账号的链路与宿主机告警；公网 IP 变化通知会保留',
  );
  expect(screen.getByText('Console-Agent 链路已恢复')).toBeTruthy();
  expect(screen.getByText('3 台机器同批发生')).toBeTruthy();
  const incidentWindow = screen.getByText(/3 分 2 秒/);
  expect(incidentWindow.textContent).toContain('2026/10/6');
  expect(incidentWindow.textContent).not.toMatch(/AM|PM/);
  await waitFor(() =>
    expect(fetcher).toHaveBeenCalledWith(
      '/notifications/read',
      expect.objectContaining({ method: 'POST', body: JSON.stringify({ through_event_id: 6 }) }),
    ),
  );

  client.setQueryData<MachineEventList>(['machine-notifications', 'operator'], current => ({
    ...(current ?? recoveredFleetEvent),
    latest_event_id: 7,
    unread_count: 1,
  }));
  await waitFor(() => expect(fetcher.mock.calls.filter(([, init]) => init?.method === 'POST')).toHaveLength(1));

  fireEvent.click(screen.getByRole('button', { name: /香港 AKILE/ }));
  expect(onNode).toHaveBeenCalledWith('hk-1');
});

it('默认只显示十条通知，点查看更多后展示其余内容', () => {
  const manyIpEvents: MachineEventList = {
    ...recoveredFleetEvent,
    latest_event_id: 18,
    last_seen_event_id: 18,
    unread_count: 0,
    events: Array.from({ length: 12 }, (_, index) => ({
      id: 18 - index,
      node_id: `node-${index}`,
      node_name: `机器 ${index}`,
      event_kind: 'public_ip_changed' as const,
      family: 4 as const,
      previous_value: `198.51.100.${index}`,
      current_value: `203.0.113.${index}`,
      last_contact_at: null,
      occurred_at: `2026-10-06T16:00:${String(59 - index).padStart(2, '0')}+08:00`,
    })),
  };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  client.setQueryData(['machine-notifications', 'operator'], manyIpEvents);

  render(
    <QueryClientProvider client={client}>
      <Harness onNode={vi.fn()} />
    </QueryClientProvider>,
  );

  fireEvent.click(screen.getByRole('button', { name: '机器通知' }));
  expect(screen.getAllByRole('button', { name: /机器 \d/ })).toHaveLength(10);
  fireEvent.click(screen.getByRole('button', { name: '查看更多 2 条' }));
  expect(screen.getAllByRole('button', { name: /机器 \d/ })).toHaveLength(12);
  expect(screen.getByRole('button', { name: '收起' })).toBeTruthy();
});

it('访客读取独立的脱敏缓存且不会提交已读游标', async () => {
  const publicEvents: MachineEventList = {
    ...recoveredFleetEvent,
    latest_event_id: 8,
    last_seen_event_id: 8,
    unread_count: 0,
    events: [
      {
        id: 8,
        node_id: 'hk-1',
        node_name: '香港 AKILE',
        event_kind: 'public_ip_changed',
        family: 4,
        previous_value: '198.51.***.***',
        current_value: '203.0.***.***',
        last_contact_at: null,
        occurred_at: '2026-10-06T16:00:40+08:00',
      },
    ],
  };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  client.setQueryData(['machine-notifications', 'public'], publicEvents);
  const fetcher = vi.fn();
  vi.stubGlobal('fetch', fetcher);

  render(
    <QueryClientProvider client={client}>
      <Harness publicView onNode={vi.fn()} />
    </QueryClientProvider>,
  );

  fireEvent.click(screen.getByRole('button', { name: '机器通知' }));
  expect(screen.getByText('198.51.***.*** → 203.0.***.***')).toBeTruthy();
  expect(screen.queryByRole('button', { name: '清空告警' })).toBeNull();
  await new Promise(resolve => setTimeout(resolve, 0));
  expect(fetcher).not.toHaveBeenCalled();
});

it('清空机器告警后仍保留已有的公网 IP 变化通知', async () => {
  const publicIpEvent: MachineEventList['events'][number] = {
    id: 7,
    node_id: 'hk-1',
    node_name: '香港 AKILE',
    event_kind: 'public_ip_changed',
    family: 4,
    previous_value: '198.51.100.20',
    current_value: '203.0.113.42',
    last_contact_at: null,
    occurred_at: '2026-10-06T16:01:00+08:00',
  };
  const original: MachineEventList = {
    ...recoveredFleetEvent,
    latest_event_id: 7,
    last_seen_event_id: 7,
    unread_count: 0,
    events: [publicIpEvent, ...recoveredFleetEvent.events],
  };
  const cleared: MachineEventList = {
    ...original,
    cleared_through_event_id: 7,
    events: [publicIpEvent],
  };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  client.setQueryData(['machine-notifications', 'operator'], original);
  const response = (body: unknown) =>
    new Response(JSON.stringify(body), { headers: { 'content-type': 'application/json' } });
  const fetcher = vi.fn(async (input: RequestInfo | URL) =>
    input === '/notifications/clear' ? response({ cleared_through_event_id: 7 }) : response(cleared),
  );
  vi.stubGlobal('fetch', fetcher);

  render(
    <QueryClientProvider client={client}>
      <Harness onNode={vi.fn()} />
    </QueryClientProvider>,
  );

  fireEvent.click(screen.getByRole('button', { name: '机器通知' }));
  expect(screen.getByRole('button', { name: '清空告警' }).getAttribute('title')).toBe(
    '只清空当前账号的链路与宿主机告警；公网 IP 变化通知会保留',
  );
  expect(screen.getByText('Console-Agent 链路已恢复')).toBeTruthy();
  expect(screen.getByText('香港 AKILE 公网 IPv4 已变化')).toBeTruthy();
  fireEvent.click(screen.getByRole('button', { name: '清空告警' }));

  await waitFor(() => expect(screen.queryByText('Console-Agent 链路已恢复')).toBeNull());
  expect(screen.getByText('香港 AKILE 公网 IPv4 已变化')).toBeTruthy();
  expect(screen.getByText('198.51.100.20 → 203.0.113.42')).toBeTruthy();
  expect((screen.getByRole('button', { name: '清空告警' }) as HTMLButtonElement).disabled).toBe(true);
});

it('一键清空后刷新仍为空，不删除新到达的通知，也不受旧请求迟到影响', async () => {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  const original = { ...recoveredFleetEvent, last_seen_event_id: 6, unread_count: 0 };
  client.setQueryData(['machine-notifications', 'operator'], original);
  const cleared: MachineEventList = { ...original, cleared_through_event_id: 6, events: [] };
  let server = cleared;
  let finishOldRead: (response: Response) => void = () => {};
  let firstRead = true;
  const response = (body: unknown) =>
    new Response(JSON.stringify(body), { headers: { 'content-type': 'application/json' } });
  const fetcher = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    if (input === '/notifications/clear') return response({ cleared_through_event_id: 6 });
    if (init?.method === 'POST') return response({ last_seen_event_id: 6 });
    if (firstRead) {
      firstRead = false;
      return new Promise<Response>(resolve => {
        finishOldRead = resolve;
      });
    }
    return response(server);
  });
  vi.stubGlobal('fetch', fetcher);
  render(
    <QueryClientProvider client={client}>
      <Harness onNode={vi.fn()} />
    </QueryClientProvider>,
  );
  fireEvent.click(screen.getByRole('button', { name: '机器通知' }));
  act(() => {
    void client.invalidateQueries({ queryKey: ['machine-notifications', 'operator'] });
  });
  await waitFor(() => expect(fetcher).toHaveBeenCalled());
  fireEvent.click(screen.getByRole('button', { name: '清空告警' }));
  await screen.findByText('暂无新通知');
  expect(fetcher).toHaveBeenCalledWith(
    '/notifications/clear',
    expect.objectContaining({
      method: 'POST',
      body: JSON.stringify({ through_event_id: 6 }),
    }),
  );
  await act(async () => {
    finishOldRead(response(original));
  });
  expect(screen.queryByText('Console-Agent 链路已恢复')).toBeNull();
  expect((screen.getByRole('button', { name: '清空告警' }) as HTMLButtonElement).disabled).toBe(true);
  server = {
    ...cleared,
    latest_event_id: 7,
    unread_count: 1,
    events: [
      {
        id: 7,
        node_id: 'new',
        node_name: '新机器',
        event_kind: 'public_ip_changed',
        family: 4,
        previous_value: '198.51.100.1',
        current_value: '203.0.113.1',
        last_contact_at: null,
        occurred_at: '2026-10-08T16:00:00+08:00',
      },
    ],
  };
  await act(async () => {
    await client.invalidateQueries({ queryKey: ['machine-notifications', 'operator'] });
  });
  expect(await screen.findByText('新机器 公网 IPv4 已变化')).toBeTruthy();
  expect(screen.queryByText('Console-Agent 链路已恢复')).toBeNull();
});

it('清空失败保留通知，禁止重复提交并显示可重试错误', async () => {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  client.setQueryData(['machine-notifications', 'operator'], { ...recoveredFleetEvent, last_seen_event_id: 6 });
  let finish: (response: Response) => void = () => {};
  const fetcher = vi.fn(
    () =>
      new Promise<Response>(resolve => {
        finish = resolve;
      }),
  );
  vi.stubGlobal('fetch', fetcher);
  render(
    <QueryClientProvider client={client}>
      <Harness onNode={vi.fn()} />
    </QueryClientProvider>,
  );
  fireEvent.click(screen.getByRole('button', { name: '6 条未读机器通知' }));
  fireEvent.click(screen.getByRole('button', { name: '清空告警' }));
  const pending = await screen.findByRole('button', { name: '清空中…' });
  expect((pending as HTMLButtonElement).disabled).toBe(true);
  fireEvent.click(pending);
  expect(fetcher).toHaveBeenCalledTimes(1);
  await act(async () => {
    finish(new Response(JSON.stringify({ error: '清空失败，请重试' }), { status: 500 }));
  });
  await screen.findByText('清空失败，请重试');
  expect(screen.getByText('Console-Agent 链路已恢复')).toBeTruthy();
  expect((screen.getByRole('button', { name: '清空告警' }) as HTMLButtonElement).disabled).toBe(false);
});

it('清空进行中事故后不再显示，后续恢复仍展示原来的起止时间', () => {
  expect(groupMachineIncidents({ ...activeControlObservationIncident, cleared_through_event_id: 7 })).toEqual([]);
  const recovery: MachineEventList = {
    ...activeControlObservationIncident,
    cleared_through_event_id: 7,
    last_seen_event_id: 7,
    latest_event_id: 8,
    active: [],
    events: [
      {
        id: 8,
        node_id: 'mo-1',
        node_name: '澳门 AKILE',
        event_kind: 'node_online',
        family: null,
        previous_value: 'offline',
        current_value: 'online',
        last_contact_at: null,
        occurred_at: '2026-10-06T18:25:00+08:00',
      },
      ...activeControlObservationIncident.events,
    ],
  };
  expect(groupMachineIncidents(recovery)).toMatchObject([
    {
      status: 'recovered',
      unread: true,
      lastEventId: 8,
      incidents: [{ startedAt: '2026-10-06T18:18:22+08:00', recoveredAt: '2026-10-06T18:25:00+08:00' }],
    },
  ]);
});

it('进行中的事故始终使用红色活动角标，即使事件已经读过', () => {
  const active: MachineEventList = {
    ...recoveredFleetEvent,
    latest_event_id: 7,
    last_seen_event_id: 7,
    unread_count: 0,
    active: [
      {
        event_id: 7,
        node_id: 'hk-1',
        node_name: '香港 AKILE',
        incident_kind: 'control_plane_offline',
        started_at: '2026-10-06T16:20:00+08:00',
        detected_at: '2026-10-06T16:21:31+08:00',
        last_observed_at: null,
        current_value: null,
        peak_value: null,
      },
    ],
    events: [],
  };
  const groups = groupMachineIncidents(active);
  expect(groups).toHaveLength(1);
  expect(groups[0]?.status).toBe('active');
  expect(groups[0]?.unread).toBe(false);
});
