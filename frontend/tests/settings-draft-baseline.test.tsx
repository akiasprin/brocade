/* 设置页的分段保存写的是草稿（`saveSettings` → `draft.push({op:'update_settings'})`），
 * 而基准 `pristine` 读的是 `GET /settings`——直连接口，草稿提交前不会变。
 *
 * 与机器详情页那三处是同一个根因（见 draft-discard-refresh.test.tsx），但症状不同：
 * 表单里的值不会回落（TanStack 的结构共享让 `settings.data` 引用不变，重填分支不触发），
 * 变的是「这一段有没有未保存的改动」这个判断——它永远为真。
 *
 * 取数路径必须是真的：stub 的是 `fetch`，不是预置 `setQueryData`。 */
import { useEffect, useState } from 'react';
import { QueryClient, QueryClientProvider, useQueryClient } from '@tanstack/react-query';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

/* settings.tsx 经 ui/branding → … 在模块求值期读 matchMedia，jsdom 没有实现。
   import 是提升的，所以装在这里而不是 beforeEach。 */
window.matchMedia = ((query: string) => ({
  matches: false,
  media: query,
  onchange: null,
  addEventListener: () => {},
  removeEventListener: () => {},
  addListener: () => {},
  removeListener: () => {},
  dispatchEvent: () => false,
})) as unknown as typeof window.matchMedia;

const { draft } = await import('../src/draft');
const { SessionProvider } = await import('../src/session');
const { SettingsPane } = await import('../src/panes/settings');

const SESSION = {
  who: {
    operator_id: 'tester',
    role: 'system-admin' as const,
    tenant_scope: null,
    token_prefix: null,
    masked_assets: false,
  },
};

/** 已提交的全局设置。分段保存写草稿，这一份始终不变——正是问题所在。 */
const committedSettings = () => ({
  reality_client: { min_client_ver: null, max_client_ver: null, max_time_diff_ms: null },
  reality_site: {
    dest: 'www.committed.example:443',
    server_names: ['www.committed.example'],
    fingerprint: 'chrome',
    flow: 'xtls-rprx-vision',
  },
  overlay: { keepalive_secs: 25, mtu: 1420, disabled_links: [] },
  ports: { ingress_base: 8443, anytls_base: 16000, hop_base: 20000, hy2_base: 18000 },
  probe: { endpoint_url: 'http://cp.cloudflare.com/cdn-cgi/trace', timeout_secs: 10, interval_secs: 60 },
  geodata: { cron: 'CRON_TZ=Asia/Shanghai 30 6 * * *', geoip_url: 'https://geoip', geosite_url: 'https://geosite' },
  connection: {
    conn_idle_secs: 300,
    uplink_only_secs: 2,
    downlink_only_secs: 5,
    buffer_size_kb: null,
    handshake_secs: 60,
  },
  anytls_padding_scheme: ['stop=4'],
  stats_user_online: false,
});

const ROUTES: Record<string, () => unknown> = {
  '/settings': committedSettings,
  '/branding': () => ({ site_name: 'brocade', icon: null, accent: null }),
  '/auth/state': () => ({ public_open: false }),
  '/certs': () => ({ groups: [], nodes: [], letsencrypt: 'https://acme', letsencrypt_staging: 'https://acme-staging' }),
  '/distribution': () => ({
    stored: { agent_public_url: 'https://example', xray_version: '26.7.28' },
    effective: { agent_public_url: 'https://example', xray_version: '26.7.28' },
  }),
  '/agent-log-policy': () => ({ global_max_mib: 100, nodes: [] }),
  '/ping-probe/settings': () => ({ targets: [], interval_secs: 60, timeout_ms: 420 }),
  '/links/mtu': () => ({ default_mtu: 1420, nodes: [], links: [] }),
  '/revisions?limit=50': () => ({ current_revision: 7, revisions: [{ id: 7 }] }),
};

