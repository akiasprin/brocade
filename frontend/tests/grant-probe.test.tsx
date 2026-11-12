import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, fireEvent, render, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { GrantProbeJob, GrantProbePlan } from '../src/api';
import { SubscriptionViewer } from '../src/panes/subscription';

// 网络拨测并在订阅与节点弹窗的「节点」页签里：节点列表来自 uri.txt，拨测计划按名称对到列出的地址上。

const GB = '\u{1F1EC}\u{1F1E7}';
const JP = '\u{1F1EF}\u{1F1F5}';
const HK = '\u{1F1ED}\u{1F1F0}';
const named = (name: string) => encodeURIComponent(name);

// 拨测条目的名称带地区旗前缀（core/physical/user.rs），uri.txt 的链接名称经 URI 编码后带同一个旗。
const plan: GrantProbePlan = {
  serving_revision: 582,
  serving_generation: 19,
  items: [
    {
      id: 'g-friendly:ipv4:vless',
      name: `${GB}伦敦入口`,
      app_id: 'app-global',
      app_name: '全球线路',
      chain_id: 'ch-blue',
      ingress_id: 'in-green',
      family: 'ipv4',
      protocol: 'vless',
    },
    {
      id: 'g-friendly:ipv6:hysteria2',
      name: `${GB}伦敦入口 | QUIC | v6`,
      app_id: 'app-global',
      app_name: '全球线路',
      chain_id: 'ch-blue',
      ingress_id: 'in-green',
      family: 'ipv6',
      protocol: 'hysteria2',
    },
  ],
};
const tokyo = {
  id: 'g-tokyo:ipv4:vless',
  name: `${JP}东京入口`,
  app_id: 'app-standard',
  app_name: '标准线路',
  chain_id: 'ch-tokyo',
  ingress_id: 'in-tokyo',
  family: 'ipv4' as const,
  protocol: 'vless' as const,
};

const finishedJob: GrantProbeJob = {
  id: 'p1',
  tenant_id: 'platform.acme',
  user_id: 'alice',
  serving_revision: plan.serving_revision,
  serving_generation: plan.serving_generation,
  status: 'completed',
  message: '网络拨测全部通过',
  created_at_unix_secs: 1,
  finished_at_unix_secs: 2,
  items: plan.items.map(item => ({ ...item, status: 'passed', ttfb_ms: 42, detail: null })),
};

const vlessLink = `vless://u@lon.example:443?encryption=none&type=tcp&security=reality&sni=www.microsoft.com&fp=chrome&pbk=K&sid=1#${named(`${GB}伦敦入口`)}`;
const hy2Link = `hysteria2://u@[2001:db8::7]:50000-50009/?sni=lon.example&insecure=1#${named(`${GB}伦敦入口 | QUIC | v6`)}`;
const mtproxyLink = 'tg://proxy?server=lon.example&port=15443&secret=dd00112233445566778899aabbccddeeff';
const tokyoLink = `vless://u@tyo.example:443?encryption=none&type=tcp&security=reality#${named(`${JP}东京入口`)}`;
const uriText = [
  vlessLink,
  hy2Link,
  `# @brocade-name=${named(`${GB}伦敦入口 | MTProxy`)}`,
  mtproxyLink,
  '',
  '# 以下条目带前置代理，URI 列表表达不了，已跳过：',
  `# ${HK}香港 CMI`,
  '# 换 Clash 目标即可。',
  '',
].join('\n');

const planPath = '/users/platform.acme/alice/grant-probes';
const linkPath = '/artifacts/content/user/platform.acme%3Aalice/uri?serving=true&insecure=true';

const jsonResponse = (body: unknown) =>
  ({
    ok: true,
    status: 200,
    statusText: 'OK',
    json: async () => body,
  }) as Response;

const artifact = (content: string) =>
  jsonResponse({
    revision: plan.serving_revision,
    target_kind: 'user',
    target_id: 'platform.acme:alice',
    artifact_kind: 'uri',
    state: 'present',
    sha256: 'abc',
    byte_len: content.length,
    content,
    redacted: false,
  });

const capability = () => jsonResponse({ available: true, version: '26.4.25', reason: null, concurrency: 30 });

type Route = (path: string, init?: RequestInit) => Response | Promise<Response> | undefined;

// 默认路由：修订号、能力、计划与节点链接；用例按需覆盖，未知请求直接失败。
const probeFetch = (extra?: Route) =>
  vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const path = String(input);
    const handled = await extra?.(path, init);
    if (handled) return handled;
    if (path === '/revisions?limit=50') return jsonResponse({ current_revision: 582, revisions: [] });
    if (path === '/grant-probes/capability') return capability();
    if (path === planPath && !init?.method) return jsonResponse(plan);
    if (path === linkPath) return artifact(uriText);
    throw new Error(`unexpected request ${init?.method ?? 'GET'} ${path}`);
  });

