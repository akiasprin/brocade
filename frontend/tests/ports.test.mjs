import assert from 'node:assert/strict';
import test from 'node:test';
import { freePortAcross, freeSpanAcross, occupiedPorts, spanClash } from '../src/panes/ports.ts';

const taken = entries => new Map([['n1', new Map(entries)]]);

test('a hop range skips forward past the port that blocked it, not by one', () => {
  // 50002 被占：以 50000、50001、50002 起头的 3 连段全都不成立，所以下一个候选是 50003。
  // 逐个 +1 也能得出同样的答案，只是要多试三次；差别在于中间那次「起点空着、段内撞了」
  // 的情形——写成 start+1 的实现会在这里反复试，段一长就退化。
  const busy = taken([[50002, '别人']]);
  assert.equal(freeSpanAcross(busy, ['n1'], 50000, 3), 50003);
});

test('the span has to be clear all the way, not just at its head', () => {
  const busy = taken([[50005, '别人']]);
  // 50000 起头的 10 连段覆盖 50000-50009，里面有 50005，所以整段作废。
  assert.equal(freeSpanAcross(busy, ['n1'], 50000, 10), 50006);
  // 而单个口的分配器只看段首，会满意地返回 50000——这正是跳转不能复用它的原因。
  assert.equal(freePortAcross(busy, ['n1'], 50000), 50000);
});

test('a free run is returned as-is', () => {
  assert.equal(freeSpanAcross(new Map(), ['n1'], 50000, 10), 50000);
});

test('spanClash names the first occupied port in the range', () => {
  const busy = taken([[50007, '项目 a 的接入面 i-1']]);
  assert.match(spanClash(busy, ['n1'], 50000, 50009), /50007 已经被项目 a 的接入面 i-1占着/);
  assert.equal(spanClash(busy, ['n1'], 50000, 50006), null);
});

test('the UDP side counts the hysteria2 port and its whole hop range', () => {
  const apps = [
    {
      id: 'a',
      ingresses: [
        {
          id: 'i-1',
          node: 'n1',
          port: 443,
          wires: {
            vless: { kind: 'vless-reality' },
            hysteria2: { port: 50000, hop: { start: 50000, end: 50009 } },
          },
        },
      ],
      steps: [],
    },
  ];

  const udp = occupiedPorts(apps, [], undefined, undefined, 'udp');
  const owners = udp.get('n1');
  // 监听口和整段都算 UDP 上的占用；跳转把那一段全重定向走了，谁开在里面谁收不到包。
  assert.ok(owners.has(50000));
  assert.ok(owners.has(50009));
  assert.equal(owners.has(50010), false);
  // TCP 那半的 443 不该出现在 UDP 这张表里，反过来也一样。
  assert.equal(owners.has(443), false);

  const tcp = occupiedPorts(apps, [], undefined, undefined, 'tcp');
  assert.ok(tcp.get('n1').has(443));
  assert.equal(tcp.get('n1').has(50000), false);
});
