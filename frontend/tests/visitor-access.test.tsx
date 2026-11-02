import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';
import { ApiError, type Whoami } from '../src/api';
import { routeForViewer } from '../src/forge/access';
import { ErrorBox } from '../src/ui/bits';

const viewer = (role: Whoami['role'], operatorId: string = role): Whoami => ({
  operator_id: operatorId,
  role,
  tenant_scope: role === 'system-admin' ? null : 'platform',
  token_prefix: null,
  masked_assets: role === 'readonly' || role === 'user',
});

afterEach(cleanup);

describe('访客路由权限', () => {
  const guest = viewer('readonly', 'public');

  it.each([
    [{ nav: 'settings' as const }, { nav: 'nodes' as const }],
    [{ nav: 'deploy' as const, drill: { p: 'plan' } }, { nav: 'nodes' as const }],
    [{ nav: 'password' as const }, { nav: 'nodes' as const }],
    [{ nav: 'capacity' as const }, { nav: 'nodes' as const }],
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
    expect(routeForViewer({ nav: 'capacity' }, viewer('user'))).toEqual({ nav: 'users' });
    expect(routeForViewer({ nav: 'capacity' }, viewer('system-admin'))).toEqual({ nav: 'capacity' });
    const provision = { nav: 'nodes' as const, drill: { p: 'provision' } };
    expect(routeForViewer(provision, viewer('system-admin'))).toEqual(provision);
  });
});

it('权限拒绝使用面向用户的状态，不直出 forbidden', () => {
  render(<ErrorBox error={new ApiError(403, 'forbidden')} />);
  expect(screen.getByText('当前账号不能查看这项内容')).toBeTruthy();
  expect(screen.queryByText(/forbidden/i)).toBeNull();
});
