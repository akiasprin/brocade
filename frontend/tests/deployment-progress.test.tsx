import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, render, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { DeploymentDetail, DeploymentListItem, DeploymentTargetDetail } from '../src/api';
import type { Win } from '../src/wm/store';

window.matchMedia = ((media: string) => ({
  matches: false,
  media,
  addEventListener() {},
  removeEventListener() {},
})) as unknown as typeof window.matchMedia;

const { DeployPane } = await import('../src/panes/deploy');
const { SessionProvider } = await import('../src/session');

const who = {
  operator_id: 'root',
  role: 'system-admin' as const,
  tenant_scope: null,
  token_prefix: null,
  masked_assets: false,
};
const win: Win = {
  id: 81,
  key: 'tab:deploy',
  title: '发布',
  x: 0,
  y: 0,
  w: 1280,
  h: 800,
  z: 1,
  min: false,
  home: 'desk',
  data: { drill: { p: 'detail', id: 538 } },
};

const NODES = [
  { node_id: 'sg-edge-1', name: '新加坡示例一号', public_ipv4_country: 'SG' },
  { node_id: 'hk-edge-2', name: '香港示例二号', public_ipv4_country: 'HK' },
  { node_id: 'jp-edge-1', name: '东京示例一号', public_ipv4_country: 'JP' },
  { node_id: 'mo-edge-1', name: '澳门示例一号', public_ipv4_country: 'MO' },
  { node_id: 'mo-edge-2', name: '澳门示例二号', public_ipv4_country: 'MO' },
  { node_id: 'jp-edge-2', name: '东京示例二号', public_ipv4_country: 'JP' },
  { node_id: 'us-edge-1', name: '洛杉矶示例一号', public_ipv4_country: null },
];

function target(
  node: string,
  wave: number,
  actions: string[],
  status: string,
  extra: Partial<DeploymentTargetDetail> = {},
): DeploymentTargetDetail {
  return {
    node_id: node,
    status,
    error: null,
    wave,
    disruptive: actions.includes('apply-xray'),
    desired_structure: { actions },
    observed_before: null,
    observed_after: null,
    verdict: null,
    dispatched_at: status === 'pending' ? null : `2026-10-09T10:30:0${wave}Z`,
    ...extra,
  };
}

/** 更新机器配置（两台、动作不同）→ 发布验证（一台）→ 全量发布两步，最后一步由参数决定。 */
function deployment(last: DeploymentTargetDetail[], overrides: Partial<DeploymentDetail> = {}): DeploymentDetail {
  return {
    id: 538,
    revision_id: 1792,
    status: 'running',
    activation_status: 'waiting',
    settlement_status: 'converged',
    activated_at: null,
    debt_targets: 0,
    active: true,
    actor: 'root',
    note: '规则 chn-demo/tw-relay-2、规则 chn-demo/mo-edge-2、清理链状态 app-demo/chn-demo、等 11 条 · 7 台',
    base_revision_id: 1791,
    warnings: null,
    created_at: '2026-10-09T10:29:50Z',
    started_at: '2026-10-09T10:30:00Z',
    halted_at: null,
    finished_at: null,
    rollback_of_deployment_id: null,
    sync_of_deployment_id: null,
    divergence_cleared_at: null,
    targets: [
      target('sg-edge-1', 0, ['sync-grants'], 'succeeded'),
      target('hk-edge-2', 0, ['apply-wire-guard'], 'succeeded'),
      target('jp-edge-1', 1, ['apply-xray'], 'succeeded'),
      target('mo-edge-1', 2, ['apply-xray'], 'succeeded'),
      target('mo-edge-2', 2, ['apply-xray'], 'succeeded'),
      ...last,
    ],
    ...overrides,
  };
}

