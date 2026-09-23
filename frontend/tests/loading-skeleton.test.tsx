import { lazy, StrictMode, useState, type ComponentProps, type ComponentType } from 'react';
import { act, cleanup, render } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { paneLoadingRouteKey } from '../src/panes';
import { Loading as LoadingComponent, LoadingBoundary, type LoadingVariant } from '../src/ui/bits';

const Loading = (props: ComponentProps<typeof LoadingComponent>) => <LoadingComponent {...props} showSkeleton />;

afterEach(cleanup);

it('生产加载策略不渲染提示或骨架', () => {
  const view = render(<LoadingComponent variant="detail" />);
  expect(view.container.innerHTML).toBe('');
});

it('局部加载同样不渲染提示或占位内容', () => {
  const view = render(<LoadingComponent variant="chart" />);
  expect(view.container.innerHTML).toBe('');
});

it('API 查询读取边界渲染对应的骨架元素', () => {
  const variants: LoadingVariant[] = [
    'panel',
    'rows',
    'table',
    'nodes',
    'chains',
    'tunnels',
    'users',
    'usage',
    'settings',
    'detail',
    'chain-detail',
    'vpngate',
    'form',
    'plan',
    'deployment',
    'canvas',
  ];

  for (const variant of variants) {
    const view = render(<Loading variant={variant} rows={2} sheeted userDetail />);
    expect(view.getByRole('status').querySelector('.loading-skeleton')).toBeTruthy();
    cleanup();
  }
});

it('机器详情骨架表达完整的观测首屏', () => {
  const view = render(<Loading variant="detail" sheeted />);
  const status = view.getByRole('status');

  expect(status.querySelectorAll('.loading-skeleton-metric')).toHaveLength(6);
  expect(status.querySelectorAll('.loading-skeleton-observation-panel')).toHaveLength(2);
  expect(status.querySelectorAll('.loading-skeleton-observation-panel .loading-skeleton-chart')).toHaveLength(4);
  expect(status.querySelectorAll('.loading-skeleton-runtime')).toHaveLength(1);
  expect(status.querySelectorAll('.loading-skeleton-runtime-band')).toHaveLength(3);
  expect(status.querySelectorAll('.loading-skeleton-runtime-cells > span')).toHaveLength(19);
  expect(status.querySelectorAll('.loading-skeleton-tabs > i')).toHaveLength(1);
  expect(status.querySelector('.loading-skeleton-detail-columns')).toBeNull();
});

it('线路列表使用线路分组和卡片，不复用机器卡片', () => {
  const view = render(<Loading variant="chains" />);
  expect(view.container.querySelector('.chain-section > .chain-card-grid')).toBeTruthy();
  const cards = view.container.querySelectorAll('.chain-card');
  expect(cards.length).toBeGreaterThan(0);
  expect(view.container.querySelectorAll('.chain-card-path')).toHaveLength(cards.length);
  expect(view.container.querySelectorAll('.loading-skeleton-node-card')).toHaveLength(0);
  expect(view.container.querySelector('button, input, select, [tabindex]')).toBeNull();
});

it.each(['nodes', 'chains', 'tunnels', 'users'] as const)('主列表骨架不占位数量和状态汇总：%s', variant => {
  const view = render(<Loading variant={variant} />);
  const head = view.container.querySelector('.loading-skeleton-panel-head.page-title');

  expect(head?.querySelector('.loading-bar.hint')).toBeNull();
  expect(head?.querySelector('.loading-skeleton-head-stat')).toBeNull();
  expect(head?.querySelector('.loading-skeleton-head-action')).toBeTruthy();
});

it('用量骨架只占位摘要和每日流量', () => {
  const view = render(<Loading variant="usage" />);
  expect(view.container.querySelector('.loading-skeleton-usage-summary')).toBeTruthy();
  expect(view.container.querySelector('.loading-skeleton-framed-chart')).toBeTruthy();
  expect(view.container.querySelector('.loading-skeleton-usage-raw')).toBeNull();
});

