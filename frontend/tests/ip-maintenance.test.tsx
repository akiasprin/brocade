import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { IpMaintenanceCard } from '../src/panes/ip-maintenance';
import type { IpMaintenancePolicy, IpMaintenanceReport, IpMaintenanceView, NodeAgentStateItem } from '../src/api';
import {
  maintenanceRegions,
  maintenanceView,
  maintenanceResetReport,
  maintenancePartialReport,
} from './fixtures/ip-maintenance-data';

let view: IpMaintenanceView;
let client: QueryClient;
let writes: { expected_generation: number; policy: IpMaintenancePolicy }[];
let commands: { kind: string; body: Record<string, unknown> }[];
let history: IpMaintenanceReport[];
let historyPages: IpMaintenanceReport[][] | null;
let historyFailure: boolean;
let saveGate: Promise<void> | null;
let resetGate: Promise<void> | null;
let viewGate: Promise<void> | null;
let resetAcknowledged: boolean;
let reads: number;
let missingRoundReads: number;
const node = () =>
  ({
    node_id: 'n1',
    name: '东京 · 服务出口',
    public_ipv4: '192.0.2.25',
    public_ipv4_country: 'JP',
    token_last_used_at: new Date().toISOString(),
    last_poll_at: new Date().toISOString(),
    agent_protocol_version: 70,
  }) as NodeAgentStateItem;

beforeEach(() => {
  view = maintenanceView();
  writes = [];
  commands = [];
  history = [{ ...structuredClone(view.report!), phase: 'complete', round_id: 'past-round' }];
  historyPages = null;
  historyFailure = false;
  saveGate = null;
  resetGate = null;
  viewGate = null;
  resetAcknowledged = true;
  reads = 0;
  missingRoundReads = 0;
  client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity }, mutations: { retry: false } },
  });
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = new URL(String(input), 'http://localhost');
      const path = url.pathname;
      if (path === '/ip-maintenance/regions') return Response.json(maintenanceRegions);
      if (path === '/nodes/n1/ip-maintenance') {
        if (init?.method === 'PUT') {
          const body = JSON.parse(String(init.body)) as (typeof writes)[number];
          writes.push(body);
          if (body.expected_generation !== view.desired.generation)
            return Response.json({ error: '养护设置已被更新，请刷新后重试' }, { status: 409 });
          if (saveGate) await saveGate;
          view.desired = { ...view.desired, generation: view.desired.generation + 1, policy: body.policy };
          return new Response(null, { status: 204 });
        }
        if (viewGate) await viewGate;
        return Response.json(structuredClone(view));
      }
      if (path === '/nodes/n1/ip-maintenance/runs') {
        reads++;
        if (historyFailure) return Response.json({ error: '记录读取失败' }, { status: 503 });
        if (historyPages) {
          const page = Number(url.searchParams.get('cursor') ?? 0);
          return Response.json({
            runs: historyPages[page],
            next_cursor: page < historyPages.length - 1 ? page + 1 : null,
          });
        }
        return Response.json({ runs: history, next_cursor: null });
      }
      if (path.startsWith('/nodes/n1/ip-maintenance/runs/')) {
        if (!history.length) {
          missingRoundReads++;
          return Response.json({ error: '养护轮次不存在' }, { status: 404 });
        }
        return Response.json(history[0]);
      }
      if (/\/ip-maintenance\/(run|stop|session-reset)$/.test(path)) {
        const kind = path.split('/').at(-1)!;
        const body = JSON.parse(String(init?.body));
        commands.push({ kind, body });
        view.desired.control_sequence++;
        if (kind === 'session-reset') {
          if (resetGate) await resetGate;
          view.desired.generation++;
          view.desired.session_generation++;
          view.desired.policy.enabled = false;
          view.report = resetAcknowledged ? maintenanceResetReport(view.desired) : null;
          history = [];
        }
        if (kind === 'stop') {
          expect(init?.signal).toBeInstanceOf(AbortSignal);
          view.desired.stop_sequence = view.desired.control_sequence;
          view.desired.target_round_id = body.round_id;
        }
        return new Response(null, { status: 204 });
      }
      return Response.json({ error: 'unexpected test request' }, { status: 500 });
    }),
  );
});
afterEach(() => {
  cleanup();
  client.clear();
  vi.unstubAllGlobals();
});
function mount(machine = node(), readOnly = false) {
  return render(
    <QueryClientProvider client={client}>
      <IpMaintenanceCard node={machine} canEdit={!readOnly} />
    </QueryClientProvider>,
  );
}
async function ready() {
  await waitFor(() => expect(document.querySelector('.care-config')?.getAttribute('aria-busy')).toBe('false'));
}
function automatic(enabled: boolean) {
  return within(screen.getByRole('group', { name: '自动养护' })).getByRole('button', {
    name: enabled ? '开启' : '关闭',
  }) as HTMLButtonElement;
}
async function records() {
  await ready();
  fireEvent.click(screen.getByRole('button', { name: '记录' }));
  const sheet = await screen.findByRole('dialog', { name: '养护记录' });
  await waitFor(() => expect(within(sheet).queryAllByText('正在读取记录…')).toHaveLength(0));
  return sheet;
}
async function policy() {
  await ready();
  fireEvent.click(screen.getByRole('button', { name: '策略' }));
  return screen.findByRole('dialog', { name: '养护策略' });
}