const certsWithGroup = () => ({
  sealing_available: true,
  domain: {
    id: 'private.example',
    domain: 'private.example',
    dns_provider: 'cloudflare',
    acme_directory: 'self-signed',
    signing_method: 'self-signed',
    acme_contact: null,
    renew_before_days: 30,
    has_credential: false,
    has_account: false,
  },
  groups: [
    {
      id: 'group-1',
      domain: 'private.example',
      label: 'a1b2c3d4',
      name: 'CA-1',
      note: null,
      status: 'active',
      names: ['*.a1b2c3d4.private.example', 'a1b2c3d4.private.example'],
      nodes: [],
      certificates: [],
    },
  ],
  nodes: [],
  letsencrypt: 'https://acme',
  letsencrypt_staging: 'https://acme-staging',
});

function stubFetch() {
  vi.stubGlobal(
    'fetch',
    vi.fn(async (path: string) => {
      const body = ROUTES[path];
      if (!body) throw new Error(`未预期的请求：${path}`);
      return new Response(JSON.stringify(body()), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      });
    }),
  );
}

/* 复刻 forge/shell.tsx 的集中失效器：草稿任何变动都失效这两个键。生产里它始终挂载。 */
function ShellDraftInvalidation() {
  const qc = useQueryClient();
  useEffect(
    () =>
      draft.subscribe(() => {
        qc.invalidateQueries({ queryKey: ['snapshot'] });
        qc.invalidateQueries({ queryKey: ['compile'] });
      }),
    [qc],
  );
  return null;
}

function Harness({ role = 'system-admin' }: { role?: 'system-admin' | 'editor' }) {
  const [client] = useState(
    () => new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } }),
  );
  return (
    <QueryClientProvider client={client}>
      <SessionProvider value={{ who: { ...SESSION.who, role } }}>
        <ShellDraftInvalidation />
        <SettingsPane />
      </SessionProvider>
    </QueryClientProvider>
  );
}

/** 段标题栏。段内的「保存这一段」「有未保存的改动」都在它里面。 */
const header = (id: string) => {
  const section = document.getElementById(id);
  if (!section) throw new Error(`没有找到设置段 ${id}`);
  return within(section.querySelector('header') as HTMLElement);
};

const settingsOp = () => draft.ops().find(op => op.op === 'update_settings');

beforeEach(() => {
  draft.init(`settings-baseline-${Math.random()}`);
  draft.clear();
  stubFetch();
});

afterEach(() => {
  cleanup();
  draft.clear();
  vi.unstubAllGlobals();
});

