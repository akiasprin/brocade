import { describe, expect, it } from 'vitest';
import { buildFlowModel, flowCovers, flowKey } from '../src/panes/rule-flow/model';
import { flowGeositeSets, parseFlowTarget, traceFlow } from '../src/panes/rule-flow/trace';
import { COMPILED, NODES, SNAPSHOT } from './fixtures/rule-flow-data';

const app = SNAPSHOT.snapshot.apps[0];
const inputs = {
  chain: app.chains[0],
  root: 'hk-fxt',
  steps: app.steps.filter(step => step.chain === app.chains[0].id),
  snapshot: SNAPSHOT,
  nodes: NODES,
  compiled: COMPILED,
};
describe('A 流向数据投影', () => {
  it('separates listener ownership on the same machine and follows the referenced table', () => {
    const model = buildFlowModel(inputs);
    expect(model.machines.find(machine => machine.id === model.entry)?.listenPorts).toEqual([443]);
    const taiwan = model.machines.filter(machine => machine.node === 'tw-hinet');
    expect(taiwan).toHaveLength(2);
    expect(taiwan.find(machine => machine.chain === 'shared')?.rules[0].rule.a.t).toBe('block');
    expect(taiwan.find(machine => machine.chain === inputs.chain.id)?.rules[0].rule.a.t).toBe('egress');
    expect(taiwan.find(machine => machine.chain === 'shared')?.reference).toBe('共享监听');
    const trace = traceFlow(model, parseFlowTarget('ai.example', 'tcp')!);
    expect(trace.message).toContain('拒绝');
    expect(trace.machines.has(flowKey('shared', 'tw-hinet'))).toBe(true);
    expect(trace.machines.has(flowKey(inputs.chain.id, 'tw-hinet'))).toBe(false);
  });
  it('reads compiler tails and transport facts without inventing a default rule', () => {
    const model = buildFlowModel(inputs);
    const empty = model.machines.find(machine => machine.node === 'hk-akile')!;
    expect(empty.listenPorts).toEqual([20001]);
    expect(empty.rules[0].generated).toBe(true);
    expect(empty.rules[0].rule.a.t).toBe('egress');
    expect(model.machines.flatMap(machine => machine.rules).find(row => row.hop?.plaintext)?.hop?.label).toBe(
      '公网 IPv4',
    );
    const missing = buildFlowModel({ ...inputs, compiled: [] });
    expect(missing.machines.find(machine => machine.node === 'hk-akile')?.rules).toEqual([]);
    expect(missing.machines[0].rules[0].hop?.label).toBe('待编译');
  });
  it('updates unsaved graph edits and invalidates compiled hop details', () => {
    const rules = structuredClone(inputs.steps[0].rules);
    rules[0].a = { t: 'forward', to: 'jp', dial: { t: 'overlay' }, pool: { t: 'none' } };
    const model = buildFlowModel({ ...inputs, localRules: { 'hk-fxt': rules } });
    expect(model.machines[0].rules[0].target).toBe(flowKey(inputs.chain.id, 'jp'));
    expect(model.machines[0].rules[0].hop?.label).toBe('待编译');
    expect(model.machines[0].rules[0].hop?.plaintext).toBe(false);
    const pending = buildFlowModel({ ...inputs, uncompiledNodes: new Set(['hk-fxt']) });
    expect(pending.machines[0].rules[0].hop?.endpoint).toBe(null);
    const dns = buildFlowModel({
      ...inputs,
      pendingDnsNodes: new Set(['hk-akile']),
      pendingListeners: new Set(['hk-akile']),
    });
    expect(dns.machines.find(machine => machine.node === 'hk-akile')?.dnsPending).toBe(true);
    expect(dns.machines.find(machine => machine.node === 'hk-akile')?.listen).toContain('未保存');
    expect(dns.machines.find(machine => machine.node === 'hk-akile')?.listenPorts).toEqual([]);
  });
  it('proves literal coverage but never infers geosite membership from sample domains', () => {
    const model = buildFlowModel(inputs);
    expect(model.machines[0].rules[1].shadowedBy).toBe(1);
    expect(flowCovers({ t: 'geosite', v: ['category-ai-!cn'] }, { t: 'domain_suffix', v: ['openai.com'] })).toBe(false);
  });
  it('terminates cyclic graphs and retains unreachable tables', () => {
    const steps = structuredClone(inputs.steps);
    steps[0].rules = [
      { m: { t: 'any' }, a: { t: 'forward', to: 'hk-fxt', dial: { t: 'overlay' }, pool: { t: 'none' } } },
    ];
    const model = buildFlowModel({ ...inputs, steps });
    expect(model.machines.length).toBeGreaterThan(1);
    expect(model.machines.find(machine => machine.node === 'hk-akile')?.reachable).toBe(false);
    expect(traceFlow(model, parseFlowTarget('video.example', 'tcp')!).message).toContain('循环');
  });
});
describe('保守配置追踪', () => {
  it('does not bypass domain rules when an IP target may still be sniffed', () => {
    const model = buildFlowModel(inputs);
    const result = traceFlow(model, parseFlowTarget('192.0.2.1', 'tcp')!);
    expect(result.uncertain).toBe(model.machines[0].rules[0].id);
    expect(result.rules.size).toBe(0);
    expect(result.message).toContain('嗅探');
  });
  it('waits for the shared geosite instead of highlighting a later catch-all', () => {
    const model = buildFlowModel(inputs);
    const result = traceFlow(model, parseFlowTarget('unknown.example', 'tcp')!);
    expect(result.uncertain).toBe(model.machines[0].rules[10].id);
    expect(result.rules.size).toBe(0);
    expect(result.message).toContain('正在加载统一 geosite');
    expect(result.pending).toBe(true);
    expect(result.reason).toBe('正在加载统一 geosite 规则集');
  });
  it('follows geosite hits and continues past confirmed misses using the shared ruleset', () => {
    const model = buildFlowModel(inputs);
    const target = parseFlowTarget('openai.com', 'tcp')!;
    const hit = traceFlow(model, target, { matches: { 'category-ai-!cn': { hit: true } } });
    expect(hit.uncertain).toBeNull();
    expect(hit.machines.has(flowKey(inputs.chain.id, 'jp'))).toBe(true);
    expect(hit.rules.has(model.machines[0].rules[10].id)).toBe(true);
    const miss = traceFlow(model, target, { matches: { 'category-ai-!cn': { hit: false } } });
    expect(miss.rules.has(model.machines[0].rules[11].id)).toBe(true);
    expect(miss.machines.size).toBe(1);
  });
  it('preserves lookup failures and missing categories as unknown', () => {
    const model = buildFlowModel(inputs);
    const target = parseFlowTarget('unknown.example', 'tcp')!;
    for (const state of [
      { error: '统一 geosite 规则集暂时无法加载' },
      { matches: { 'category-ai-!cn': { hit: null, reason: '统一规则集不含该分类' } } },
    ]) {
      const result = traceFlow(model, target, state);
      expect(result.uncertain).toBe(model.machines[0].rules[10].id);
      expect(result.pending).toBe(false);
      expect(result.reason).toBeTruthy();
      expect(result.rules.size).toBe(0);
    }
  });
  it('collects nested selectors and evaluates geosite OR within all conditions', () => {
    const steps = structuredClone(inputs.steps);
    steps[0].rules = [
      {
        m: {
          t: 'all',
          v: [
            { t: 'geosite', v: ['missing', 'cn'] },
            { t: 'network', v: 'tcp' },
          ],
        },
        a: { t: 'block' },
      },
      { m: { t: 'any' }, a: { t: 'egress' } },
    ];
    const model = buildFlowModel({ ...inputs, steps });
    expect(flowGeositeSets(model)).toEqual(['cn', 'missing']);
    expect(
      traceFlow(model, parseFlowTarget('example.com', 'tcp')!, { matches: { cn: { hit: true } } }).message,
    ).toContain('拒绝');
    expect(
      traceFlow(model, parseFlowTarget('example.com', 'udp')!, { matches: { cn: { hit: true } } }).message,
    ).toContain('本机出网');
  });
  it('does not treat an IP as evidence of sniff failure', () => {
    const steps = structuredClone(inputs.steps);
    steps[0].rules = [
      { m: { t: 'sniffing_failed' }, a: { t: 'block' } },
      { m: { t: 'any' }, a: { t: 'egress' } },
    ];
    const model = buildFlowModel({ ...inputs, steps });
    const result = traceFlow(model, parseFlowTarget('192.0.2.1', 'tcp')!);
    expect(result.uncertain).toBe(model.machines[0].rules[0].id);
    expect(result.rules.size).toBe(0);
  });
  it.each([
    'https://example.com',
    '127.1',
    '999.1.1.1',
    'example.com:0',
    'example.com:65536',
    'a b',
    '[::invalid]',
    'a..b',
  ])('rejects invalid target %s', value => expect(parseFlowTarget(value, 'tcp')).toBe(null));
  it.each(['example.com:443', '[2001:db8::1]:53', '2001:db8::1', '192.0.2.1'])('accepts unambiguous target %s', value =>
    expect(parseFlowTarget(value, 'udp')?.network).toBe('udp'),
  );
});
