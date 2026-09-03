import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';
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
  samples: [],
});

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
});

describe('chain subscription country', () => {
  it('turns an ISO code into the regional-indicator flag used by subscription names', () => {
    expect(subscriptionFlag('TW')).toBe('🇹🇼');
    expect(subscriptionFlag('tw')).toBe('');
    expect(subscriptionFlag('ZZ')).toBe('');
  });

  it('writes the explicit country while preserving the rest of the chain upsert', async () => {
    const view = mount(chain());
    expect(view.getByRole('option', { name: '🇹🇼 TW · 台湾' })).toBeTruthy();
    fireEvent.change(view.getByRole('combobox', { name: '出口地区标识' }), { target: { value: 'TW' } });

    await waitFor(() =>
      expect(draft.ops()).toEqual([
        {
          op: 'upsert_chain',
          app_id: 'app-main',
          chain: {
            id: 'c-tw',
            tenant_id: 'platform.acme',
            name: '台北直连',
            subscription_country: 'TW',
          },
        },
      ]),
    );
  });

  it('preserves the configured country when renaming a chain', async () => {
    const client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
    const view = render(
      <QueryClientProvider client={client}>
        <ChainTitle appId="app-main" chain={chain('TW')} editable />
      </QueryClientProvider>,
    );

    fireEvent.click(view.getByRole('button', { name: '台北直连' }));
    const input = view.getByRole('textbox', { name: '链名' });
    fireEvent.change(input, { target: { value: '台湾高速' } });
    fireEvent.keyDown(input, { key: 'Enter' });

    await waitFor(() =>
      expect(draft.ops()).toEqual([
        {
          op: 'upsert_chain',
          app_id: 'app-main',
          chain: {
            id: 'c-tw',
            tenant_id: 'platform.acme',
            name: '台湾高速',
            subscription_country: 'TW',
          },
        },
      ]),
    );
  });

  it('offers a successful E2E country as an explicit choice and previews the flag', async () => {
    const view = mount(chain(), probe('TW'));
    fireEvent.click(view.getByRole('button', { name: '采用当前出口 TW' }));

    await waitFor(() => expect(draft.ops()[0]).toMatchObject({ chain: { subscription_country: 'TW' } }));
    expect(subscriptionFlag('TW')).toBe('🇹🇼');
  });

  it('shows configured metadata without an editor to readonly users', () => {
    const view = mount(chain('TW'), probe('JP'), false);
    expect(view.queryByRole('combobox', { name: '出口地区标识' })).toBeNull();
    expect(view.getByText('🇹🇼 TW · 台湾')).toBeTruthy();
    expect(view.getByText('🇹🇼台北直连')).toBeTruthy();
  });
});
