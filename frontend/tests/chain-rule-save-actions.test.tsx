import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import type { ConsoleSnapshot, NodeAgentStateItem, Rule, SnapshotStep } from '../src/api';
import { draft } from '../src/draft';
import { RuleDraftScope, RuleEditor, type ForwardPeer } from '../src/panes/rules';

const forward = (match: Rule['m'], to: string): Rule => ({
  m: match,
  a: { t: 'forward', to, dial: { t: 'overlay' }, pool: { t: 'none' } },
});

const egress = (match: Rule['m'] = { t: 'any' }): Rule => ({
  m: match,
  a: { t: 'egress', send_through: null },
});

const step = (node: string, rules: Rule[], port: number | null = null): SnapshotStep => ({
  chain: 'stream',
  node,
  accept: node === 'source' ? null : { uuid: `uuid-${node}`, label: `stream@${node}` },
  hop_in: port == null ? null : { port, security: { t: 'none' } },
  rules,
});

const node = (nodeId: string, name: string): NodeAgentStateItem =>
  ({
    node_id: nodeId,
    tenant_id: 'platform',
    name,
    public_ipv4: `${nodeId}.example.net`,
    public_ipv6: null,
    public_ipv4_nat: false,
    public_ipv6_nat: false,
    retired_at: null,
    egress_allowed: true,
  }) as NodeAgentStateItem;

const peer = (item: SnapshotStep, name: string, where: ForwardPeer['where'] = 'inside'): ForwardPeer => ({
  id: item.node,
  name,
  public_ipv4: `${item.node}.example.net`,
  public_ipv6: null,
  public_ipv4_nat: false,
  public_ipv6_nat: false,
  step: item,
  where,
  blocked: null,
});

function clientFor(steps: SnapshotStep[]) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Number.POSITIVE_INFINITY } },
  });
  client.setQueryData<ConsoleSnapshot>(['snapshot'], {
    snapshot: {
      revision: 1,
      apps: [
        {
          id: 'video',
          label: '视频',
          chains: [{ id: 'stream', tenant: 'platform', name: '流媒体' }],
          steps,
          ingresses: [],
          fronts: [],
          grants: [],
        },
      ],
      external_outbounds: [],
    },
    node_egress_dns: [],
    redacted: false,
  });
  client.setQueryData(['revisions'], { current_revision: null, revisions: [] });
  client.setQueryData(['settings'], { ports: { hop_base: 20000 } });
  client.setQueryData(['nodes'], {
    nodes: [node('source', '香港入口'), node('old', '旧转发节点'), node('next', '新转发节点')],
  });
  return client;
}

function renderScopedEditor(sourceRules: Rule[], relaySteps: SnapshotStep[]) {
  const source = step('source', sourceRules);
  const steps = [source, ...relaySteps];
  const peers = relaySteps.map((item, index) => peer(item, index === 0 ? '旧转发节点' : '新转发节点'));
  return render(
    <QueryClientProvider client={clientFor(steps)}>
      <RuleDraftScope hint="改动落进草稿">
        <RuleEditor
          appId="video"
          chainId="stream"
          nodeId="source"
          initial={sourceRules}
          accept={null}
          peers={peers}
          isForwardTarget={false}
          steps={steps}
          root="source"
          fallback={{ rules: [], pending: false }}
        />
      </RuleDraftScope>
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  draft.init(`chain-rule-save-${Math.random()}`);
  draft.clear();
});

afterEach(() => {
  cleanup();
  draft.clear();
});

