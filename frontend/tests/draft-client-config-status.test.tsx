import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { draft } from '../src/draft';
import { DraftBar } from '../src/forge/draft-bar';
import { SessionProvider } from '../src/session';

function mount() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={client}>
      <SessionProvider
        value={{
          who: {
            operator_id: 'test-editor',
            role: 'editor',
            tenant_scope: null,
            token_prefix: null,
            masked_assets: false,
          },
        }}
      >
        <DraftBar current={6} />
      </SessionProvider>
    </QueryClientProvider>,
  );
}

afterEach(() => {
  cleanup();
  draft.clear();
  vi.unstubAllGlobals();
});

describe('draft submission result', () => {
  it.each([
    ['unchanged', []],
    ['activated', []],
    ['activated', ['ingress:i-main:projection']],
    ['awaiting-first-topology', []],
  ])('stays silent when client config is %s', async (status, pendingTopology) => {
    draft.push({ op: 'upsert_app', app: { id: 'app-main', label: 'Main' } });
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue({
        ok: true,
        status: 200,
        json: async () => ({
          revision_id: 7,
          changed: 1,
          client_config: {
            snapshot_id: 4,
            status,
            serving_generation: 12,
            pending_topology: pendingTopology,
          },
        }),
      }),
    );
    const view = mount();

    fireEvent.click(view.getByRole('button', { name: '提交' }));

    await waitFor(() => expect(view.queryByRole('button', { name: '提交' })).toBeNull());
    expect(view.queryByText(/已提交修订|客户端配置|节点产物变化|拓扑发布/)).toBeNull();
  });
});