it('VPN Gate 骨架保留地区栏和出口池工作区的主要边缘', () => {
  const view = render(<Loading variant="vpngate" sheeted />);
  expect(view.container.querySelector('.loading-skeleton-vpngate-rail')).toBeTruthy();
  expect(view.container.querySelector('.loading-skeleton-vpngate-work')).toBeTruthy();
  expect(view.container.querySelector('.loading-skeleton-vpngate-work .loading-skeleton-table')).toBeTruthy();
  expect(view.container.querySelectorAll('.loading-skeleton-tabs > i')).toHaveLength(2);
  expect(view.container.querySelector('button, input, select, summary, [tabindex]')).toBeNull();
});

it('首帧资料决定机器数以及每个线路分组的成员数', () => {
  const initial = {
    node_count: 2,
    chain_group_count: [
      ['app-a1b2', 3],
      ['app-c3d4', 0],
    ] as [string, number][],
  };

  const nodes = render(<Loading variant="nodes" initial={initial} />);
  expect(nodes.container.querySelectorAll('.loading-skeleton-node-card')).toHaveLength(2);
  cleanup();

  const chains = render(<Loading variant="chains" initial={initial} />);
  const groups = chains.container.querySelectorAll('.chain-section');
  expect(groups).toHaveLength(2);
  expect(groups[0]?.querySelectorAll('.chain-card')).toHaveLength(3);
  expect(groups[1]?.querySelectorAll('.chain-card')).toHaveLength(0);
});

it('线路详情按配置双列与通栏规则占位，不渲染机器观测面板', () => {
  const view = render(<Loading variant="chain-detail" />);
  expect(view.container.querySelectorAll('.chain-config-grid > .col')).toHaveLength(2);
  expect(view.container.querySelector('.loading-skeleton-chain-probe')).toBeTruthy();
  expect(view.container.querySelector('.loading-skeleton-chain-ingress')).toBeTruthy();
  expect(view.container.querySelector('.loading-skeleton-chain-client')).toBeTruthy();
  expect(view.container.querySelector('.loading-skeleton-chain-protocols')).toBeTruthy();
  expect(view.container.querySelector('.loading-skeleton-chain-rules')).toBeTruthy();
  expect(view.container.querySelectorAll('.protocol-choice')).toHaveLength(4);
  expect(view.container.querySelector('.loading-skeleton-metrics, .loading-skeleton-observation-panel')).toBeNull();
  expect(view.container.querySelector('button, input, select, summary, [tabindex]')).toBeNull();
});

it.each(['nodes', 'chains', 'chain-detail'] as const)(
  '分包完成后由 API 等待接力时不重新挂载进度提示：%s',
  async variant => {
    let resolveModule: ((module: { default: ComponentType }) => void) | undefined;
    const modulePromise = new Promise<{ default: ComponentType }>(resolve => {
      resolveModule = resolve;
    });
    const LazyPane = lazy(() => modulePromise);
    let finishRequest: (() => void) | undefined;

    function DeferredPane() {
      const [pending, setPending] = useState(true);
      finishRequest = () => setPending(false);
      return pending ? <Loading variant={variant} /> : <p>页面内容</p>;
    }

    const view = render(
      <LoadingBoundary fallback={<Loading variant={variant} />} variant={variant}>
        <LazyPane />
      </LoadingBoundary>,
    );
    const originalIndicator = view.getByRole('status');

    await act(async () => {
      resolveModule?.({ default: DeferredPane });
      await modulePromise;
    });

    expect(view.getByRole('status')).toBe(originalIndicator);
    expect(finishRequest).toBeTypeOf('function');

    act(() => finishRequest?.());

    expect(view.queryByRole('status')).toBeNull();
    expect(view.container.querySelector('.loading-boundary-guard')).toBeNull();
    expect(view.container.querySelector('.loading-boundary-content-entering')).toBeNull();
    expect(view.getByText('页面内容')).toBeTruthy();
  },
);

