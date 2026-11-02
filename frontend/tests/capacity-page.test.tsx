import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, waitFor, within } from '@testing-library/react';
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import type { MachineTrafficModelNode, MachineTrafficModelView } from '../src/api';
import * as route from '../src/forge/route';

let CapacityPane: typeof import('../src/panes/capacity').CapacityPane;
let cumulativeSeries: typeof import('../src/panes/capacity').cumulativeSeries;

beforeAll(async () => {
  ({ CapacityPane, cumulativeSeries } = await import('../src/panes/capacity'));
});

const DAY = 86_400;
const GiB = 1024 ** 3;
const TiB = 1024 ** 4;
const start = Date.UTC(2026, 9, 1) / 1000;
const end = Date.UTC(2026, 10, 1) / 1000;
// 10-13 12:00 UTC：本期已过 12.5 天、剩 18.5 天。
const now = start + 12.5 * DAY;

const bytes = (gib: number) => String(gib * GiB);
const ledger = (days: number, gibPerDay: number) =>
  Array.from({ length: days }, (_, index) => ({
    day_start_unix_secs: start + index * DAY,
    bytes: bytes(gibPerDay),
  }));

function machine(node_id: string, name: string, extra: Partial<MachineTrafficModelNode>): MachineTrafficModelNode {
  return {
    node_id,
    tenant_id: 'platform',
    name,
    cycle_kind: 'monthly',
    accounting_mode: 'bidirectional_sum',
    reset_month: null,
    reset_day: 1,
    period_start_unix_secs: start,
    period_end_unix_secs: end,
    previous_period_start_unix_secs: start - 31 * DAY,
    quota_bytes: null,
    used_bytes: '0',
    previous_period_bytes: '0',
    daily_rate_bytes: '0',
    rate_days: 28,
    projected_p50_bytes: '0',
    projected_p90_bytes: '0',
    band: { basis: 'fleet', horizon_days: 19, samples: 120, upper_error_basis_points: 2000 },
    confidence: 'medium',
    needs_calibration: false,
    days: [],
    previous_days: [],
    tracking_started_at_unix_secs: start - 90 * DAY,
    calibrated_at_unix_secs: null,
    gap_at_unix_secs: null,
    gap_reason: null,
    last_period_accuracy: null,
    users: [],
    unattributed_bytes: '0',
    projected_unattributed_bytes: '0',
    ...extra,
  };
}