const newClient = () => new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });

const dialog = (
  client: QueryClient,
  selfService = false,
  onProbePassed?: (result: { probeId: string; itemId: string }) => void,
) => (
  <QueryClientProvider client={client}>
    <SubscriptionViewer
      tenant="platform.acme"
      user="alice"
      kind="uri"
      selfService={selfService}
      onProbePassed={onProbePassed}
      onClose={() => undefined}
    />
  </QueryClientProvider>
);

const renderDialog = (
  selfService = false,
  client = newClient(),
  onProbePassed?: (result: { probeId: string; itemId: string }) => void,
) => ({
  ...render(dialog(client, selfService, onProbePassed)),
  client,
});

const address = (view: ReturnType<typeof render>, name: string) => view.getByRole('button', { name });
const lampOf = (cell: HTMLElement) => cell.querySelector('.node-copy-lamp');
const posts = (fetchMock: ReturnType<typeof probeFetch>) =>
  fetchMock.mock.calls
    .filter(([input, init]) => String(input) === planPath && init?.method === 'POST')
    .map(([, init]) => JSON.parse(String(init?.body)) as { item_ids: string[] });

// 弹窗按 useNarrow 选择筛选控件：默认桌面宽度。
beforeEach(() =>
  vi.stubGlobal('matchMedia', () => ({ matches: false, addEventListener() {}, removeEventListener() {} })),
);

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe('network probe in the node tab', () => {
  it('reports the first passed item as durable onboarding evidence', async () => {
    const onProbePassed = vi.fn();
    const fetchMock = probeFetch((path, init) =>
      path === planPath && init?.method === 'POST' ? jsonResponse({ job: finishedJob, reused: false }) : undefined,
    );
    vi.stubGlobal('fetch', fetchMock);
    const view = renderDialog(false, newClient(), onProbePassed);

    await view.findByRole('region', { name: '伦敦入口' });
    fireEvent.click(view.getByRole('button', { name: '拨测全部' }));
    await waitFor(() =>
      expect(onProbePassed).toHaveBeenCalledWith({
        probeId: 'p1',
        itemId: 'g-friendly:ipv4:vless',
      }),
    );
  });

  it('lights every listed address with its probe state and reports status without latency', async () => {
    const fetchMock = probeFetch((path, init) =>
      path === planPath && init?.method === 'POST' ? jsonResponse({ job: finishedJob, reused: false }) : undefined,
    );
    vi.stubGlobal('fetch', fetchMock);
    const view = renderDialog();

    await view.findByRole('region', { name: '伦敦入口' });
    const vless = address(view, '复制 伦敦入口（IPv4）');
    const hy2 = address(view, '复制 伦敦入口 | QUIC | v6（IPv6）');
    expect(lampOf(vless)?.className).toBe('node-copy-lamp');
    expect(vless.getAttribute('aria-description')).toBe('拨测：尚未验证');
    expect(vless.title).toBe('lon.example:443 · 复制分享链接\n拨测：尚未验证');
    // MTProxy 不是拨测项：留出同宽的空位，不画灯，也没有拨测描述。
    const mtproxy = address(view, '复制 伦敦入口 | MTProxy（IPv4）');
    expect(lampOf(mtproxy)?.className).toBe('node-copy-lamp');
    expect(mtproxy.hasAttribute('aria-description')).toBe(false);
    // 没有分享链接的条目只在说明里列出，不进列表。
    expect(view.getByText(/已跳过：香港 CMI 换 Clash 目标即可/)).toBeTruthy();
    expect(view.getByRole('button', { name: '拨测全部' }).title).toContain('拨测流量计入该用户用量');
    expect(view.queryByText(/通过 \d+ · 运行/)).toBeNull();

    fireEvent.click(view.getByRole('button', { name: '拨测全部' }));

    await waitFor(() => expect(lampOf(vless)?.className).toBe('node-copy-lamp cg-lamp ok'));
    expect(lampOf(hy2)?.className).toBe('node-copy-lamp cg-lamp ok');
    expect(vless.getAttribute('aria-description')).toBe('拨测：通过');
    expect(view.getByRole('region', { name: '伦敦入口' }).querySelector('.node-group-result')?.textContent).toBe(
      '通过',
    );
    expect(view.getByText('通过 2 · 运行 0 · 等待 0 · 失败 0').closest('[role="status"]')).toBeTruthy();
    // 拨测从控制台发起，首字节时延反映的是控制台到节点的距离，不显示。
    expect(view.baseElement.querySelector('.sub-dialog')?.textContent).not.toMatch(/\d\s*ms/);
    // 列出的条目就是计划的全部：空列表交给服务端表示全部，不在请求里带连接材料。
    expect(posts(fetchMock)).toEqual([{ item_ids: [] }]);
    const body = String(fetchMock.mock.calls.find(([, init]) => init?.method === 'POST')?.[1]?.body);
    expect(body).not.toContain('uuid');
    expect(body).not.toContain('server');
  });

  it('probes only the listed nodes when the filter narrows the server listing', async () => {
    const fetchMock = probeFetch((path, init) => {
      if (path === '/artifacts/content/user/platform.acme%3Aalice/uri?protocol=hysteria2&serving=true&insecure=true')
        return artifact(`${hy2Link}\n`);
      if (path === planPath && init?.method === 'POST')
        return jsonResponse({ job: { ...finishedJob, items: [finishedJob.items[1]] }, reused: false });
      return undefined;
    });
    vi.stubGlobal('fetch', fetchMock);
    const view = renderDialog();

    await view.findByRole('region', { name: '伦敦入口' });
    fireEvent.click(view.getByRole('combobox', { name: '协议' }));
    fireEvent.click(view.getByRole('option', { name: 'Hysteria 2' }));
    await waitFor(() => expect(view.queryByRole('button', { name: '复制 伦敦入口（IPv4）' })).toBeNull());
    fireEvent.click(await view.findByRole('button', { name: '拨测全部' }));

    await waitFor(() => expect(posts(fetchMock)).toEqual([{ item_ids: ['g-friendly:ipv6:hysteria2'] }]));
    await view.findByText('通过 1 · 运行 0 · 等待 0 · 失败 0');
  });

  it('copies and probes only the selected region, including non-probe links in copying', async () => {
    const writeText = vi.fn(async () => undefined);
    vi.stubGlobal('navigator', { ...navigator, clipboard: { writeText } });
    const fetchMock = probeFetch((path, init) => {
      if (path === planPath && init?.method === 'POST')
        return jsonResponse({
          job: { ...finishedJob, items: [{ ...tokyo, status: 'passed', detail: null, ttfb_ms: null }] },
          reused: false,
        });
      if (path === planPath) return jsonResponse({ ...plan, items: [...plan.items, tokyo] });
      if (path === linkPath) return artifact(`${uriText}\n${tokyoLink}\n`);
      return undefined;
    });
    vi.stubGlobal('fetch', fetchMock);
    const view = renderDialog();
    await view.findByRole('region', { name: '东京入口' });
    expect(view.getByRole('button', { name: '全部 4' }).getAttribute('aria-current')).toBe('true');
    fireEvent.click(view.getByRole('button', { name: '英国 3' }));
    expect(view.queryByRole('region', { name: '东京入口' })).toBeNull();
    fireEvent.click(view.getByRole('button', { name: '复制 3 个' }));
    await waitFor(() => expect(writeText).toHaveBeenLastCalledWith([vlessLink, hy2Link, mtproxyLink].join('\n')));
    expect(view.getByRole('button', { name: '拨测 2 个' })).toBeTruthy();
    fireEvent.click(view.getByRole('button', { name: '日本 1' }));
    expect(view.queryByRole('region', { name: '伦敦入口' })).toBeNull();
    fireEvent.click(view.getByRole('button', { name: '复制 1 个' }));
    await waitFor(() => expect(writeText).toHaveBeenLastCalledWith(tokyoLink));
    fireEvent.click(view.getByRole('button', { name: '拨测 1 个' }));
    await waitFor(() => expect(posts(fetchMock)).toEqual([{ item_ids: [tokyo.id] }]));
    fireEvent.click(view.getByRole('button', { name: '全部 4' }));
    expect(view.getByRole('region', { name: '伦敦入口' })).toBeTruthy();
  });

  it('shows only failed addresses and scopes failure copying and probing to those addresses', async () => {
    const writeText = vi.fn(async () => undefined);
    vi.stubGlobal('navigator', { ...navigator, clipboard: { writeText } });
    const failedJob: GrantProbeJob = {
      ...finishedJob,
      items: [
        { ...finishedJob.items[0], status: 'failed', detail: '握手超时' },
        finishedJob.items[1],
        { ...tokyo, status: 'failed', detail: '连接被拒绝', ttfb_ms: null },
      ],
    };
    const fetchMock = probeFetch((path, init) => {
      if (path === planPath && init?.method === 'POST') return jsonResponse({ job: failedJob, reused: false });
      if (path === planPath) return jsonResponse({ ...plan, items: [...plan.items, tokyo] });
      if (path === linkPath) return artifact(`${uriText}\n${tokyoLink}\n`);
      return undefined;
    });
    vi.stubGlobal('fetch', fetchMock);
    const view = renderDialog();
    await view.findByRole('region', { name: '东京入口' });
    expect(view.queryByRole('button', { name: /^失败 \d/ })).toBeNull();
    fireEvent.click(view.getByRole('button', { name: '拨测全部' }));
    fireEvent.click(await view.findByRole('button', { name: '失败 2' }));
    expect(view.queryByRole('button', { name: '复制 伦敦入口 | QUIC | v6（IPv6）' })).toBeNull();
    expect(view.queryByRole('button', { name: '复制 伦敦入口 | MTProxy（IPv4）' })).toBeNull();
    expect(view.getByText('握手超时')).toBeTruthy();
    expect(view.getByText('连接被拒绝')).toBeTruthy();
    fireEvent.click(view.getByRole('button', { name: '复制 2 个' }));
    await waitFor(() => expect(writeText).toHaveBeenLastCalledWith([vlessLink, tokyoLink].join('\n')));
    fireEvent.click(view.getByRole('button', { name: '拨测 2 个' }));
    await waitFor(() =>
      expect(posts(fetchMock)).toEqual([{ item_ids: [] }, { item_ids: [plan.items[0].id, tokyo.id] }]),
    );
    fireEvent.click(view.getByRole('button', { name: '日本 1' }));
    fireEvent.click(view.getByRole('button', { name: '重试失败项' }));
    await waitFor(() => expect(posts(fetchMock).at(-1)).toEqual({ item_ids: [tokyo.id] }));
  });

  it('returns to all when a server filter removes the selected region', async () => {
    const fetchMock = probeFetch(path => {
      if (path.includes('protocol=hysteria2')) return artifact(`${hy2Link}\n`);
      if (path === planPath) return jsonResponse({ ...plan, items: [...plan.items, tokyo] });
      if (path === linkPath) return artifact(`${uriText}\n${tokyoLink}\n`);
      return undefined;
    });
    vi.stubGlobal('fetch', fetchMock);
    const view = renderDialog();
    fireEvent.click(await view.findByRole('button', { name: '日本 1' }));
    fireEvent.click(view.getByRole('combobox', { name: '协议' }));
    fireEvent.click(view.getByRole('option', { name: 'Hysteria 2' }));
    await waitFor(() => expect(view.getByRole('button', { name: '全部 1' }).getAttribute('aria-current')).toBe('true'));
    expect(view.queryByRole('button', { name: '日本 1' })).toBeNull();
    expect(view.getByRole('region', { name: '伦敦入口' })).toBeTruthy();
  });

  it('probes one access point from its group head and keeps every other node on screen', async () => {
    const rowJob: GrantProbeJob = { ...finishedJob, id: 'p-row' };
    const fetchMock = probeFetch((path, init) => {
      if (path === planPath && init?.method === 'POST') return jsonResponse({ job: rowJob, reused: false });
      if (path === planPath) return jsonResponse({ ...plan, items: [...plan.items, tokyo] });
      if (path === linkPath) return artifact(`${uriText}\n${tokyoLink}\n`);
      return undefined;
    });
    vi.stubGlobal('fetch', fetchMock);
    const view = renderDialog();

    await view.findByRole('region', { name: '东京入口' });
    // 组头是这一组的「拨测」；拨测前不写结果。
    expect(view.baseElement.querySelectorAll('.node-group-result')).toHaveLength(0);
    fireEvent.click(view.getByRole('button', { name: '拨测 伦敦入口' }));

    await waitFor(() => expect(lampOf(address(view, '复制 伦敦入口（IPv4）'))?.className).toContain('ok'));
    expect(posts(fetchMock)).toEqual([{ item_ids: ['g-friendly:ipv4:vless', 'g-friendly:ipv6:hysteria2'] }]);
    const tokyoCell = address(view, '复制 东京入口（IPv4）');
    expect(lampOf(tokyoCell)?.className).toBe('node-copy-lamp');
    expect(tokyoCell.getAttribute('aria-description')).toBe('拨测：尚未验证');
    expect(view.getByRole('region', { name: '东京入口' }).querySelector('.node-group-result')).toBeNull();
  });

  it('shows progress on the probe button, the failure reason under the address and the tally in the content pane', async () => {
    let emit: ((job: GrantProbeJob) => void) | null = null;
    let latest: GrantProbeJob | null = null;
    class ManualEventSource {
      onerror: (() => void) | null = null;
      addEventListener(_type: string, listener: (event: MessageEvent<string>) => void) {
        emit = job => {
          latest = job;
          listener({ data: JSON.stringify(job) } as MessageEvent<string>);
        };
      }
      close() {}
    }
    vi.stubGlobal('EventSource', ManualEventSource);
    const runningJob: GrantProbeJob = {
      ...finishedJob,
      id: 'p-live',
      status: 'running',
      message: null,
      finished_at_unix_secs: null,
      items: plan.items.map((item, index) => ({
        ...item,
        status: index === 0 ? 'running' : 'waiting',
        ttfb_ms: null,
        detail: null,
      })),
    };
    latest = runningJob;
    const fetchMock = probeFetch((path, init) => {
      if (path === planPath && init?.method === 'POST') {
        const ids = (JSON.parse(String(init.body)) as { item_ids: string[] }).item_ids;
        return jsonResponse({
          job: ids.length === 0 ? runningJob : { ...finishedJob, id: 'p-retry', items: [finishedJob.items[0]] },
          reused: false,
        });
      }
      if (path === '/grant-probes/p-live') return jsonResponse(latest);
      return undefined;
    });
    vi.stubGlobal('fetch', fetchMock);
    const view = renderDialog();

    await view.findByRole('region', { name: '伦敦入口' });
    fireEvent.click(view.getByRole('button', { name: '拨测全部' }));
    const cancel = await view.findByRole('button', { name: '取消拨测' });
    expect(cancel.classList.contains('node-probe-progress')).toBe(true);
    expect(cancel.style.getPropertyValue('--progress')).toBe('0%');
    const vless = address(view, '复制 伦敦入口（IPv4）');
    expect(lampOf(vless)?.className).toContain('run');
    expect(lampOf(address(view, '复制 伦敦入口 | QUIC | v6（IPv6）'))?.className).toContain('defer');
    expect(view.getByRole('region', { name: '伦敦入口' }).querySelector('.node-group-result')?.textContent).toBe(
      '拨测中',
    );
    await waitFor(() => expect(emit).not.toBeNull());

    act(() =>
      emit?.({
        ...runningJob,
        items: [
          { ...runningJob.items[0], status: 'failed', detail: '握手超时 5 秒' },
          { ...runningJob.items[1], status: 'running' },
        ],
      }),
    );
    // 任务快照写进查询缓存，观察者在下一轮事件循环收到通知。
    await waitFor(() => expect(cancel.style.getPropertyValue('--progress')).toBe('50%'));
    expect(view.getByText('通过 0 · 运行 1 · 等待 0 · 失败 1')).toBeTruthy();

    act(() =>
      emit?.({
        ...runningJob,
        status: 'completed',
        message: '1 项网络拨测失败',
        finished_at_unix_secs: 2,
        items: [
          { ...runningJob.items[0], status: 'failed', detail: '握手超时 5 秒' },
          { ...runningJob.items[1], status: 'passed', ttfb_ms: 88 },
        ],
      }),
    );
    await waitFor(() => expect(vless.classList.contains('is-failed')).toBe(true));
    expect(vless.classList.contains('has-note')).toBe(true);
    expect(vless.querySelector('.node-copy-note')?.textContent).toBe('握手超时 5 秒');
    expect(vless.getAttribute('aria-description')).toBe('拨测：失败（握手超时 5 秒）');
    expect(lampOf(vless)?.className).toContain('err');
    const result = view.getByRole('region', { name: '伦敦入口' }).querySelector('.node-group-result');
    expect(result?.textContent).toBe('失败 1');
    expect(result?.classList.contains('bad')).toBe(true);
    expect(view.getByRole('button', { name: '拨测 伦敦入口' }).textContent).toBe('重试');
    expect(view.getByRole('button', { name: '拨测全部' }).classList.contains('node-probe-progress')).toBe(false);
    expect(view.getByText('通过 1 · 运行 0 · 等待 0 · 失败 1').closest('[role="status"]')).toBeTruthy();
    expect(view.baseElement.textContent).not.toContain('88ms');

    fireEvent.click(view.getByRole('button', { name: '重试失败项' }));
    await waitFor(() => expect(posts(fetchMock)).toEqual([{ item_ids: [] }, { item_ids: ['g-friendly:ipv4:vless'] }]));
    await waitFor(() => expect(vless.classList.contains('is-failed')).toBe(false));
    expect(view.queryByRole('button', { name: '重试失败项' })).toBeNull();
  });

  it('polls a running job to completion when the SSE stream delivers no snapshots', async () => {
    class SilentEventSource {
      onerror: (() => void) | null = null;
      addEventListener() {}
      close() {}
    }
    vi.stubGlobal('EventSource', SilentEventSource);
    const runningJob: GrantProbeJob = {
      ...finishedJob,
      id: 'p-live',
      status: 'running',
      message: null,
      finished_at_unix_secs: null,
      items: plan.items.map(item => ({ ...item, status: 'running', ttfb_ms: null, detail: null })),
    };
    const fetchMock = probeFetch((path, init) => {
      if (path === planPath && init?.method === 'POST') return jsonResponse({ job: runningJob, reused: false });
      if (path === '/grant-probes/p-live') return jsonResponse({ ...finishedJob, id: 'p-live' });
      return undefined;
    });
    vi.stubGlobal('fetch', fetchMock);
    const view = renderDialog();

    await view.findByRole('region', { name: '伦敦入口' });
    fireEvent.click(view.getByRole('button', { name: '拨测全部' }));
    await view.findByRole('button', { name: '取消拨测' });
    await waitFor(() => expect(lampOf(address(view, '复制 伦敦入口（IPv4）'))?.className).toContain('ok'), {
      timeout: 3_000,
    });
    expect(fetchMock.mock.calls.some(([input]) => String(input) === '/grant-probes/p-live')).toBe(true);
  });

  it('explains a superseded round in the content tally', async () => {
    const supersededJob: GrantProbeJob = {
      ...finishedJob,
      status: 'superseded',
      message: 'Serving 状态已变化，本轮结果已作废，请重新拨测',
      items: plan.items.map(item => ({ ...item, status: 'canceled', ttfb_ms: null, detail: null })),
    };
    vi.stubGlobal(
      'fetch',
      probeFetch((path, init) =>
        path === planPath && init?.method === 'POST' ? jsonResponse({ job: supersededJob, reused: false }) : undefined,
      ),
    );
    const view = renderDialog();

    await view.findByRole('region', { name: '伦敦入口' });
    fireEvent.click(view.getByRole('button', { name: '拨测全部' }));
    const note = await view.findByText('Serving 状态已变化，本轮结果已作废，请重新拨测');
    expect(note.closest('.node-probe-tally')?.textContent).toContain('通过 0 · 运行 0 · 等待 0 · 失败 0');
    expect(address(view, '复制 伦敦入口（IPv4）').getAttribute('aria-description')).toBe('拨测：已取消');
  });

  it('keeps probing disabled and explains why when the local Xray capability is unavailable', async () => {
    const fetchMock = probeFetch(path =>
      path === '/grant-probes/capability'
        ? jsonResponse({
            available: false,
            version: null,
            reason: 'Console 未安装或无法执行拨测 Xray',
            concurrency: 30,
          })
        : undefined,
    );
    vi.stubGlobal('fetch', fetchMock);
    const view = renderDialog();

    await view.findByRole('region', { name: '伦敦入口' });
    expect(view.getByText('拨测不可用：Console 未安装或无法执行拨测 Xray')).toBeTruthy();
    expect((view.getByRole('button', { name: '拨测全部' }) as HTMLButtonElement).disabled).toBe(true);
    // 不能拨测时列表保持原样：不留状态灯位置，组头没有拨测按钮，也不读计划。
    expect(lampOf(address(view, '复制 伦敦入口（IPv4）'))).toBeNull();
    expect(view.queryByRole('button', { name: '拨测 伦敦入口' })).toBeNull();
    expect(view.queryByText('拨测使用真实凭据，流量计入该用户用量')).toBeNull();
    expect(fetchMock.mock.calls.some(([input]) => String(input) === planPath)).toBe(false);
  });

  it('clears a removed Serving plan instead of retaining old entries or completed job results', async () => {
    let revoked = false;
    vi.stubGlobal(
      'fetch',
      probeFetch((path, init) => {
        if (path === planPath && init?.method === 'POST') return jsonResponse({ job: finishedJob, reused: false });
        if (path === planPath && revoked)
          return Response.json({ error: 'has no effective serving grants' }, { status: 404 });
        return undefined;
      }),
    );
    const view = renderDialog();

    await view.findByRole('region', { name: '伦敦入口' });
    fireEvent.click(view.getByRole('button', { name: '拨测全部' }));
    await waitFor(() => expect(lampOf(address(view, '复制 伦敦入口（IPv4）'))?.className).toContain('ok'));
    revoked = true;
    await act(async () => {
      await view.client.invalidateQueries({ queryKey: ['grant-probe-plan'] });
    });

    await waitFor(() => expect(lampOf(address(view, '复制 伦敦入口（IPv4）'))).toBeNull());
    expect(view.queryByRole('button', { name: '拨测 伦敦入口' })).toBeNull();
    expect((view.getByRole('button', { name: '拨测全部' }) as HTMLButtonElement).disabled).toBe(true);
  });

  it('keeps the latest round when the dialog is closed and opened again', async () => {
    const fetchMock = probeFetch((path, init) =>
      path === planPath && init?.method === 'POST' ? jsonResponse({ job: finishedJob, reused: false }) : undefined,
    );
    vi.stubGlobal('fetch', fetchMock);
    const view = renderDialog();

    await view.findByRole('region', { name: '伦敦入口' });
    fireEvent.click(view.getByRole('button', { name: '拨测全部' }));
    await view.findByText('通过 2 · 运行 0 · 等待 0 · 失败 0');
    view.rerender(<QueryClientProvider client={view.client}>{null}</QueryClientProvider>);
    expect(view.queryByRole('dialog')).toBeNull();

    view.rerender(dialog(view.client));
    await view.findByRole('region', { name: '伦敦入口' });
    expect(lampOf(address(view, '复制 伦敦入口（IPv4）'))?.className).toContain('ok');
    expect(view.getByText('通过 2 · 运行 0 · 等待 0 · 失败 0')).toBeTruthy();
    expect(posts(fetchMock)).toHaveLength(1);
  });

  it('refreshes only the edited user until the bounded Serving propagation window ends', async () => {
    vi.useFakeTimers({ toFake: ['Date', 'setInterval', 'clearInterval', 'setTimeout', 'clearTimeout'] });
    vi.setSystemTime(new Date('2026-09-28T00:00:00Z'));
    // RTL 的 findBy / waitFor 不会推进 Vitest 的假时钟：首屏需要的读取预先放进缓存，之后只用 act 推进时间。
    const client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
    client.setQueryData(['grant-probe-capability'], {
      available: true,
      version: '26.4.25',
      reason: null,
      concurrency: 30,
    });
    client.setQueryData(['grant-probe-plan', 'platform.acme', 'alice'], plan);
    client.setQueryData(['revisions'], { current_revision: 582, revisions: [] });
    client.setQueryData(['artifact', 582, 'user', 'platform.acme:alice', 'uri', 'both', 'both', true], {
      revision: plan.serving_revision,
      target_kind: 'user',
      target_id: 'platform.acme:alice',
      artifact_kind: 'uri',
      state: 'present',
      sha256: 'abc',
      byte_len: 0,
      content: `${uriText}\n${tokyoLink}\n`,
      redacted: false,
    });
    let servingCaughtUp = false;
    const fetchMock = probeFetch(path =>
      path === planPath
        ? jsonResponse(servingCaughtUp ? { ...plan, serving_generation: 20, items: [...plan.items, tokyo] } : plan)
        : undefined,
    );
    vi.stubGlobal('fetch', fetchMock);
    const planReads = () => fetchMock.mock.calls.filter(([input]) => String(input) === planPath).length;
    const view = render(dialog(client));
    const edit = async (userId: string) => {
      const mutation = client.getMutationCache().build(client, {
        mutationKey: ['grant'],
        mutationFn: async () => ({ revision_id: 583 }),
      });
      await act(async () => {
        await mutation.execute({
          app_id: 'app-global',
          tenant_id: 'platform.acme',
          user_id: userId,
          ingress_id: 'in-green',
          enabled: true,
        });
        await vi.advanceTimersByTimeAsync(1);
      });
    };
    try {
      await act(async () => {
        await vi.advanceTimersByTimeAsync(10);
      });
      expect(view.getByRole('region', { name: '东京入口' })).toBeTruthy();
      // 计划里还没有东京入口：对不上的链接不画灯。
      expect(lampOf(address(view, '复制 东京入口（IPv4）'))?.className).toBe('node-copy-lamp');
      await edit('bob');
      await act(async () => {
        await vi.advanceTimersByTimeAsync(6_000);
      });
      expect(planReads()).toBe(0);
      await edit('alice');
      await act(async () => {
        await vi.advanceTimersByTimeAsync(3_010);
      });
      expect(planReads()).toBe(1);
      servingCaughtUp = true;
      await act(async () => {
        await vi.advanceTimersByTimeAsync(3_010);
      });
      expect(lampOf(address(view, '复制 东京入口（IPv4）'))?.className).toBe('node-copy-lamp');
      await act(async () => {
        await vi.advanceTimersByTimeAsync(60_000);
      });
      const count = planReads();
      expect(count).toBeGreaterThan(1);
      expect(count).toBeLessThanOrEqual(21);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(90_000);
      });
      expect(planReads()).toBe(count);
      view.unmount();
      await edit('alice');
      await act(async () => {
        await vi.advanceTimersByTimeAsync(10_000);
      });
      expect(planReads()).toBe(count);
    } finally {
      view.unmount();
      client.clear();
    }
  });
});

