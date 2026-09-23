import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { draft } from '../src/draft';
import { ChainSubscriptionCountryRow, ChainTitle, subscriptionFlag } from '../src/panes/chains';
import type { E2eProbeItem, SnapshotChain } from '../src/api';

const chain = (country: string | null = null): SnapshotChain => ({
  id: 'c-tw',
  tenant: 'platform.acme',
  name: '台北直连',
  subscription_country: country,
});

const probe = (country: string): E2eProbeItem => ({
  app_id: 'app-main',
  chain_id: 'c-tw',
  chain_name: '台北直连',
  node_id: 'tw-1',
  status: 'ok',
  ttfb_ms: 30,
  exit_ip: '203.0.113.8',
  exit_loc: country,
  exit_verdict: 'match',
  detail: null,
  probed_at: '2026-08-30T12:00:00Z',
  samples: { probed_at_unix_secs: [], status: [], ttfb_ms: [] },
});

const writes: { path: string; body: unknown }[] = [];

const mount = (value: SnapshotChain, observed?: E2eProbeItem, editable = true) => {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  return render(
    <QueryClientProvider client={client}>
      <dl>
        <ChainSubscriptionCountryRow appId="app-main" chain={value} probe={observed} editable={editable} />
      </dl>
    </QueryClientProvider>,
  );
};

afterEach(() => {
  cleanup();
  draft.clear();
  vi.unstubAllGlobals();
});

beforeEach(() => {
  draft.init(`chain-country-${Math.random()}`);
  draft.clear();
  writes.length = 0;
  vi.stubGlobal(
    'fetch',
    vi.fn(async (path: string, init?: RequestInit) => {
      writes.push({ path, body: init?.body ? JSON.parse(String(init.body)) : null });
      return Response.json({ revision_id: writes.length });
    }),
  );
});

describe('chain subscription country', () => {
  it('turns an ISO code into the regional-indicator flag used by subscription names', () => {
    expect(subscriptionFlag('TW')).toBe('🇹🇼');
    expect(subscriptionFlag('tw')).toBe('');
    expect(subscriptionFlag('ZZ')).toBe('');
  });

  it('immediately writes the explicit country while preserving the rest of the chain upsert', async () => {
    const view = mount(chain());
    expect(view.getByRole('option', { name: '🇹🇼 TW · 台湾' })).toBeTruthy();
    fireEvent.change(view.getByRole('combobox', { name: '出口地区标识' }), { target: { value: 'TW' } });

    await waitFor(() => expect(writes).toHaveLength(1));
    expect(writes[0]).toEqual({
      path: '/apps/app-main/chains',
      body: {
        id: 'c-tw',
        tenant_id: 'platform.acme',
        name: '台北直连',
        subscription_country: 'TW',
      },
    });
    expect(draft.ops()).toEqual([]);
  });

  it('preserves the configured country when renaming a chain', async () => {
    const client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
    const view = render(
      <QueryClientProvider client={client}>
        <ChainTitle appId="app-main" chain={chain('TW')} editable />
      </QueryClientProvider>,
    );

    expect(view.getByRole('img', { name: 'TW 地区旗' })).toBeTruthy();
    fireEvent.click(view.getByRole('button', { name: '台北直连' }));
    const input = view.getByRole('textbox', { name: '链名' });
    expect((input as HTMLInputElement).value).toBe('台北直连');
    expect(view.getByRole('img', { name: 'TW 地区旗' })).toBeTruthy();
    fireEvent.change(input, { target: { value: '台湾高速' } });
    fireEvent.keyDown(input, { key: 'Enter' });

    await waitFor(() => expect(writes).toHaveLength(1));
    expect(writes[0]).toEqual({
      path: '/apps/app-main/chains',
      body: {
        id: 'c-tw',
        tenant_id: 'platform.acme',
        name: '台湾高速',
        subscription_country: 'TW',
      },
    });
    expect(draft.ops()).toEqual([]);
  });

  it('automatically adopts a successful E2E country when no region is configured', async () => {
    const view = mount(chain(), probe('TW'));

    await waitFor(() => expect(writes).toHaveLength(1));
    expect(writes[0]).toMatchObject({ body: { subscription_country: 'TW' } });
    expect((view.getByRole('combobox', { name: '出口地区标识' }) as HTMLSelectElement).value).toBe('TW');
    expect(view.queryByRole('button', { name: '采用当前出口 TW' })).toBeNull();
    expect(view.getByText('🇹🇼台北直连')).toBeTruthy();
    expect(draft.ops()).toEqual([]);
    expect(subscriptionFlag('TW')).toBe('🇹🇼');
  });

  it('does not adopt an unknown probe location and uses region terminology', async () => {
    const view = mount(chain(), probe('ZZ'));
    expect(view.getByRole('option', { name: '不显示地区标识' })).toBeTruthy();
    expect(view.queryByText(/国旗/)).toBeNull();
    await waitFor(() => expect(writes).toHaveLength(0));
  });

  it('shows configured metadata without an editor to readonly users', () => {
    const view = mount(chain('TW'), probe('JP'), false);
    expect(view.queryByRole('combobox', { name: '出口地区标识' })).toBeNull();
    expect(view.getByText('🇹🇼 TW · 台湾')).toBeTruthy();
    expect(view.getByText('🇹🇼台北直连')).toBeTruthy();
  });
});
