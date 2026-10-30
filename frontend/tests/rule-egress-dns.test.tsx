import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, waitFor, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { ConsoleSnapshot, NodeAgentStateItem, Rule } from '../src/api';
import * as api from '../src/api';
import { draft } from '../src/draft';
import { MachineEgressDnsRules, RuleEditor, type ForwardPeer } from '../src/panes/rules';
import { confirmDiscardChanges } from '../src/ui/navigation-guard';

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
  vi.restoreAllMocks();
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
  readOnly = false,
) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Number.POSITIVE_INFINITY } },
  });
  client.setQueryData<ConsoleSnapshot>(['snapshot'], {
    snapshot: {
      revision: 1,
      nodes: [
        { id: 'hk', name: '香港落地' },
        { id: 'sg', name: '新加坡落地' },
        { id: 'empty', name: '空节点' },
      ],
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
      <MachineEgressDnsRules nodeId="hk" nodeName="香港落地" showHeader={showHeader} readOnly={readOnly} />
    </QueryClientProvider>,
  );
}

describe('machine-scoped egress DNS', () => {
  it('stages a comma-separated group using one protocol and structured addresses', async () => {
    const view = renderMachineRules();
    const address = view.getByLabelText('DNS 地址（域名后缀 netflix.com）');
    fireEvent.change(address, { target: { value: '192.0.2.53, 2001:db8::53, 198.51.100.53' } });
    expect((address as HTMLInputElement).value).toBe('192.0.2.53, 2001:db8::53, 198.51.100.53');
    fireEvent.click(view.getByRole('button', { name: '保存到草稿' }));
    await waitFor(() => expect(draft.ops().some(op => op.op === 'set_node_egress_dns')).toBe(true));
    const op = draft.ops().find(op => op.op === 'set_node_egress_dns');
    expect(op).toMatchObject({
      resolution: { ...resolution, address: ['192.0.2.53', '2001:db8::53', '198.51.100.53'] },
    });
  });

  it('keeps typed addresses and reports an invalid group only after saving', async () => {
    const view = renderMachineRules();
    const value = '192.0.2.1, 192.0.2.2, 192.0.2.3, 192.0.2.4';
    const address = view.getByLabelText('DNS 地址（域名后缀 netflix.com）');
    fireEvent.change(address, { target: { value } });
    expect((address as HTMLInputElement).value).toBe(value);
    expect(view.queryByRole('alert')).toBeNull();
    expect(address.getAttribute('aria-invalid')).toBe('false');
    fireEvent.click(view.getByRole('button', { name: '保存到草稿' }));
    await waitFor(() => expect(view.getByRole('alert').textContent).toBe('每组最多 3 个 DNS 地址'));
    expect(address.getAttribute('aria-describedby')).toBe(view.getByRole('alert').id);
    expect(view.getByRole('alert').closest('.egress-dns-editor')).toBe(address.closest('.egress-dns-editor'));
    expect(draft.ops()).toHaveLength(0);
    fireEvent.change(address, { target: { value: '192.0.2.1, 2001:db8::53' } });
    await waitFor(() => expect(view.queryByRole('alert')).toBeNull());
    expect(address.getAttribute('aria-invalid')).toBe('false');
  });

  it('loads grouped addresses and rejects unfinished comma input on save before staging', async () => {
    const view = renderMachineRules(false, false, [
      {
        node: 'hk',
        position: 0,
        selector: { t: 'domain_suffix', v: ['netflix.com'] },
        resolution: { ...resolution, address: ['192.0.2.53', '2001:db8::53'] },
      },
    ]);
    const address = view.getByLabelText('DNS 地址（域名后缀 netflix.com）');
    expect((address as HTMLInputElement).value).toBe('192.0.2.53, 2001:db8::53');
    fireEvent.change(address, { target: { value: '192.0.2.53,' } });
    expect((address as HTMLInputElement).value).toBe('192.0.2.53,');
    expect(view.queryByRole('alert')).toBeNull();
    fireEvent.click(view.getByRole('button', { name: '保存到草稿' }));
    await waitFor(() => expect(view.getByRole('alert').textContent).toBe('请填写完整的 DNS 地址，用逗号分隔'));
    expect(draft.ops()).toHaveLength(0);
  });

  it('validates every policy before staging selector removals or other valid edits', async () => {
    const view = renderMachineRules();
    fireEvent.change(view.getByLabelText('DNS 匹配内容（域名后缀 netflix.com）'), {
      target: { value: 'disneyplus.com' },
    });
    const address = view.getByLabelText('DNS 地址（geosite media）');
    fireEvent.change(address, { target: { value: '' } });
    fireEvent.click(view.getByRole('button', { name: '保存到草稿' }));
    await waitFor(() => expect(view.getByRole('alert')).toBeTruthy());
    expect(view.getByRole('alert').closest('tr')).toBe(address.closest('tr'));
    expect(draft.ops()).toHaveLength(0);
  });

  it('shows a storage failure on its policy row and clears it when editing', async () => {
    vi.spyOn(api, 'setNodeEgressDns').mockRejectedValueOnce(new Error('草稿保存失败'));
    const view = renderMachineRules();
    const address = view.getByLabelText('DNS 地址（geosite media）');
    fireEvent.change(address, { target: { value: '198.51.100.53' } });
    expect(view.queryByRole('alert')).toBeNull();
    fireEvent.click(view.getByRole('button', { name: '保存到草稿' }));
    await waitFor(() => expect(view.getByRole('alert').textContent).toBe('草稿保存失败'));
    expect(view.getByRole('alert').closest('tr')).toBe(address.closest('tr'));
    expect(view.queryAllByRole('alert')).toHaveLength(1);
    fireEvent.change(address, { target: { value: '203.0.113.53' } });
    await waitFor(() => expect(view.queryByRole('alert')).toBeNull());
  });

  it('does not report a read-only rule table as an unsaved local edit', () => {
    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(false);
    renderEditor(
      false,
      [
        { m: { t: 'any' }, a: { t: 'block' } },
        { m: { t: 'domain_suffix', v: ['netflix.com'] }, a: { t: 'egress', send_through: null } },
      ],
      true,
    );

    expect(confirmDiscardChanges()).toBe(true);
    expect(confirm).not.toHaveBeenCalled();
  });

  it('moves an edited rule into the browser draft on navigation without prompting', () => {
    draft.init('rule-navigation-autosave-test');
    draft.clear();
    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(false);
    const view = renderEditor(false, initial, false, []);
    const match = view.container.querySelector<HTMLInputElement>('td.rule-match-cell input')!;

    fireEvent.change(match, { target: { value: 'disneyplus.com' } });

    expect(confirmDiscardChanges()).toBe(true);
    expect(confirm).not.toHaveBeenCalled();
    expect(draft.ops()).toEqual([
      {
        op: 'put_step',
        app_id: 'video',
        chain_id: 'stream',
        node_id: 'hk',
        step: {
          rules: [{ m: { t: 'domain_suffix', v: ['disneyplus.com'] }, a: { t: 'egress', send_through: null } }],
        },
      },
      { op: 'prune_chain', app_id: 'video', chain_id: 'stream' },
    ]);
  });

  it('offers a safe sniffing fallback and pins it immediately before Any', () => {
    const view = renderEditor(
      false,
      [
        { m: { t: 'geosite', v: ['netflix'] }, a: { t: 'egress', send_through: null } },
        { m: { t: 'any' }, a: { t: 'egress', send_through: null } },
      ],
      false,
      [],
      true,
      true,
      [],
    );
    const matchSelects = () =>
      Array.from(view.container.querySelectorAll<HTMLSelectElement>('td.rule-match-cell > select'));

    expect(matchSelects().map(select => select.value)).toEqual(['geosite', 'any']);
    fireEvent.click(view.getByRole('button', { name: '＋ 嗅探失败兜底' }));

    expect(matchSelects().map(select => select.value)).toEqual(['geosite', 'sniffing_failed', 'any']);
    const fallbackSelect = matchSelects().find(select => select.value === 'sniffing_failed')!;
    const fallbackRow = fallbackSelect.closest('tr')!;
    const action = fallbackRow.querySelector<HTMLSelectElement>('select.rule-action-select')!;
    const egress = action.querySelector<HTMLOptionElement>('option[value="egress"]')!;
    const moveButtons = fallbackRow.querySelectorAll<HTMLButtonElement>('button.btn:not(.danger)');
    expect(action.value).toBe('egress');
    expect(egress.disabled).toBe(false);
    expect(moveButtons[0].disabled).toBe(true);
    expect(moveButtons[1].disabled).toBe(true);
    fireEvent.change(action, { target: { value: 'block' } });
    expect(action.value).toBe('block');

    const geositeFallbackOption = matchSelects()[0].querySelector<HTMLOptionElement>(
      'option[value="sniffing_failed"]',
    )!;
    expect(geositeFallbackOption.disabled).toBe(true);

    fireEvent.click(view.getByRole('button', { name: '＋ 加一条' }));
    expect(matchSelects().map(select => select.value)).toEqual(['geosite', 'domain_suffix', 'sniffing_failed', 'any']);
  });

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
    expect(view.queryByRole('alert')).toBeNull();
    expect((view.getByLabelText('DNS 匹配类型（geosite）') as HTMLSelectElement).value).toBe('geosite');
    fireEvent.change(view.getByLabelText('DNS 匹配内容（geosite）'), { target: { value: 'netflix' } });
    fireEvent.click(save);
    await waitFor(() => expect(view.getByRole('alert').textContent).toBe('请填写完整的 DNS 地址，用逗号分隔'));
    expect(draft.ops()).toHaveLength(0);
    fireEvent.change(view.getByLabelText('DNS 地址（geosite netflix）'), { target: { value: '1.1.1.1' } });
    await waitFor(() => expect(view.queryByRole('alert')).toBeNull());
    expect(save.disabled).toBe(false);
    fireEvent.click(save);

    await waitFor(() => expect(draft.ops()).toHaveLength(2));
    expect(draft.ops()[0]).toEqual({
      op: 'set_node_egress_dns',
      node_id: 'hk',
      selector: { t: 'geosite', v: ['netflix'] },
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
      selectors: [{ t: 'geosite', v: ['netflix'] }],
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
      { t: 'geosite', v: ['one.example'] },
      { t: 'geosite', v: ['two.example'] },
    ]);
    expect(draft.ops()[2]).toMatchObject({
      op: 'reorder_node_egress_dns',
      selectors: [
        { t: 'geosite', v: ['one.example'] },
        { t: 'geosite', v: ['two.example'] },
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

  it('copies the edited policy beside its source and saves the renamed copy with all DNS options', async () => {
    draft.init('machine-dns-copy-test');
    draft.clear();
    const grouped = {
      address: ['192.0.2.53', '2001:db8::53'],
      port: 5353,
      transport: 'udp' as const,
      address_strategy: 'use_ipv6' as const,
      fallback: 'machine' as const,
    };
    const view = renderMachineRules(false, true, [
      { node: 'hk', position: 0, selector: { t: 'geosite', v: ['disney'] }, resolution: grouped },
      { node: 'hk', position: 1, selector: { t: 'geosite', v: ['media'] }, resolution },
    ]);
    fireEvent.change(view.getByLabelText('端口（geosite disney）'), { target: { value: '5354' } });
    fireEvent.click(view.getByRole('button', { name: '复制 DNS 策略（geosite disney）' }));

    const rows = view.container.querySelectorAll('.node-egress-rules-table tbody tr');
    expect(rows).toHaveLength(3);
    const copy = within(rows[1] as HTMLElement);
    const match = copy.getByLabelText('DNS 匹配内容（geosite disney）');
    expect(document.activeElement).toBe(match);
    expect((copy.getByLabelText('DNS 地址（geosite disney）') as HTMLInputElement).value).toBe(
      '192.0.2.53, 2001:db8::53',
    );
    expect((copy.getByLabelText('端口（geosite disney）') as HTMLInputElement).value).toBe('5354');
    expect((copy.getByLabelText('传输（geosite disney）') as HTMLSelectElement).value).toBe('udp');
    expect((copy.getByLabelText('地址策略（geosite disney）') as HTMLSelectElement).value).toBe('use_ipv6');
    expect((copy.getByLabelText('失败处理（geosite disney）') as HTMLSelectElement).value).toBe('machine');
    expect(view.queryByRole('alert')).toBeNull();
    fireEvent.change(match, { target: { value: 'dazn' } });
    fireEvent.change(copy.getByLabelText('DNS 地址（geosite dazn）'), { target: { value: '198.51.100.53' } });
    expect((view.getByLabelText('DNS 地址（geosite disney）') as HTMLInputElement).value).toBe(
      '192.0.2.53, 2001:db8::53',
    );
    expect(grouped.address).toEqual(['192.0.2.53', '2001:db8::53']);
    fireEvent.click(view.getByRole('button', { name: '保存到草稿' }));

    await waitFor(() => expect(draft.ops()).toHaveLength(3));
    expect(draft.ops()[1]).toMatchObject({
      op: 'set_node_egress_dns',
      node_id: 'hk',
      selector: { t: 'geosite', v: ['dazn'] },
      resolution: { ...grouped, address: '198.51.100.53', port: 5354 },
    });
    expect(draft.ops()[2]).toEqual({
      op: 'reorder_node_egress_dns',
      node_id: 'hk',
      selectors: [
        { t: 'geosite', v: ['disney'] },
        { t: 'geosite', v: ['dazn'] },
        { t: 'geosite', v: ['media'] },
      ],
    });
  });

  it('appends imported rows in source order without checking overlapping selectors or replacing local edits', () => {
    draft.init('machine-dns-import-overlap-test');
    draft.clear();
    const view = renderMachineRules(false, true, [
      { node: 'hk', position: 0, selector: { t: 'geosite', v: ['disney'] }, resolution },
      { node: 'sg', position: 1, selector: { t: 'geosite', v: ['netflix'] }, resolution },
      { node: 'sg', position: 0, selector: { t: 'geosite', v: ['disney'] }, resolution },
    ]);
    fireEvent.change(view.getByLabelText('DNS 地址（geosite disney）'), { target: { value: '203.0.113.53' } });
    fireEvent.click(view.getByRole('button', { name: '从节点导入' }));
    const dialog = within(view.getByRole('dialog', { name: '从节点导入 DNS 策略' }));
    const source = dialog.getByLabelText('来源节点');
    expect(within(source).queryByRole('option', { name: '香港落地' })).toBeNull();
    fireEvent.change(source, { target: { value: 'sg' } });
    expect(dialog.getByText('将导入 2 条策略，按原顺序追加到当前列表末尾。')).toBeTruthy();
    fireEvent.click(dialog.getByRole('button', { name: '追加导入' }));

    const rows = [...view.container.querySelectorAll('.node-egress-rules-table tbody tr')];
    expect(rows.map(row => row.querySelector<HTMLInputElement>('.rule-new-dns-value')!.value)).toEqual([
      'disney',
      'disney',
      'netflix',
    ]);
    expect(
      view.getAllByLabelText('DNS 地址（geosite disney）').map(input => (input as HTMLInputElement).value),
    ).toEqual(['203.0.113.53', '192.0.2.53']);
    expect(view.queryByRole('alert')).toBeNull();
    expect(view.queryByRole('dialog')).toBeNull();
    expect(draft.ops()).toHaveLength(0);
  });

  it('saves imported DNS groups and their appended order without writing the source node', async () => {
    draft.init('machine-dns-import-save-test');
    draft.clear();
    const imported = { ...resolution, address: ['198.51.100.53', '2001:db8::53'], transport: 'udp' as const };
    const view = renderMachineRules(false, true, [
      { node: 'hk', position: 0, selector: { t: 'geosite', v: ['disney'] }, resolution },
      { node: 'sg', position: 1, selector: { t: 'geosite', v: ['spotify'] }, resolution: imported },
      { node: 'sg', position: 0, selector: { t: 'geosite', v: ['netflix'] }, resolution: imported },
    ]);
    fireEvent.click(view.getByRole('button', { name: '从节点导入' }));
    fireEvent.change(view.getByLabelText('来源节点'), { target: { value: 'sg' } });
    fireEvent.click(view.getByRole('button', { name: '追加导入' }));
    fireEvent.click(view.getByRole('button', { name: '保存到草稿' }));

    await waitFor(() => expect(draft.ops()).toHaveLength(3));
    expect(draft.ops()).toEqual([
      { op: 'set_node_egress_dns', node_id: 'hk', selector: { t: 'geosite', v: ['netflix'] }, resolution: imported },
      { op: 'set_node_egress_dns', node_id: 'hk', selector: { t: 'geosite', v: ['spotify'] }, resolution: imported },
      {
        op: 'reorder_node_egress_dns',
        node_id: 'hk',
        selectors: [
          { t: 'geosite', v: ['disney'] },
          { t: 'geosite', v: ['netflix'] },
          { t: 'geosite', v: ['spotify'] },
        ],
      },
    ]);
  });

  it('imports into an empty machine and allows cancelling the added rows', () => {
    const view = renderMachineRules(false, true, [
      { node: 'sg', position: 0, selector: { t: 'geosite', v: ['netflix'] }, resolution },
    ]);
    fireEvent.click(view.getByRole('button', { name: '从节点导入' }));
    fireEvent.change(view.getByLabelText('来源节点'), { target: { value: 'sg' } });
    fireEvent.click(view.getByRole('button', { name: '追加导入' }));
    expect(view.getByLabelText('DNS 匹配内容（geosite netflix）')).toBeTruthy();
    fireEvent.click(view.getByRole('button', { name: '取消' }));
    expect(view.queryByLabelText('DNS 匹配内容（geosite netflix）')).toBeNull();
  });

  it('supports keyboard sorting from the drag handle without altering the DNS fields', async () => {
    draft.init('machine-dns-keyboard-order-test');
    draft.clear();
    const view = renderMachineRules();
    const handle = view.getByRole('button', { name: '拖动排序 DNS 策略（geosite media）' });
    fireEvent.keyDown(handle, { key: 'ArrowUp' });
    expect(document.activeElement).toBe(handle);
    expect(
      [...view.container.querySelectorAll<HTMLInputElement>('.rule-new-dns-value')].map(input => input.value),
    ).toEqual(['media', 'netflix.com']);
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

  it('does not leave a focus request behind when a keyboard move reaches the list boundary', () => {
    const view = renderMachineRules();
    fireEvent.keyDown(view.getByRole('button', { name: '拖动排序 DNS 策略（域名后缀 netflix.com）' }), {
      key: 'ArrowUp',
    });
    const address = view.getByLabelText('DNS 地址（域名后缀 netflix.com）');
    address.focus();
    fireEvent.change(address, { target: { value: '203.0.113.53' } });
    expect(document.activeElement).toBe(address);
  });

  it('keeps copying, importing and drag sorting unavailable to read-only viewers', () => {
    const view = renderMachineRules(false, true, undefined, true);
    expect(view.queryByRole('button', { name: /复制 DNS 策略/ })).toBeNull();
    expect(view.queryByRole('button', { name: /拖动排序 DNS 策略/ })).toBeNull();
    expect((view.getByRole('button', { name: '从节点导入' }) as HTMLButtonElement).disabled).toBe(true);
  });

  it('states the group input and machine-wide DNS limitation plainly', () => {
    const view = renderMachineRules();

    const hint = view.getByText(
      'DNS 地址可用逗号分隔，最多 3 个，同组竞速。Xray 的解析结果无法按出站隔离。',
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
    expect(view.queryByText('机器全局下发')).toBeNull();
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
    expect(view.queryByText('机器全局下发')).toBeNull();
  });

  it('does not change policy status when another chain has the same route selector', () => {
    const view = renderEditor(true, [
      { m: { t: 'domain_suffix', v: ['netflix.com'] }, a: { t: 'egress', send_through: null } },
    ]);

    expect(view.queryByText('机器全局下发')).toBeNull();
    expect(view.container.textContent).not.toContain('其他链路触发');
  });

  it('renders every compiled fallback as a full read-only rule row', () => {
    const view = renderEditor();
    const fallback = view.container.querySelector('.rule-fallback-row');

    expect(fallback).not.toBeNull();
    expect(fallback?.querySelectorAll('td')).toHaveLength(4);
    expect(fallback?.textContent).toContain('任意');
    expect(fallback?.textContent).toContain('拒绝');
    expect(fallback?.textContent).toContain('自动补齐');
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
    expect(view.queryByText('机器全局下发')).toBeNull();

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
    expect(view.container.querySelector('.egress-dns-note')).toBeNull();
    expect(view.container.textContent).not.toContain('Xray 全局生效');
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

  it('shows group input guidance and the DNS limitation in the chain editor', () => {
    const view = renderEditor();

    expect(
      view.getByText('DNS 地址可用逗号分隔，最多 3 个，同组竞速。Xray 的解析结果无法按出站隔离。'),
    ).toBeTruthy();
  });

  it('highlights the NODE target instead of the Forward action and keeps focus on the trigger', () => {
    const forwardRules: Rule[] = [
      {
        m: { t: 'domain_suffix', v: ['example.com'] },
        a: { t: 'forward', to: 'sg', dial: { t: 'overlay' }, pool: { t: 'none' } },
      },
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
      {
        id: 'jp',
        name: '东京节点',
        public_ipv4: 'jp.example.net',
        public_ipv6: null,
        public_ipv4_nat: false,
        public_ipv6_nat: false,
        step: null,
        where: 'outside',
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
    expect(view.getByPlaceholderText('搜索机器或代理出站').hasAttribute('autofocus')).toBe(false);
    expect(trigger.getAttribute('aria-expanded')).toBe('true');
    const newListener = view.getByRole('button', { name: /NODE.*东京节点.*加入本链/ });
    expect(newListener.querySelector('.external-target-kind')?.textContent).toBe('NODE');
    expect(newListener.querySelector('.new-listener')).toBeNull();
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
