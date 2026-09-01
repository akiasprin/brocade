import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';
import type { ConsoleSnapshot, NodeAgentStateItem, Rule } from '../src/api';
import { draft } from '../src/draft';
import { MachineEgressDnsRules, RuleEditor, type ForwardPeer } from '../src/panes/rules';

const initial: Rule[] = [
  {
    m: { t: 'domain_suffix', v: ['netflix.com'] },
    a: { t: 'egress', send_through: null },
  },
];

const resolution = {
  address: '192.0.2.53',
  port: 53,
  transport: 'tcp' as const,
  address_strategy: 'use_ip' as const,
  fallback: 'stop' as const,
};

afterEach(() => {
  cleanup();
  draft.clear();
});

function renderEditor(
  withSharedReference = false,
  initialRules = initial,
  readOnly = false,
  policies: ConsoleSnapshot['node_egress_dns'] = [
    { node: 'hk', position: 0, selector: { t: 'domain_suffix', v: ['netflix.com'] }, resolution },
    { node: 'hk', position: 1, selector: { t: 'geosite', v: ['media'] }, resolution },
  ],
  _showInheritedDns = true,
  egressAllowed = true,
  fallbackRules: Rule[] = [{ m: { t: 'any' }, a: { t: 'block' } }],
  peers: ForwardPeer[] = [],
) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Number.POSITIVE_INFINITY } },
  });
  const snapshot: ConsoleSnapshot = {
    snapshot: {
      revision: 1,
      apps: [
        {
          id: 'video',
          label: '视频',
          chains: [
            { id: 'stream', tenant: 'platform', name: '流媒体' },
            ...(withSharedReference ? [{ id: 'backup', tenant: 'platform', name: '备用线路' }] : []),
          ],
          steps: withSharedReference
            ? [
                {
                  chain: 'backup',
                  node: 'hk',
                  accept: null,
                  hop_in: null,
                  rules: [
                    {
                      m: { t: 'domain_suffix', v: ['netflix.com'] },
                      a: {
                        t: 'egress',
                        send_through: null,
                      },
                    },
                  ],
                },
              ]
            : [],
          ingresses: [],
          fronts: [],
          grants: [],
        },
      ],
      external_outbounds: [],
    },
    node_egress_dns: policies,
    redacted: false,
  };
  client.setQueryData(['snapshot'], snapshot);
  client.setQueryData(['revisions'], { current_revision: null, revisions: [] });
  client.setQueryData(['settings'], { ports: { hop_base: 20000 } });
  client.setQueryData(['nodes'], {
    nodes: [
      {
        node_id: 'hk',
        name: '香港落地',
        public_ipv4: 'hk.example.net',
        public_ipv6: null,
        public_ipv4_nat: false,
        public_ipv6_nat: false,
        egress_allowed: egressAllowed,
      } as NodeAgentStateItem,
    ],
  });

  return render(
    <QueryClientProvider client={client}>
      <RuleEditor
        appId="video"
        chainId="stream"
        nodeId="hk"
        initial={initialRules}
        accept={null}
        peers={peers}
        isForwardTarget={false}
        readOnly={readOnly}
        fallback={{ rules: fallbackRules, pending: false }}
      />
    </QueryClientProvider>,
  );
}

function renderMachineRules(
  referenced = false,
  showHeader = false,
  policies: ConsoleSnapshot['node_egress_dns'] = [
    { node: 'hk', position: 0, selector: { t: 'domain_suffix', v: ['netflix.com'] }, resolution },
    { node: 'hk', position: 1, selector: { t: 'geosite', v: ['media'] }, resolution },
    { node: 'sg', position: 0, selector: { t: 'geosite', v: ['other'] }, resolution },
  ],
) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Number.POSITIVE_INFINITY } },
  });
  client.setQueryData<ConsoleSnapshot>(['snapshot'], {
    snapshot: {
      revision: 1,
      apps: referenced
        ? [
            {
              id: 'video',
              label: '视频',
              chains: [{ id: 'stream', tenant: 'platform', name: '流媒体' }],
              steps: [
                {
                  chain: 'stream',
                  node: 'hk',
                  accept: null,
                  hop_in: null,
                  rules: initial,
                },
              ],
              ingresses: [],
              fronts: [],
              grants: [],
            },
          ]
        : [],
      external_outbounds: [],
    },
    node_egress_dns: policies,
    redacted: false,
  });
  return render(
    <QueryClientProvider client={client}>
      <MachineEgressDnsRules nodeId="hk" nodeName="香港落地" showHeader={showHeader} />
    </QueryClientProvider>,
  );
}