describe('probe items paired with node links', () => {
  it('does not light links whose duplicate names cannot be paired one to one', async () => {
    // 同一链路的两个接入点名称相同；uri.txt 只列出其中一个，无法确定剩下的链接属于哪个接入点。
    const twinPlan: GrantProbePlan = {
      ...plan,
      items: [plan.items[0], { ...plan.items[0], id: 'g-twin:ipv4:vless', ingress_id: 'in-twin' }, plan.items[1]],
    };
    vi.stubGlobal(
      'fetch',
      probeFetch(path => (path === planPath ? jsonResponse(twinPlan) : undefined)),
    );
    const view = renderDialog();

    await view.findByRole('region', { name: '伦敦入口' });
    expect(lampOf(address(view, '复制 伦敦入口（IPv4）'))?.className).toBe('node-copy-lamp');
    expect(lampOf(address(view, '复制 伦敦入口 | QUIC | v6（IPv6）'))?.className).toBe('node-copy-lamp');
  });

  it('pairs duplicate names in order when every entry has a link', async () => {
    const twinPlan: GrantProbePlan = {
      ...plan,
      items: [plan.items[0], { ...plan.items[0], id: 'g-twin:ipv4:vless', ingress_id: 'in-twin' }],
    };
    const twinLink = vlessLink.replace('lon.example', 'lon-2.example');
    const twinJob: GrantProbeJob = {
      ...finishedJob,
      items: [
        { ...twinPlan.items[0], status: 'passed', ttfb_ms: null, detail: null },
        { ...twinPlan.items[1], status: 'failed', ttfb_ms: null, detail: '连接超时' },
      ],
    };
    vi.stubGlobal(
      'fetch',
      probeFetch((path, init) => {
        if (path === planPath && init?.method === 'POST') return jsonResponse({ job: twinJob, reused: false });
        if (path === planPath) return jsonResponse(twinPlan);
        if (path === linkPath) return artifact([vlessLink, twinLink, ''].join('\n'));
        return undefined;
      }),
    );
    const view = renderDialog();

    await view.findByRole('region', { name: '伦敦入口' });
    fireEvent.click(view.getByRole('button', { name: '拨测全部' }));
    await view.findByText('连接超时');
    const cells = view.getAllByRole('button', { name: '复制 伦敦入口（IPv4）' });
    expect(cells.map(cell => [cell.title.split(' · ')[0], cell.getAttribute('aria-description')])).toEqual([
      ['lon.example:443', '拨测：通过'],
      ['lon-2.example:443', '拨测：失败（连接超时）'],
    ]);
  });

  it('treats a VLESS Encryption entry like every other listed node', async () => {
    const encryption = {
      ...plan.items[0],
      id: 'g-friendly:ipv4:vless-encryption',
      name: `${GB}伦敦入口 | VLESS Encryption`,
      protocol: 'vless-encryption' as const,
    };
    const encryptionLink = `vless://u@lon.example:8444?encryption=mlkem768x25519plus.native.0rtt.K&type=xhttp&security=none#${named(encryption.name)}`;
    const fetchMock = probeFetch((path, init) => {
      if (path === planPath && init?.method === 'POST') return jsonResponse({ job: finishedJob, reused: false });
      if (path === planPath) return jsonResponse({ ...plan, items: [...plan.items, encryption] });
      if (path === linkPath) return artifact(`${uriText}\n${encryptionLink}\n`);
      return undefined;
    });
    vi.stubGlobal('fetch', fetchMock);
    const view = renderDialog();

    await view.findByRole('region', { name: '伦敦入口' });
    expect(lampOf(address(view, '复制 伦敦入口 | VLESS Encryption（IPv4）'))?.className).toBe('node-copy-lamp');
    fireEvent.click(view.getByRole('button', { name: '拨测全部' }));
    await waitFor(() => expect(posts(fetchMock)).toEqual([{ item_ids: [] }]));
  });

  it('reads the self-service listing from the user artifact endpoint and probes the own plan', async () => {
    const fetchMock = probeFetch((path, init) => {
      if (path === '/me/artifact?insecure=true') return artifact(uriText);
      if (path === planPath && init?.method === 'POST') return jsonResponse({ job: finishedJob, reused: false });
      return undefined;
    });
    vi.stubGlobal('fetch', fetchMock);
    const view = renderDialog(true);

    await view.findByRole('region', { name: '伦敦入口' });
    fireEvent.click(view.getByRole('button', { name: '拨测全部' }));
    await view.findByText('通过 2 · 运行 0 · 等待 0 · 失败 0');
    expect(fetchMock.mock.calls.some(([input]) => String(input) === linkPath)).toBe(false);
  });
});
