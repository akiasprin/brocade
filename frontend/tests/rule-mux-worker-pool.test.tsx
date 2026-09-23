import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { ConsoleSnapshot, HopMux, HopPool, NodeAgentStateItem, Rule } from '../src/api';
import { DEFAULT_HOP_MUX, hopMuxError } from '../src/api';
import { draft } from '../src/draft';
import { RuleEditor, type ForwardPeer } from '../src/panes/rules';

const GLOBAL_MUX: HopMux = {
  concurrency: 7,
  prewarm_workers: 1,
  reuse_threshold: 4,
  max_probing_workers: 2,
  probe_interval_ms: 9125,
  probe_timeout_ms: 1200,
  idle_ttl_ms: 22125,
  max_requests_per_worker: 300,
};

const forwardRule = (match: Rule['m'], pool: HopPool): Rule => ({
  m: match,
  a: { t: 'forward', to: 'relay', dial: { t: 'overlay' }, pool },
});

const relay: ForwardPeer = {
  id: 'relay',
  name: '新加坡中继',
  public_ipv4: 'relay.example.net',
  public_ipv6: null,
  public_ipv4_nat: false,
  public_ipv6_nat: false,
  step: {
    chain: 'stream',
    node: 'relay',
    accept: null,
    hop_in: { port: 20000, security: { t: 'none' } },
    rules: [],
  },
  where: 'next',
  blocked: null,
};

function renderEditor(pool: HopPool = { t: 'none' }, readOnly = false) {
  const rules = [forwardRule({ t: 'domain_suffix', v: ['example.com'] }, pool), forwardRule({ t: 'any' }, pool)];
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Number.POSITIVE_INFINITY } },
  });
  const snapshot = {
    snapshot: {
      revision: 1,
      settings: {
        relay_mux: GLOBAL_MUX,
      },
      apps: [
        {
          id: 'video',
          label: '视频',
          chains: [{ id: 'stream', tenant: 'platform', name: '流媒体' }],
          steps: [],
          ingresses: [],
          fronts: [],
          grants: [],
        },
      ],
      external_outbounds: [],
    },
    node_egress_dns: [],
    redacted: readOnly,
  } as unknown as ConsoleSnapshot;
  client.setQueryData(['snapshot'], snapshot);
  client.setQueryData(['revisions'], { current_revision: null, revisions: [] });
  client.setQueryData(['settings'], { ports: { hop_base: 20000 } });
  client.setQueryData(['nodes'], {
    nodes: [
      {
        node_id: 'source',
        tenant_id: 'platform',
        name: '香港入口',
        public_ipv4: 'source.example.net',
        public_ipv6: null,
        public_ipv4_nat: false,
        public_ipv6_nat: false,
        egress_allowed: false,
      } as NodeAgentStateItem,
      {
        node_id: 'relay',
        tenant_id: 'platform',
        name: '新加坡中继',
        public_ipv4: 'relay.example.net',
        public_ipv6: null,
        public_ipv4_nat: false,
        public_ipv6_nat: false,
        egress_allowed: true,
      } as NodeAgentStateItem,
    ],
  });

  return render(
    <QueryClientProvider client={client}>
      <RuleEditor
        appId="video"
        chainId="stream"
        nodeId="source"
        initial={rules}
        accept={null}
        peers={[relay]}
        isForwardTarget={false}
        readOnly={readOnly}
        fallback={{ rules: [], pending: false }}
      />
    </QueryClientProvider>,
  );
}

function connectionSelect(view: ReturnType<typeof renderEditor>) {
  const field = view.getByText('连接复用').closest('.hopfld');
  const select = field?.querySelector('select');
  if (!(select instanceof HTMLSelectElement)) throw new Error('没有找到出站连接选择器');
  return select;
}

function muxField(dialog: HTMLElement, label: string) {
  const input = within(dialog).getByText(label).closest('label')?.querySelector('input');
  if (!(input instanceof HTMLInputElement)) throw new Error(`没有找到 Mux 字段 ${label}`);
  return input;
}

function savedPools(): HopPool[] {
  const op = draft.ops().find(candidate => candidate.op === 'put_step' && candidate.node_id === 'source');
  if (!op || op.op !== 'put_step') throw new Error('没有找到 put_step 草稿');
  return op.step.rules.flatMap(rule => (rule.a.t === 'forward' ? [rule.a.pool ?? { t: 'none' }] : []));
}

beforeEach(() => {
  draft.init(`mux-rule-${Math.random()}`);
  draft.clear();
});

afterEach(() => {
  cleanup();
  draft.clear();
});

