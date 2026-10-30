import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest';
import type {
  NodeAgentStateItem,
  NodeTrafficItem,
  NodeTrafficLimitView,
  NodeTrafficView,
  TrafficLimitObservation,
} from '../src/api';
import { trafficLimitForm, trafficLimitPolicy, trafficLimitStatus } from '../src/panes/nodes-traffic-limit';

let TrafficAccountingCard: typeof import('../src/panes/nodes').TrafficAccountingCard;

beforeAll(async () => {
  vi.stubGlobal(
    'matchMedia',
    vi.fn(() => ({ matches: false, addEventListener: vi.fn(), removeEventListener: vi.fn() })),
  );
  ({ TrafficAccountingCard } = await import('../src/panes/nodes'));
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const GIB = 1024 ** 3;
const TIB = 1024 ** 4;
const DAY = 86_400;
const nowSecs = () => Math.floor(Date.now() / 1000);

function traffic(patch: Partial<NodeTrafficItem> = {}, limit: Partial<NodeTrafficLimitView> = {}): NodeTrafficItem {
  return {
    node_id: 'hk',
    tenant_id: 'platform',
    name: '香港',
    cycle_kind: 'monthly',
    accounting_mode: 'bidirectional_sum',
    reset_month: null,
    reset_day: 1,
    quota_bytes: String(10 * TIB),
    period_start_unix_secs: nowSecs() - 20 * DAY,
    period_end_unix_secs: nowSecs() + 10 * DAY,
    rx_bytes: String(TIB),
    tx_bytes: String(0.2 * TIB),
    total_bytes: String(1.2 * TIB),
    interface: 'eth0',
    tracking_started_at_unix_secs: nowSecs() - 30 * DAY,
    last_reported_at_unix_secs: nowSecs(),
    calibrated_at_unix_secs: null,
    last_gap_at_unix_secs: null,
    last_gap_reason: null,
    has_gap: false,
    limit: {
      revision: 0,
      policy: { enabled: false, layer: 'application', rules: [] },
      supported: true,
      observation: null,
      reported_at_unix_secs: null,
      ...limit,
    },
    ...patch,
  };
}

function observed(patch: Partial<TrafficLimitObservation>): TrafficLimitObservation {
  return {
    revision: 1,
    phase: 'ready',
    interface: 'eth0',
    rate_bps: null,
    used_bytes: '0',
    period_start_unix_secs: 1,
    error: null,
    system: null,
    application: null,
    ...patch,
  };
}

const ok = (item: NodeTrafficItem) => ({ ok: true, status: 200, json: async () => ({ nodes: [item] }) });

describe('machine traffic limit confirmation', () => {
  it('retains a confirmed limit through missed heartbeats and expires it at 300 seconds', () => {
    const limit = traffic(
      {},
      {
        revision: 1,
        policy: { enabled: true, layer: 'application', rules: [] },
        observation: observed({ phase: 'limited', rate_bps: 100_000_000 }),
        reported_at_unix_secs: 1_000,
      },
    ).limit!;
    for (const elapsed of [60, 90, 120, 180, 240, 299.999]) {
      const status = trafficLimitStatus(limit, 1_000 + elapsed);
      expect(status.confirmed).toBe(true);
      expect(status.text).toBe('已限速');
      expect(status.rate).toBe(100_000_000);
    }
    const expired = trafficLimitStatus(limit, 1_300);
    expect(expired.confirmed).toBe(false);
    expect(expired.text).toBe('等待应用确认');
    expect(expired.rate).toBeNull();
    expect(trafficLimitStatus({ ...limit, revision: 2 }, 1_060).confirmed).toBe(false);
  });

  it('shows generation handoff as pending while retaining individual layer results', () => {
    const limit = traffic(
      {},
      {
        revision: 1,
        policy: { enabled: true, layer: 'both', rules: [] },
        observation: observed({
          phase: 'waiting',
          rate_bps: 100_000_000,
          system: { phase: 'limited', rate_bps: 100_000_000, error: null },
          application: { phase: 'waiting', rate_bps: null, error: '等待旧代排空后再确认应用层上限' },
        }),
        reported_at_unix_secs: 1_000,
      },
    ).limit!;
    const status = trafficLimitStatus(limit, 1_060);
    expect(status.text).toBe('等待应用确认');
    expect(status.chip?.text).toBe('限速待确认');
    expect(status.tone).toBe('mute');
    expect(status.rate).toBeNull();
    expect(status.layers.map(layer => layer.text)).toEqual(['已限速', '等待应用确认']);
  });
});

function renderCard(item: NodeTrafficItem, canEdit = true, fetchMock = vi.fn().mockResolvedValue(ok(item))) {
  vi.stubGlobal('fetch', fetchMock);
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  client.setQueryData<NodeTrafficView>(['node-traffic'], { nodes: [item] });
  render(
    <QueryClientProvider client={client}>
      <TrafficAccountingCard node={{ node_id: 'hk', name: '香港' } as NodeAgentStateItem} canEdit={canEdit} />
    </QueryClientProvider>,
  );
  const panel = document.querySelector('.node-traffic-accounting');
  expect(panel).toBeTruthy();
  return { panel: panel!, fetchMock };
}

/** jsdom 没有布局；旗标排布按 400px 宽的额度条计算。 */
function stubBarWidth(width: number) {
  vi.stubGlobal(
    'ResizeObserver',
    class {
      constructor(private readonly callback: ResizeObserverCallback) {}
      observe() {
        this.callback([{ contentRect: { width } } as ResizeObserverEntry], this as unknown as ResizeObserver);
      }
      disconnect() {}
      unobserve() {}
    },
  );
}

const bodyOf = (fetchMock: ReturnType<typeof vi.fn>, call: number) =>
  JSON.parse(String((fetchMock.mock.calls[call] as [string, RequestInit])[1].body));
const urlOf = (fetchMock: ReturnType<typeof vi.fn>, call: number) => (fetchMock.mock.calls[call] as [string])[0];

describe('machine traffic limits in the traffic accounting card', () => {
  it('saves layered percentage and absolute rules with the shared save bar', async () => {
    const { fetchMock } = renderCard(traffic());
    fireEvent.click(screen.getByRole('button', { name: '开启' }));
    // 开启时有额度，补的第一条是 90% 限至 100 Mbps。
    expect((screen.getByLabelText('规则 1 阈值') as HTMLInputElement).value).toBe('90');
    fireEvent.click(screen.getByRole('button', { name: '双层' }));
    fireEvent.click(screen.getByRole('button', { name: '添加规则' }));
    fireEvent.change(screen.getByLabelText('规则 2 阈值单位'), { target: { value: 'GiB' } });
    fireEvent.change(screen.getByLabelText('规则 2 阈值'), { target: { value: '800' } });
    fireEvent.change(screen.getByLabelText('规则 2 限速'), { target: { value: '512' } });
    fireEvent.change(screen.getByLabelText('规则 2 速度单位'), { target: { value: 'Kbps' } });
    expect(document.querySelector('.config-panel-savebar')?.textContent).toBe('保存还原');
    fireEvent.click(screen.getByRole('button', { name: '保存' }));

    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(1));
    expect(urlOf(fetchMock, 0)).toBe('/node-traffic/nodes/hk/limit');
    expect(bodyOf(fetchMock, 0)).toEqual({
      enabled: true,
      layer: 'both',
      rules: [
        { threshold: { kind: 'quota_percent', basis_points: 9000 }, rate_bps: 100_000_000 },
        { threshold: { kind: 'absolute', bytes: String(800n * 1024n ** 3n) }, rate_bps: 512_000 },
      ],
    });
  });

  it('requires a quota for percentage rules and saves the quota before the limit', async () => {
    const fetchMock = vi.fn().mockResolvedValue(ok(traffic()));
    renderCard(traffic({ quota_bytes: null }), true, fetchMock);
    fireEvent.click(screen.getByRole('button', { name: '开启' }));
    // 没有额度时补的第一条是绝对用量，可以直接保存。
    expect((screen.getByLabelText('规则 1 阈值单位') as HTMLSelectElement).value).toBe('TiB');
    expect(screen.getByRole('button', { name: '保存' }).hasAttribute('disabled')).toBe(false);
    fireEvent.change(screen.getByLabelText('规则 1 阈值单位'), { target: { value: '%' } });
    fireEvent.change(screen.getByLabelText('规则 1 阈值'), { target: { value: '80' } });
    expect(screen.getByText('按百分比限速需要先填写流量额度。')).toBeTruthy();
    expect(screen.getByLabelText('规则 1 阈值').getAttribute('aria-invalid')).toBe('true');
    expect(screen.getByRole('button', { name: '保存' }).hasAttribute('disabled')).toBe(true);

    fireEvent.change(screen.getByLabelText('机器流量额度'), { target: { value: '2' } });
    expect(screen.queryByText('按百分比限速需要先填写流量额度。')).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: '保存' }));

    // 服务端用已保存的额度校验百分比限速，所以先存额度。
    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(2));
    expect(urlOf(fetchMock, 0)).toBe('/node-traffic/nodes/hk');
    expect(bodyOf(fetchMock, 0).quota_bytes).toBe(String(2 * TIB));
    expect(urlOf(fetchMock, 1)).toBe('/node-traffic/nodes/hk/limit');
    expect(bodyOf(fetchMock, 1).rules).toEqual([
      { threshold: { kind: 'quota_percent', basis_points: 8000 }, rate_bps: 100_000_000 },
    ]);
  });

  it('saves the limit first when the quota it depended on is cleared', async () => {
    const percent = traffic(
      { quota_bytes: String(2 * TIB) },
      {
        revision: 1,
        policy: {
          enabled: true,
          layer: 'application',
          rules: [{ threshold: { kind: 'quota_percent', basis_points: 8000 }, rate_bps: 100_000_000 }],
        },
      },
    );
    const { fetchMock } = renderCard(percent);
    fireEvent.change(screen.getByLabelText('机器流量额度'), { target: { value: '' } });
    expect(screen.getByText('按百分比限速需要先填写流量额度。')).toBeTruthy();
    fireEvent.change(screen.getByLabelText('规则 1 阈值单位'), { target: { value: 'TiB' } });
    fireEvent.change(screen.getByLabelText('规则 1 阈值'), { target: { value: '1.6' } });
    fireEvent.click(screen.getByRole('button', { name: '保存' }));

    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(2));
    expect(urlOf(fetchMock, 0)).toBe('/node-traffic/nodes/hk/limit');
    // 1.6 TiB 不是整数字节，四舍五入到最近的字节。
    expect(bodyOf(fetchMock, 0).rules[0].threshold).toEqual({
      kind: 'absolute',
      bytes: String((16n * 1024n ** 4n + 5n) / 10n),
    });
    expect(urlOf(fetchMock, 1)).toBe('/node-traffic/nodes/hk');
    expect(bodyOf(fetchMock, 1).quota_bytes).toBeNull();
  });

  it('keeps the unsaved limit when its request fails after the settings were saved', async () => {
    const saved = traffic({ quota_bytes: String(3 * TIB) });
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(ok(saved))
      .mockResolvedValueOnce({
        ok: false,
        status: 400,
        statusText: 'Bad Request',
        json: async () => ({ error: '限速策略被拒绝' }),
      });
    renderCard(traffic(), true, fetchMock);
    fireEvent.change(screen.getByLabelText('机器流量额度'), { target: { value: '3' } });
    fireEvent.click(screen.getByRole('button', { name: '开启' }));
    fireEvent.click(screen.getByRole('button', { name: '保存' }));

    expect(await screen.findByText('限速策略被拒绝')).toBeTruthy();
    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect((screen.getByLabelText('机器流量额度') as HTMLInputElement).value).toBe('3');
    expect(screen.getByRole('button', { name: '开启' }).getAttribute('aria-pressed')).toBe('true');
    expect(document.querySelector('.config-panel-savebar')).toBeTruthy();
  });

  it('cuts the quota bar at each threshold and marks the throttled part once the machine confirms', () => {
    stubBarWidth(400);
    const { panel } = renderCard(
      traffic(
        { quota_bytes: String(2 * TIB), total_bytes: String(1.7 * TIB) },
        {
          revision: 1,
          policy: {
            enabled: true,
            layer: 'application',
            rules: [
              { threshold: { kind: 'quota_percent', basis_points: 9500 }, rate_bps: 10_000_000 },
              { threshold: { kind: 'quota_percent', basis_points: 8000 }, rate_bps: 100_000_000 },
            ],
          },
          observation: observed({
            phase: 'limited',
            rate_bps: 100_000_000,
            application: { phase: 'limited', rate_bps: 100_000_000, error: null },
          }),
          reported_at_unix_secs: nowSecs(),
        },
      ),
    );

    expect(panel.querySelector('.node-traffic-limit-chip.gold')?.textContent).toBe('限速 100 Mbps');
    const cuts = [...panel.querySelectorAll<HTMLElement>('.node-traffic-cut')].map(cut =>
      Number.parseFloat(cut.style.left),
    );
    expect(cuts).toHaveLength(2);
    expect(cuts[0]).toBeCloseTo(80, 5);
    expect(cuts[1]).toBeCloseTo(95, 5);
    expect(panel.querySelector('.node-traffic-hatch')).toBeTruthy();
    const flags = [...panel.querySelectorAll('.node-traffic-flag')];
    expect(flags.map(flag => flag.textContent)).toEqual(['80%100 Mbps', '95%10 Mbps']);
    expect(flags[0].classList.contains('on')).toBe(true);
    // 规则按触发用量排序，正在起作用的一条带金色圆点。
    expect((screen.getByLabelText('规则 1 阈值') as HTMLInputElement).value).toBe('80');
    expect(screen.getByLabelText('规则 1 阈值').closest('.node-traffic-rule')?.classList.contains('on')).toBe(true);
    expect(screen.getByRole('status').textContent).toBe('已限速 100 Mbps');
    // 只有一层且正常时，执行层下面写说明，不重复状态。
    expect(screen.queryByLabelText('限速执行结果')).toBeNull();
  });

  it('draws absolute thresholds without a quota and dims rules that can never apply', () => {
    const { panel } = renderCard(
      traffic(
        { quota_bytes: null, total_bytes: String(0.5 * TIB) },
        {
          revision: 1,
          policy: {
            enabled: true,
            layer: 'system',
            rules: [
              { threshold: { kind: 'absolute', bytes: String(TIB) }, rate_bps: 200_000_000 },
              { threshold: { kind: 'absolute', bytes: String(2 * TIB) }, rate_bps: 300_000_000 },
            ],
          },
        },
      ),
    );

    expect(panel.querySelector('.node-traffic-meter')).toBeTruthy();
    expect(panel.querySelector('.node-traffic-pace')).toBeNull();
    expect(panel.querySelector('.node-traffic-hatch')).toBeNull();
    expect(panel.querySelectorAll('.node-traffic-cut.idle')).toHaveLength(1);
    expect((screen.getByLabelText('规则 1 阈值') as HTMLInputElement).value).toBe('1');
    expect((screen.getByLabelText('规则 1 阈值单位') as HTMLSelectElement).value).toBe('TiB');
    const dominated = screen.getByLabelText('规则 2 阈值').closest('.node-traffic-rule');
    expect(dominated?.classList.contains('idle')).toBe(true);
    expect(dominated?.getAttribute('title')).toContain('不会生效');
  });

  it('distinguishes partial layer failure from a confirmed limit and respects read-only permissions', () => {
    const { panel } = renderCard(
      traffic(
        {},
        {
          revision: 1,
          policy: {
            enabled: true,
            layer: 'both',
            rules: [{ threshold: { kind: 'quota_percent', basis_points: 1000 }, rate_bps: 100_000_000 }],
          },
          observation: observed({
            phase: 'error',
            rate_bps: 100_000_000,
            error: 'IFB 不可用',
            system: { phase: 'error', rate_bps: null, error: 'IFB 不可用' },
            application: { phase: 'limited', rate_bps: 100_000_000, error: null },
          }),
          reported_at_unix_secs: nowSecs(),
        },
      ),
      false,
    );

    expect(panel.querySelector('.node-traffic-limit-chip.err')?.textContent).toBe('限速应用失败');
    expect(screen.getByRole('status').textContent).toBe('应用失败');
    const receipts = screen.getByLabelText('限速执行结果').textContent;
    expect(receipts).toContain('网卡 应用失败：IFB 不可用');
    expect(receipts).toContain('Xray 已限速 100 Mbps');
    expect(screen.getByRole('button', { name: '双层' }).hasAttribute('disabled')).toBe(true);
    expect(screen.queryByRole('button', { name: '添加规则' })).toBeNull();
    expect(screen.queryByRole('button', { name: '删除限速规则 1' })).toBeNull();
    expect(panel.querySelector('.config-panel-savebar')).toBeNull();
  });

  it('shows upgrade requirements and never presents a stale receipt as current success', () => {
    const rules = [{ threshold: { kind: 'quota_percent' as const, basis_points: 1000 }, rate_bps: 100_000_000 }];
    renderCard(traffic({}, { supported: false, policy: { enabled: true, layer: 'system', rules } }));
    expect(screen.getByRole('status').textContent).toBe('Agent 待升级');
    cleanup();

    const { panel } = renderCard(
      traffic(
        {},
        {
          revision: 2,
          policy: { enabled: true, layer: 'system', rules },
          observation: observed({
            revision: 1,
            phase: 'limited',
            rate_bps: 100_000_000,
            system: { phase: 'limited', rate_bps: 100_000_000, error: null },
          }),
          reported_at_unix_secs: nowSecs(),
        },
      ),
    );
    expect(screen.getByRole('status').textContent).toBe('等待应用确认');
    expect(panel.querySelector('.node-traffic-limit-chip')?.textContent).toBe('限速待确认');
    expect(panel.querySelector('.node-traffic-flag.on, .node-traffic-rule.on')).toBeNull();
  });

  it('waits for a disable receipt even when the previous error report lacked a rate', () => {
    renderCard(
      traffic(
        {},
        {
          revision: 2,
          observation: observed({ revision: 1, phase: 'error', interface: null, error: '计量不可用' }),
          reported_at_unix_secs: nowSecs(),
        },
      ),
    );
    expect(screen.getByRole('status').textContent).toBe('等待应用确认');
  });
});

