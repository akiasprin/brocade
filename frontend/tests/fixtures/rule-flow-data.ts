import type { ConsoleSnapshot, Rule, SnapshotStep } from '../../src/api';
import type { FlowCompiledApp } from '../../src/panes/rule-flow/model';
import { AGENT_NODES as BASE_NODES, SETTINGS } from './rule-layout-data';

export { SETTINGS };
export const APP_ID = 'flow-app';
export const CHAIN_ID = 'flow-1';
export const NODES = [
  ...BASE_NODES.nodes.map((node, index) => ({ ...node, public_ipv4_country: index < 2 ? 'HK' : 'TW' })),
  {
    ...BASE_NODES.nodes[2],
    node_id: 'jp',
    name: '东京出口',
    public_ipv4: '192.0.2.99',
    public_ipv6: null,
    public_ipv4_country: 'JP',
  },
];
const forward = (to: string): Rule['a'] => ({ t: 'forward', to, dial: { t: 'overlay' }, pool: { t: 'none' } });
const egress: Rule = { m: { t: 'any' }, a: { t: 'egress', send_through: null } };
const step = (chain: string, node: string, rules: Rule[]): SnapshotStep => ({
  chain,
  node,
  rules,
  accept: node === 'hk-fxt' ? null : { uuid: 'synthetic-test-credential', label: `${chain}/${node}` },
  hop_in: { port: 20001, security: { t: 'none' } },
});
const chains = [
  '香港 01 @ CN2',
  '香港 02 @ CN2',
  '台湾 01 @ CN2',
  '日本 01 @ CN2',
  '美国 01 @ CN2',
  '新加坡 01 @ CN2',
  '德国 01 @ CN2',
].map((name, index) => ({ id: `flow-${index + 1}`, name, tenant: 'preview', subscription_country: 'HK' }));
const rootRules: Rule[] = [
  { m: { t: 'domain_suffix', v: ['video.example'] }, a: forward('hk-akile') },
  { m: { t: 'domain_suffix', v: ['shadow.video.example'] }, a: { t: 'block' } },
  {
    m: { t: 'domain_suffix', v: ['ai.example'] },
    a: {
      t: 'reuse_listener',
      listener: { chain: 'shared', node: 'tw-hinet' },
      dial: { t: 'overlay' },
      pool: { t: 'none' },
    },
  },
  { m: { t: 'domain_suffix', v: ['tw.example'] }, a: forward('tw-hinet') },
  ...['music', 'games', 'news', 'work', 'cloud', 'images'].map(
    name => ({ m: { t: 'domain_suffix', v: [`${name}.example`] }, a: forward('hk-akile') }) as Rule,
  ),
  { m: { t: 'geosite', v: ['category-ai-!cn'] }, a: forward('jp') },
  egress,
];
const steps = chains.flatMap((chain, index) =>
  index < 3
    ? [
        step(chain.id, 'hk-fxt', rootRules),
        step(chain.id, 'hk-akile', []),
        step(chain.id, 'tw-hinet', [egress]),
        step(chain.id, 'jp', [egress]),
      ]
    : [step(chain.id, 'hk-fxt', [{ m: { t: 'any' }, a: forward('jp') }]), step(chain.id, 'jp', [egress])],
);
steps.push(step('shared', 'tw-hinet', [{ m: { t: 'any' }, a: { t: 'block' } }]));
export const SNAPSHOT: ConsoleSnapshot = {
  snapshot: {
    revision: 1,
    nodes: NODES.map(node => ({ id: node.node_id, name: node.name, domain_strategy: 'as_is' })),
    apps: [
      {
        id: APP_ID,
        label: '合成规则验收',
        chains: [...chains, { id: 'shared', name: '共享监听', tenant: 'preview' }],
        steps,
        ingresses: chains.map(
          chain =>
            ({
              id: `in-${chain.id}`,
              chain: chain.id,
              node: 'hk-fxt',
              bind: '0.0.0.0',
              port: 443,
              projection: {},
              guard: {},
              identity: {},
              wires: {},
            }) as ConsoleSnapshot['snapshot']['apps'][number]['ingresses'][number],
        ),
        fronts: [],
        grants: [],
      },
    ],
    external_outbounds: [],
  },
  node_egress_dns: [
    {
      node: 'hk-akile',
      position: 0,
      selector: { t: 'geosite', v: ['video'] },
      resolution: {
        address: '192.0.2.53',
        port: 53,
        transport: 'udp',
        address_strategy: 'use_ip',
        fallback: 'machine',
      },
    },
    ...['dazn', 'abema'].map((category, index) => ({
      node: 'hk-akile',
      position: index + 1,
      selector: { t: 'geosite' as const, v: [category] },
      resolution: {
        address: ['192.0.2.53', '198.51.100.53', '2001:db8::53'],
        port: 53,
        transport: 'tcp' as const,
        address_strategy: 'use_ip' as const,
        fallback: 'machine' as const,
      },
    })),
  ],
  redacted: false,
};
export const COMPILED: FlowCompiledApp[] = [
  {
    steps: steps.map(step => ({
      chain: step.chain,
      node: step.node,
      rules: (step.rules.length ? step.rules : [egress]).map(rule => ({ dest_match: rule.m, action: rule.a })),
    })),
    hops: steps.flatMap(step => {
      const seen = new Set<string>();
      return step.rules.flatMap(rule => {
        const action = rule.a;
        const to = action.t === 'forward' ? action.to : action.t === 'reuse_listener' ? action.listener.node : null;
        const targetChain = action.t === 'reuse_listener' ? action.listener.chain : step.chain;
        if (!to || seen.has(`${targetChain}/${to}`)) return [];
        seen.add(`${targetChain}/${to}`);
        return [
          {
            chain: step.chain,
            target_chain: targetChain,
            from: step.node,
            to,
            address: to === 'jp' ? '192.0.2.99' : '10.66.0.2',
            port: 20001,
            path: to === 'jp' ? ('direct' as const) : ('overlay' as const),
            security: { t: to === 'jp' ? 'none' : 'encryption' },
            pool: { t: 'none' as const },
          },
        ];
      });
    }),
  },
];
