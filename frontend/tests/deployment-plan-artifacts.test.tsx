import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { ArtifactIndexEntry, DeploymentPlan } from '../src/api';
import type { Win } from '../src/wm/store';

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

const { ArtifactChanges, DeployPane } = await import('../src/panes/deploy');
const { SessionProvider } = await import('../src/session');

const json = (body: unknown) =>
  new Response(JSON.stringify(body), { status: 200, headers: { 'content-type': 'application/json' } });

const rules = (generation: string, outbounds: string[]) =>
  `${JSON.stringify(
    {
      routing: {
        rules: outbounds.map((outboundTag, index) => ({
          ruleTag: `r:${generation}:${String(index).padStart(3, '0')}`,
          outboundTag,
        })),
      },
    },
    null,
    2,
  )}\n`;

const CONTENT: Record<string, { content: string; redacted: boolean }> = {
  'n1/xray?revision=21': { content: rules('bbbbbbbb', ['direct', 'proxy-b']), redacted: false },
  'n1/xray?revision=20': { content: rules('aaaaaaaa', ['direct', 'proxy-a']), redacted: false },
  'n2/wireguard?revision=21': { content: '[Interface]\nPrivateKey = <redacted>\nListenPort = 51821\n', redacted: true },
  'n2/wireguard?revision=20': { content: '[Interface]\nPrivateKey = <redacted>\nListenPort = 51820\n', redacted: true },
};

function stubFetch() {
  const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
    const path = String(input);
    const content = path.match(/^\/artifacts\/content\/node\/(.+)$/);
    if (content && CONTENT[content[1]]) return json(CONTENT[content[1]]);
    throw new Error(`未预期的请求：${path}`);
  });
  vi.stubGlobal('fetch', fetchMock);
  return fetchMock;
}
const contentRequests = (fetchMock: ReturnType<typeof stubFetch>) =>
  fetchMock.mock.calls.filter(([path]) => String(path).startsWith('/artifacts/content'));

const entry = (target_id: string, artifact_kind: string, sha256: string): ArtifactIndexEntry => ({
  target_kind: 'node',
  target_id,
  artifact_kind,
  state: 'present',
  sha256,
  byte_len: 64,
});

function queryClient() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Number.POSITIVE_INFINITY } },
  });
  client.setQueryData(['nodes'], {
    nodes: [
      { node_id: 'n1', name: '台北一号' },
      { node_id: 'n2', name: '东京二号' },
    ],
  });
  client.setQueryData(['artifact-index', 21], {
    revision: 21,
    artifacts: [entry('n1', 'xray', 'x21'), entry('n2', 'wireguard', 'w21')],
  });
  client.setQueryData(['artifact-index', 20], {
    revision: 20,
    artifacts: [entry('n1', 'xray', 'x20'), entry('n2', 'wireguard', 'w20')],
  });
  return client;
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe('创建变更单的产物差异', () => {
  it('一台一行；没展开的机器也读出增删，打码的文件写明原因', async () => {
    stubFetch();
    const view = render(
      <QueryClientProvider client={queryClient()}>
        <ArtifactChanges revision={21} base={20} targets={[{ node_id: 'n1' }, { node_id: 'n2' }]} />
      </QueryClientProvider>,
    );
    const xray = await view.findByRole('button', { name: /xray\.json/ });
    const wireguard = view.getByRole('button', { name: /wg0\.conf/ });
    await waitFor(() => expect(xray.textContent).toBe('xray.json+1−1'));
    await waitFor(() => expect(wireguard.textContent).toBe('wg0.conf+1−1'));
    expect(xray.getAttribute('aria-pressed')).toBe('true');
    expect(wireguard.getAttribute('aria-pressed')).toBe('false');
    // 规则表重算的 ruleTag 重新编号与变更单详情一样默认折叠
    expect(view.getByText(/ruleTag 重新编号 2 处，已折叠/)).toBeTruthy();
    expect(view.queryByText('私钥原文仅 system-admin 可见。')).toBeNull();

    fireEvent.click(wireguard);
    expect(await view.findByText('私钥原文仅 system-admin 可见。')).toBeTruthy();
    const second = wireguard.closest('.cga-node')!;
    expect(second.querySelector('tr.add')?.textContent).toContain('ListenPort = 51821');
    expect(second.querySelector('tr.del')?.textContent).toContain('ListenPort = 51820');
  });

  it('产物差异第一次展开时才拉取文件内容', async () => {
    const fetchMock = stubFetch();
    const client = queryClient();
    const plan: DeploymentPlan = {
      revision: 21,
      base_revision_id: 20,
      warnings: [],
      summary: {
        total_targets: 2,
        changed_targets: 2,
        skipped_targets: 0,
        deferred_targets: 0,
        disruptive_targets: 1,
        max_wave: 1,
      },
      targets: [
        { node_id: 'n1', status: 'pending', wave: 1, disruptive: true, actions: ['apply-xray'] },
        { node_id: 'n2', status: 'pending', wave: 0, disruptive: false, actions: ['apply-wire-guard'] },
      ],
    };
    client.setQueryData(['plan', 21], plan);
    client.setQueryData(['revisions'], {
      current_revision: 21,
      revisions: [
        {
          id: 21,
          created_at: '2026-10-10T02:00:00Z',
          author: 'release-reviewer',
          note: '调整出口',
          status: 'committed',
          current: true,
          has_snapshot: true,
        },
      ],
    });
    client.setQueryData(['deployments'], { deployments: [{ id: 4, revision_id: 20, status: 'succeeded' }] });
    const win: Win = {
      id: 92,
      key: 'tab:deploy',
      title: '发布',
      x: 0,
      y: 0,
      w: 980,
      h: 720,
      z: 1,
      min: false,
      home: 'desk',
      data: { drill: { p: 'plan', revision: 21, key: 'review-key' } },
    };
    const view = render(
      <QueryClientProvider client={client}>
        <SessionProvider
          value={{
            initial: { node_count: 2, chain_group_count: [] },
            who: {
              operator_id: 'release-reviewer',
              role: 'system-admin',
              tenant_scope: null,
              token_prefix: null,
              masked_assets: false,
            },
          }}
        >
          <DeployPane win={win} />
        </SessionProvider>
      </QueryClientProvider>,
    );

    const details = view.getByText('产物差异').closest('details')!;
    expect(details.open).toBe(false);
    expect(view.queryByRole('button', { name: /xray\.json/ })).toBeNull();
    expect(contentRequests(fetchMock)).toHaveLength(0);

    details.open = true;
    fireEvent(details, new Event('toggle'));
    expect(await view.findByRole('button', { name: /xray\.json/ })).toBeTruthy();
    // 两份变更文件，各拉本次与基线两个修订
    await waitFor(() => expect(contentRequests(fetchMock)).toHaveLength(4));
  });
});