describe('machine traffic limit policy conversion', () => {
  it('keeps decimal Mbps exact without floating point rejection', () => {
    const parsed = trafficLimitPolicy(
      { enabled: true, layer: 'application', rules: [{ amount: '1', unit: 'GiB', rate: '1.001', rateUnit: 'Mbps' }] },
      null,
    );
    expect(parsed?.rules[0].rate_bps).toBe(1_001_000);
  });

  it('rejects overflow, empty enabled policies, and rates below the supported minimum', () => {
    const rule = { amount: '18446744073709551616', unit: 'TiB' as const, rate: '100', rateUnit: 'Mbps' as const };
    expect(trafficLimitPolicy({ enabled: true, layer: 'system', rules: [rule] }, null)).toBeNull();
    expect(trafficLimitPolicy({ enabled: true, layer: 'system', rules: [] }, null)).toBeNull();
    expect(
      trafficLimitPolicy(
        { enabled: true, layer: 'system', rules: [{ ...rule, amount: '1', rate: '0.001', rateUnit: 'Kbps' }] },
        null,
      ),
    ).toBeNull();
  });

  it('round-trips saved thresholds and rates exactly when they are left unchanged', () => {
    const policy = {
      enabled: true,
      layer: 'application' as const,
      rules: [
        { threshold: { kind: 'absolute' as const, bytes: String(GIB + 7) }, rate_bps: 1_234_567 },
        { threshold: { kind: 'quota_percent' as const, basis_points: 9050 }, rate_bps: 1_001_000 },
      ],
    };
    const form = trafficLimitForm(policy, String(TIB));
    expect(form.rules.map(rule => [rule.amount, rule.unit, rule.rate, rule.rateUnit])).toEqual([
      ['1', 'GiB', '1234.567', 'Kbps'],
      ['90.5', '%', '1.001', 'Mbps'],
    ]);
    expect(trafficLimitPolicy(form, String(TIB))).toEqual(policy);
  });
});
