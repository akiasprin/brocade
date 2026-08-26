import assert from 'node:assert/strict';
import test from 'node:test';
import { hopPathCopy } from '../src/panes/hop-inspect.ts';

test('a reverse hop is described as an inbound path rather than a direct target', () => {
  assert.deepEqual(hopPathCopy('reverse'), {
    endpointLabel: '反向拨入',
    badge: 'reverse',
    detail: '下游拨入上游，流量沿隧道反向传输',
    plaintextDetail: '公网反向接入，UUID 和拨入地址是明文的',
    publicNetwork: true,
  });
});

test('ordinary hop path descriptions keep their existing meanings', () => {
  assert.equal(hopPathCopy('overlay').endpointLabel, '目标');
  assert.equal(hopPathCopy('overlay').publicNetwork, false);
  assert.equal(hopPathCopy('direct').badge, 'direct');
  assert.match(hopPathCopy('direct').plaintextDetail, /目标地址/);
});