describe('设置页分段保存的基准', () => {
  it('添加备用证书期间锁住证书动作，连续点击只提交一次', async () => {
    let resolveSpare!: (response: Response) => void;
    const spareResponse = new Promise<Response>(resolve => {
      resolveSpare = resolve;
    });
    const fetchMock = vi.fn(async (path: string, init?: RequestInit) => {
      if (path === '/certs/groups/group-1/spare' && init?.method === 'POST') return spareResponse;
      const body = path === '/certs' ? certsWithGroup : ROUTES[path];
      if (!body) throw new Error(`未预期的请求：${path}`);
      return new Response(JSON.stringify(body()), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      });
    });
    vi.stubGlobal('fetch', fetchMock);

    render(<Harness />);
    const add = await screen.findByRole('button', { name: '加一张备用' });
    fireEvent.click(add);
    fireEvent.click(add);

    await waitFor(() => {
      const posts = fetchMock.mock.calls.filter(
        ([path, init]) => path === '/certs/groups/group-1/spare' && init?.method === 'POST',
      );
      expect(posts).toHaveLength(1);
    });
    expect((screen.getByRole('button', { name: '添加中…' }) as HTMLButtonElement).disabled).toBe(true);

    resolveSpare(
      new Response(JSON.stringify({ id: 'spare-1' }), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      }),
    );
    await waitFor(() =>
      expect((screen.getByRole('button', { name: '加一张备用' }) as HTMLButtonElement).disabled).toBe(false),
    );
  });

  it('证书组创建失败时保留表单和输入，不把失败表现成已完成', async () => {
    const fetchMock = vi.fn(async (path: string, init?: RequestInit) => {
      if (path === '/certs/groups' && init?.method === 'POST') {
        return new Response(JSON.stringify({ error: '证书组写入失败' }), {
          status: 500,
          headers: { 'content-type': 'application/json' },
        });
      }
      const body = path === '/certs' ? certsWithGroup : ROUTES[path];
      if (!body) throw new Error(`未预期的请求：${path}`);
      return new Response(JSON.stringify(body()), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      });
    });
    vi.stubGlobal('fetch', fetchMock);

    render(<Harness />);
    fireEvent.click(await screen.findByRole('button', { name: '新建证书组' }));
    const name = screen.getByPlaceholderText('香港前置') as HTMLInputElement;
    fireEvent.change(name, { target: { value: '新加坡备用' } });
    fireEvent.click(screen.getByRole('button', { name: '保存' }));

    await screen.findByText(/证书组写入失败/);
    expect(screen.getByPlaceholderText('香港前置')).toBe(name);
    expect(name.value).toBe('新加坡备用');
  });

  it('非系统管理员看到的设置控件是真只读，不会产生无法保存的脏表单', async () => {
    render(<Harness role="editor" />);

    const dest = (await screen.findByPlaceholderText('example.com:443')) as HTMLInputElement;
    expect(dest.matches(':disabled')).toBe(true);
    expect(header('set-xray').getByRole('button', { name: '保存这一段' }).matches(':disabled')).toBe(true);
  });

  it('保存一段之后该段不再显示「有未保存的改动」', async () => {
    render(<Harness />);

    const dest = (await screen.findByPlaceholderText('example.com:443')) as HTMLInputElement;
    expect(dest.value).toBe('www.committed.example:443');

    fireEvent.change(dest, { target: { value: 'www.edited.example:443' } });
    expect(header('set-xray').getByText('有未保存的改动')).toBeTruthy();

    fireEvent.click(header('set-xray').getByText('保存这一段'));

    /* 改动确实进了草稿 */
    await waitFor(() =>
      expect(settingsOp()).toMatchObject({ settings: { reality_site: { dest: 'www.edited.example:443' } } }),
    );

    /* 但基准仍是 GET /settings 的已提交值，该段会一直自认为有未保存的改动 */
    await waitFor(() => expect(header('set-xray').queryByText('有未保存的改动')).toBeNull());
  });

  it('保存另一段不会把前一段已入草稿的改动写回旧值', async () => {
    render(<Harness />);

    const dest = (await screen.findByPlaceholderText('example.com:443')) as HTMLInputElement;
    fireEvent.change(dest, { target: { value: 'www.edited.example:443' } });
    fireEvent.click(header('set-xray').getByText('保存这一段'));
    await waitFor(() =>
      expect(settingsOp()).toMatchObject({ settings: { reality_site: { dest: 'www.edited.example:443' } } }),
    );

    /* 再改另一段并保存。两段互不相干，前一段已经在草稿里，不应被这次保存覆盖回去。 */
    const hopBase = (await screen.findByDisplayValue('20000')) as HTMLInputElement;
    fireEvent.change(hopBase, { target: { value: '20100' } });
    fireEvent.click(header('set-ports').getByText('保存这一段'));

    await waitFor(() => expect(settingsOp()).toMatchObject({ settings: { ports: { hop_base: 20100 } } }));
    expect(settingsOp()).toMatchObject({ settings: { reality_site: { dest: 'www.edited.example:443' } } });
  });

  /* 分段保存的本意：保存 A 段时，B 段「改了但没点保存」的输入既不能被提交，也不能被抹掉。
     基准改成草稿之后这条仍须成立——重填表单的触发条件因此没有跟着草稿走。 */
  it('保存一段不会提交、也不会抹掉另一段未保存的输入', async () => {
    render(<Harness />);

    const dest = (await screen.findByPlaceholderText('example.com:443')) as HTMLInputElement;
    const hopBase = (await screen.findByDisplayValue('20000')) as HTMLInputElement;

    fireEvent.change(dest, { target: { value: 'www.edited.example:443' } });
    fireEvent.change(hopBase, { target: { value: '20100' } });

    fireEvent.click(header('set-xray').getByText('保存这一段'));

    await waitFor(() =>
      expect(settingsOp()).toMatchObject({ settings: { reality_site: { dest: 'www.edited.example:443' } } }),
    );
    /* 端口段没点保存，不进草稿 */
    expect(settingsOp()).toMatchObject({ settings: { ports: { hop_base: 20000 } } });
    /* 但输入框里的值要留着，并且该段仍标为有未保存的改动 */
    expect(hopBase.value).toBe('20100');
    expect(header('set-ports').getByText('有未保存的改动')).toBeTruthy();
  });
});
