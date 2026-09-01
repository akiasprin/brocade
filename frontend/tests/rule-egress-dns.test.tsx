import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';
import type { ConsoleSnapshot, NodeAgentStateItem, Rule } from '../src/api';
import { draft } from '../src/draft';
import { MachineEgressDnsRules, RuleEditor } from '../src/panes/rules';

const initial: Rule[] = [
  {
    m: { t: 'domain_suffix', v: ['netflix.com'] },
    a: { t: 'egress', send_through: null, dns: true },
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
                        dns: true,
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
        peers={[]}
        isForwardTarget={false}
        readOnly={readOnly}
        fallback={{ rules: fallbackRules, pending: false }}
      />
    </QueryClientProvider>,
  );
}

function renderMachineRules(referenced = false) {
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
    node_egress_dns: [
      { node: 'hk', position: 0, selector: { t: 'domain_suffix', v: ['netflix.com'] }, resolution },
      { node: 'hk', position: 1, selector: { t: 'geosite', v: ['media'] }, resolution },
      { node: 'sg', position: 0, selector: { t: 'geosite', v: ['other'] }, resolution },
    ],
    redacted: false,
  });
  return render(
    <QueryClientProvider client={client}>
      <MachineEgressDnsRules nodeId="hk" nodeName="香港落地" />
    </QueryClientProvider>,
  );
}

