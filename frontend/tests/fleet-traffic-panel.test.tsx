import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, fireEvent, render, waitFor, within } from '@testing-library/react';
import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest';

import type { FleetTrafficSnapshot } from '../src/fleet-traffic-source';

let FleetTrafficTab: typeof import('../src/panes/fleet-traffic').FleetTrafficTab;

beforeAll(async () => {
  window.matchMedia = ((media: string) => ({
    matches: false,
    media,
    addEventListener() {},
    removeEventListener() {},
  })) as unknown as typeof window.matchMedia;
  ({ FleetTrafficTab } = await import('../src/panes/fleet-traffic'));
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

const MB = 1_000_000 / 8;

/** Two regions, three machines: Hong Kong carries alice, Tokyo is idle, Taipei lost its Agent. */
function snapshot(enabled = true): FleetTrafficSnapshot {
  const now = Date.now();
  const node = (node_id: string, users: { user_id: string; down: number; up: number }[], connected = true) => ({
    node_id,
    connected,
    active: connected,
    interval_secs: 1,
    samples: [
      {
        node_id,
        received_at_unix_millis: connected ? now : now - 60_000,
        sample: {
          sampled_at_unix_millis: now,
          rx_bytes_per_sec: 50 * MB,
          tx_bytes_per_sec: 52 * MB,
          user_traffic: {
            state: 'ready' as const,
            all_users: true,
            truncated: false,
            has_gap: false,
            users: users.map(user => ({
              tenant_id: 'platform',
              user_id: user.user_id,
              downlink_bytes_per_sec: user.down,
              uplink_bytes_per_sec: user.up,
            })),
          },
        },
      },
    ],
  });
  return {
    policy: { enabled },
    nodes: [node('hk', [{ user_id: 'alice', down: 24 * MB, up: 1 * MB }]), node('jp', []), node('tw', [], false)],
    expected: [
      {
        node_id: 'hk',
        users: [
          { tenant_id: 'platform', user_id: 'alice' },
          { tenant_id: 'platform', user_id: 'bob' },
        ],
      },
      { node_id: 'jp', users: [{ tenant_id: 'platform', user_id: 'bob' }] },
      { node_id: 'tw', users: [{ tenant_id: 'platform', user_id: 'alice' }] },
    ],
  };
}

function serve(payload: FleetTrafficSnapshot, deferred?: { path: string; until: Promise<void> }) {
  vi.stubGlobal('EventSource', undefined);
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input: RequestInfo | URL) => {
      const path = new URL(String(input), 'http://console.test').pathname;
      if (path === deferred?.path) await deferred.until;
      const body =
        path === '/nodes/agent-state'
          ? {
              nodes: [
                { node_id: 'hk', name: '香港 HKIX', lifecycle_phase: 'active', public_ipv4_country: 'HK' },
                { node_id: 'jp', name: '东京 IIJ', lifecycle_phase: 'active', public_ipv4_country: 'JP' },
                { node_id: 'tw', name: '台北 家宽', lifecycle_phase: 'active', public_ipv4_country: 'TW' },
              ],
            }
          : path === '/users'
            ? {
                users: ['alice', 'bob'].map(id => ({ id, tenant_id: 'platform', status: 'active', created_at: '' })),
              }
            : path === '/realtime/fleet-traffic/snapshot'
              ? payload
              : null;
      return body
        ? new Response(JSON.stringify(body), { status: 200, headers: { 'content-type': 'application/json' } })
        : new Response('{}', { status: 404 });
    }),
  );
}

function mount(allowMachineDetails = true) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, refetchOnWindowFocus: false } } });
  return render(
    <QueryClientProvider client={client}>
      <FleetTrafficTab allowMachineDetails={allowMachineDetails} />
    </QueryClientProvider>,
  );
}

const readingOf = (ledger: HTMLElement, label: string) =>
  within(ledger).getByText(label, { selector: 'dt' }).parentElement?.querySelector('dd')?.textContent;