it('页面首次揭示后不接受同级或局部读取重新注册', () => {
  function Page({ phase }: { phase: 'ready' | 'page-loading' | 'chart-loading' }) {
    if (phase === 'page-loading') return <Loading variant="nodes" />;
    return (
      <section data-testid="real-page">
        <p>已经显示的页面</p>
        {phase === 'chart-loading' && <Loading variant="chart" />}
      </section>
    );
  }

  const view = render(
    <LoadingBoundary fallback={<Loading variant="nodes" />} variant="nodes">
      <Page phase="ready" />
    </LoadingBoundary>,
  );
  const page = view.getByTestId('real-page');
  expect(view.queryByRole('status')).toBeNull();

  view.rerender(
    <LoadingBoundary fallback={<Loading variant="nodes" />} variant="nodes">
      <Page phase="chart-loading" />
    </LoadingBoundary>,
  );
  expect(view.queryByRole('status')).toBeNull();
  expect(view.getByTestId('real-page')).toBe(page);

  view.rerender(
    <LoadingBoundary fallback={<Loading variant="nodes" />} variant="nodes">
      <Page phase="page-loading" />
    </LoadingBoundary>,
  );
  expect(view.queryByRole('status')).toBeNull();
});

it('首屏里的局部读取只延长原页面进度提示，不另画第二个提示', () => {
  let finishRequest: (() => void) | undefined;
  function Page() {
    const [pending, setPending] = useState(true);
    finishRequest = () => setPending(false);
    return <section>{pending ? <Loading variant="chart" /> : <p>完整页面</p>}</section>;
  }

  const view = render(
    <LoadingBoundary fallback={<Loading variant="nodes" />} variant="nodes">
      <Page />
    </LoadingBoundary>,
  );
  expect(view.getAllByRole('status')).toHaveLength(1);
  expect(view.getByRole('status').classList.contains('loading-nodes')).toBe(true);
  expect(view.container.querySelector('.loading-chart')).toBeNull();

  act(() => finishRequest?.());
  expect(view.queryByRole('status')).toBeNull();
  expect(view.getByText('完整页面')).toBeTruthy();
});

it('同一代码包内切换到已有数据的详情时不重启骨架', () => {
  const listRoute = paneLoadingRouteKey('tab:nodes');
  const detailRoute = paneLoadingRouteKey('tab:nodes', { drill: { p: 'node', id: 'hk-01' } });
  const view = render(
    <LoadingBoundary routeKey={listRoute} fallback={<Loading variant="nodes" />} variant="nodes">
      <p>机器列表</p>
    </LoadingBoundary>,
  );
  const surface = view.getByText('机器列表');
  expect(view.queryByRole('status')).toBeNull();

  view.rerender(
    <LoadingBoundary routeKey={detailRoute} fallback={<Loading variant="detail" />} variant="detail">
      <p>机器详情</p>
    </LoadingBoundary>,
  );

  expect(view.queryByRole('status')).toBeNull();
  expect(view.getByText('机器详情')).toBe(surface);
});

