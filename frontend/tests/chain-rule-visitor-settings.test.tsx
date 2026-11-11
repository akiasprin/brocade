import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { SnapshotChain, SnapshotIngress, SnapshotStep } from '../src/api';
import { ChainRulesPanel, IngressStreamRow } from '../src/panes/chains';
import { RuleEditor } from '../src/panes/rules';

const chain: SnapshotChain = { id: 'chain-1', tenant: 'platform', name: '访客链路' };
const step: SnapshotStep = {
  chain: chain.id,
  node: 'entry',
  accept: null,
  hop_in: null,
  rules: [],
};
const ingress: SnapshotIngress = {
  id: 'ingress-1',
  chain: chain.id,
  node: 'entry',
  bind: '0.0.0.0',
  port: 443,
  projection: {},
  guard: {
    no_private: true,
    no_bittorrent: true,
    no_mail: true,
    no_udp_amplification: true,
    tcp_and_quic_only: false,
    cn_source_block: 'off',
  },
  identity: { public_key: 'public-key', short_ids: ['0123456789abcdef'] },
  wires: { vless: { kind: 'vless-reality' } },
};

async function visitorClientWithCachedForbidden(): Promise<QueryClient> {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity }, mutations: { retry: false } },
  });
  client.setQueryData(['snapshot'], { snapshot: { apps: [], nodes: [] } });
  client.setQueryData(['nodes'], { nodes: [] });
  client.setQueryData(['revisions'], { current_revision: null });
  await client
    .fetchQuery({
      queryKey: ['settings'],
      queryFn: async () => {
        throw new Error('forbidden');
      },
    })
    .catch(() => undefined);
  return client;
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

/** 机器行头右侧的本机监听摘要。 */
function machineHop(view: ReturnType<typeof render>, node: string) {
  const head = [...view.container.querySelectorAll<HTMLElement>('.chain-rule-node-head')].find(
    candidate => candidate.querySelector('.who b')?.textContent === node,
  );
  return head?.querySelector('.m-hop')?.textContent;
}