describe('fleet realtime traffic panel', () => {
  it.each([true, false])(
    'hides owner text and tooltips while keeping same-name users separate (details=%s)',
    async allowDetails => {
      const payload = snapshot();
      payload.nodes[0].samples[0].sample.user_traffic!.users!.push({
        tenant_id: 'hidden-branch',
        user_id: 'alice',
        downlink_bytes_per_sec: 3 * MB,
        uplink_bytes_per_sec: MB,
      });
      payload.expected[0].users.push({ tenant_id: 'hidden-branch', user_id: 'alice' });
      serve(payload);
      const view = mount(allowDetails);
      const table = await view.findByRole('table');
      const identities = [...table.querySelectorAll('tbody tr[data-user]')].map(row => row.getAttribute('data-user'));
      expect(identities).toContain('platform/alice');
      expect(identities).toContain('hidden-branch/alice');
      expect(view.container.textContent).not.toMatch(/platform|hidden-branch/);
      for (const element of view.container.querySelectorAll('[title], [aria-label]')) {
        expect(`${element.getAttribute('title')} ${element.getAttribute('aria-label')}`).not.toMatch(
          /platform|hidden-branch/,
        );
      }
      expect(view.container.querySelector('.flt-utenant')).toBeNull();
    },
  );
  it('opens machine usage in place from the column header and matrix cells without a separate picker', async () => {
    const route = await import('../src/forge/route');
    const navigate = vi.spyOn(route, 'navigate').mockReturnValue(true);
    serve(snapshot());
    const view = mount();
    const table = await view.findByRole('table');
    const header = within(table).getByRole('button', { name: '香港 HKIX，查看机器用量' });
    expect(view.queryByRole('combobox', { name: '选择机器查看用量' })).toBeNull();
    expect(header.getAttribute('aria-haspopup')).toBe('dialog');
    expect(vi.mocked(fetch).mock.calls.some(([url]) => String(url).includes('/model/snapshot'))).toBe(false);
    header.focus();
    fireEvent.click(header);
    await view.findByRole('dialog', { name: '香港 HKIX · 用量' });
    expect(navigate).not.toHaveBeenCalled();
    expect(view.getByRole('table')).toBe(table);
    fireEvent.click(view.getByRole('button', { name: '关闭机器用量' }));
    await waitFor(() => expect(view.queryByRole('dialog', { hidden: true })).toBeNull());
    await waitFor(() => expect(document.activeElement).toBe(header));
    fireEvent.click(within(table).getByRole('button', { name: /alice 在 香港 HKIX.*查看机器用量/ }));
    await view.findByRole('dialog', { name: '香港 HKIX · 用量' });
    expect(navigate).not.toHaveBeenCalled();
    expect(view.getByRole('table')).toBe(table);
  });

  it('leads with the fleet sum and keeps coverage beside it', async () => {
    serve(snapshot());
    const view = mount();

    const ledger = await view.findByLabelText('机队当前速率');
    expect(ledger.querySelector('.user-usage-hero-value')?.textContent).toBe('25.0 Mbit/s');
    expect(ledger.querySelector('.user-usage-hero-label')?.getAttribute('title')).toContain('各机独立采样');
    expect(ledger.querySelector('.rut-hero-sub')?.textContent).toBe('1 位活跃 · 2 位已授权');
    expect(readingOf(ledger, '最高用户')).toBe('alice · 25.0 Mbit/s');
    expect(readingOf(ledger, '最忙机器')).toBe('香港 HKIX · 25.0 Mbit/s');
    // NIC readings come only from machines whose stream is current; Taipei's last frame expired.
    expect(readingOf(ledger, '网卡收 / 发')).toBe('100 Mbit/s / 104 Mbit/s');
    expect(readingOf(ledger, '有读数')).toBe('2 / 3 台');
    expect(view.getByRole('status').textContent).toBe('实时 · 每秒更新');
    // Narrow ledgers wrap these values; each reading stays whole so a unit never starts a line.
    const unbroken = (label: string) =>
      Array.from(
        within(ledger).getByText(label, { selector: 'dt' }).parentElement!.querySelectorAll('dd .usage-nobreak'),
        segment => segment.textContent,
      );
    expect(unbroken('最高用户')).toEqual(['25.0 Mbit/s']);
    expect(unbroken('最忙机器')).toEqual(['25.0 Mbit/s']);
    expect(unbroken('网卡收 / 发')).toEqual(['100 Mbit/s', '104 Mbit/s']);
  });

  it('lays the fleet out as a user × machine table whose cells say what was measured', async () => {
    serve(snapshot());
    const view = mount();

    const table = await view.findByRole('table');
    const columns = within(table)
      .getAllByRole('columnheader')
      .map(header => header.textContent);
    expect(columns).toEqual(['用户', '香港 HKIX', '东京 IIJ', '台北 家宽', '合计']);
    // Regions run in groups; each region's first column carries its flag.
    expect(table.querySelectorAll('.flt-mflag')).toHaveLength(3);
    expect(table.querySelector('th[data-node="tw"]')?.classList.contains('dim')).toBe(true);

    const alice = within(table).getByRole('rowheader', { name: /alice/ }).closest('tr')!;
    const bob = within(table).getByRole('rowheader', { name: /bob/ }).closest('tr')!;
    const cell = (row: HTMLElement, node: string) => row.querySelector(`td[data-node="${node}"]`)!;
    const said = (row: HTMLElement, node: string) => cell(row, node).querySelector('.flt-sr')?.textContent;
    const mark = (row: HTMLElement, node: string) => cell(row, node).querySelector('.flt-cell')?.classList;
    expect(said(alice, 'hk')).toBe('下行 24.0 Mbit/s · 上行 1.00 Mbit/s');
    expect(mark(alice, 'hk')?.contains('l3')).toBe(true);
    // Three machines leave room to print the reading next to the swatch.
    expect(cell(alice, 'hk').querySelector('.flt-cv')?.textContent).toBe('25.0 Mbit/s');
    expect(said(alice, 'jp')).toBe('未授权到这台机器');
    expect(mark(alice, 'jp')?.contains('na')).toBe(true);
    expect(said(alice, 'tw')).toBe('实时通道未连接');
    expect(mark(alice, 'tw')?.contains('unknown')).toBe(true);
    expect(said(bob, 'jp')).toBe('已采集，当前没有流量');
    expect(mark(bob, 'jp')?.contains('idle')).toBe(true);
    expect(alice.querySelector('td.flt-sum')?.textContent).toBe('25.0 Mbit/s');
    expect(bob.querySelector('td.flt-sum')?.textContent).toBe('0');
    // The avatar is not the name block; its own grid centering must survive the row header.
    expect(alice.querySelector('.flt-ubtn > .user-avatar')).toBeTruthy();
    expect(alice.querySelector('.flt-ubtn > .flt-uname')?.textContent).toBe('alice');
  });

  it('reads out the focused cell, user or machine and falls back to the strongest flow', async () => {
    serve(snapshot());
    const view = mount();
    const table = await view.findByRole('table');
    const readout = view.container.querySelector('.flt-readout')!;

    expect(readout.textContent).toBe('当前最高 · alice → 香港 HKIX · 下行 24.0 Mbit/s · 上行 1.00 Mbit/s');

    const bob = within(table).getByRole('button', { name: 'bob' });
    fireEvent.focus(bob);
    expect(readout.textContent).toBe('bob · 当前空闲 · 0 台有流量 / 2 台已授权');
    fireEvent.blur(bob);
    expect(readout.textContent).toContain('当前最高');

    fireEvent.focus(within(table).getByRole('button', { name: '台北 家宽，实时通道未连接，查看机器用量' }));
    expect(readout.textContent).toBe('台北 家宽 · 实时通道未连接');

    fireEvent.pointerOver(table.querySelector('tr[data-user="platform/bob"] td[data-node="hk"]')!);
    expect(readout.textContent).toBe('bob → 香港 HKIX · 已采集，当前没有流量');
  });

  it('turns into a per-user flow list on narrow screens', async () => {
    serve(snapshot());
    const view = mount();

    const list = await view.findByLabelText('按用户的当前速率');
    expect(within(list).getAllByRole('listitem')).toHaveLength(1);
    expect(within(list).getByRole('button', { name: /alice/ }).textContent).toContain('香港 HKIX 25.0 Mbit/s');
    const rest = [...list.querySelectorAll('.flt-list-rest')].map(line => line.textContent);
    expect(rest).toEqual(['空闲 1 位bob', '读数不完整 1 台台北 家宽（实时通道未连接）']);
  });

  it('groups initial user-rate loading as connecting and clears it when reports arrive', async () => {
    const payload = snapshot();
    delete payload.nodes[0].samples[0].sample.user_traffic;
    payload.nodes[1].samples[0].sample.user_traffic!.state = 'warming';
    payload.nodes[2].samples = [];
    serve(payload);
    const view = mount();

    const list = await view.findByLabelText('按用户的当前速率');
    expect(view.getByRole('status').textContent).toBe('连接中');
    expect(within(list).getByText('正在等待速率数据')).toBeTruthy();
    expect(list.querySelector('.flt-list-rest')?.textContent).toBe('连接中：香港 HKIX、东京 IIJ、台北 家宽');
    expect(within(list).queryByText(/读数不完整|尚未支持用户速率|空闲/)).toBeNull();
    expect(view.queryByRole('alert')).toBeNull();

    Object.assign(payload, snapshot());
    await waitFor(() => expect(view.getByRole('status').textContent).toBe('实时 · 每秒更新'), {
      timeout: 4_000,
    });
    expect(within(list).queryByText('连接中：')).toBeNull();
    expect(within(list).getByText('空闲 1 位')).toBeTruthy();
  });

  it.each(['/nodes/agent-state', '/users'])(
    'shows the first reading while other machines are warming and %s is still loading',
    async path => {
      let finish!: () => void;
      const until = new Promise<void>(resolve => {
        finish = resolve;
      });
      const payload = snapshot();
      payload.nodes[1].samples[0].sample.user_traffic!.state = 'warming';
      payload.nodes[2].connected = true;
      payload.nodes[2].active = true;
      payload.nodes[2].samples = [];
      serve(payload, { path, until });
      const view = mount();

      const ledger = await view.findByLabelText('机队当前速率');
      expect(ledger.querySelector('.user-usage-hero-value')?.textContent).toBe('25.0 Mbit/s');
      expect(readingOf(ledger, '有读数')).toBe('1 / 3 台');
      expect(view.getByRole('status').textContent).toBe('实时 · 每秒更新');
      expect(view.queryByRole('status', { name: '读取实时用量' })).toBeNull();
      const table = view.getByRole('table');
      const bob = within(table).getByRole('rowheader', { name: /bob/ }).closest('tr')!;
      expect(bob.querySelector('td[data-node="jp"] .flt-sr')?.textContent).toBe('正在建立速率基线');
      expect(bob.querySelector('td[data-node="jp"] .flt-cell')?.classList.contains('unknown')).toBe(true);
      await act(async () => finish());
      await view.findByRole('button', { name: '香港 HKIX，查看机器用量' });
      expect(view.getByRole('table')).toBe(table);
      expect(view.getByLabelText('机队当前速率')).toBe(ledger);
    },
  );

  it('keeps realtime readings visible when the machine roster fails', async () => {
    serve(snapshot());
    const fetchSnapshot = vi.mocked(fetch).getMockImplementation()!;
    vi.mocked(fetch).mockImplementation((input, init) => {
      if (String(input) === '/nodes/agent-state') {
        return Promise.resolve(Response.json({ error: '机器名册读取失败' }, { status: 503 }));
      }
      return fetchSnapshot(input, init);
    });
    const view = mount();

    const ledger = await view.findByLabelText('机队当前速率');
    await view.findByText(/机器名册读取失败/);
    expect(ledger.querySelector('.user-usage-hero-value')?.textContent).toBe('25.0 Mbit/s');
    expect(view.getByRole('table')).toBeTruthy();
    expect(view.queryByRole('status', { name: '读取实时用量' })).toBeNull();
  });

  it('keeps a large connecting fleet concise instead of repeating one message for every machine', async () => {
    const payload = snapshot();
    const extra = Array.from({ length: 22 }, (_, index) => ({
      node_id: `extra-${String(index).padStart(2, '0')}`,
      connected: false,
      active: false,
      interval_secs: 1,
      samples: [],
    }));
    payload.nodes = [...payload.nodes.map(node => ({ ...node, samples: [] })), ...extra];
    payload.expected = payload.nodes.map(node => ({
      node_id: node.node_id,
      users: [{ tenant_id: 'platform', user_id: 'alice' }],
    }));
    serve(payload);
    const view = mount();

    const list = await view.findByLabelText('按用户的当前速率');
    const line = list.querySelector('.flt-list-rest')!;
    expect(line.textContent).toMatch(/^连接中：.* 等 25 台$/);
    expect(line.querySelector('span')?.title.split('、')).toHaveLength(25);
    expect(line.textContent).not.toContain('extra-21');
    expect(within(list).queryByText(/读数不完整|尚未支持用户速率/)).toBeNull();
  });

  it('shows public viewers the complete matrix without any detail action', async () => {
    serve(snapshot());
    const visitor = mount(false);
    const table = await visitor.findByRole('table');
    expect(visitor.getByLabelText('机队当前速率').textContent).toContain('25.0 Mbit/s');
    expect(within(table).queryByRole('button', { name: /查看机器用量/ })).toBeNull();
    expect(visitor.queryByRole('combobox', { name: '选择机器查看用量' })).toBeNull();
    expect(table.querySelector('th[data-node="hk"] .flt-mlabel')?.tagName).toBe('SPAN');
    fireEvent.click(table.querySelector('tr[data-user="platform/alice"] td[data-node="hk"]')!);
    expect(visitor.queryByRole('dialog')).toBeNull();
    expect(vi.mocked(fetch).mock.calls.some(([input]) => String(input) === '/realtime/fleet-traffic/snapshot')).toBe(
      true,
    );
    visitor.unmount();

    serve(snapshot(false));
    const disabled = mount();
    await waitFor(() => expect(disabled.getByText('实时观测已在设置中关闭。')).toBeTruthy());
    expect(disabled.getByRole('status').textContent).toBe('实时观测已关闭');
    expect(disabled.queryByRole('table')).toBeNull();
  });
});