const machineModel = (): MachineTrafficModelView => ({
  generated_at_unix_secs: now,
  attribution: 'current_topology_proportional',
  nodes: [
    // 预计超额：P50 = 520 + 42 × 18.5 = 1297 GiB ≥ 配额 1 TiB；按 P50 的速度从现在起 12 天后到达配额。
    machine('jp-risk', '东京 RISK', {
      quota_bytes: String(TiB),
      used_bytes: bytes(520),
      days: ledger(13, 40),
      previous_days: ledger(31, 40).map(day => ({ ...day, day_start_unix_secs: day.day_start_unix_secs - 31 * DAY })),
      previous_period_bytes: bytes(1240),
      daily_rate_bytes: bytes(42),
      projected_p50_bytes: bytes(1297),
      projected_p90_bytes: bytes(1536),
      band: { basis: 'node', horizon_days: 19, samples: 160, upper_error_basis_points: 3076 },
      confidence: 'high',
      last_period_accuracy: {
        period_start_unix_secs: Date.UTC(2026, 8, 1) / 1000,
        period_end_unix_secs: start,
        actual_bytes: bytes(1200),
        forecasts: 30,
        p50_error_basis_points: 640,
        p90_covered: 28,
      },
    }),
    // 可能超额：只有 P90 越过 2 TiB。
    machine('us-maybe', '洛杉矶 MAYBE', {
      quota_bytes: String(2 * TiB),
      used_bytes: bytes(820),
      days: ledger(13, 63),
      daily_rate_bytes: bytes(55),
      projected_p50_bytes: bytes(1843),
      projected_p90_bytes: bytes(2150),
      // 控制面在重置前后失联：总量可能含上期流量。P90 已越过配额，状态仍是可能超额。
      needs_calibration: true,
      gap_at_unix_secs: start + 600,
      gap_reason: 'report-gap',
      confidence: 'low',
    }),
    // 已超额：每天 50 GiB，第 10 天结束时累计达到 500 GiB 配额。
    machine('mo-over', '澳门 OVER', {
      quota_bytes: bytes(500),
      used_bytes: bytes(600),
      days: ledger(12, 50),
      daily_rate_bytes: bytes(48),
      projected_p50_bytes: bytes(1488),
      projected_p90_bytes: bytes(1711),
      band: { basis: 'node', horizon_days: 19, samples: 160, upper_error_basis_points: 2511 },
      confidence: 'high',
      users: [
        {
          tenant_id: 'platform',
          user_id: 'alice',
          attributed_bytes: bytes(400),
          projected_bytes: bytes(992),
          share_basis_points: 6667,
          apps: ['CN2 入口'],
        },
      ],
      unattributed_bytes: bytes(200),
      projected_unattributed_bytes: bytes(496),
    }),
    machine('hk-ok', '香港 OK', {
      quota_bytes: String(4 * TiB),
      used_bytes: bytes(1040),
      days: ledger(13, 80),
      projected_p50_bytes: String(2 * TiB),
      projected_p90_bytes: String(2.5 * TiB),
    }),
    // 10-10 12:00 才开始记账、没有校准：预计远低于配额，但不能据此判余量充足。
    machine('kr-late', '首尔 LATE', {
      quota_bytes: String(2 * TiB),
      used_bytes: bytes(120),
      days: [
        { day_start_unix_secs: start + 9 * DAY, bytes: bytes(20) },
        { day_start_unix_secs: start + 10 * DAY, bytes: bytes(40) },
        { day_start_unix_secs: start + 11 * DAY, bytes: bytes(40) },
        { day_start_unix_secs: start + 12 * DAY, bytes: bytes(20) },
      ],
      tracking_started_at_unix_secs: start + 9.5 * DAY,
      daily_rate_bytes: bytes(40),
      rate_days: 2,
      projected_p50_bytes: bytes(860),
      projected_p90_bytes: bytes(1008),
      needs_calibration: true,
      confidence: 'low',
    }),
    machine('sg-unset', '新加坡 UNSET', {
      used_bytes: bytes(299),
      days: ledger(13, 23),
      projected_p50_bytes: bytes(700),
      projected_p90_bytes: bytes(900),
    }),
    machine('de-new', '法兰克福 NEW', {
      tracking_started_at_unix_secs: null,
      rate_days: 0,
      band: { basis: 'default', horizon_days: 19, samples: 0, upper_error_basis_points: 5000 },
      confidence: 'low',
    }),
  ],
});

const json = (body: unknown) =>
  new Response(JSON.stringify(body), { status: 200, headers: { 'content-type': 'application/json' } });

let fetchMock: ReturnType<typeof vi.fn>;

beforeEach(() => {
  // 走势图按容器像素宽度绘制；jsdom 没有布局，给它一个固定宽度。
  vi.stubGlobal(
    'ResizeObserver',
    class {
      constructor(private readonly callback: ResizeObserverCallback) {}
      observe() {
        this.callback([{ contentRect: { width: 640 } } as ResizeObserverEntry], this as unknown as ResizeObserver);
      }
      disconnect() {}
    },
  );
  fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const path = String(input);
    if (path === '/usage/machine-model') return json(machineModel());
    if (path === '/nodes/agent-state') return json({ nodes: [{ node_id: 'jp-risk', public_ipv4_country: 'JP' }] });
    if (path === '/node-traffic/nodes/sg-unset' && init?.method === 'PUT') return json({ nodes: [] });
    throw new Error(`unexpected request: ${path}`);
  });
  vi.stubGlobal('fetch', fetchMock);
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const renderPane = (canEdit = true) =>
  render(
    <QueryClientProvider
      client={
        new QueryClient({
          defaultOptions: { queries: { retry: false, staleTime: Infinity, refetchOnWindowFocus: false } },
        })
      }
    >
      <CapacityPane canEdit={canEdit} />
    </QueryClientProvider>,
  );