describe('规则页 Mux Worker 池', () => {
  it('使用毫秒编辑并保存大连接数量和 24 小时超额空闲寿命', async () => {
    const view = renderEditor({ t: 'mux' });
    fireEvent.click(view.getByRole('button', { name: '配置参数' }));
    const dialog = view.getByRole('dialog', { name: '配置 新加坡中继 的 Mux' });
    fireEvent.click(within(dialog).getByRole('button', { name: '单独配置' }));
    expect(muxField(dialog, '预热目标').getAttribute('max')).toBeNull();
    expect(muxField(dialog, '复用阈值').getAttribute('max')).toBeNull();
    expect(muxField(dialog, '超额空闲寿命').getAttribute('max')).toBeNull();
    expect(muxField(dialog, '超额空闲寿命').value).toBe('22125');
    expect(muxField(dialog, '探活周期').value).toBe('9125');
    fireEvent.change(muxField(dialog, '复用阈值'), { target: { value: '100000' } });
    fireEvent.change(muxField(dialog, '预热目标'), { target: { value: '70000' } });
    fireEvent.change(muxField(dialog, '探活并发'), { target: { value: '32' } });
    fireEvent.change(muxField(dialog, '累计子连接'), { target: { value: '65535' } });
    fireEvent.change(muxField(dialog, '超额空闲寿命'), { target: { value: '86400125' } });
    fireEvent.click(within(dialog).getByRole('button', { name: '应用' }));
    fireEvent.click(view.getByRole('button', { name: '保存到草稿' }));
    const pool = {
      t: 'mux',
      v: {
        ...GLOBAL_MUX,
        prewarm_workers: 70000,
        reuse_threshold: 100000,
        max_probing_workers: 32,
        max_requests_per_worker: 65535,
        idle_ttl_ms: 86400125,
      },
    };
    await waitFor(() => expect(savedPools()).toEqual([pool, pool]));
    expect(DEFAULT_HOP_MUX.idle_ttl_ms).toBe(24000);
    expect(DEFAULT_HOP_MUX.max_requests_per_worker).toBe(128);
  });

  it('选择 Mux 只写入跟随全局形状，并同步所有指向同一目标的规则', async () => {
    const view = renderEditor();

    const panel = view.container.querySelector('.hop-target-panel');
    expect(panel).not.toBeNull();
    expect(panel?.textContent).toContain('端口和协议属于目标监听；拨号和 Mux 属于当前链边');
    expect(panel?.textContent?.replace(/\s+/g, '')).toContain(
      '端口开在新加坡中继，连接由香港入口发起；目标端口必须唯一。',
    );

    expect(connectionSelect(view).value).toBe('none');
    fireEvent.change(connectionSelect(view), { target: { value: 'mux' } });

    expect(view.getByText('跟随全局')).toBeTruthy();
    const muxSummary = view.getByText('Mux 参数').closest('.hop-target-mux');
    expect(muxSummary?.classList.contains('hopfld')).toBe(true);
    expect(muxSummary?.textContent).toContain('Mux 参数跟随全局配置参数');
    expect(panel?.textContent).toContain('“跟随全局”只继承参数，不与其他边共池。');
    expect(panel?.textContent).not.toContain('发布会重启受影响的 Xray');
    expect(view.container.querySelector('.mux-drawer-grid')).toBeNull();
    fireEvent.click(view.getByRole('button', { name: '保存到草稿' }));

    await waitFor(() => expect(savedPools()).toEqual([{ t: 'mux' }, { t: 'mux' }]));
  });

  it('参数只在右侧抽屉出现，单独配置先复制当前全局值，取消不改草稿', async () => {
    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(true);
    const view = renderEditor({ t: 'mux' });

    expect(view.container.querySelector('.mux-drawer-grid')).toBeNull();
    fireEvent.click(view.getByRole('button', { name: '配置参数' }));
    let dialog = view.getByRole('dialog', { name: '配置 新加坡中继 的 Mux' });
    expect(muxField(dialog, '复用流数量').value).toBe('7');
    expect(muxField(dialog, '复用流数量').disabled).toBe(true);

    fireEvent.click(within(dialog).getByRole('button', { name: '单独配置' }));
    expect(muxField(dialog, '复用流数量').value).toBe('7');
    fireEvent.change(muxField(dialog, '复用流数量'), { target: { value: '8' } });
    fireEvent.click(within(dialog).getByRole('button', { name: '取消' }));

    await waitFor(() => expect(document.body.querySelector('.dialog-layer')).toBeNull());
    expect(confirm).not.toHaveBeenCalled();
    expect(view.getByText('跟随全局')).toBeTruthy();
    fireEvent.click(view.getByRole('button', { name: '配置参数' }));
    dialog = view.getByRole('dialog', { name: '配置 新加坡中继 的 Mux' });
    expect(muxField(dialog, '复用流数量').value).toBe('7');
    expect(muxField(dialog, '复用流数量').disabled).toBe(true);
  });

  it('应用后写入完整覆盖，切回跟随全局会删除覆盖对象', async () => {
    const view = renderEditor({ t: 'mux' });
    fireEvent.click(view.getByRole('button', { name: '配置参数' }));
    let dialog = view.getByRole('dialog', { name: '配置 新加坡中继 的 Mux' });
    fireEvent.click(within(dialog).getByRole('button', { name: '单独配置' }));
    fireEvent.change(muxField(dialog, '复用流数量'), { target: { value: '8' } });
    fireEvent.click(within(dialog).getByRole('button', { name: '应用' }));

    expect(view.getByText('单独配置')).toBeTruthy();
    fireEvent.click(view.getByRole('button', { name: '保存到草稿' }));
    await waitFor(() =>
      expect(savedPools()).toEqual([
        { t: 'mux', v: { ...GLOBAL_MUX, concurrency: 8 } },
        { t: 'mux', v: { ...GLOBAL_MUX, concurrency: 8 } },
      ]),
    );

    view.unmount();
    draft.clear();
    const followView = renderEditor({ t: 'mux', v: { ...GLOBAL_MUX, concurrency: 8 } });
    fireEvent.click(followView.getByRole('button', { name: '配置参数' }));
    dialog = followView.getByRole('dialog', { name: '配置 新加坡中继 的 Mux' });
    fireEvent.click(within(dialog).getByRole('button', { name: '跟随全局' }));
    fireEvent.click(within(dialog).getByRole('button', { name: '应用' }));
    fireEvent.click(followView.getByRole('button', { name: '保存到草稿' }));
    await waitFor(() => expect(savedPools()).toEqual([{ t: 'mux' }, { t: 'mux' }]));
  });

  it('交叉约束非法时禁止应用，切回每次新建会删除整个 Mux 子对象', async () => {
    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(true);
    const view = renderEditor({ t: 'mux' });
    fireEvent.click(view.getByRole('button', { name: '配置参数' }));
    const dialog = view.getByRole('dialog', { name: '配置 新加坡中继 的 Mux' });
    fireEvent.click(within(dialog).getByRole('button', { name: '单独配置' }));
    fireEvent.change(muxField(dialog, '预热目标'), { target: { value: '5' } });
    fireEvent.change(muxField(dialog, '复用阈值'), { target: { value: '4' } });
    expect(within(dialog).getByText('预热目标不能大于复用阈值')).toBeTruthy();
    expect((within(dialog).getByRole('button', { name: '应用' }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(within(dialog).getByRole('button', { name: '取消' }));
    await waitFor(() => expect(document.body.querySelector('.dialog-layer')).toBeNull());
    expect(confirm).not.toHaveBeenCalled();

    fireEvent.change(connectionSelect(view), { target: { value: 'none' } });
    expect(view.queryByText('跟随全局')).toBeNull();
    fireEvent.click(view.getByRole('button', { name: '保存到草稿' }));
    await waitFor(() => expect(savedPools()).toEqual([{ t: 'none' }, { t: 'none' }]));
  });
});

describe('Mux 参数前端校验', () => {
  it.each([
    ['复用流数量', { concurrency: 0 }, '复用流数量必须在 1–128 之间'],
    ['预热目标', { prewarm_workers: -1 }, '预热目标必须为非负整数'],
    ['复用阈值', { reuse_threshold: 0 }, '复用阈值必须为正整数'],
    ['探活并发', { max_probing_workers: 17 }, '探活并发必须在 1 与复用阈值之间'],
    ['探活周期', { probe_interval_ms: 1 }, '探活周期必须在 2000–60000 ms 之间'],
    ['探活超时', { probe_timeout_ms: 199 }, '探活超时必须在 200–10000 ms 之间，且小于探活周期'],
    ['超额空闲寿命', { idle_ttl_ms: 0 }, '超额空闲寿命必须不小于 1000 ms'],
    ['超额空闲寿命类型溢出', { idle_ttl_ms: 4294967296 }, '超额空闲寿命超出可表示范围（4294967295 ms）'],
    ['累计流上限', { max_requests_per_worker: 0 }, '累计子连接上限必须在 1–65535 之间'],
    ['累计子连接溢出', { max_requests_per_worker: 65536 }, '累计子连接上限必须在 1–65535 之间'],
    ['空闲连接类型溢出', { reuse_threshold: 4294967296 }, '连接池数量超出 uint32 字段可表示范围（4294967295）'],
    ['空闲上下限', { prewarm_workers: 3, reuse_threshold: 2 }, '预热目标不能大于复用阈值'],
    ['探活并发上限', { max_probing_workers: 3, reuse_threshold: 2 }, '探活并发必须在 1 与复用阈值之间'],
    [
      '超时与周期',
      { probe_interval_ms: 2000, probe_timeout_ms: 2000 },
      '探活超时必须在 200–10000 ms 之间，且小于探活周期',
    ],
    [
      '寿命窗口',
      { probe_interval_ms: 5000, probe_timeout_ms: 2000, idle_ttl_ms: 6999 },
      '超额空闲寿命必须覆盖一个探活周期和探活超时',
    ],
  ])('拒绝%s非法值', (_name, patch, message) => {
    expect(hopMuxError({ ...DEFAULT_HOP_MUX, ...patch })).toBe(message);
  });

  it('接受所有边界值和合法交叉组合', () => {
    expect(
      hopMuxError({
        concurrency: 128,
        prewarm_workers: 0xffffffff,
        reuse_threshold: 0xffffffff,
        max_probing_workers: 0xffffffff,
        probe_interval_ms: 60000,
        probe_timeout_ms: 10000,
        idle_ttl_ms: 0xffffffff,
        max_requests_per_worker: 65535,
      }),
    ).toBeNull();
  });
});
