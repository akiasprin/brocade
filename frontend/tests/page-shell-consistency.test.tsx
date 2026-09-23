import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, render } from '@testing-library/react';
import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest';
import type { HopLinkView } from '../src/api';
import { SessionProvider } from '../src/session';

const initial = { node_count: 0, chain_group_count: [] };

vi.mock('echarts/core', () => ({
  use: vi.fn(),
  connect: vi.fn(),
  init: vi.fn(() => ({ setOption: vi.fn(), resize: vi.fn(), dispose: vi.fn() })),
}));
vi.mock('echarts/charts', () => ({ LineChart: {} }));
vi.mock('echarts/components', () => ({ GridComponent: {}, MarkLineComponent: {}, TooltipComponent: {} }));
vi.mock('echarts/renderers', () => ({ CanvasRenderer: {} }));

let HopLinkTable: typeof import('../src/panes/telemetry').HopLinkTable;
let PasswordPane: typeof import('../src/panes/password').PasswordPane;

beforeAll(async () => {
  window.matchMedia = ((media: string) => ({
    matches: false,
    media,
    addEventListener() {},
    removeEventListener() {},
  })) as unknown as typeof window.matchMedia;
  ({ HopLinkTable } = await import('../src/panes/telemetry'));
  ({ PasswordPane } = await import('../src/panes/password'));
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe('page shell consistency', () => {
  it('gives the password page the same titled page panel as the primary lists', () => {
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const view = render(
      <QueryClientProvider client={queryClient}>
        <SessionProvider
          value={{
            initial,
            who: {
              operator_id: 'operator',
              role: 'editor',
              tenant_scope: null,
              token_prefix: null,
              masked_assets: false,
            },
          }}
        >
          <PasswordPane />
        </SessionProvider>
      </QueryClientProvider>,
    );

    const title = view.getByRole('heading', { name: '改密码' });
    expect(title.closest('.panel.titled')?.getAttribute('data-page-title')).toBe('true');
    expect(title.parentElement?.querySelector('.list-ico')).toBeTruthy();
  });

  it('renders grouped hop rows in a titled panel without React key warnings', () => {
    const hop: HopLinkView = {
      node_id: 'taipei',
      cc_algo: 'bbr',
      sample: {
        chain_id: 'primary',
        peer_node_id: 'tokyo',
        window_start_unix_secs: 100,
        window_end_unix_secs: 130,
        conns: 2,
        conns_measured: 2,
        btlbw_p50_bps: 1_000_000,
        btlbw_p90_bps: 1_200_000,
        min_rtt_us: 10_000,
        rtt_p50_us: 12_000,
        rtt_p90_us: 14_000,
        retrans_pct: 0,
        busy_pct: 10,
        rwnd_limited_pct: 0,
        sndbuf_limited_pct: 0,
      },
    };
    const consoleError = vi.spyOn(console, 'error').mockImplementation(() => undefined);

    const view = render(<HopLinkTable hops={[hop]} nodeName={id => id} title="逐跳质量" />);

    expect(view.getByRole('heading', { name: '逐跳质量' }).closest('.panel.titled')).toBeTruthy();
    expect(consoleError.mock.calls.flat().join(' ')).not.toContain('unique "key"');
  });
});