describe('machine maintenance production page', () => {
  it('lets a read-only visitor inspect status, policy, behavior and history without writes', async () => {
    mount(node(), true);
    await ready();
    expect(automatic(true).disabled).toBe(true);
    expect(((await screen.findByRole('button', { name: '停止' })) as HTMLButtonElement).disabled).toBe(true);
    const sheet = await policy();
    const limit = within(sheet).getByRole('spinbutton', { name: '每日请求上限' });
    expect(limit.matches(':disabled')).toBe(true);
    fireEvent.change(limit, { target: { value: '1600' } });
    expect(within(sheet).queryByText('有未保存的改动')).toBeNull();
    fireEvent.click(within(sheet).getByRole('button', { name: /行为参数/ }));
    const behavior = await screen.findByRole('dialog', { name: '行为参数' });
    expect(within(behavior).getByRole('spinbutton', { name: 'Google 动作下限' }).matches(':disabled')).toBe(true);
    expect((within(behavior).getByRole('button', { name: '恢复原版' }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(within(behavior).getByRole('button', { name: '完成' }));
    await waitFor(() => expect(screen.queryByRole('dialog', { name: '行为参数' })).toBeNull());
    fireEvent.click(within(sheet.querySelector('header')!).getByRole('button', { name: '关闭' }));
    fireEvent.click(screen.getByRole('button', { name: '记录' }));
    await screen.findByRole('dialog', { name: '养护记录' });
    expect(writes).toEqual([]);
    expect(commands).toEqual([]);
  });

  it('does not save or start a first maintenance round in a read-only view', async () => {
    view.desired.generation = 0;
    view.desired.policy.enabled = false;
    view.report = null;
    mount(node(), true);
    await ready();
    for (const name of ['开启', '立即执行']) {
      const button = screen.getByRole('button', { name });
      expect((button as HTMLButtonElement).disabled).toBe(true);
      fireEvent.click(button);
    }
    expect(writes).toEqual([]);
    expect(commands).toEqual([]);
  });

  it('keeps location scores out of the card and retains the flags in records', async () => {
    mount();
    await ready();
    expect(document.querySelector('.care-location-progress')).toBeNull();
    expect(document.querySelector('.care-config')?.textContent).not.toContain('Google 定位');
    expect(document.querySelector('.care-config > header')?.textContent).not.toMatch(/策略|记录/);
    expect(reads).toBe(0);
    const sheet = await records();
    await waitFor(() => expect(sheet.querySelectorAll('.care-round-geo .geo-flag').length).toBeGreaterThan(0));
  });

  it('saves dual-stack intent and separate source and expected exit addresses', async () => {
    mount();
    const sheet = await policy();
    fireEvent.click(within(sheet).getByRole('button', { name: 'IPv4 + IPv6' }));
    fireEvent.change(within(sheet).getByRole('textbox', { name: 'IPv4 本机来源地址' }), {
      target: { value: '192.0.2.25' },
    });
    fireEvent.change(within(sheet).getByRole('textbox', { name: 'IPv6 本机来源地址' }), {
      target: { value: '2001:db8::25' },
    });
    fireEvent.change(within(sheet).getByRole('textbox', { name: 'IPv6 预期公网出口' }), {
      target: { value: '2001:db8::25' },
    });
    fireEvent.click(within(sheet).getByRole('button', { name: '保存' }));
    await waitFor(() => expect(writes[0]?.policy.family).toBe('dual'));
    expect(writes[0].policy.source_ip).toBe('192.0.2.25');
    expect(writes[0].policy.source_ipv6).toBe('2001:db8::25');
    expect(writes[0].policy.expected_public_ipv6).toBe('2001:db8::25');
  });
  it('keeps the dual-stack option disabled on a legacy Agent', async () => {
    view.dual_stack_supported = false;
    mount();
    const sheet = await policy();
    const choice = within(sheet).getByRole('button', { name: 'IPv4 + IPv6' }) as HTMLButtonElement;
    expect(choice.disabled).toBe(true);
    fireEvent.click(choice);
    expect(writes).toEqual([]);
  });
  it('shows one overall status and stops the active family without changing the saved policy', async () => {
    view.desired.policy.family = 'dual';
    const a = {
      ...structuredClone(view.report!),
      family: 'v4' as const,
      phase: 'complete' as const,
      round_id: 'v4-round',
    };
    const b = {
      ...structuredClone(view.report!),
      family: 'v6' as const,
      phase: 'dwelling' as const,
      round_id: 'v6-round',
      actual_exit_ip: '2001:db8::25',
      evidence: view.report!.evidence.map(e => ({ ...e, country: 'US' })),
    };
    view.family_reports = [a, b];
    view.report = b;
    mount();
    await ready();
    expect(screen.queryByRole('group', { name: '查看养护出口' })).toBeNull();
    expect(screen.queryByRole('group', { name: 'IPv4 养护状态' })).toBeNull();
    expect(screen.queryByRole('group', { name: 'IPv6 养护状态' })).toBeNull();
    expect(screen.getAllByRole('status')).toHaveLength(1);
    expect(screen.getByRole('status').textContent).toBe('正在养护');
    fireEvent.click(screen.getByRole('button', { name: '停止' }));
    await waitFor(() => expect(commands[0]?.body.round_id).toBe('v6-round'));
    expect(writes).toEqual([]);
    await waitFor(() => expect(screen.getByRole('status').textContent).toBe('正在停止'));
    const sheet = await policy();
    expect((within(sheet).getByRole('button', { name: '重置养护会话' }) as HTMLButtonElement).disabled).toBe(true);
  });

  it('serializes dual-stack stops until the targeted family acknowledges the stop', async () => {
    view.desired.policy.family = 'dual';
    view.family_reports = ['v4', 'v6'].map(family => ({
      ...structuredClone(view.report!),
      family: family as 'v4' | 'v6',
      round_id: `${family}-round`,
    }));
    mount();
    await ready();
    fireEvent.click(screen.getByRole('button', { name: '停止' }));
    await waitFor(() => expect(commands[0]?.body.round_id).toBe('v4-round'));
    await waitFor(() => expect(screen.getByRole('status').textContent).toBe('正在停止'));
    expect(commands).toHaveLength(1);
    expect((screen.getByRole('button', { name: '正在停止' }) as HTMLButtonElement).disabled).toBe(true);
    view.family_reports![0].phase = 'interrupted';
    view.family_reports![0].applied_stop_sequence = view.desired.stop_sequence;
    await waitFor(() => expect(commands[1]?.body.round_id).toBe('v6-round'), { timeout: 2500 });
    expect(writes).toEqual([]);
  });
  it('keeps the overall status unconfirmed when one family has stale receipts', async () => {
    view.desired.policy.family = 'dual';
    view.family_reports = ['v4', 'v6'].map(family => ({
      ...structuredClone(view.report!),
      family: family as 'v4' | 'v6',
      round_id: `${family}-round`,
    }));
    view.family_reports[1].worker_observed_at_unix_secs -= 120;
    mount();
    await ready();
    expect(screen.getByRole('status').textContent).toBe('执行器无回执');
    expect((screen.getByRole('button', { name: '停止' }) as HTMLButtonElement).disabled).toBe(false);
  });
  it.each([
    ['idle', '空闲'],
    ['partial', '部分成功'],
    ['failed', '执行失败'],
    ['running', '正在养护'],
  ])('aggregates dual-stack %s without separate family status rows', async (scene, status) => {
    view.desired.policy.family = 'dual';
    view.desired.policy.enabled = false;
    view.family_reports = ['v4', 'v6'].map(family => ({
      ...structuredClone(view.report!),
      family: family as 'v4' | 'v6',
      round_id: `${family}-round`,
      phase: 'complete',
    }));
    if (scene !== 'idle') {
      view.family_reports[0].phase = 'failed';
      view.family_reports[0].actions = [];
      view.family_reports[0].reason = 'network_timeout';
    }
    if (scene === 'failed') {
      view.family_reports[1].phase = 'failed';
      view.family_reports[1].actions = [];
    }
    if (scene === 'running') view.family_reports[1].phase = 'dwelling';
    mount();
    await ready();
    expect(screen.getAllByRole('status')).toHaveLength(1);
    expect(screen.getByRole('status').textContent).toBe(status);
  });
  it('does not stop an automatic round that starts while the other family stops', async () => {
    view.desired.policy.family = 'dual';
    view.family_reports = ['v4', 'v6'].map(family => ({
      ...structuredClone(view.report!),
      family: family as 'v4' | 'v6',
      round_id: `${family}-round`,
    }));
    mount();
    await ready();
    fireEvent.click(screen.getByRole('button', { name: '停止' }));
    await waitFor(() => expect(commands).toHaveLength(1));
    view.family_reports[0].phase = 'interrupted';
    view.family_reports[0].applied_stop_sequence = view.desired.stop_sequence;
    view.family_reports[1].round_id = 'later-automatic-round';
    await waitFor(
      () => expect((screen.getByRole('button', { name: '停止' }) as HTMLButtonElement).disabled).toBe(false),
      { timeout: 2500 },
    );
    expect(commands).toHaveLength(1);
  });
  it('does not stop a different policy after waiting for the first dual-stack stop', async () => {
    view.desired.policy.family = 'dual';
    view.family_reports = ['v4', 'v6'].map(family => ({
      ...structuredClone(view.report!),
      family: family as 'v4' | 'v6',
      round_id: `${family}-round`,
    }));
    mount();
    await ready();
    fireEvent.click(screen.getByRole('button', { name: '停止' }));
    await waitFor(() => expect(commands).toHaveLength(1));
    view.desired.generation++;
    await screen.findByText('养护策略或会话已变化，请重新确认状态', {}, { timeout: 2500 });
    expect(commands).toHaveLength(1);
  });

  it('preserves modal stacking, draft values and focus when closing behavior parameters', async () => {
    mount();
    const sheet = await policy();
    expect(sheet.classList.contains('modal')).toBe(true);
    fireEvent.click(within(sheet).getByRole('button', { name: /行为参数/ }));
    const behavior = await screen.findByRole('dialog', { name: '行为参数' });
    fireEvent.change(within(behavior).getByRole('spinbutton', { name: 'Google 动作下限' }), { target: { value: '6' } });
    fireEvent.click(within(behavior).getByRole('button', { name: '完成' }));
    await waitFor(() => expect(screen.queryByRole('dialog', { name: '行为参数' })).toBeNull());
    expect(screen.getByRole('dialog', { name: '养护策略' })).toBe(sheet);
    expect(within(sheet).getByText('有未保存的改动')).toBeTruthy();
    fireEvent.click(within(sheet).getByRole('button', { name: '保存' }));
    await waitFor(() => expect(writes[0]?.policy.behavior.google_actions.min).toBe(6));
  });

  it('keeps save acceptance separate from Agent confirmation', async () => {
    mount();
    const sheet = await policy();
    fireEvent.change(within(sheet).getByRole('spinbutton', { name: '每日请求上限' }), { target: { value: '1600' } });
    fireEvent.click(within(sheet).getByRole('button', { name: '保存' }));
    await within(sheet).findByText('等待节点确认');
    expect(writes[0].expected_generation).toBe(12);
    view.report!.generation = view.desired.generation;
    await act(() => client.invalidateQueries({ queryKey: ['node-ip-maintenance', 'n1'] }));
    await within(sheet).findByText('已应用');
  });

  it('sends a stale draft generation after a background refresh instead of overwriting concurrent changes', async () => {
    mount();
    const sheet = await policy();
    fireEvent.change(within(sheet).getByRole('spinbutton', { name: '每日请求上限' }), { target: { value: '1600' } });
    view.desired.generation++;
    view.desired.policy.interval_minutes = 30;
    await act(() => client.invalidateQueries({ queryKey: ['node-ip-maintenance', 'n1'] }));
    fireEvent.click(within(sheet).getByRole('button', { name: '保存' }));
    await within(sheet).findByText(/养护设置已被更新/);
    expect(writes[0].expected_generation).toBe(12);
    expect(view.desired.policy.daily_requests).toBe(1500);
    expect((within(sheet).getByRole('spinbutton', { name: '每日请求上限' }) as HTMLInputElement).value).toBe('1600');
  });

  it('retains changes typed while a save is in flight and saves them against the accepted generation', async () => {
    let finish!: () => void;
    saveGate = new Promise<void>(resolve => {
      finish = resolve;
    });
    mount();
    const sheet = await policy();
    const input = within(sheet).getByRole('spinbutton', { name: '每日请求上限' });
    fireEvent.change(input, { target: { value: '1600' } });
    fireEvent.click(within(sheet).getByRole('button', { name: '保存' }));
    await waitFor(() => expect(writes).toHaveLength(1));
    fireEvent.change(input, { target: { value: '1700' } });
    await act(async () => {
      finish();
      await saveGate;
    });
    saveGate = null;
    const save = await within(sheet).findByRole('button', { name: '保存' });
    expect((input as HTMLInputElement).value).toBe('1700');
    fireEvent.click(save);
    await waitFor(() => expect(writes).toHaveLength(2));
    expect(writes[1].expected_generation).toBe(13);
    expect(writes[1].policy.daily_requests).toBe(1700);
  });

  it('saves the automatic toggle immediately and rolls it back after a rejected request', async () => {
    view.desired.policy.enabled = false;
    mount();
    await ready();
    await waitFor(() => expect(automatic(true).getAttribute('aria-pressed')).toBe('false'));
    view.desired.generation++;
    fireEvent.click(automatic(true));
    await screen.findByText(/养护设置已被更新/);
    await waitFor(() => expect(automatic(true).getAttribute('aria-pressed')).toBe('false'));
    expect(view.desired.policy.enabled).toBe(false);
  });

  it.each([
    { supported: false, active: true, message: '需要升级 Agent' },
    { supported: true, active: false, message: '机器已停用' },
  ])('explains first-use availability: $message', async ({ supported, active, message }) => {
    view.desired.generation = 0;
    view.desired.policy.enabled = false;
    view.report = null;
    view.supported = supported;
    view.lifecycle_active = active;
    history = [];
    mount();
    await screen.findByText(message);
    expect((screen.getByRole('button', { name: '立即执行' }) as HTMLButtonElement).disabled).toBe(true);
    if (!supported) await screen.findByText('协议 68 起支持 IP 养护');
    expect(writes).toHaveLength(0);
    expect(commands).toHaveLength(0);
  });

  it('persists untouched defaults before a first manual round and keeps automatic maintenance disabled', async () => {
    view.desired.generation = 0;
    view.desired.policy.enabled = false;
    view.report = null;
    history = [];
    mount();
    const run = await screen.findByRole('button', { name: '立即执行' });
    await waitFor(() => expect((run as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(run);
    await waitFor(() => expect(commands).toHaveLength(1));
    expect(writes[0].expected_generation).toBe(0);
    expect(writes[0].policy.enabled).toBe(false);
    expect(writes[0].policy.interval_minutes).toBe(45);
    expect(commands[0].body.expected_generation).toBe(1);
    expect(commands[0].body.request_id).toEqual(expect.any(String));
  });

  it('saves the machine-derived region before its first manual round', async () => {
    view.desired.generation = 0;
    view.desired.policy.enabled = false;
    view.report = null;
    history = [];
    mount({ ...node(), name: '洛杉矶 DMIT', public_ipv4_country: 'US' });
    const run = await screen.findByRole('button', { name: '立即执行' });
    await waitFor(() => expect((run as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(run);
    await waitFor(() => expect(commands).toHaveLength(1));
    expect(writes[0].policy.region).toBe('US/CA/Los_Angeles');
    expect(writes[0].policy.enabled).toBe(false);
  });

  it('requires an explicit region when the machine flag has no template', async () => {
    view.desired.generation = 0;
    view.desired.policy.enabled = false;
    view.report = null;
    history = [];
    mount({ ...node(), public_ipv4_country: 'CN' });
    await screen.findByText('请选择养护地区');
    expect((screen.getByRole('button', { name: '立即执行' }) as HTMLButtonElement).disabled).toBe(true);
    expect(writes).toHaveLength(0);
  });

  it('keeps an explicitly saved target when the machine flag changes', async () => {
    mount({ ...node(), public_ipv4_country: 'US' });
    await ready();
    expect(view.desired.policy.region).toBe('JP/Default/Tokyo');
  });

  it('keeps unreported usage and location unknown instead of fabricating zeroes', async () => {
    view.report = null;
    mount();
    await records();
    expect([...document.querySelectorAll('.care-records-usage b')].map(e => e.textContent)).toEqual(['—', '—']);
    expect(screen.queryByText('0/3')).toBeNull();
  });

  it.each(['offline', 'inactive', 'stale'] as const)(
    'keeps independent stop available while %s state remains unconfirmed',
    async scene => {
      const machine = node();
      if (scene !== 'stale') machine.last_poll_at = '2020-01-01T00:00:00Z';
      if (scene === 'inactive') view.lifecycle_active = false;
      if (scene === 'stale') view.report!.worker_observed_at_unix_secs -= 120;
      mount(machine);
      const stop = await screen.findByRole('button', { name: '停止' });
      expect((stop as HTMLButtonElement).disabled).toBe(false);
      fireEvent.click(stop);
      await waitFor(() => expect(commands[0]?.kind).toBe('stop'));
      expect(commands[0].body.round_id).toBe('fixture-round-19');
      await waitFor(() =>
        expect((screen.getByRole('button', { name: '正在停止' }) as HTMLButtonElement).disabled).toBe(true),
      );
    },
  );

  it('shows upgrade state and disables new execution for an unsupported Agent', async () => {
    view.supported = false;
    view.report = null;
    mount();
    await screen.findByText('需要升级 Agent');
    expect((screen.getByRole('button', { name: '立即执行' }) as HTMLButtonElement).disabled).toBe(true);
    expect((automatic(true) as HTMLInputElement).disabled).toBe(true);
  });

  it('bounds the day chart to the latest 100 retained rounds on an older Console', async () => {
    historyPages = Array.from({ length: 7 }, (_, page) =>
      Array.from({ length: 20 }, (_, i) => ({
        ...structuredClone(view.report!),
        round_id: `history-${page}-${i}`,
        phase: 'complete' as const,
      })),
    );
    mount();
    await ready();
    expect(reads).toBe(0);
    await records();
    await waitFor(() => expect(document.querySelector('.care-record-count')?.textContent).toBe('100'));
    expect(reads).toBe(6);
  });

  it('does not count a persisted active round as interrupted or count its actions twice', async () => {
    history = [structuredClone(view.report!)];
    mount();
    await records();
    await waitFor(() => expect(document.querySelector('.care-record-count')?.textContent).toBe('1'));
    const current = document.querySelector('.care-rhythm-col[data-live]');
    expect(current?.getAttribute('title')).toMatch(/Google 1 · 地区站点 0$/);
    expect(screen.queryByText('1 轮未完成')).toBeNull();
  });

  it('shows history read errors instead of presenting a successful empty day', async () => {
    historyFailure = true;
    mount();
    await records();
    await screen.findAllByText(/记录读取失败/);
    expect(document.querySelector('.care-record-count')?.textContent).toBe('—');
    expect(screen.queryByText('还没有执行记录')).toBeNull();
  });
});

it('confirms a complete session reset, retains the policy and refreshes empty history', async () => {
  view.report!.phase = 'complete';
  mount();
  const sheet = await policy();
  fireEvent.click(within(sheet).getByRole('button', { name: '重置养护会话' }));
  let confirmation = await screen.findByRole('dialog', { name: '重置养护会话' });
  expect(commands).toHaveLength(0);
  fireEvent.click(within(confirmation).getByRole('button', { name: '取消' }));
  expect(commands).toHaveLength(0);
  fireEvent.click(within(sheet).getByRole('button', { name: '重置养护会话' }));
  confirmation = await screen.findByRole('dialog', { name: '重置养护会话' });
  fireEvent.click(within(confirmation).getByRole('button', { name: '确认重置' }));
  await waitFor(() => expect(commands.map(c => c.kind)).toEqual(['session-reset']));
  await waitFor(() =>
    expect((within(sheet).getByRole('button', { name: '重置养护会话' }) as HTMLButtonElement).disabled).toBe(true),
  );
  fireEvent.click(sheet.querySelector<HTMLButtonElement>('.care-sheet-close')!);
  await screen.findByText('尚未开始');
  expect(automatic(false).getAttribute('aria-pressed')).toBe('true');
  await records();
  await screen.findByText('还没有执行记录');
  expect(view.report!.daily_requests).toBe(0);
  expect(view.desired.policy.interval_minutes).toBe(20);
});

it('shows resetting throughout the request and the interval before the Agent acknowledges', async () => {
  view.report!.phase = 'failed';
  view.report!.reason = 'all_actions_failed';
  view.report!.actions = [];
  resetAcknowledged = false;
  let finish!: () => void;
  resetGate = new Promise<void>(resolve => {
    finish = resolve;
  });
  mount();
  const sheet = await policy();
  fireEvent.click(within(sheet).getByRole('button', { name: '重置养护会话' }));
  const confirmation = await screen.findByRole('dialog', { name: '重置养护会话' });
  fireEvent.click(within(confirmation).getByRole('button', { name: '确认重置' }));
  await waitFor(() =>
    expect(document.querySelector('.care-config-state [role="status"]')?.textContent).toBe('正在重置会话'),
  );
  await act(async () => {
    finish();
    await resetGate;
  });
  await waitFor(() => expect(view.report).toBeNull());
  expect(document.querySelector('.care-config-state [role="status"]')?.textContent).toBe('正在重置会话');
  view.report = maintenanceResetReport(view.desired);
  await act(() => client.invalidateQueries({ queryKey: ['node-ip-maintenance', 'n1'] }));
  await waitFor(() =>
    expect(document.querySelector('.care-config-state [role="status"]')?.textContent).toBe('尚未开始'),
  );
  expect(document.querySelector('.care-config .callout.err')).toBeNull();
});

it('closes deleted round details when a different window resets the session', async () => {
  view.report!.phase = 'complete';
  mount();
  const recordsSheet = await records();
  fireEvent.click(within(recordsSheet).getByRole('button', { name: /Google.*5 个动作/ }));
  await screen.findByRole('dialog', { name: '养护轮次' });
  await waitFor(() => expect(client.isFetching()).toBe(0));
  view.desired.generation++;
  view.desired.session_generation++;
  view.desired.policy.enabled = false;
  view.report = maintenanceResetReport(view.desired);
  history = [];
  let finish!: () => void;
  viewGate = new Promise<void>(resolve => {
    finish = resolve;
  });
  let refreshing!: Promise<unknown>;
  act(() => {
    refreshing = Promise.all(
      ['node-ip-maintenance', 'node-ip-maintenance-runs', 'node-ip-maintenance-day', 'node-ip-maintenance-run'].map(
        key => client.invalidateQueries({ queryKey: [key, 'n1'] }),
      ),
    );
  });
  await waitFor(() => expect(missingRoundReads).toBe(1));
  expect(recordsSheet.querySelector('.callout.err')).toBeNull();
  await act(async () => {
    finish();
    await refreshing;
  });
  await waitFor(() => expect(screen.queryByRole('dialog', { name: '养护轮次' })).toBeNull());
  await within(recordsSheet).findByText('还没有执行记录');
  expect(missingRoundReads).toBe(1);
  expect(recordsSheet.querySelector('.callout.err')).toBeNull();
});

it('keeps a real missing-round error visible when the session has not changed', async () => {
  mount();
  const recordsSheet = await records();
  history = [];
  fireEvent.click(within(recordsSheet).getByRole('button', { name: /Google.*5 个动作/ }));
  const detail = await screen.findByRole('dialog', { name: '养护轮次' });
  await within(detail).findByText('养护轮次不存在');
  expect(missingRoundReads).toBe(1);
});

it('starts automatic maintenance after a reset and keeps it on before the first round begins', async () => {
  view.desired.session_generation++;
  view.desired.policy.enabled = false;
  view.report = maintenanceResetReport(view.desired);
  history = [];
  let finish!: () => void;
  saveGate = new Promise<void>(resolve => {
    finish = resolve;
  });
  mount();
  await ready();
  const start = automatic(true);
  fireEvent.click(start);
  await waitFor(() => expect(writes).toHaveLength(1));
  expect(automatic(true).getAttribute('aria-pressed')).toBe('true');
  expect(automatic(false).getAttribute('aria-pressed')).toBe('false');
  await screen.findByText('正在开启自动养护');
  await act(async () => {
    finish();
    await saveGate;
  });
  saveGate = null;
  await screen.findByText('正在应用新策略');
  view.report!.generation = view.desired.generation;
  view.report!.phase = 'waiting';
  view.report!.next_run_unix_secs = Math.floor(Date.now() / 1000) + 600;
  await act(() => client.invalidateQueries({ queryKey: ['node-ip-maintenance', 'n1'] }));
  await screen.findByText(/^下一轮/);
  expect(automatic(true).getAttribute('aria-pressed')).toBe('true');
  expect(automatic(false).getAttribute('aria-pressed')).toBe('false');
  expect(view.report!.last_started_at_unix_secs).toBeNull();
  expect(commands).toEqual([]);
});

it('shows partial success and skipped actions while preserving later successful actions', async () => {
  view.report = maintenancePartialReport();
  history = [structuredClone(view.report)];
  mount();
  await screen.findAllByText('部分成功');
  expect(document.querySelector('.care-config-state')?.textContent).toContain('部分成功');
  expect(screen.queryByText('执行失败')).toBeNull();
  await records();
  fireEvent.click(screen.getByRole('button', { name: /Google.*4 个动作/ }));
  const sheet = await screen.findByRole('dialog', { name: '养护轮次' });
  await within(sheet).findByText('页面过大，已跳过');
  expect(sheet.querySelectorAll('.care-steps li[data-state="skip"]')).toHaveLength(1);
  expect(sheet.querySelectorAll('.care-steps li[data-state="done"]')).toHaveLength(2);
  expect(within(sheet).getByText('请求超时')).toBeTruthy();
  expect(document.querySelector('.care-rhythm-col[title*="Google 2"]')).toBeTruthy();
});

it('shows a failed result when every action failed', async () => {
  view.report = maintenancePartialReport();
  view.report.phase = 'failed';
  view.report.reason = 'all_actions_failed';
  view.report.actions = view.report.actions.map(action => ({ ...action, status: null, error: 'network_timeout' }));
  history = [structuredClone(view.report)];
  mount();
  await screen.findByText('执行失败');
  expect(screen.queryByText('部分成功')).toBeNull();
  await screen.findAllByText('本轮没有成功动作');
});

it('keeps rate-limit cooldown text short even after a successful action', async () => {
  view.report = maintenancePartialReport();
  view.report.phase = 'cooldown';
  view.report.reason = 'service_restricted';
  history = [structuredClone(view.report)];
  mount();
  await records();
  expect(document.querySelector('.care-round-result')?.textContent).toBe('冷却：限流');
});

it('explains historical blocked and empty pages while keeping a later successful visit', async () => {
  view.report = maintenancePartialReport();
  const errors = ['consent_required', 'javascript_required', 'verification_required', 'empty_response'];
  view.report.actions = [
    ...errors.map((error, index) => ({ ...view.report!.actions[0], index: index + 1, status: null, error })),
    { ...view.report.actions[3], index: 5 },
  ];
  view.report.actions_total = 5;
  view.report.action_index = 5;
  history = [structuredClone(view.report)];
  mount();
  await screen.findAllByText('部分成功');
  await records();
  fireEvent.click(screen.getByRole('button', { name: /Google.*5 个动作/ }));
  const sheet = await screen.findByRole('dialog', { name: '养护轮次' });
  for (const reason of ['停留在网站同意页', '页面需要 JavaScript', '网站要求验证', '网站未返回页面内容']) {
    expect(within(sheet).getByText(reason)).toBeTruthy();
  }
  expect(sheet.querySelectorAll('.care-steps li[data-state="done"]')).toHaveLength(1);
  expect(screen.queryByText('执行失败')).toBeNull();
});

it('preserves partial success when public masking blanks action error text', async () => {
  view.report = maintenancePartialReport();
  view.report.actions = view.report.actions.map(action => ({ ...action, error: action.error === null ? null : '' }));
  history = [structuredClone(view.report)];
  mount(node(), true);
  await screen.findAllByText('部分成功');
  expect(document.querySelector('.care-config-state')?.textContent).toContain('部分成功');
  expect(automatic(true).matches(':disabled')).toBe(true);
});