describe('machine-scoped egress DNS', () => {
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
    expect(view.container.querySelector('.node-egress-rules-table tbody')?.textContent).toMatch(
      /D1.*media.*D2.*netflix/s,
    );
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

  it('explains the Xray machine-wide matching limitation in the machine panel', () => {
    const view = renderMachineRules();

    expect(view.getByText(/Xray 在一台机器内共用一个 DNS 实例/).textContent).toContain(
      '不能用 tag 隔离同一域名的不同 DNS',
    );
    expect(view.container.textContent).not.toContain('从这台落地');
  });

  it('shows policy references and requires removing them from chain rules first', () => {
    const view = renderMachineRules(true);
    const choice = view.getByLabelText('DNS 解析方式（域名后缀 netflix.com）') as HTMLSelectElement;

    expect(view.getByText('1 条出站引用')).toBeTruthy();
    expect(Array.from(choice.options).find(option => option.value === 'machine')?.disabled).toBe(true);
    expect(choice.title).toContain('请先在链路规则中取消引用');
    expect(view.getByText('未引用 · 不下发')).toBeTruthy();
  });

  it('shows a masked DNS port instead of feeding it to a numeric input for public viewers', () => {
    const maskedRules: Rule[] = [
      {
        m: { t: 'domain_suffix', v: ['netflix.com'] },
        a: {
          t: 'egress',
          send_through: null,
          dns: true,
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
    const port = view.getByLabelText('端口（域名后缀 netflix.com）') as HTMLInputElement;

    expect(port.type).toBe('text');
    expect(port.value).toBe('***');
    expect(view.queryByRole('spinbutton', { name: '端口（域名后缀 netflix.com）' })).toBeNull();
  });

  it('does not turn machine DNS policies into route rows', () => {
    const view = renderEditor();
    const rows = view.container.querySelectorAll('.rule-editor > .tbl > tbody > tr');

    expect(rows).toHaveLength(2);
    expect(view.container.querySelector('.machine-dns-shared-row')).toBeNull();
    expect(view.queryByText('media')).toBeNull();
    expect((rows[0].querySelector('input') as HTMLInputElement).value).toBe('netflix.com');
    expect(rows[1].classList.contains('rule-fallback-row')).toBe(true);
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

  it('does not expose a DNS reference on a non-domain match', () => {
    const view = renderEditor(false, initial, false, undefined, true, false, [{ m: { t: 'any' }, a: { t: 'block' } }]);
    fireEvent.change(view.getByDisplayValue('域名后缀'), { target: { value: 'ip_cidr' } });

    const reference = view.getByLabelText(/DNS 策略/) as HTMLSelectElement;
    expect(reference.value).toBe('none');
    expect(Array.from(reference.options).find(option => option.value === 'policy')?.disabled).toBe(true);
    expect(view.queryByLabelText('DNS 地址')).toBeNull();
  });

  it('creates and references a machine policy from the authored egress rule', async () => {
    draft.init('new-machine-dns-test');
    draft.clear();
    const unreferenced: Rule[] = [
      { m: { t: 'domain_suffix', v: ['netflix.com'] }, a: { t: 'egress', send_through: null } },
    ];
    const view = renderEditor(false, unreferenced, false, []);

    const reference = view.getByLabelText('DNS 策略（域名后缀 netflix.com）') as HTMLSelectElement;
    expect(reference.value).toBe('none');
    fireEvent.change(reference, { target: { value: 'policy' } });
    fireEvent.change(view.getByLabelText('DNS 地址（域名后缀 netflix.com）'), {
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
    expect(draft.ops().find(op => op.op === 'put_step')).toMatchObject({
      step: { rules: [{ a: { t: 'egress', dns: true } }] },
    });
  });

  it('does not render an unreferenced policy inside a chain rule', () => {
    const unreferenced: Rule[] = [
      { m: { t: 'domain_suffix', v: ['netflix.com'] }, a: { t: 'egress', send_through: null } },
    ];
    const view = renderEditor(false, unreferenced);

    expect((view.getByLabelText('DNS 策略（域名后缀 netflix.com）') as HTMLSelectElement).value).toBe('none');
    expect(view.queryByLabelText('DNS 地址（域名后缀 netflix.com）')).toBeNull();
    expect(view.queryByText('media')).toBeNull();
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

  it('edits the referenced machine policy inline with the authored route', () => {
    const view = renderEditor();
    const reference = view.getByLabelText('DNS 策略（域名后缀 netflix.com）') as HTMLSelectElement;
    expect(reference.value).toBe('policy');

    expect((view.getByLabelText('DNS 地址（域名后缀 netflix.com）') as HTMLInputElement).value).toBe('192.0.2.53');
    expect((view.getByLabelText('端口（域名后缀 netflix.com）') as HTMLInputElement).value).toBe('53');
    expect((view.getByLabelText('传输（域名后缀 netflix.com）') as HTMLSelectElement).value).toBe('tcp');
    const addressStrategy = view.getByLabelText('地址策略（域名后缀 netflix.com）') as HTMLSelectElement;
    expect(addressStrategy.value).toBe('use_ip');
    expect(Array.from(addressStrategy.options).map(option => option.text)).toEqual([
      'UseIP',
      'UseIPv4v6',
      'UseIPv6v4',
      'UseIPv4',
      'UseIPv6',
    ]);
    expect((view.getByLabelText('失败处理（域名后缀 netflix.com）') as HTMLSelectElement).value).toBe('stop');
    expect(view.container.querySelector('.egress-dns-note')?.textContent).toBe('机器全局 · 香港落地');
    expect(view.container.querySelector('.egress-dns-editor')?.querySelector('header, footer')).toBeNull();
    expect(view.getByLabelText('DNS 地址（域名后缀 netflix.com）').closest('tr')).toBe(
      view.getByDisplayValue('netflix.com').closest('tr'),
    );
    fireEvent.change(reference, { target: { value: 'none' } });
    expect(view.queryByLabelText('DNS 地址（域名后缀 netflix.com）')).toBeNull();
  });

  it('clears the DNS reference when the route selector changes', () => {
    const view = renderEditor();
    fireEvent.change(view.getByDisplayValue('域名后缀'), { target: { value: 'ip_cidr' } });

    expect((view.getByLabelText(/DNS 策略/) as HTMLSelectElement).value).toBe('none');
    expect(view.queryByLabelText('DNS 地址（域名后缀 netflix.com）')).toBeNull();
  });

  it('states that a reference is activation, not outbound isolation', () => {
    const view = renderEditor();

    expect(view.getByText(/Xray 限制：DNS 在机器内全局匹配/).textContent).toContain('tag 不能提供出站级隔离');
  });

  it('saves a referenced policy edit without rewriting an unchanged chain rule', async () => {
    draft.init('machine-dns-test');
    draft.clear();
    const view = renderEditor();
    const address = view.getByLabelText('DNS 地址（域名后缀 netflix.com）') as HTMLInputElement;

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
      expect((view.getByLabelText('DNS 地址（域名后缀 netflix.com）') as HTMLInputElement).value).toBe('192.0.2.53'),
    );
  });
});
