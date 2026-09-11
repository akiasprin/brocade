import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type {
  ConsoleSnapshot,
  NodeAgentStateItem,
  Rule,
  SnapshotApp,
  SnapshotChain,
  SnapshotIngress,
  SnapshotStep,
} from '../src/api';
import { draft } from '../src/draft';
import { ListenerDecisionTree } from '../src/panes/chains';
import { RuleEditor, forwardPeers, listenerRefKey, reusableListeners } from '../src/panes/rules';

const chain = (id: string, name: string, tenant = 'platform.acme'): SnapshotChain => ({ id, name, tenant });

const ingress = (id: string, owner: string, node: string): SnapshotIngress => ({
  id,
  chain: owner,
  node,
  bind: '0.0.0.0',
  port: 443,
  projection: {},
  guard: {
    no_private: true,
    no_bittorrent: true,
    no_mail: true,
    no_udp_amplification: true,
    tcp_and_quic_only: false,
  },
  identity: { public_key: 'public', short_ids: ['0123abcd'] },
  wires: { vless: { kind: 'vless-reality' } },
});

const egressRule = (): Rule => ({ m: { t: 'any' }, a: { t: 'egress', send_through: null } });
const forwardRule = (to: string): Rule => ({
  m: { t: 'any' },
  a: { t: 'forward', to, dial: { t: 'overlay' }, pool: { t: 'none' } },
});
const referenceRule = (owner: string, node: string, match: Rule['m'] = { t: 'any' }): Rule => ({
  m: match,
  a: {
    t: 'reuse_listener',
    listener: { chain: owner, node },
    dial: { t: 'overlay' },
    pool: { t: 'none' },
  },
});

const listenerStep = (owner: string, node: string, port: number, rules: Rule[]): SnapshotStep => ({
  chain: owner,
  node,
  accept: { uuid: `uuid-${owner}-${node}`, label: `${owner}@${node}` },
  hop_in: { port, security: { t: 'none' } },
  rules,
});

const node = (node_id: string, name: string, tenant_id = 'platform.acme') => ({
  node_id,
  name,
  tenant_id,
  public_ipv4: `${node_id}.example.net`,
  public_ipv6: null,
  public_ipv4_nat: false,
  public_ipv6_nat: false,
  retired_at: null,
});

afterEach(() => {
  cleanup();
  draft.clear();
  vi.unstubAllGlobals();
});

beforeEach(() => {
  draft.init(`listener-reuse-${Math.random()}`);
  draft.clear();
});

