import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { UserListItem, UserTrafficPolicyView } from '../src/api';
import { fetchUserTrafficPolicies, saveUserTrafficPolicy } from '../src/api';
import { QuotaRow } from '../src/panes/users';
import { SessionProvider } from '../src/session';
import { userTrafficPolicyKey } from '../src/panes/user-traffic-policy';
const user = {
  tenant_id: 'platform',
  id: 'alice',
  status: 'active',
  created_at: '',
  created_revision: null,
} as UserListItem;
const views = (
  policy: UserTrafficPolicyView['policy'] = null,
  extra: Partial<UserTrafficPolicyView> = {},
): UserTrafficPolicyView[] =>
  ['access', 'forwarding'].map(kind => ({
    kind: kind as 'access' | 'forwarding',
    app_id: kind === 'access' ? 'app-main' : null,
    policy,
    used_bytes: '0',
    exhausted: false,
    service_available: true,
    unsupported_nodes: [],
    base_rate_supported: true,
    ...extra,
  }));
const clients: QueryClient[] = [];
beforeEach(() => {
  vi.stubGlobal(
    'matchMedia',
    vi.fn(() => ({ matches: false, addEventListener: vi.fn(), removeEventListener: vi.fn() })),
  );
  vi.stubGlobal(
    'confirm',
    vi.fn(() => true),
  );
});
afterEach(() => {
  cleanup();
  clients.splice(0).forEach(client => client.clear());
  vi.unstubAllGlobals();
});
function mount(data = views(), _kind: 'access' = 'access', editable = true) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity }, mutations: { retry: false } },
  });
  clients.push(client);
  client.setQueryData(userTrafficPolicyKey(user), data);
  return render(
    <QueryClientProvider client={client}>
      <SessionProvider
        value={{
          who: {
            operator_id: 'operator',
            role: 'system-admin',
            tenant_scope: null,
            token_prefix: null,
            masked_assets: false,
          },
          initial: { node_count: 1, chain_group_count: [] },
        }}
      >
        {data
          .filter(row => row.kind === 'access')
          .map(row => (
            <QuotaRow
              key={row.app_id}
              app={{
                id: row.app_id!,
                label: row.app_id === 'app-main' ? 'Main' : 'Second',
                chains: [],
                steps: [],
                ingresses: [],
                fronts: [],
                grants: [],
              }}
              user={user}
              editable={editable}
              policy={row}
              used={Number(row.used_bytes)}
              limit={row.policy?.quota_bytes ? Number(row.policy.quota_bytes) : null}
              over={row.exhausted}
            />
          ))}
      </SessionProvider>
    </QueryClientProvider>,
  );
}
describe('app access quota presentation', () => {
  it('ignores retired forwarding policies from older Consoles and cannot save them', async () => {
    const request = vi.fn().mockResolvedValue(Response.json(views()));
    vi.stubGlobal('fetch', request);
    const policies = await fetchUserTrafficPolicies(user.tenant_id, user.id);
    expect(policies.map(policy => policy.kind)).toEqual(['access']);
    expect(() =>
      saveUserTrafficPolicy(
        user.tenant_id,
        user.id,
        { kind: 'forwarding', app_id: null },
        {
          quota_bytes: '1024',
          exhausted_action: { kind: 'suspend' },
        },
      ),
    ).toThrow('接入额度必须指定 app');
    expect(request).toHaveBeenCalledTimes(1);
  });
  it('shows the exhaustion rule at the quota boundary without requiring interaction', () => {
    mount(views({ quota_bytes: '1024', exhausted_action: { kind: 'throttle', rate_bps: 5_000_000 } }), 'access', false);
    expect(screen.getByText('用满后限速 5 Mbps').closest('.qta-meter-run')).toBeTruthy();
    expect(screen.queryByRole('button')).toBeNull();
    expect(screen.queryByRole('dialog')).toBeNull();
  });
  it.each([
    { policy: null, extra: {}, description: null, tone: null },
    {
      policy: { quota_bytes: '1024', exhausted_action: { kind: 'suspend' as const } },
      extra: {},
      description: '用满后停用',
      tone: '',
    },
    {
      policy: { quota_bytes: '1024', exhausted_action: { kind: 'throttle' as const, rate_bps: 5_000_000 } },
      extra: { exhausted: true, used_bytes: '2048' },
      description: '限速中 · 5 Mbps',
      tone: 'warn',
    },
    {
      policy: { quota_bytes: '1024', exhausted_action: { kind: 'suspend' as const } },
      extra: { exhausted: true, used_bytes: '2048' },
      description: '已停用',
      tone: 'err',
    },
    { policy: null, extra: { service_available: false }, description: '服务暂停', tone: 'idle' },
    {
      policy: { quota_bytes: '1024', exhausted_action: { kind: 'throttle' as const, rate_bps: 5_000_000 } },
      extra: { exhausted: true, service_available: false },
      description: '服务暂停',
      tone: 'idle',
    },
  ])('shows the current state: $description', ({ policy, extra, description, tone }) => {
    const { container } = mount(views(policy, extra), 'access', false);
    if (description === null) expect(container.querySelector('.qta-policy-flag')).toBeNull();
    else expect(screen.getByText(description).className).toBe(`qta-policy-flag ${tone}`);
    expect(screen.queryByRole('button')).toBeNull();
  });
  it('shows synchronization with one activity indicator and keeps unlimited policies free of a track', () => {
    const { container } = mount(
      views(
        { quota_bytes: null, exhausted_action: { kind: 'throttle', rate_bps: 5_000_000 } },
        { pending_nodes: ['东京', '香港'] },
      ),
      'access',
      false,
    );
    expect(screen.getByText('同步中').title).toBe('等待 2 台机器同步');
    expect(container.querySelectorAll('.qta-policy-spin')).toHaveLength(1);
    expect(container.querySelector('.qta-meter')).toBeNull();
    expect(screen.queryByText(/用满后/)).toBeNull();
  });
  it('saves independent and post-exhaustion speeds only for the selected app', async () => {
    const fetcher = vi
      .fn()
      .mockResolvedValue(
        Response.json(
          views({ quota_bytes: String(500 * 1024 ** 3), exhausted_action: { kind: 'throttle', rate_bps: 5_000_000 } }),
        ),
      );
    vi.stubGlobal('fetch', fetcher);
    mount();
    fireEvent.click(screen.getByRole('button', { name: 'Main · 额度与限速' }));
    expect(screen.getByRole('form', { name: 'Main · 额度与限速' }).closest('.qta-r')).toBeTruthy();
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(screen.queryByLabelText('正常速度上限')).toBeNull();
    expect(screen.queryByLabelText('超额速度上限（Mbps）')).toBeNull();
    fireEvent.change(screen.getByLabelText('本期流量额度（GiB）'), { target: { value: '500' } });
    fireEvent.click(screen.getByRole('button', { name: '限速' }));
    fireEvent.change(screen.getByLabelText('超额速度上限（Mbps）'), { target: { value: '5' } });
    fireEvent.click(screen.getByRole('button', { name: '保存' }));
    await waitFor(() => expect(fetcher).toHaveBeenCalledTimes(1));
    const [url, init] = fetcher.mock.calls[0] as [string, RequestInit];
    expect(url).toBe('/users/platform/alice/traffic-policy/access/app-main');
    expect(JSON.parse(String(init.body))).toEqual({
      quota_bytes: String(500 * 1024 ** 3),
      base_rate_bps: null,
      exhausted_action: { kind: 'throttle', rate_bps: 5_000_000 },
    });
  });
  it('blocks unsupported nodes and invalid values, then permits suspend', () => {
    mount(views(null, { unsupported_nodes: ['东京'] }));
    fireEvent.click(screen.getByRole('button', { name: 'Main · 额度与限速' }));
    fireEvent.change(screen.getByLabelText('本期流量额度（GiB）'), { target: { value: '100' } });
    fireEvent.click(screen.getByRole('button', { name: '限速' }));
    expect(screen.getByRole('alert').textContent).toContain('东京');
    expect((screen.getByRole('button', { name: '保存' }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(screen.getByRole('button', { name: '停用' }));
    fireEvent.change(screen.getByLabelText('本期流量额度（GiB）'), { target: { value: '-1' } });
    expect((screen.getByRole('button', { name: '保存' }) as HTMLButtonElement).disabled).toBe(true);
  });
  it('retains entered values when saving fails', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(Response.json({ error: '保存失败' }, { status: 500 })));
    mount();
    fireEvent.click(screen.getByRole('button', { name: 'Main · 额度与限速' }));
    fireEvent.change(screen.getByLabelText('本期流量额度（GiB）'), { target: { value: '100' } });
    fireEvent.click(screen.getByRole('button', { name: '保存' }));
    await screen.findByText('保存失败');
    expect((screen.getByLabelText('本期流量额度（GiB）') as HTMLInputElement).value).toBe('100');
  });
  it('cancels in place without saving and returns focus to the row action', async () => {
    const fetcher = vi.fn();
    vi.stubGlobal('fetch', fetcher);
    mount(views({ quota_bytes: String(100 * 1024 ** 3), exhausted_action: { kind: 'suspend' } }));
    const edit = screen.getByRole('button', { name: 'Main · 额度与限速' });
    fireEvent.click(edit);
    fireEvent.change(screen.getByLabelText('本期流量额度（GiB）'), { target: { value: '120' } });
    fireEvent.click(screen.getByRole('button', { name: '取消' }));
    expect(screen.queryByRole('form')).toBeNull();
    expect(fetcher).not.toHaveBeenCalled();
    await waitFor(() => expect(document.activeElement).toBe(edit));
    fireEvent.click(edit);
    expect((screen.getByLabelText('本期流量额度（GiB）') as HTMLInputElement).value).toBe('100');
  });
  it('edits only the chosen app and keeps the other row unchanged', async () => {
    const first = views({ quota_bytes: String(100 * 1024 ** 3), exhausted_action: { kind: 'suspend' } });
    const second = {
      ...first[0],
      app_id: 'app-second',
      policy: { quota_bytes: String(50 * 1024 ** 3), exhausted_action: { kind: 'suspend' as const } },
    };
    const data = [first[0], second, first[1]];
    const fetcher = vi.fn().mockResolvedValue(Response.json(data));
    vi.stubGlobal('fetch', fetcher);
    mount(data);
    fireEvent.click(screen.getByRole('button', { name: 'Second · 额度与限速' }));
    expect(screen.getByRole('button', { name: 'Main · 额度与限速' }).getAttribute('aria-expanded')).toBe('false');
    expect((screen.getByLabelText('本期流量额度（GiB）') as HTMLInputElement).value).toBe('50');
    fireEvent.change(screen.getByLabelText('本期流量额度（GiB）'), { target: { value: '60' } });
    fireEvent.click(screen.getByRole('button', { name: '保存' }));
    await waitFor(() => expect(fetcher).toHaveBeenCalledTimes(1));
    expect(fetcher.mock.calls[0][0]).toBe('/users/platform/alice/traffic-policy/access/app-second');
    expect(JSON.parse(fetcher.mock.calls[0][1].body).quota_bytes).toBe(String(60 * 1024 ** 3));
  });
});

describe('independent speed units and compatibility', () => {
  it('saves a quota-free base rate in Kbps and keeps equivalent unit switches clean', async () => {
    const data = views({ quota_bytes: null, base_rate_bps: 1001, exhausted_action: { kind: 'suspend' } });
    const fetcher = vi
      .fn()
      .mockImplementation((_url, init) => Promise.resolve(Response.json(views(JSON.parse(init.body)))));
    vi.stubGlobal('fetch', fetcher);
    mount(data);
    expect(screen.getByText('常态限速 1.001 Kbps')).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: 'Main · 额度与限速' }));
    expect((screen.getByLabelText('常态速度单位') as HTMLSelectElement).value).toBe('Kbps');
    fireEvent.change(screen.getByLabelText('常态速度单位'), { target: { value: 'Mbps' } });
    expect((screen.getByLabelText('常态速度上限（Mbps）') as HTMLInputElement).value).toBe('0.001001');
    expect((screen.getByRole('button', { name: '保存' }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.change(screen.getByLabelText('常态速度单位'), { target: { value: 'Kbps' } });
    fireEvent.change(screen.getByLabelText('常态速度上限（Kbps）'), { target: { value: '512' } });
    expect(screen.queryByLabelText('超额速度单位')).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: '保存' }));
    await waitFor(() => expect(fetcher).toHaveBeenCalledTimes(1));
    expect(JSON.parse(fetcher.mock.calls[0][1].body)).toMatchObject({ quota_bytes: null, base_rate_bps: 512000 });
  });
  it('saves the exhausted cap in Kbps while the base cap remains in Mbps', async () => {
    const data = views({
      quota_bytes: String(100 * 1024 ** 3),
      base_rate_bps: 50000000,
      exhausted_action: { kind: 'throttle', rate_bps: 5000000 },
    });
    const fetcher = vi
      .fn()
      .mockImplementation((_url, init) => Promise.resolve(Response.json(views(JSON.parse(init.body)))));
    vi.stubGlobal('fetch', fetcher);
    mount(data);
    fireEvent.click(screen.getByRole('button', { name: 'Main · 额度与限速' }));
    fireEvent.change(screen.getByLabelText('超额速度单位'), { target: { value: 'Kbps' } });
    expect((screen.getByLabelText('超额速度上限（Kbps）') as HTMLInputElement).value).toBe('5000');
    fireEvent.change(screen.getByLabelText('超额速度上限（Kbps）'), { target: { value: '512' } });
    fireEvent.click(screen.getByRole('button', { name: '保存' }));
    await waitFor(() => expect(fetcher).toHaveBeenCalledTimes(1));
    expect(JSON.parse(fetcher.mock.calls[0][1].body)).toMatchObject({
      base_rate_bps: 50000000,
      exhausted_action: { kind: 'throttle', rate_bps: 512000 },
    });
  });
  it('disables the new control against an older Console and never submits a silently ignored cap', () => {
    mount(views(null, { base_rate_supported: false }));
    fireEvent.click(screen.getByRole('button', { name: 'Main · 额度与限速' }));
    expect((screen.getByLabelText('常态速度上限（Mbps）') as HTMLInputElement).disabled).toBe(true);
    expect(screen.getByText('请升级 Console，以支持常态限速。')).toBeTruthy();
    expect(() =>
      saveUserTrafficPolicy(
        'platform',
        'alice',
        { kind: 'access', app_id: 'app-main' },
        { quota_bytes: null, base_rate_bps: 1000, exhausted_action: { kind: 'suspend' } },
      ),
    ).toThrow('请先升级 Console');
  });
});

it('does not treat an ignored base-rate field in an old 200 response as successful', async () => {
  vi.stubGlobal('fetch', vi.fn().mockResolvedValue(Response.json(views(null, { base_rate_supported: false }))));
  await expect(
    saveUserTrafficPolicy(
      'platform',
      'alice',
      { kind: 'access', app_id: 'app-main', base_rate_supported: true },
      { quota_bytes: null, base_rate_bps: 512000, exhausted_action: { kind: 'suspend' } },
    ),
  ).rejects.toThrow('Console 未确认常态限速');
});