describe('machine-scoped egress DNS', () => {
  it('adds a new machine DNS policy from the machine-detail panel header', async () => {
    draft.init('machine-detail-dns-create-test');
    draft.clear();
    const view = renderMachineRules(false, true, []);

    const add = view.getByRole('button', { name: '添加新策略' });
    expect(add.closest('header')).not.toBeNull();
    fireEvent.click(add);

    expect(view.container.querySelector('.node-egress-rules-table')?.classList.contains('rule-table')).toBe(true);
    const save = view.getByRole('button', { name: '保存到草稿' }) as HTMLButtonElement;
    const footer = save.closest('.toolbar');
    expect(footer?.className).toBe('toolbar');
    expect(footer?.textContent).toContain('改动落进草稿，顶栏按「提交」才写进库。');
    expect(footer?.textContent).toContain('1 项配置有改动');
    expect(save.disabled).toBe(true);
    fireEvent.change(view.getByLabelText('DNS 匹配内容（域名后缀）'), { target: { value: 'example.com' } });
    fireEvent.change(view.getByLabelText('DNS 地址（域名后缀 example.com）'), { target: { value: '1.1.1.1' } });
    expect(save.disabled).toBe(false);
    fireEvent.click(save);

    await waitFor(() => expect(draft.ops()).toHaveLength(2));
    expect(draft.ops()[0]).toEqual({
      op: 'set_node_egress_dns',
      node_id: 'hk',
      selector: { t: 'domain_suffix', v: ['example.com'] },
      resolution: {
        address: '1.1.1.1',
        port: 53,
        transport: 'tcp',
        address_strategy: 'use_ip',
        fallback: 'stop',
      },
    });
    expect(draft.ops()[1]).toEqual({
      op: 'reorder_node_egress_dns',
      node_id: 'hk',
      selectors: [{ t: 'domain_suffix', v: ['example.com'] }],
    });
  });

  it('keeps the add action available and saves several new policies in their visible order', async () => {
    draft.init('machine-detail-dns-create-many-test');
    draft.clear();
    const view = renderMachineRules(false, true, []);
    const add = view.getByRole('button', { name: '添加新策略' });

    fireEvent.click(add);
    fireEvent.click(add);
    expect((add as HTMLButtonElement).disabled).toBe(false);
    expect(view.container.querySelectorAll('.node-egress-dns-new-row')).toHaveLength(2);

    const contents = view.getAllByLabelText(/DNS 匹配内容/);
    const addresses = view.getAllByLabelText(/DNS 地址/);
    fireEvent.change(contents[0], { target: { value: 'one.example' } });
    fireEvent.change(addresses[0], { target: { value: '1.1.1.1' } });
    fireEvent.change(contents[1], { target: { value: 'two.example' } });
    fireEvent.change(addresses[1], { target: { value: '8.8.8.8' } });
    fireEvent.click(view.getByRole('button', { name: '保存到草稿' }));

    await waitFor(() => expect(draft.ops()).toHaveLength(3));
    expect(
      draft
        .ops()
        .slice(0, 2)
        .map(op => (op.op === 'set_node_egress_dns' ? op.selector : null)),
    ).toEqual([
      { t: 'domain_suffix', v: ['one.example'] },
      { t: 'domain_suffix', v: ['two.example'] },
    ]);
    expect(draft.ops()[2]).toMatchObject({
      op: 'reorder_node_egress_dns',
      selectors: [
        { t: 'domain_suffix', v: ['one.example'] },
        { t: 'domain_suffix', v: ['two.example'] },
      ],
    });
  });

  it('renames an existing selector by removing the old key and creating the new key', async () => {
    draft.init('machine-detail-dns-rename-test');
    draft.clear();
    const view = renderMachineRules();

    const content = view.getByLabelText('DNS 匹配内容（域名后缀 netflix.com）');
    fireEvent.change(content, { target: { value: 'disneyplus.com' } });
    fireEvent.click(view.getByRole('button', { name: '保存到草稿' }));

    await waitFor(() => expect(draft.ops()).toHaveLength(3));
    expect(draft.ops()[0]).toEqual({
      op: 'set_node_egress_dns',
      node_id: 'hk',
      selector: { t: 'domain_suffix', v: ['netflix.com'] },
      resolution: null,
    });
    expect(draft.ops()[1]).toMatchObject({
      op: 'set_node_egress_dns',
      node_id: 'hk',
      selector: { t: 'domain_suffix', v: ['disneyplus.com'] },
      resolution,
    });
    expect(draft.ops()[2]).toMatchObject({
      op: 'reorder_node_egress_dns',
      selectors: [
        { t: 'domain_suffix', v: ['disneyplus.com'] },
        { t: 'geosite', v: ['media'] },
      ],
    });
  });

  it('renders and saves the selected machine policies without a chain operation', async () => {
    draft.init('machine-detail-dns-test');
    draft.clear();
    const view = renderMachineRules();

    expect(view.container.querySelectorAll('.node-egress-rules-table tbody tr')).toHaveLength(2);
    expect(view.queryByText('other')).toBeNull();
    fireEvent.change(view.getByLabelText('DNS 地址（域名后缀 netflix.com）'), {
      target: { value: '198.51.100.53' },
    });
    fireEvent.click(view.getByRole('button', { name: '保存到草稿' }));

    await waitFor(() => expect(draft.ops()).toHaveLength(1));
    expect(draft.ops()[0]).toMatchObject({
      op: 'set_node_egress_dns',
      node_id: 'hk',
      selector: { t: 'domain_suffix', v: ['netflix.com'] },
      resolution: { address: '198.51.100.53' },
    });
    expect(draft.ops().some(op => op.op === 'put_step')).toBe(false);
  });

  it('reorders the machine DNS priority as one machine-wide draft operation', async () => {
    draft.init('machine-detail-dns-order-test');
    draft.clear();
    const view = renderMachineRules();

    fireEvent.click(view.getByRole('button', { name: 'DNS 优先级下移（域名后缀 netflix.com）' }));
    const rows = view.container.querySelectorAll('.node-egress-rules-table tbody tr');
    expect((rows[0].querySelector('input') as HTMLInputElement).value).toBe('media');
    expect((rows[1].querySelector('input') as HTMLInputElement).value).toBe('netflix.com');
    fireEvent.click(view.getByRole('button', { name: '保存到草稿' }));

    await waitFor(() => expect(draft.ops()).toHaveLength(1));
    expect(draft.ops()[0]).toEqual({
      op: 'reorder_node_egress_dns',
      node_id: 'hk',
      selectors: [
        { t: 'geosite', v: ['media'] },
        { t: 'domain_suffix', v: ['netflix.com'] },
      ],
    });
  });

  it('states the machine-wide DNS limitation as one plain sentence', () => {
    const view = renderMachineRules();

    const hint = view.getByText(
      'Xray 的 DNS 选择不携带原路由和出站上下文；DNS 查询可以指定出口，但解析结果无法按出站隔离。',
    );
    expect(hint.classList.contains('note')).toBe(true);
    expect(hint.classList.contains('st-warn')).toBe(false);
    expect(hint.classList.contains('err')).toBe(false);
    expect(view.container.textContent).not.toContain('从这台落地');
  });

  it('allows removing a machine policy regardless of chain rules', () => {
    const view = renderMachineRules(true);
    const choice = view.getByLabelText('DNS 解析方式（域名后缀 netflix.com）') as HTMLSelectElement;

    expect(Array.from(choice.options).map(option => option.text)).toEqual(['默认 DNS 解析', '自定义 DNS 解析']);
    expect(Array.from(choice.options).find(option => option.value === 'machine')?.disabled).toBe(false);
    expect(view.getAllByText('机器全局下发')).toHaveLength(2);
    expect(view.container.textContent).not.toContain('引用');
  });

  it('shows a masked DNS port instead of feeding it to a numeric input for public viewers', () => {
    const maskedRules: Rule[] = [
      {
        m: { t: 'domain_suffix', v: ['netflix.com'] },
        a: {
          t: 'egress',
          send_through: null,
        },
      },
    ];
    const maskedResolution = {
      ...resolution,
      address: '192.0.***.***',
      port: '***' as unknown as number,
    };
    const view = renderEditor(false, maskedRules, true, [
      {
        node: 'hk',
        position: 0,
        selector: { t: 'domain_suffix', v: ['netflix.com'] },
        resolution: maskedResolution,
      },
    ]);
    const port = view.getByLabelText('端口（机器策略：域名后缀 netflix.com）') as HTMLInputElement;

    expect(port.type).toBe('text');
    expect(port.value).toBe('***');
    expect(view.queryByRole('spinbutton', { name: '端口（机器策略：域名后缀 netflix.com）' })).toBeNull();
  });

  it('renders every machine DNS policy as an independently ordered shared row', () => {
    const view = renderEditor();
    expect(view.container.querySelector('.rule-editor > table')?.classList.contains('rule-table')).toBe(true);
    const rows = view.container.querySelectorAll('.rule-editor > .tbl > tbody > tr');

    expect(rows).toHaveLength(4);
    expect(view.container.querySelectorAll('.machine-dns-shared-row')).toHaveLength(2);
    expect(view.getByText('media')).toBeTruthy();
    expect((rows[0].querySelector('input') as HTMLInputElement).value).toBe('netflix.com');
    expect(rows[1].textContent).toContain('D1');
    expect(rows[2].textContent).toContain('D2');
    expect(rows[3].classList.contains('rule-fallback-row')).toBe(true);
  });

  it('inserts a newly added rule before an existing Any egress fallback', () => {
    const view = renderEditor(
      false,
      [{ m: { t: 'any' }, a: { t: 'egress', send_through: null } }],
      false,
      [],
      true,
      true,
      [],
    );

    fireEvent.click(view.getByRole('button', { name: '＋ 加一条' }));
    const authoredRows = Array.from(view.container.querySelectorAll('.rule-editor > .tbl > tbody > tr'));

    expect(authoredRows).toHaveLength(2);
    expect((authoredRows[0].querySelector('select') as HTMLSelectElement).value).toBe('domain_suffix');
    expect((authoredRows[1].querySelector('select') as HTMLSelectElement).value).toBe('any');
    expect((authoredRows[1].querySelector('.rule-action-select') as HTMLSelectElement).value).toBe('egress');
  });

  it('marks forward, egress, and block actions with their visual tone', () => {
    const view = renderEditor();
    const authoredRow = view.container.querySelector('.rule-editor > .tbl > tbody > tr:not(.rule-fallback-row)')!;
    const action = authoredRow.querySelector('.rule-action-select') as HTMLSelectElement;

    expect(action.classList.contains('rule-action-egress')).toBe(true);
    expect(view.container.querySelector('.rule-fallback-row .rule-action-block')).not.toBeNull();

    fireEvent.change(action, { target: { value: 'forward' } });
    expect(action.classList.contains('rule-action-forward')).toBe(true);
    fireEvent.change(action, { target: { value: 'block' } });
    expect(action.classList.contains('rule-action-block')).toBe(true);
  });

  it('moves a rule to the end when it is changed to Any', () => {
    const view = renderEditor(
      false,
      [
        { m: { t: 'domain_suffix', v: ['first.example'] }, a: { t: 'egress', send_through: null } },
        { m: { t: 'geosite', v: ['media'] }, a: { t: 'egress', send_through: null } },
      ],
      false,
      [],
      true,
      true,
      [],
    );

    fireEvent.change(view.getByDisplayValue('域名后缀'), { target: { value: 'any' } });
    const rows = Array.from(view.container.querySelectorAll('.rule-editor > .tbl > tbody > tr'));

    expect((rows[0].querySelector('select') as HTMLSelectElement).value).toBe('geosite');
    expect((rows[0].querySelector('input') as HTMLInputElement).value).toBe('media');
    expect((rows[1].querySelector('select') as HTMLSelectElement).value).toBe('any');
  });

  it('disables custom DNS on a non-domain match', () => {
    const view = renderEditor(false, initial, false, [], true, false, [{ m: { t: 'any' }, a: { t: 'block' } }]);
    fireEvent.change(view.getByDisplayValue('域名后缀'), { target: { value: 'ip_cidr' } });

    const choice = view.getByLabelText('DNS 解析方式（线路规则：IP 段）') as HTMLSelectElement;
    expect(choice.value).toBe('machine');
    expect(Array.from(choice.options).find(option => option.value === 'custom')?.disabled).toBe(true);
    expect(view.queryByLabelText('DNS 地址')).toBeNull();
  });

  it('creates a machine policy from an egress row without rewriting the route', async () => {
    draft.init('new-machine-dns-test');
    draft.clear();
    const unreferenced: Rule[] = [
      { m: { t: 'domain_suffix', v: ['netflix.com'] }, a: { t: 'egress', send_through: null } },
    ];
    const view = renderEditor(false, unreferenced, false, []);

    const choice = view.getByLabelText('DNS 解析方式（线路规则：域名后缀 netflix.com）') as HTMLSelectElement;
    expect(choice.value).toBe('machine');
    fireEvent.change(choice, { target: { value: 'custom' } });
    fireEvent.change(view.getByLabelText('DNS 地址（机器策略：域名后缀 netflix.com）'), {
      target: { value: '198.51.100.53' },
    });
    fireEvent.click(view.getByRole('button', { name: '保存到草稿' }));

    await waitFor(() => expect(draft.ops().some(op => op.op === 'set_node_egress_dns')).toBe(true));
    expect(draft.ops().find(op => op.op === 'set_node_egress_dns')).toMatchObject({
      op: 'set_node_egress_dns',
      node_id: 'hk',
      selector: { t: 'domain_suffix', v: ['netflix.com'] },
      resolution: { address: '198.51.100.53' },
    });
    expect(draft.ops().some(op => op.op === 'put_step')).toBe(false);
  });

  it('renders every stored policy as globally active without chain ownership', () => {
    const rules: Rule[] = [{ m: { t: 'domain_suffix', v: ['netflix.com'] }, a: { t: 'egress', send_through: null } }];
    const view = renderEditor(false, rules);

    expect((view.getByLabelText('DNS 解析方式（线路规则：域名后缀 netflix.com）') as HTMLSelectElement).value).toBe(
      'custom',
    );
    expect(view.getByLabelText('DNS 地址（机器策略：域名后缀 netflix.com）')).toBeTruthy();
    expect(view.getByText('media')).toBeTruthy();
    expect(view.getAllByText('机器全局下发')).toHaveLength(2);
  });

  it('does not change policy status when another chain has the same route selector', () => {
    const view = renderEditor(true, [
      { m: { t: 'domain_suffix', v: ['netflix.com'] }, a: { t: 'egress', send_through: null } },
    ]);

    expect(view.getAllByText('机器全局下发')).toHaveLength(2);
    expect(view.container.textContent).not.toContain('其他链路触发');
  });

  it('renders every compiled fallback as a full read-only rule row', () => {
    const view = renderEditor();
    const fallback = view.container.querySelector('.rule-fallback-row');

    expect(fallback).not.toBeNull();
    expect(fallback?.querySelectorAll('td')).toHaveLength(4);
    expect(fallback?.textContent).toContain('任意');
    expect(fallback?.textContent).toContain('拒绝');
    expect(fallback?.textContent).toContain('自动补齐 · 只读');
    expect(fallback?.querySelector('select, input, button')).toBeNull();
    expect(fallback?.querySelector('td')?.textContent).toBe('*');
  });

  it('uses real dropdowns and edits the machine policy in its shared row', () => {
    const view = renderEditor();
    const routeChoice = view.getByLabelText('DNS 解析方式（线路规则：域名后缀 netflix.com）') as HTMLSelectElement;
    const policyChoice = view.getByLabelText('DNS 解析方式（机器策略：域名后缀 netflix.com）') as HTMLSelectElement;
    expect(routeChoice.value).toBe('custom');
    expect(policyChoice.value).toBe('custom');
    expect(Array.from(policyChoice.options).map(option => option.text)).toEqual(['默认 DNS 解析', '自定义 DNS 解析']);
    expect(policyChoice.tagName).toBe('SELECT');
    expect(view.getAllByText('机器全局下发')).toHaveLength(2);

    expect((view.getByLabelText('DNS 地址（机器策略：域名后缀 netflix.com）') as HTMLInputElement).value).toBe(
      '192.0.2.53',
    );
    expect((view.getByLabelText('端口（机器策略：域名后缀 netflix.com）') as HTMLInputElement).value).toBe('53');
    expect((view.getByLabelText('传输（机器策略：域名后缀 netflix.com）') as HTMLSelectElement).value).toBe('tcp');
    const addressStrategy = view.getByLabelText('地址策略（机器策略：域名后缀 netflix.com）') as HTMLSelectElement;
    expect(addressStrategy.value).toBe('use_ip');
    expect(Array.from(addressStrategy.options).map(option => option.text)).toEqual([
      'UseIP',
      'UseIPv4v6',
      'UseIPv6v4',
      'UseIPv4',
      'UseIPv6',
    ]);
    const fallback = view.getByLabelText('失败处理（机器策略：域名后缀 netflix.com）') as HTMLSelectElement;
    expect(fallback.value).toBe('stop');
    expect(Array.from(fallback.options).map(option => option.text)).toEqual(['停止连接', '回退机器 DNS']);
    expect(view.container.querySelector('.egress-dns-note')?.textContent).toBe('Xray 全局生效 · 香港落地');
    expect(view.container.querySelector('.egress-dns-editor')?.querySelector('header, footer')).toBeNull();
    expect(view.getByLabelText('DNS 地址（机器策略：域名后缀 netflix.com）').closest('tr')).not.toBe(
      view.getByDisplayValue('netflix.com').closest('tr'),
    );
    expect(
      view
        .getByLabelText('DNS 地址（机器策略：域名后缀 netflix.com）')
        .closest('tr')
        ?.classList.contains('machine-dns-shared-row'),
    ).toBe(true);
    fireEvent.change(policyChoice, { target: { value: 'machine' } });
    expect(view.queryByLabelText('DNS 地址（机器策略：域名后缀 netflix.com）')).toBeNull();
  });

  it('keeps the machine policy when an unrelated route selector changes', () => {
    const view = renderEditor();
    fireEvent.change(view.getByDisplayValue('域名后缀'), { target: { value: 'ip_cidr' } });

    expect((view.getByLabelText('DNS 解析方式（线路规则：IP 段）') as HTMLSelectElement).value).toBe('machine');
    expect(view.getByLabelText('DNS 地址（机器策略：域名后缀 netflix.com）')).toBeTruthy();
  });

  it('keeps the machine-wide DNS limitation concise in the chain editor', () => {
    const view = renderEditor();

    expect(
      view.getByText('Xray 的 DNS 选择不携带原路由和出站上下文；DNS 查询可以指定出口，但解析结果无法按出站隔离。'),
    ).toBeTruthy();
  });

  it('highlights the NODE target instead of the Forward action and keeps focus on the trigger', () => {
    const forwardRules: Rule[] = [
      { m: { t: 'domain_suffix', v: ['example.com'] }, a: { t: 'forward', to: 'sg', dial: { t: 'overlay' } } },
    ];
    const peers: ForwardPeer[] = [
      {
        id: 'sg',
        name: '新加坡节点',
        public_ipv4: 'sg.example.net',
        public_ipv6: null,
        public_ipv4_nat: false,
        public_ipv6_nat: false,
        step: null,
        where: 'next',
        blocked: null,
      },
    ];
    const view = renderEditor(false, forwardRules, false, [], true, true, [], peers);
    const action = view.getByDisplayValue('转发给');
    const trigger = view.getByRole('button', { name: /新加坡节点/ });

    expect(action.classList.contains('rule-action-forward')).toBe(true);
    expect(trigger.classList.contains('node-target')).toBe(true);
    trigger.focus();
    fireEvent.click(trigger);
    expect(document.activeElement).toBe(trigger);
    expect(view.getByPlaceholderText('搜索节点或外部出站').hasAttribute('autofocus')).toBe(false);
    expect(trigger.getAttribute('aria-expanded')).toBe('true');
  });

  it('saves a policy edit without rewriting an unchanged chain rule', async () => {
    draft.init('machine-dns-test');
    draft.clear();
    const view = renderEditor();
    const address = view.getByLabelText('DNS 地址（机器策略：域名后缀 netflix.com）') as HTMLInputElement;

    fireEvent.change(address, { target: { value: '198.51.100.53' } });
    fireEvent.click(view.getByRole('button', { name: '保存到草稿' }));

    await waitFor(() => expect(draft.ops()).toHaveLength(1));
    expect(draft.ops()[0]).toMatchObject({
      op: 'set_node_egress_dns',
      node_id: 'hk',
      selector: { t: 'domain_suffix', v: ['netflix.com'] },
      resolution: { address: '198.51.100.53' },
    });
    expect(draft.ops().some(op => op.op === 'put_step' || op.op === 'prune_chain')).toBe(false);
    // Saving copies the value into the global browser draft and must release the editor-local
    // override. Otherwise "discard draft" reveals this stale value again and marks the row dirty.
    await waitFor(() =>
      expect((view.getByLabelText('DNS 地址（机器策略：域名后缀 netflix.com）') as HTMLInputElement).value).toBe(
        '192.0.2.53',
      ),
    );
  });
});
