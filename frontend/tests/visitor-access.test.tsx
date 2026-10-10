import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest';
import { ApiError, type Whoami } from '../src/api';
import { routeForViewer } from '../src/forge/access';
import { ErrorBox } from '../src/ui/bits';

let useHopStats: typeof import('../src/panes/nodes').useHopStats;

const viewer = (role: Whoami['role'], operatorId: string = role): Whoami => ({
  operator_id: operatorId,
  role,
  tenant_scope: role === 'system-admin' ? null : 'platform',
  token_prefix: null,
  masked_assets: role === 'readonly' || role === 'user',
});

beforeAll(async () => {
  vi.stubGlobal(
    'matchMedia',
    vi.fn(() => ({
      matches: false,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
    })),
  );
  ({ useHopStats } = await import('../src/panes/nodes'));
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe('访客路由权限', () => {
  const guest = viewer('readonly', 'public');

  it.each([
    [{ nav: 'settings' as const }, { nav: 'nodes' as const }],
    [{ nav: 'deploy' as const, drill: { p: 'plan' } }, { nav: 'nodes' as const }],
    [{ nav: 'password' as const }, { nav: 'nodes' as const }],
    [{ nav: 'nodes' as const, drill: { p: 'provision' } }, { nav: 'nodes' as const }],
    [{ nav: 'nodes' as const, drill: { p: 'install', node: 'hk' } }, { nav: 'nodes' as const }],
    [{ nav: 'nodes' as const, drill: { p: 'chain', id: 'hk' } }, { nav: 'nodes' as const }],
    [{ nav: 'chains' as const, drill: { p: 'new', app: 'main' } }, { nav: 'chains' as const }],
  ])('在页面挂载前收回无权打开的深链 %#', (location, expected) => {
    expect(routeForViewer(location, guest)).toEqual(expected);
  });

  it('保留访客可读取的机器、线路和用户详情', () => {
    for (const location of [
      { nav: 'nodes' as const, drill: { p: 'node', id: 'hk' } },
      { nav: 'chains' as const, drill: { p: 'chain', app: 'main', chain: 'hk-jp' } },
      { nav: 'users' as const, drill: { p: 'user', id: 'alice' } },
      { nav: 'tunnels' as const, drill: { p: 'warp', id: 'warp-1111-2222' } },
      { nav: 'usage' as const },
      { nav: 'topo' as const },
    ]) {
      expect(routeForViewer(location, guest)).toEqual(location);
    }
  });

  it('普通用户退回自己的用户页，系统管理员保留管理深链', () => {
    expect(routeForViewer({ nav: 'settings' }, viewer('user'))).toEqual({ nav: 'users' });
    const provision = { nav: 'nodes' as const, drill: { p: 'provision' } };
    expect(routeForViewer(provision, viewer('system-admin'))).toEqual(provision);
  });
});

describe('访客机器详情请求', () => {
  function HopHarness() {
    const state = useHopStats('hk', false);
    return <span>{`${state.reported}/${state.expected}`}</span>;
  }

  it('只读取公开链路健康，不尝试获取 Xray 产物', async () => {
    const request = vi.fn(async (input: RequestInfo | URL) => {
      const path = String(input);
      if (path === '/links/health') {
        return Response.json({
          hops: [
            {
              node_id: 'hk',
              chain_id: 'hk-jp',
              peer_node_id: 'jp',
              alive: true,
              downlink_bytes: 128,
              window_secs: 60,
              checked_at: '2026-10-10T00:00:00Z',
            },
          ],
        });
      }
      throw new Error(`访客不应请求 ${path}`);
    });
    vi.stubGlobal('fetch', request);
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    render(
      <QueryClientProvider client={client}>
        <HopHarness />
      </QueryClientProvider>,
    );

    await waitFor(() => expect(screen.getByText('1/1')).toBeTruthy());
    expect(request.mock.calls.map(call => String(call[0]))).toEqual(['/links/health']);
  });
});

it('权限拒绝使用面向用户的状态，不直出 forbidden', () => {
  render(<ErrorBox error={new ApiError(403, 'forbidden')} />);
  expect(screen.getByText('当前账号不能查看这项内容')).toBeTruthy();
  expect(screen.queryByText(/forbidden/i)).toBeNull();
});