describe('machine capacity forecast', () => {
  it('groups machines by risk and judges overrun by the expected period end', async () => {
    const view = renderPane();
    expect(await view.findByRole('heading', { name: '流量预测' })).toBeTruthy();
    await view.findByText('澳门 OVER');
    const readout = view.container.querySelector('.cap-rd')!;
    expect(readout.textContent).toBe('7 台·1 已超额·1 预计超额·1 可能超额·1 待校准·1 未设配额·1 未上报');

    const groups = Array.from(view.container.querySelectorAll('.cap-body > [role="list"]')).map(list => [
      list.getAttribute('aria-label'),
      within(list as HTMLElement)
        .getAllByRole('listitem')
        .map(item => item.querySelector('.cap-id b')!.textContent),
    ]);
    expect(groups).toEqual([
      ['需关注', ['澳门 OVER', '东京 RISK', '洛杉矶 MAYBE', '首尔 LATE']],
      ['余量充足', ['香港 OK']],
      ['未设配额', ['新加坡 UNSET']],
      ['未上报', ['法兰克福 NEW']],
    ]);

    const row = (name: string) => view.getByText(name).closest('.cap-row') as HTMLElement;
    expect(within(row('澳门 OVER')).getByText('已超出 100 GiB')).toBeTruthy();
    expect(within(row('澳门 OVER')).getByText('10-11 越过配额')).toBeTruthy();
    expect(within(row('东京 RISK')).getByText('预计 10-25 用尽')).toBeTruthy();
    expect(within(row('东京 RISK')).getByText('期末超出 273 GiB')).toBeTruthy();
    expect(within(row('东京 RISK')).getByText('配额的 127%').className).toBe('bad');
    expect(within(row('洛杉矶 MAYBE')).getByText('可能超额')).toBeTruthy();
    expect(within(row('洛杉矶 MAYBE')).getByText('P90 超出 102 GiB')).toBeTruthy();
    expect(within(row('香港 OK')).getByText('期末余 2.00 TiB')).toBeTruthy();
    expect(within(row('首尔 LATE')).getByText('待校准').closest('.cap-verdict')!.className).toBe('cap-verdict warn');
    expect(within(row('首尔 LATE')).getByText('10-10 起才有记录')).toBeTruthy();
    expect(within(row('法兰克福 NEW')).getByText('未上报')).toBeTruthy();
    expect(within(row('法兰克福 NEW')).getAllByText('—')).toHaveLength(2);
    expect(within(row('东京 RISK')).getByText('剩 19 天 · 每月 1 日重置')).toBeTruthy();
    await waitFor(() => expect(row('东京 RISK').querySelector('.geo-flag')?.getAttribute('title')).toBe('JP'));
  });

  it('opens the riskiest machine with its burn-up chart, estimates and user attribution', async () => {
    const view = renderPane();
    const over = await view.findByRole('button', { name: /澳门 OVER/ });
    expect(over.getAttribute('aria-expanded')).toBe('true');
    const detail = document.getElementById(over.getAttribute('aria-controls')!)!;
    expect(within(detail).getByRole('img', { name: '澳门 OVER 本期累计流量与预计' })).toBeTruthy();
    expect(within(detail).getByText('10-11 越过')).toBeTruthy();
    expect(within(detail).getByText('配额 500 GiB')).toBeTruthy();
    expect(within(detail).getByText('近期日均').closest('[title]')!.getAttribute('title')).toBe(
      '最近 28 天中记录完整的 28 天加权平均，每早 4 天权重减半',
    );
    expect(within(detail).getByText('48.0 GiB/天')).toBeTruthy();
    expect(within(detail).getByText('暂无记录').className).toBe('mute');
    expect(detail.querySelector('.cap-conf')!.textContent).toBe(
      '可信度 高 · 近 28 天中 28 天记录完整 · 误差按本机历史',
    );
    expect(within(detail).getByText('alice')).toBeTruthy();
    // 用户占用：与名册同一张身份牌；第二行只写线路，不写租户；「未归因」只留空位
    expect(within(detail).getByText('CN2 入口')).toBeTruthy();
    expect(within(detail).queryByText(/platform/)).toBeNull();
    const aliceAvatar = detail.querySelector('.cap-user:not(.head):not(.rest) .user-avatar.cell');
    expect(aliceAvatar?.getAttribute('data-avatar-mark')).toBe('apple');
    expect(aliceAvatar?.querySelector('.user-avatar-mark')).not.toBeNull();
    expect(detail.querySelector('.cap-user.rest .user-avatar')).toBeNull();
    expect(within(detail).getByText('66.7%')).toBeTruthy();
    expect(within(detail).getByText('未归因')).toBeTruthy();
    expect(within(detail).getByText('33.3%')).toBeTruthy();

    fireEvent.click(view.getByRole('button', { name: /东京 RISK/ }));
    const risk = view.getByRole('button', { name: /东京 RISK/ });
    expect(risk.getAttribute('aria-expanded')).toBe('true');
    const riskDetail = document.getElementById(risk.getAttribute('aria-controls')!)!;
    expect(within(riskDetail).getByText('10-25 用尽')).toBeTruthy();
    expect(within(riskDetail).getByText('上期')).toBeTruthy();
    expect(within(riskDetail).getByText('42.0 GiB/天')).toBeTruthy();
    expect(within(riskDetail).getByText('P90', { selector: 'dt' }).closest('[title]')!.getAttribute('title')).toBe(
      '本期已用 + 近期日均 × 剩余天数 × (1 + 31%)；31% 是本机 160 次「预测之后 19 天」的误差 90% 分位',
    );
    const accuracy = within(riskDetail).getByText('6.4%');
    expect(accuracy.closest('[title]')!.getAttribute('title')).toBe(
      '09-01 → 10-01 每天记录的 30 次预测：P50 与期末实际 1.17 TiB 平均相差 6.4%，28 次实际没有超过 P90',
    );

    // 点行内空白处同样收起。
    fireEvent.click(view.getByText('澳门 OVER').closest('.cap-row')!.querySelector('.cap-run')!);
    expect(over.getAttribute('aria-expanded')).toBe('false');
  });

  it('asks for a calibration instead of calling an incomplete total safe', async () => {
    const navigate = vi.spyOn(route, 'navigate').mockReturnValue(true);
    const view = renderPane();
    fireEvent.click(await view.findByRole('button', { name: /首尔 LATE/ }));
    const late = view.getByRole('button', { name: /首尔 LATE/ });
    const detail = document.getElementById(late.getAttribute('aria-controls')!)!;
    expect(detail.querySelector('.cap-conf')!.textContent).toBe(
      '可信度 低 · 10-10 起记账，此前的流量未计入 · 近 28 天中 2 天记录完整 · 误差按全部机器的历史',
    );
    fireEvent.click(within(detail).getByRole('button', { name: '去校准 ›' }));
    expect(navigate).toHaveBeenCalledExactlyOnceWith('nodes', { p: 'node', id: 'kr-late' });

    // 已越过 P90 的机器仍按风险排，缺口写在可信度与图上。
    fireEvent.click(view.getByRole('button', { name: /洛杉矶 MAYBE/ }));
    const maybe = view.getByRole('button', { name: /洛杉矶 MAYBE/ });
    const maybeDetail = document.getElementById(maybe.getAttribute('aria-controls')!)!;
    expect(maybeDetail.querySelector('.cap-conf')!.textContent).toBe(
      '可信度 低 · 10-01 控制面跨日失联，可能计入了上期流量 · 近 28 天中 28 天记录完整 · 误差按全部机器的历史',
    );
    expect(maybeDetail.querySelector('.cap-chart .gap title')!.textContent).toBe(
      '10-01 控制面跨日失联，可能计入了上期流量',
    );
  });

  it('sets a quota from an unset row and validates the TiB input', async () => {
    const view = renderPane();
    fireEvent.click(await view.findByRole('button', { name: '设置配额 ›' }));
    const input = view.getByRole('textbox', { name: '新加坡 UNSET 机器配额' }) as HTMLInputElement;
    expect(document.activeElement).toBe(input);

    fireEvent.change(input, { target: { value: '1.555' } });
    fireEvent.click(view.getByRole('button', { name: '保存' }));
    expect(view.getByText('需为大于 0 的数字，最多两位小数；留空表示不设配额')).toBeTruthy();
    expect(input.getAttribute('aria-invalid')).toBe('true');
    expect(fetchMock.mock.calls.some(([, init]) => (init as RequestInit | undefined)?.method === 'PUT')).toBe(false);

    fireEvent.change(input, { target: { value: '1.5' } });
    fireEvent.click(view.getByRole('button', { name: '保存' }));
    await waitFor(() => expect(view.queryByRole('textbox', { name: '新加坡 UNSET 机器配额' })).toBeNull());
    const put = fetchMock.mock.calls.find(([, init]) => (init as RequestInit | undefined)?.method === 'PUT')!;
    expect(String(put[0])).toBe('/node-traffic/nodes/sg-unset');
    expect(JSON.parse(String((put[1] as RequestInit).body))).toEqual({
      cycle_kind: 'monthly',
      accounting_mode: 'bidirectional_sum',
      reset_month: null,
      reset_day: 1,
      quota_bytes: String(1.5 * TiB),
      calibrated_total_bytes: null,
    });
    expect(fetchMock.mock.calls.filter(([input]) => String(input) === '/usage/machine-model')).toHaveLength(2);
  });

  it('leaves quotas read-only without edit rights and links to the machine detail', async () => {
    const navigate = vi.spyOn(route, 'navigate').mockReturnValue(true);
    const view = renderPane(false);
    await view.findByRole('heading', { name: '流量预测' });
    expect(view.queryByRole('button', { name: '设置配额 ›' })).toBeNull();
    expect(view.queryByRole('button', { name: '修改' })).toBeNull();
    fireEvent.click(await view.findByRole('button', { name: '机器详情 ›' }));
    expect(navigate).toHaveBeenCalledExactlyOnceWith('nodes', { p: 'node', id: 'mo-over' });
  });

  it('saves a quota in fractional GiB and clears it while that unit is selected', async () => {
    const view = renderPane();
    fireEvent.click(await view.findByRole('button', { name: '设置配额 ›' }));
    const input = view.getByRole('textbox', { name: '新加坡 UNSET 机器配额' });
    fireEvent.change(view.getByLabelText('新加坡 UNSET 机器配额单位'), { target: { value: 'GiB' } });
    fireEvent.change(input, { target: { value: '100.25' } });
    fireEvent.click(view.getByRole('button', { name: '保存' }));
    await waitFor(() => expect(view.queryByRole('textbox', { name: '新加坡 UNSET 机器配额' })).toBeNull());
    const puts = () => fetchMock.mock.calls.filter(([, init]) => (init as RequestInit | undefined)?.method === 'PUT');
    expect(JSON.parse(String((puts()[0][1] as RequestInit).body)).quota_bytes).toBe('107642617856');

    fireEvent.click(view.getByRole('button', { name: '设置配额' }));
    fireEvent.change(view.getByLabelText('新加坡 UNSET 机器配额单位'), { target: { value: 'GiB' } });
    fireEvent.change(view.getByRole('textbox', { name: '新加坡 UNSET 机器配额' }), { target: { value: '' } });
    fireEvent.click(view.getByRole('button', { name: '保存' }));
    await waitFor(() => expect(puts()).toHaveLength(2));
    expect(JSON.parse(String((puts()[1][1] as RequestInit).body)).quota_bytes).toBeNull();
  });
});