function mount(detail: DeploymentDetail, awaiting: boolean) {
  // 轮询请求挂起不返回：断言只针对预置的数据，不让测试期间的重新拉取改写结果
  vi.stubGlobal(
    'fetch',
    vi.fn(() => new Promise(() => {})),
  );
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  client.setQueryData(['deployment', 538, true], detail);
  client.setQueryData(['nodes'], { nodes: NODES });
  client.setQueryData(['deployments', 'runtime'], {
    deployments: detail.active
      ? [{ id: 538, active: true, awaiting_confirmation: awaiting } as DeploymentListItem]
      : [],
  });
  return render(
    <QueryClientProvider client={client}>
      <SessionProvider value={{ who, initial: { node_count: 7, chain_group_count: [] } }}>
        <DeployPane win={win} />
      </SessionProvider>
    </QueryClientProvider>,
  );
}

const progress = (container: HTMLElement) => container.querySelector('.cgp-line') as HTMLElement;
const stepRow = (container: HTMLElement, title: string) =>
  [...container.querySelectorAll<HTMLElement>('.cgp-step')].find(
    step => step.querySelector('.cgp-title b')?.textContent === title,
  )!;

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe('执行进度', () => {
  it('每一步是时间线上的一个节点；多步的全量发布先写分组，成功的机器收成机器格', () => {
    const view = mount(
      deployment([
        target('jp-edge-2', 3, ['apply-xray'], 'converging'),
        target('us-edge-1', 3, ['apply-xray'], 'pending'),
      ]),
      false,
    );
    const line = progress(view.container);
    expect([...line.querySelectorAll('.cgp-title b')].map(title => title.textContent)).toEqual([
      '更新机器配置',
      '发布验证',
      '全量发布',
      '第 1 步',
      '第 2 步',
    ]);
    // 阶段的固定说明进 title，不占正文
    expect(stepRow(view.container, '发布验证').querySelector('.cgp-title b')?.getAttribute('title')).toBe(
      '先验证一台机器，再继续扩大范围',
    );
    expect(line.querySelector('.cgp-group .cgp-facts')?.textContent).toBe('2 步 · 更新 Xray · 会中断连接');

    // 各台动作不同：并列写在步骤上，格内再写各自的动作
    const config = stepRow(view.container, '更新机器配置');
    expect(config.querySelector('.cgp-facts')?.textContent).toBe('同步授权、更新 WireGuard');
    expect(within(config).getByText('sg-edge-1 · 同步授权')).toBeTruthy();
    // 动作相同：格内只写名称与 ID；成功只画图标，不逐台写「成功」与时间
    const first = stepRow(view.container, '第 1 步');
    expect(first.classList.contains('ok')).toBe(true);
    expect(within(first).getByText('mo-edge-1')).toBeTruthy();
    expect(first.textContent).not.toContain('成功');
    expect(first.querySelector('.cgp-time')?.getAttribute('dateTime')).toBe('2026-10-09T10:30:02.000Z');
    expect(first.querySelector('.cgp-state')?.textContent).toBe('');
  });

  it('机器名前是地区旗；没有地区的机器留出同宽的空位', () => {
    const view = mount(
      deployment([
        target('jp-edge-2', 3, ['apply-xray'], 'converging'),
        target('us-edge-1', 3, ['apply-xray'], 'pending'),
      ]),
      false,
    );
    const cells = [...progress(view.container).querySelectorAll('.cgp-m')];
    expect(cells).toHaveLength(7);
    const jp = cells.find(cell => cell.textContent?.includes('东京示例二号'))!;
    expect(jp.querySelector('.cgp-flag .geo-flag')?.getAttribute('aria-label')).toBe('JP 地区旗');
    const unknown = cells.find(cell => cell.textContent?.includes('洛杉矶示例一号'))!;
    expect(unknown.querySelector('.cgp-flag')).toBeTruthy();
    expect(unknown.querySelector('.geo-flag')).toBeNull();
  });

  it('已确认、已下发的步骤显示进行中，不再显示等待确认和确认按钮', () => {
    const view = mount(
      deployment([
        target('jp-edge-2', 3, ['apply-xray'], 'converging'),
        target('us-edge-1', 3, ['apply-xray'], 'pending'),
      ]),
      false,
    );
    const second = stepRow(view.container, '第 2 步');
    expect(second.querySelector('.cgp-state')?.textContent).toBe('进行中');
    expect(view.container.textContent).not.toContain('等待确认');
    expect(view.queryByRole('button', { name: '更新这 2 台机器' })).toBeNull();
    expect(within(second).getByText('收敛中')).toBeTruthy();
    expect(within(second).getByText('待推')).toBeTruthy();
    // 在途的机器在格内就能隔离
    expect(within(second).getByRole('button', { name: '隔离 东京示例二号' })).toBeTruthy();
    expect(view.container.querySelector('.cg-plate .cg-lamp')?.classList.contains('run')).toBe(true);
  });

  it('需要确认、服务端还没有确认记录的步骤显示等待确认，并给出确认按钮', () => {
    const view = mount(
      deployment([
        target('jp-edge-2', 3, ['apply-xray'], 'pending'),
        target('us-edge-1', 3, ['apply-xray'], 'pending'),
      ]),
      true,
    );
    const second = stepRow(view.container, '第 2 步');
    expect(second.classList.contains('warn')).toBe(true);
    expect(second.querySelector('.cgp-state')?.textContent).toBe('等待确认');
    // 分组行已写过动作；等待确认时这一步再写一遍，「会中断连接」标金色
    expect(second.querySelector('.cgp-facts em')?.classList.contains('warn')).toBe(true);
    expect(view.getByRole('button', { name: '更新这 2 台机器' })).toBeTruthy();
    expect(view.container.querySelector('.cg-plate .cg-lamp')?.classList.contains('warn')).toBe(true);
  });

  it('失败的机器展开成整行：报错、处理方式、重试与隔离', () => {
    const error = 'apply xray: 健康检查有 1 项未通过: in:app-demo/ent-demo 没有监听 2443/tcp; 已回滚到执行前的配置';
    const view = mount(
      deployment([
        target('jp-edge-2', 3, ['apply-xray'], 'failed-recovered', { error }),
        target('us-edge-1', 3, ['apply-xray'], 'succeeded'),
      ]),
      false,
    );
    const second = stepRow(view.container, '第 2 步');
    expect(second.querySelector('.cgp-state')?.textContent).toBe('执行失败');
    const failure = second.querySelector<HTMLElement>('.cgp-fail')!;
    expect(failure.querySelector('.cgp-fhead b')?.textContent).toBe('东京示例二号');
    expect(failure.querySelector('.cgp-fstate')?.textContent).toBe('失败已回滚');
    expect(failure.querySelector('code')?.textContent).toBe(error);
    expect(within(failure).getByText('处理方式')).toBeTruthy();
    expect(within(failure).getByRole('button', { name: '重试' })).toBeTruthy();
    expect(within(failure).getByRole('button', { name: '隔离' })).toBeTruthy();
    // 失败的那台不再出现在机器格里
    expect([...second.querySelectorAll('.cgp-m')].map(cell => cell.querySelector('b')?.textContent)).toEqual([
      '洛杉矶示例一号',
    ]);
  });

  it('页头备注单独一段并带完整 title，修订范围另起一段；结束的单据写出用时', () => {
    const view = mount(
      deployment(
        [target('jp-edge-2', 3, ['apply-xray'], 'succeeded'), target('us-edge-1', 3, ['apply-xray'], 'succeeded')],
        {
          status: 'succeeded',
          active: false,
          finished_at: '2026-10-09T10:30:27Z',
        },
      ),
      false,
    );
    const note = view.container.querySelector('.cg-head .nd-ident-meta .cg-head-note')!;
    expect(note.getAttribute('title')).toBe(note.textContent);
    expect(view.container.querySelector('.cg-head-range')?.textContent).toBe('· R1791 → R1792');
    expect(view.container.querySelector('.cg-main > section.cg-sec .cg-meta')?.textContent).toBe(
      '7 / 7 台完成 · 用时 27 秒',
    );
  });
});