describe('访客查看链路规则', () => {
  it.each([undefined, 'target'])('默认只展开入口或当前机器（当前机器：%s）', async selected => {
    const client = await visitorClientWithCachedForbidden();
    const steps: SnapshotStep[] = [
      {
        ...step,
        rules: [{ m: { t: 'any' }, a: { t: 'forward', to: 'target', dial: { t: 'overlay' }, pool: { t: 'none' } } }],
      },
      { ...step, node: 'target' },
      { ...step, node: 'orphan' },
    ];
    const view = render(
      <QueryClientProvider client={client}>
        <ChainRulesPanel
          appId="app-1"
          chain={chain}
          spine={['entry', 'target']}
          steps={steps}
          nodes={[]}
          selected={selected}
          readOnly
          settingsReadable={false}
        />
      </QueryClientProvider>,
    );
    expect(view.getByRole('button', { name: /^entry/ }).getAttribute('aria-expanded')).toBe(
      String(selected !== 'target'),
    );
    expect(view.getByRole('button', { name: /^target/ }).getAttribute('aria-expanded')).toBe(
      String(selected === 'target'),
    );
    expect(view.getByRole('button', { name: /^orphan/ }).getAttribute('aria-expanded')).toBe('false');
    expect(view.container.querySelectorAll('.chain-rule-body:not([hidden])')).toHaveLength(1);
  });

  it('按需展开后折叠仍保留未保存输入，并支持键盘展开', async () => {
    const client = await visitorClientWithCachedForbidden();
    client.setQueryData(['settings'], { ports: { hop_base: 20000 } });
    const steps: SnapshotStep[] = [
      {
        ...step,
        rules: [{ m: { t: 'any' }, a: { t: 'forward', to: 'target', dial: { t: 'overlay' }, pool: { t: 'none' } } }],
      },
      {
        ...step,
        node: 'target',
        rules: [{ m: { t: 'domain_suffix', v: ['before.example'] }, a: { t: 'egress', send_through: null } }],
      },
    ];
    const view = render(
      <QueryClientProvider client={client}>
        <ChainRulesPanel appId="app-1" chain={chain} spine={['entry', 'target']} steps={steps} nodes={[]} />
      </QueryClientProvider>,
    );
    const head = await view.findByRole('button', { name: /^target/ });
    expect(head.getAttribute('aria-expanded')).toBe('false');
    fireEvent.keyDown(head, { key: 'Enter' });
    expect(head.getAttribute('aria-expanded')).toBe('true');
    const input = view.getByDisplayValue('before.example') as HTMLInputElement;
    fireEvent.change(input, { target: { value: 'unsaved.example' } });
    fireEvent.click(head);
    expect(head.getAttribute('aria-expanded')).toBe('false');
    expect(input.closest('.chain-rule-body')?.hasAttribute('hidden')).toBe(true);
    expect(input.isConnected).toBe(true);
    fireEvent.keyDown(head, { key: ' ' });
    expect(head.getAttribute('aria-expanded')).toBe('true');
    expect(view.getByDisplayValue('unsaved.example')).toBe(input);
  });

  it.each([undefined, chain.name])('只读规则树沿用机器行头并忽略设置 403（根标签：%s）', async rootLabel => {
    const client = await visitorClientWithCachedForbidden();
    const fetcher = vi.fn(async () => {
      throw new Error('访客视角不应再发请求');
    });
    vi.stubGlobal('fetch', fetcher);

    const view = render(
      <QueryClientProvider client={client}>
        <ChainRulesPanel
          appId="app-1"
          chain={chain}
          spine={['entry']}
          steps={[step]}
          nodes={[]}
          rootLabel={rootLabel}
          readOnly
          settingsReadable={false}
        />
      </QueryClientProvider>,
    );

    expect(view.queryByText('forbidden')).toBeNull();
    expect(view.getAllByText('entry').length).toBeGreaterThan(0);
    await waitFor(() => expect(view.container.querySelector('.rule-editor')).toBeTruthy());
    expect(view.container.querySelector('.rule-editor-title')).toBeNull();
    if (rootLabel) expect(view.getByText(rootLabel)).toBeTruthy();
    await waitFor(() => expect(fetcher).not.toHaveBeenCalled());
  });

  it('独立打开的只读规则表仍保留所属链与机器的标题', async () => {
    const client = await visitorClientWithCachedForbidden();
    const view = render(
      <QueryClientProvider client={client}>
        <RuleEditor
          appId="app-1"
          chainId={chain.id}
          nodeId={step.node}
          initial={step.rules}
          accept={null}
          peers={[]}
          isForwardTarget={false}
          readOnly
        />
      </QueryClientProvider>,
    );

    await waitFor(() => expect(view.container.querySelector('.rule-editor')).toBeTruthy());
    expect(view.container.querySelector('.rule-editor-title')?.textContent).toBe('chain-1 / entry');
  });

  it('接入面只读渲染不再尝试读取 settings', async () => {
    const client = await visitorClientWithCachedForbidden();
    const fetcher = vi.fn(async () => {
      throw new Error('访客视角不应再发请求');
    });
    vi.stubGlobal('fetch', fetcher);

    const view = render(
      <QueryClientProvider client={client}>
        <IngressStreamRow appId="app-1" ingress={ingress} editable={false} settingsReadable={false} section="vless" />
      </QueryClientProvider>,
    );

    expect(view.getByText('REALITY 目标站点')).toBeTruthy();
    expect(view.getByRole('option', { name: '全局站点' })).toBeTruthy();
    expect(view.queryByRole('option', { name: '全局站点（未配置）' })).toBeNull();
    await waitFor(() => expect(fetcher).not.toHaveBeenCalled());
  });

  it('详情只显示普通跳实际监听方自己的端口', async () => {
    const client = await visitorClientWithCachedForbidden();
    const steps: SnapshotStep[] = [
      {
        chain: chain.id,
        node: 'entry',
        accept: null,
        // The entry does not listen for this ordinary forward hop.
        hop_in: { port: 20002, security: { t: 'none' } },
        rules: [
          {
            m: { t: 'any' },
            a: {
              t: 'forward',
              to: 'target',
              dial: { t: 'addr', v: '192.0.2.2:20000' },
              pool: { t: 'none' },
            },
          },
        ],
      },
      {
        chain: chain.id,
        node: 'target',
        accept: null,
        hop_in: { port: 20000, security: { t: 'none' } },
        rules: [],
      },
    ];

    const view = render(
      <QueryClientProvider client={client}>
        <ChainRulesPanel
          appId="app-1"
          chain={chain}
          spine={['entry', 'target']}
          steps={steps}
          nodes={[]}
          readOnly
          settingsReadable={false}
        />
      </QueryClientProvider>,
    );

    // 机器行头右侧是这台机器自己实际监听的端口与承载；入口只发起连接，不监听。
    expect(machineHop(view, 'entry')).toBe('—');
    expect(machineHop(view, 'target')).toBe('20000 / VLESS-NONE');
    const ownedListenerPanel = [...view.container.querySelectorAll<HTMLElement>('.hop-target-panel')].find(panel =>
      panel.querySelector('.panel-title')?.textContent?.includes('下一跳'),
    );
    expect(ownedListenerPanel).toBeTruthy();
    expect(ownedListenerPanel?.querySelector('.listener-reference-copy small')).toBeNull();
    expect(ownedListenerPanel?.textContent).not.toContain('按规则地址直连');
    expect(ownedListenerPanel?.textContent).not.toContain('仅经 Overlay 接入');
  });

  it('反向跳只在上游监听方显示一次端口', async () => {
    const client = await visitorClientWithCachedForbidden();
    const steps: SnapshotStep[] = [
      {
        chain: chain.id,
        node: 'entry',
        accept: null,
        hop_in: { port: 20002, security: { t: 'encryption', v: { public_key: 'entry-public' } } },
        rules: [
          {
            m: { t: 'any' },
            a: { t: 'forward', to: 'target', dial: { t: 'reverse', v: 'v4' }, pool: { t: 'none' } },
          },
        ],
      },
      {
        chain: chain.id,
        node: 'target',
        accept: null,
        // Historical residue from when the same edge was normal.
        hop_in: { port: 20001, security: { t: 'none' } },
        rules: [],
      },
    ];

    const view = render(
      <QueryClientProvider client={client}>
        <ChainRulesPanel
          appId="app-1"
          chain={chain}
          spine={['entry', 'target']}
          steps={steps}
          nodes={[]}
          readOnly
          settingsReadable={false}
        />
      </QueryClientProvider>,
    );

    // 反向时端口开在上游自己身上；下游残留的旧 hop_in 不应冒充一个在用的监听。
    expect(machineHop(view, 'entry')).toBe('20002 / VLESS-ENCRY');
    expect(machineHop(view, 'target')).toBe('—');
    const reverseRow = [...view.container.querySelectorAll<HTMLElement>('.hop-target-panel .hop-target-row')].find(
      row => row.querySelector('.external-target-kind')?.textContent?.includes('反向'),
    );
    expect(reverseRow).toBeTruthy();
    expect(reverseRow?.querySelector('.listener-reference-copy b')?.textContent).toContain('20002');
    expect(reverseRow?.querySelector('.listener-reference-copy small')?.textContent).toBe('target 从这里接入');
  });
});