describe('cumulative ledger series', () => {
  const day = (index: number, value: number) => ({ day_start_unix_secs: start + index * DAY, bytes: String(value) });
  const series = (extra: Partial<Parameters<typeof cumulativeSeries>[0]>, used: number, elapsedDays: number) =>
    cumulativeSeries(
      {
        period_start_unix_secs: start,
        tracking_started_at_unix_secs: start - DAY,
        calibrated_at_unix_secs: null,
        days: [],
        ...extra,
      },
      used,
      elapsedDays,
    );

  it('stays flat across days without accounted bytes and ends at the current instant', () => {
    expect(series({ days: [day(0, 10), day(2, 20)] }, 30, 3.5)).toEqual([
      [0, 0],
      [1, 10],
      [2, 10],
      [3, 30],
      [3.5, 30],
    ]);
  });

  it('applies a calibration on its day so the line ends at the authoritative total', () => {
    expect(series({ days: [day(0, 10), day(2, 20)], calibrated_at_unix_secs: start + 1.25 * DAY }, 100, 3.5)).toEqual([
      [0, 0],
      [1, 10],
      [2, 80],
      [3, 100],
      [3.5, 100],
    ]);
  });

  it('starts at the tracking instant when accounting began inside the period', () => {
    expect(series({ days: [day(1, 5), day(2, 10)], tracking_started_at_unix_secs: start + 1.5 * DAY }, 15, 3)).toEqual([
      [1.5, 0],
      [2, 5],
      [3, 15],
    ]);
  });
});