describe('链路规则统一保存', () => {
  it('删除规则只更新本地表单，点击统一保存后才写入草稿并清理链图', async () => {
    const relay = step('old', [egress()], 20000);
    const view = renderScopedEditor(
      [forward({ t: 'domain_suffix', v: ['example.com'] }, 'old'), forward({ t: 'any' }, 'old')],
      [relay],
    );

    fireEvent.click(view.getAllByRole('button', { name: '删' })[0]);

    await waitFor(() => expect(view.getByText('1 项配置有改动')).toBeTruthy());
    expect(draft.ops()).toEqual([]);

    fireEvent.click(view.getByRole('button', { name: '保存到草稿' }));
    await waitFor(() =>
      expect(draft.ops().map(operation => operation.op)).toEqual(['put_step', 'put_step', 'prune_chain']),
    );

    const sourceWrite = draft.ops().find(operation => operation.op === 'put_step' && operation.node_id === 'source');
    expect(sourceWrite?.op).toBe('put_step');
    if (!sourceWrite || sourceWrite.op !== 'put_step') return;
    expect(sourceWrite.step.rules).toEqual([forward({ t: 'any' }, 'old')]);
  });

  it('替换转发节点时保留新节点的规则和监听，不再重写旧节点', async () => {
    const oldRelay = step('old', [egress({ t: 'geoip', v: ['private'] })], 20000);
    const nextRelay = step('next', [egress({ t: 'geosite', v: ['cn'] })], 21000);
    const view = renderScopedEditor([forward({ t: 'any' }, 'old')], [oldRelay, nextRelay]);

    fireEvent.click(view.getByRole('button', { name: /NODE.*旧转发节点/ }));
    fireEvent.click(view.getByRole('button', { name: /NODE.*新转发节点.*链内/ }));

    await waitFor(() => expect(view.getByText('1 项配置有改动')).toBeTruthy());
    expect(draft.ops()).toEqual([]);
    fireEvent.click(view.getByRole('button', { name: '保存到草稿' }));

    await waitFor(() =>
      expect(draft.ops().map(operation => operation.op)).toEqual(['put_step', 'put_step', 'prune_chain']),
    );
    const writes = draft.ops().filter(operation => operation.op === 'put_step');
    expect(writes.map(operation => operation.node_id)).toEqual(['next', 'source']);
    expect(writes[0]).toMatchObject({
      node_id: 'next',
      step: {
        rules: [egress({ t: 'geosite', v: ['cn'] })],
        accept: { uuid: 'uuid-next', label: 'stream@next' },
        hop_in: { port: 21000, security: { t: 'none' } },
      },
    });
    expect(writes[1]).toMatchObject({
      node_id: 'source',
      step: {
        rules: [
          {
            m: { t: 'any' },
            a: { t: 'forward', to: 'next', dial: { t: 'addr', v: 'next.example.net:21000' }, pool: { t: 'none' } },
          },
        ],
      },
    });
    expect(writes.some(operation => operation.node_id === 'old')).toBe(false);
  });

  it('一次保存上下游两张表时，下游新规则不会被上游回传的旧值覆盖', async () => {
    const childInitial = [egress({ t: 'domain_suffix', v: ['before.example'] })];
    const source = step('source', [forward({ t: 'any' }, 'old')]);
    const child = step('old', childInitial, 20000);
    const steps = [source, child];
    const client = clientFor(steps);
    const view = render(
      <QueryClientProvider client={client}>
        <RuleDraftScope>
          <RuleEditor
            appId="video"
            chainId="stream"
            nodeId="source"
            initial={source.rules}
            accept={null}
            peers={[peer(child, '旧转发节点')]}
            isForwardTarget={false}
            steps={steps}
            root="source"
            fallback={{ rules: [], pending: false }}
          />
          <RuleEditor
            appId="video"
            chainId="stream"
            nodeId="old"
            initial={child.rules}
            accept={child.accept}
            hopIn={child.hop_in}
            peers={[]}
            isForwardTarget
            steps={steps}
            root="source"
            fallback={{ rules: [], pending: false }}
          />
        </RuleDraftScope>
      </QueryClientProvider>,
    );
    const editors = view.container.querySelectorAll<HTMLElement>('.rule-editor');
    const dial = within(editors[0]).getByTitle('这一跳连接目标监听的地址');
    fireEvent.change(dial, { target: { value: 'public_ipv4' } });
    const childMatch = editors[1].querySelector<HTMLInputElement>('td.rule-match-cell input');
    if (!childMatch) throw new Error('没有找到下游规则匹配值');
    fireEvent.change(childMatch, { target: { value: 'after.example' } });

    await waitFor(() => expect(view.getByText('2 项配置有改动')).toBeTruthy());
    fireEvent.click(view.getByRole('button', { name: '保存到草稿' }));

    await waitFor(() =>
      expect(draft.ops().map(operation => operation.op)).toEqual(['put_step', 'put_step', 'prune_chain']),
    );
    const childWrite = draft.ops().find(operation => operation.op === 'put_step' && operation.node_id === 'old');
    expect(childWrite).toMatchObject({
      step: {
        rules: [egress({ t: 'domain_suffix', v: ['after.example'] })],
        accept: { uuid: 'uuid-old', label: 'stream@old' },
      },
    });
    const sourceWrite = draft.ops().find(operation => operation.op === 'put_step' && operation.node_id === 'source');
    expect(sourceWrite).toMatchObject({
      step: {
        rules: [
          {
            m: { t: 'any' },
            a: { t: 'forward', to: 'old', dial: { t: 'addr', v: 'old.example.net:20000' }, pool: { t: 'none' } },
          },
        ],
      },
    });
  });

  it('保存后丢弃全局草稿时，已落草稿的本地规则不会再浮现', async () => {
    const baseline = [egress({ t: 'domain_suffix', v: ['before.example'] })];
    const staged = [egress({ t: 'domain_suffix', v: ['after.example'] })];
    const source = step('source', baseline);
    const client = clientFor([source]);
    const editor = (initial: Rule[]) => (
      <QueryClientProvider client={client}>
        <RuleEditor
          appId="video"
          chainId="stream"
          nodeId="source"
          initial={initial}
          accept={null}
          peers={[]}
          isForwardTarget={false}
          steps={[{ ...source, rules: initial }]}
          root="source"
          fallback={{ rules: [], pending: false }}
        />
      </QueryClientProvider>
    );
    const view = render(editor(baseline));
    const match = () => view.container.querySelector<HTMLInputElement>('td.rule-match-cell input')!;

    fireEvent.change(match(), { target: { value: 'after.example' } });
    expect(match().value).toBe('after.example');

    // /model/preview 返回后，props 与本地表单一致：这一刻应将新值视为已保存基线。
    view.rerender(editor(staged));
    await waitFor(() => expect(match().value).toBe('after.example'));

    // 顶栏「全部丢弃」使快照回到已提交值；编辑器不得用自己的旧 state 把它盖回去。
    view.rerender(editor(baseline));
    await waitFor(() => expect(match().value).toBe('before.example'));
  });

  it('丢弃全局草稿时同步恢复转发监听端口', async () => {
    const sourceRules = [forward({ t: 'any' }, 'old')];
    const baselineRelay = step('old', [egress()], 20000);
    const stagedRelay = step('old', [egress()], 21000);
    const source = step('source', sourceRules);
    const client = clientFor([source, baselineRelay]);
    const editor = (relay: SnapshotStep) => (
      <QueryClientProvider client={client}>
        <RuleEditor
          appId="video"
          chainId="stream"
          nodeId="source"
          initial={sourceRules}
          accept={null}
          peers={[peer(relay, '旧转发节点')]}
          isForwardTarget={false}
          steps={[source, relay]}
          root="source"
          fallback={{ rules: [], pending: false }}
        />
      </QueryClientProvider>
    );
    const view = render(editor(baselineRelay));
    const port = () => view.getByLabelText('目标端口') as HTMLInputElement;

    fireEvent.change(port(), { target: { value: '21000' } });
    expect(port().value).toBe('21000');

    view.rerender(editor(stagedRelay));
    await waitFor(() => expect(port().value).toBe('21000'));

    view.rerender(editor(baselineRelay));
    await waitFor(() => expect(port().value).toBe('20000'));
  });
});