it.each([
  ['机器详情', 'tab:nodes', { p: 'node', id: 'hk-01' }, 'detail'],
  ['机器纳管', 'tab:nodes', { p: 'provision', step: 1 }, 'form'],
  ['Agent 安装', 'tab:nodes', { p: 'install', node: 'hk-01', step: 4 }, 'form'],
  ['从机器建链', 'tab:nodes', { p: 'chain', id: 'hk-01' }, 'form'],
  ['线路详情', 'tab:chains', { p: 'chain', app: 'app-1', chain: 'chain-1' }, 'chain-detail'],
  ['新建线路', 'tab:chains', { p: 'new', app: 'app-1' }, 'form'],
  ['自定义隧道', 'tab:tunnels', { p: 'custom', id: 'custom-1111-1111' }, 'config-detail'],
  ['WARP 隧道', 'tab:tunnels', { p: 'warp', id: 'warp-1111-1111' }, 'config-detail'],
  ['VPN Gate 概览', 'tab:tunnels', { p: 'vpngate' }, 'vpngate'],
  ['VPN Gate 节点池', 'tab:tunnels', { p: 'vpngate', id: 'vpngate-1111-1111' }, 'vpngate'],
  ['发布计划', 'tab:deploy', { p: 'plan', revision: 7 }, 'plan'],
  ['发布详情', 'tab:deploy', { p: 'detail', id: 7 }, 'deployment'],
] as const)('从%s列表进入未完成读取的二级页面时重新显示整页进度提示', (_label, paneKey, drill, variant) => {
  function Page({ pending, copy }: { pending: boolean; copy: string }) {
    return pending ? <Loading variant={variant} /> : <p>{copy}</p>;
  }

  const listRoute = paneLoadingRouteKey(paneKey);
  const detailRoute = paneLoadingRouteKey(paneKey, { drill });
  expect(detailRoute).not.toBe(listRoute);

  const view = render(
    <StrictMode>
      <LoadingBoundary routeKey={listRoute} fallback={<Loading variant="nodes" />} variant="nodes">
        <Page pending={false} copy="列表内容" />
      </LoadingBoundary>
    </StrictMode>,
  );
  expect(view.queryByRole('status')).toBeNull();

  view.rerender(
    <StrictMode>
      <LoadingBoundary routeKey={detailRoute} fallback={<Loading variant={variant} />} variant={variant}>
        <Page pending copy="详情内容" />
      </LoadingBoundary>
    </StrictMode>,
  );

  expect(view.getByRole('status').classList.contains(`loading-${variant}`)).toBe(true);
  expect(view.queryByText('列表内容')).toBeNull();
  expect(view.queryByText('详情内容')).toBeNull();

  view.rerender(
    <StrictMode>
      <LoadingBoundary routeKey={detailRoute} fallback={<Loading variant={variant} />} variant={variant}>
        <Page pending={false} copy="详情内容" />
      </LoadingBoundary>
    </StrictMode>,
  );
  expect(view.queryByRole('status')).toBeNull();
  expect(view.getByText('详情内容')).toBeTruthy();
});

it('用户主从页面选择详情时复用整页读取边界', () => {
  const listRoute = paneLoadingRouteKey('tab:users');
  const aliceRoute = paneLoadingRouteKey('tab:users', { drill: { p: 'user', id: 'alice' } });
  const bobRoute = paneLoadingRouteKey('tab:users', { drill: { p: 'user', id: 'bob' } });

  expect(aliceRoute).toBe(listRoute);
  expect(bobRoute).toBe(listRoute);

  const view = render(
    <LoadingBoundary routeKey={listRoute} fallback={<Loading variant="users" />} variant="users">
      <p>选择一个用户</p>
    </LoadingBoundary>,
  );
  const surface = view.getByText('选择一个用户');

  view.rerender(
    <LoadingBoundary routeKey={aliceRoute} fallback={<Loading variant="users" />} variant="users">
      <p>alice 的详情</p>
    </LoadingBoundary>,
  );
  expect(view.queryByRole('status')).toBeNull();
  expect(view.getByText('alice 的详情')).toBe(surface);

  view.rerender(
    <LoadingBoundary routeKey={bobRoute} fallback={<Loading variant="users" />} variant="users">
      <p>bob 的详情</p>
    </LoadingBoundary>,
  );
  expect(view.queryByRole('status')).toBeNull();
  expect(view.getByText('bob 的详情')).toBe(surface);
});

it('同一详情内切页签或更新一次性状态不重启读取生命周期', () => {
  const base = paneLoadingRouteKey('tab:nodes', { drill: { p: 'node', id: 'hk-01' } });
  expect(paneLoadingRouteKey('tab:nodes', { drill: { p: 'node', id: 'hk-01', tab: 'config' } })).toBe(base);

  const provision = paneLoadingRouteKey('tab:nodes', { drill: { p: 'provision', step: 1 } });
  expect(paneLoadingRouteKey('tab:nodes', { drill: { p: 'provision', step: 2 } })).toBe(provision);

  const plan = paneLoadingRouteKey('tab:deploy', { drill: { p: 'plan', revision: 7 } });
  expect(paneLoadingRouteKey('tab:deploy', { drill: { p: 'plan', revision: 7, key: 'generated' } })).toBe(plan);
});