describe('监听规则子树复用', () => {
  it('普通本链转发也会检查引用子树中的跨链回路', () => {
    const source = listenerStep('source-chain', 'source', 21000, [egressRule()]);
    const candidate = listenerStep('source-chain', 'candidate', 21001, [referenceRule('owner', 'shared')]);
    const shared = listenerStep('owner', 'shared', 22000, [referenceRule('source-chain', 'source')]);
    const app: SnapshotApp = {
      id: 'app',
      label: '项目',
      chains: [chain('source-chain', '源线路'), chain('owner', '共享出口')],
      ingresses: [ingress('source-in', 'source-chain', 'source'), ingress('owner-in', 'owner', 'owner-root')],
      steps: [
        source,
        candidate,
        { chain: 'owner', node: 'owner-root', accept: null, hop_in: null, rules: [egressRule()] },
        shared,
      ],
      fronts: [],
      grants: [],
    };

    const peers = forwardPeers({
      nodeId: 'source',
      sourceChain: 'source-chain',
      spine: ['source'],
      tenant: 'platform.acme',
      steps: app.steps.filter(step => step.chain === 'source-chain'),
      app,
      nodes: [
        node('source', '香港入口'),
        node('candidate', '候选监听'),
        node('owner-root', '共享入口'),
        node('shared', '共享监听'),
      ],
    });

    expect(peers.find(peer => peer.id === 'candidate')?.blocked).toContain('成环');
  });

  it('只列出现有监听，标出本机、引用数、作用域和环路原因', () => {
    const sourceRules = [referenceRule('owner', 'source')];
    const app: SnapshotApp = {
      id: 'app',
      label: '项目',
      chains: [
        chain('source-chain', '源线路'),
        chain('owner', '共享出口', 'platform'),
        chain('sibling', '其它租户', 'platform.other'),
        chain('dormant', '未运行线路'),
      ],
      ingresses: [
        ingress('source-in', 'source-chain', 'source'),
        ingress('owner-in', 'owner', 'owner-root'),
        ingress('sibling-in', 'sibling', 'sibling-root'),
      ],
      steps: [
        listenerStep('source-chain', 'source', 21000, sourceRules),
        {
          chain: 'owner',
          node: 'owner-root',
          accept: null,
          hop_in: null,
          // This historical owned edge must not inflate the explicit-reference count.
          rules: [forwardRule('source')],
        },
        listenerStep('owner', 'source', 22000, [egressRule()]),
        listenerStep('owner', 'cycle', 22001, [referenceRule('source-chain', 'source')]),
        listenerStep('owner', 'retired-tree', 22004, [forwardRule('retired-leaf')]),
        listenerStep('owner', 'retired-leaf', 22005, [egressRule()]),
        listenerStep('sibling', 'sibling-root', 22002, [egressRule()]),
        listenerStep('dormant', 'dormant-node', 22003, [egressRule()]),
      ],
      fronts: [],
      grants: [],
    };
    const candidates = reusableListeners({
      apps: [app],
      sourceApp: app.id,
      sourceChain: 'source-chain',
      sourceNode: 'source',
      sourceRules,
      nodes: [
        node('source', '香港入口'),
        node('owner-root', '共享入口', 'platform'),
        node('cycle', '环路监听', 'platform'),
        node('retired-tree', '含退役节点的监听', 'platform'),
        { ...node('retired-leaf', '已退役出口', 'platform'), retired_at: '2026-09-11T00:00:00Z' },
        node('sibling-root', '其它监听', 'platform.other'),
        node('dormant-node', '停用监听'),
      ],
    });

    const self = candidates.find(candidate => candidate.ref.chain === 'source-chain')!;
    const local = candidates.find(candidate => candidate.ref.chain === 'owner' && candidate.ref.node === 'source')!;
    const cycle = candidates.find(candidate => candidate.ref.node === 'cycle')!;
    const retiredTree = candidates.find(candidate => candidate.ref.node === 'retired-tree')!;
    const sibling = candidates.find(candidate => candidate.ref.chain === 'sibling')!;
    const dormant = candidates.find(candidate => candidate.ref.chain === 'dormant')!;
    expect(self.blocked).toContain('自身');
    expect(local.local).toBe(true);
    expect(local.references).toBe(1);
    expect(cycle.blocked).toContain('环路');
    expect(retiredTree.blocked).toContain('已退役机器');
    expect(sibling.blocked).toContain('可用范围');
    expect(dormant.blocked).toContain('没有入口');
    expect(candidates[0].local).toBe(true);
  });

  it('把引用边展开为所有者的真实规则子树，并保留规则顺序与终点', () => {
    const source = listenerStep('source-chain', 'source', 21000, [
      referenceRule('owner', 'source', { t: 'domain_suffix', v: ['openai.com'] }),
      egressRule(),
    ]);
    const shared = listenerStep('owner', 'source', 22000, [
      { m: { t: 'geoip', v: ['cn'] }, a: { t: 'block' } },
      forwardRule('tail'),
    ]);
    const tail = listenerStep('owner', 'tail', 22001, [egressRule()]);
    const sourceApp: SnapshotApp = {
      id: 'source-app',
      label: '来源项目',
      chains: [chain('source-chain', '源线路')],
      ingresses: [ingress('source-in', 'source-chain', 'source')],
      steps: [source],
      fronts: [],
      grants: [],
    };
    const ownerApp: SnapshotApp = {
      id: 'owner-app',
      label: '监听项目',
      chains: [chain('owner', '共享出口')],
      ingresses: [ingress('owner-in', 'owner', 'owner-root')],
      steps: [{ chain: 'owner', node: 'owner-root', accept: null, hop_in: null, rules: [egressRule()] }, shared, tail],
      fronts: [],
      grants: [],
    };
    const view = render(
      <ListenerDecisionTree
        app={sourceApp}
        apps={[sourceApp, ownerApp]}
        currentChain={sourceApp.chains[0]}
        currentSteps={[source]}
        root="source"
        draftRules={{}}
        compiledRules={new Map()}
        highlightedListener={{ chain: 'owner', node: 'source' }}
        nodeNames={
          new Map([
            ['source', '香港节点'],
            ['owner-root', '所有者入口'],
            ['tail', '新加坡出口'],
          ])
        }
      />,
    );

    expect(view.getByText('引用的监听子树')).toBeTruthy();
    expect(view.getAllByText('归属：监听项目 / 共享出口')).toHaveLength(2);
    expect(view.getByText('域名后缀 · openai.com')).toBeTruthy();
    expect(view.getByText('GeoIP · cn')).toBeTruthy();
    expect(view.getByText('拒绝')).toBeTruthy();
    expect(view.getAllByText('本机出网').length).toBeGreaterThan(0);
    expect(view.getByText('本机内部')).toBeTruthy();
    expect(view.getByText(/443 \/ VLESS/)).toBeTruthy();
    expect(view.getByText('22000 / VLESS-NONE')).toBeTruthy();
    expect(view.container.querySelectorAll('.listener-map-branch.is-reference')).toHaveLength(1);
    const highlighted = view.container.querySelector('.listener-map-node.is-highlighted-subtree');
    expect(highlighted).not.toBeNull();
    expect(highlighted?.textContent).toContain('新加坡出口');
    expect(
      [...view.container.querySelectorAll('.listener-map-edge > span')].map(element => element.textContent),
    ).toEqual(['01', '01', '02', '01', '02']);
  });

  it('草稿在 Node、监听复用和各类终点间切换时立即重绘', () => {
    const source = listenerStep('source-chain', 'source', 21000, [egressRule()]);
    const next = listenerStep('source-chain', 'next', 21001, [egressRule()]);
    const shared = listenerStep('owner', 'shared', 22000, [egressRule()]);
    const app: SnapshotApp = {
      id: 'app',
      label: '项目',
      chains: [chain('source-chain', '源线路'), chain('owner', '共享出口')],
      ingresses: [ingress('source-in', 'source-chain', 'source'), ingress('owner-in', 'owner', 'owner-root')],
      steps: [
        source,
        next,
        { chain: 'owner', node: 'owner-root', accept: null, hop_in: null, rules: [egressRule()] },
        shared,
      ],
      fronts: [],
      grants: [],
    };
    const props = {
      app,
      currentChain: app.chains[0],
      currentSteps: [source, next],
      root: 'source',
      compiledRules: new Map(),
      nodeNames: new Map([
        ['source', '香港入口'],
        ['next', '日本 Node'],
        ['shared', '新加坡共享监听'],
        ['new-node', '待建 Node'],
      ]),
    };
    const tree = (rules: Rule[]) => <ListenerDecisionTree {...props} draftRules={{ source: rules }} />;
    const view = render(tree([forwardRule('next')]));

    expect(view.getByText('本链监听')).toBeTruthy();
    expect(view.getByText('日本 Node')).toBeTruthy();
    expect(view.getByText('本机出网')).toBeTruthy();

    view.rerender(tree([referenceRule('owner', 'shared')]));
    expect(view.getByText('引用的监听子树')).toBeTruthy();
    expect(view.getByText('新加坡共享监听')).toBeTruthy();

    view.rerender(tree([{ m: { t: 'any' }, a: { t: 'proxy', outbound: 'warp' } }]));
    expect(view.getByText('外部代理')).toBeTruthy();
    expect(view.getByText('warp')).toBeTruthy();

    view.rerender(tree([egressRule()]));
    expect(view.getByText('本机出网')).toBeTruthy();
    expect(view.queryByText('日本 Node')).toBeNull();

    view.rerender(tree([{ m: { t: 'any' }, a: { t: 'block' } }]));
    expect(view.getByText('拒绝')).toBeTruthy();

    view.rerender(tree([forwardRule('new-node')]));
    expect(view.getByText('待保存的新监听')).toBeTruthy();
    expect(view.getByText('待建 Node')).toBeTruthy();
  });

  it('把草稿中新建但尚未保存的监听显示为待建立状态', () => {
    const source = listenerStep('source-chain', 'source', 21000, [egressRule()]);
    const app: SnapshotApp = {
      id: 'app',
      label: '项目',
      chains: [chain('source-chain', '源线路')],
      ingresses: [ingress('source-in', 'source-chain', 'source')],
      steps: [source],
      fronts: [],
      grants: [],
    };
    const view = render(
      <ListenerDecisionTree
        app={app}
        currentChain={app.chains[0]}
        currentSteps={[source]}
        root="source"
        draftRules={{ source: [forwardRule('new-listener')] }}
        compiledRules={new Map()}
        nodeNames={new Map([['new-listener', '新加坡节点']])}
      />,
    );

    expect(view.getByText('待保存的新监听')).toBeTruthy();
    expect(view.getByText('保存到草稿后建立')).toBeTruthy();
    expect(view.queryByText('找不到监听所有者')).toBeNull();
  });

  it('把没有持久化 Step 的直出入口展开为编译器默认出网', () => {
    const app: SnapshotApp = {
      id: 'app',
      label: '项目',
      chains: [chain('direct', '香港直出')],
      ingresses: [ingress('direct-in', 'direct', 'hk')],
      steps: [],
      fronts: [],
      grants: [],
    };
    const view = render(
      <ListenerDecisionTree
        app={app}
        currentChain={app.chains[0]}
        currentSteps={[]}
        root="hk"
        draftRules={{}}
        compiledRules={
          new Map([
            [listenerRefKey({ chain: 'direct', node: 'hk' }), [{ dest_match: { t: 'any' }, action: egressRule().a }]],
          ])
        }
        nodeNames={new Map([['hk', '香港节点']])}
      />,
    );

    expect(view.getByText('链路入口')).toBeTruthy();
    expect(view.getByText('本机出网')).toBeTruthy();
    expect(view.getByText('未命中以上规则')).toBeTruthy();
    expect(view.queryByText('找不到监听所有者')).toBeNull();

    view.rerender(
      <ListenerDecisionTree
        app={app}
        currentChain={app.chains[0]}
        currentSteps={[]}
        root="hk"
        draftRules={{ hk: [{ m: { t: 'any' }, a: { t: 'block' } }] }}
        compiledRules={
          new Map([
            [listenerRefKey({ chain: 'direct', node: 'hk' }), [{ dest_match: { t: 'any' }, action: egressRule().a }]],
          ])
        }
        nodeNames={new Map([['hk', '香港节点']])}
      />,
    );
    expect(view.getByText('拒绝')).toBeTruthy();
    expect(view.queryByText('本机出网')).toBeNull();
    expect(view.queryByText('编译器兜底')).toBeNull();
  });

  it('在草稿里只保存监听身份和承载选择，不复制目标端口和规则', async () => {
    const source: SnapshotStep = {
      chain: 'source-chain',
      node: 'source',
      accept: null,
      hop_in: null,
      rules: [egressRule()],
    };
    const shared = listenerStep('owner', 'shared', 22000, [
      { m: { t: 'geoip', v: ['cn'] }, a: { t: 'block' } },
      egressRule(),
    ]);
    const sameChain = listenerStep('source-chain', 'same-chain', 21001, [egressRule()]);
    const sourceApp: SnapshotApp = {
      id: 'source-app',
      label: '来源项目',
      chains: [chain('source-chain', '源线路')],
      ingresses: [ingress('source-in', 'source-chain', 'source')],
      steps: [source, sameChain],
      fronts: [],
      grants: [],
    };
    const ownerApp: SnapshotApp = {
      id: 'owner-app',
      label: '监听项目',
      chains: [chain('owner', '共享出口')],
      ingresses: [ingress('owner-in', 'owner', 'owner-root')],
      steps: [{ chain: 'owner', node: 'owner-root', accept: null, hop_in: null, rules: [egressRule()] }, shared],
      fronts: [],
      grants: [],
    };
    const client = new QueryClient({
      defaultOptions: { queries: { retry: false, staleTime: Number.POSITIVE_INFINITY } },
    });
    client.setQueryData(['snapshot'], {
      snapshot: { revision: 1, settings: {}, apps: [sourceApp, ownerApp], external_outbounds: [] },
      node_egress_dns: [],
      redacted: false,
    } as unknown as ConsoleSnapshot);
    client.setQueryData(['settings'], { ports: { hop_base: 20000 } });
    client.setQueryData(['revisions'], { current_revision: null, revisions: [] });
    client.setQueryData(['nodes'], {
      nodes: [
        { ...node('source', '香港入口'), egress_allowed: true },
        { ...node('same-chain', '同链监听'), egress_allowed: true },
        { ...node('owner-root', '所有者入口'), egress_allowed: true },
        { ...node('shared', '新加坡共享监听'), egress_allowed: true },
      ] as NodeAgentStateItem[],
    });
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        if (String(input) !== '/model/preview') throw new Error(`未预期的请求：${String(input)}`);
        return new Response(
          JSON.stringify({
            snapshot: {
              snapshot: { revision: 1, settings: {}, apps: [sourceApp, ownerApp], external_outbounds: [] },
              node_egress_dns: [],
              redacted: false,
            },
            compile: { diagnostics: [], summary: {}, system: { nodes: [] }, apps: [] },
            artifacts: { revision: 1, artifacts: [] },
          }),
          { status: 200, headers: { 'content-type': 'application/json' } },
        );
      }),
    );
    const view = render(
      <QueryClientProvider client={client}>
        <RuleEditor
          appId="source-app"
          chainId="source-chain"
          nodeId="source"
          initial={source.rules}
          accept={null}
          peers={[]}
          isForwardTarget={false}
          fallback={{ rules: [], pending: false }}
        />
      </QueryClientProvider>,
    );
    const row = view.container.querySelector('.rule-table tbody tr');
    if (!(row instanceof HTMLTableRowElement)) throw new Error('没有规则行');
    const action = row.querySelector('.rule-action-select');
    if (!(action instanceof HTMLSelectElement)) throw new Error('没有动作选择器');
    fireEvent.change(action, { target: { value: 'forward' } });
    expect(view.queryByRole('button', { name: /新加坡共享监听 · TCP 22000/ })).toBeNull();
    expect(view.queryByRole('button', { name: /同链监听 · TCP 21001/ })).toBeNull();
    const custom = await view.findByRole('button', { name: /自定义.*复用已有监听/ });
    const manage = view.getByRole('button', { name: '管理隧道' });
    expect(custom.textContent).not.toContain('↗');
    expect(manage.textContent).not.toContain('↗');
    expect(custom.querySelector('.external-target-new-icon')).not.toBeNull();
    expect(manage.querySelector('.external-target-new-icon')).not.toBeNull();
    expect(custom.querySelector('.external-target-copy')).not.toBeNull();
    expect(manage.querySelector('.external-target-copy')).not.toBeNull();
    const menu = custom.closest('.external-target-menu');
    expect(menu?.parentElement).toBe(document.body);
    expect(view.container.contains(menu)).toBe(false);
    fireEvent.click(custom);
    const crossAppSearch = view.getByPlaceholderText('跨 App 搜索链、节点或端口');
    fireEvent.change(crossAppSearch, { target: { value: '监听项目' } });
    const ownerChain = view.getByRole('button', { name: /共享出口.*1 个监听端点/ });
    fireEvent.click(ownerChain);
    const choice = await view.findByRole('button', { name: /新加坡共享监听 · TCP 22000/ });
    fireEvent.pointerDown(choice);
    expect(document.body.contains(choice)).toBe(true);
    fireEvent.click(choice);
    expect(view.getByText(/引用子树 · 共享出口/)).toBeTruthy();
    expect(view.queryByRole('button', { name: '打开源规则' })).toBeNull();
    expect(view.queryByText('高亮规则子树')).toBeNull();
    expect(view.queryByRole('button', { name: /高亮 .*规则子树/ })).toBeNull();
    const referencePanel = [...view.container.querySelectorAll<HTMLElement>('.listener-reference-panel')].find(panel =>
      panel.querySelector('.panel-title')?.textContent?.includes('引用监听'),
    );
    const referencePool = referencePanel?.querySelector('select');
    if (!(referencePool instanceof HTMLSelectElement)) throw new Error('没有引用监听的连接复用选择器');
    fireEvent.change(referencePool, { target: { value: 'mux' } });
    const referenceMux = referencePanel?.querySelector('.listener-reference-mux');
    expect(referenceMux?.textContent).toContain('Mux 参数跟随全局配置参数');
    expect(referenceMux?.parentElement?.classList.contains('listener-inline-facts')).toBe(true);
    fireEvent.change(referencePool, { target: { value: 'none' } });
    fireEvent.click(view.getByRole('button', { name: '保存到草稿' }));

    await waitFor(() => {
      const op = draft.ops().find(candidate => candidate.op === 'put_step' && candidate.chain_id === 'source-chain');
      expect(op?.op).toBe('put_step');
      if (!op || op.op !== 'put_step') return;
      expect(op.step.rules[0]).toEqual({
        ...referenceRule('owner', 'shared'),
        a: {
          ...referenceRule('owner', 'shared').a,
          dial: { t: 'public', v: 'v4' },
        },
      });
      expect(JSON.stringify(op.step.rules[0])).not.toContain('22000');
      expect(JSON.stringify(op.step.rules[0])).not.toContain('geoip');
    });
  });
});
