import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const api = readFileSync(new URL('../src/api.ts', import.meta.url), 'utf8');
const nodes = readFileSync(new URL('../src/panes/nodes.tsx', import.meta.url), 'utf8');

// 退役/移出弹层的选择、确认、焦点和横幅由 node-retire-dialog.test.tsx 按交互验证；
// 这里守住与 Console 的接口形状，以及机器列表不再承载批量操作。
test('安排移出调用机器生命周期接口，正文为空对象', () => {
  assert.match(
    api,
    /export const scheduleNodeRemoval = \(id: string\) =>\s+post<NodeLifecycleTransitionResult>\(`\/nodes\/\$\{encodeURIComponent\(id\)\}\/lifecycle\/removal`, \{\}\);/,
  );
  assert.match(api, /removal_status\?: 'queued' \| 'failed' \| null;/);
  assert.match(api, /removal_requested_at\?: string \| null;/);
  assert.match(api, /removal_last_error\?: string \| null;/);
});

test('移出调用 DELETE /nodes，并刷新随机器一起删除的线路、发布目标与用量', () => {
  assert.match(api, /api<RemoveRetiredNodesResult>\('\/nodes', '', \{/);
  assert.match(api, /method: 'DELETE'/);
  assert.match(api, /JSON\.stringify\(\{ node_ids: nodeIds \}\)/);
  assert.match(nodes, /removeRetiredNodes\(\[id\]\)/);
  for (const key of ['nodes', 'snapshot', 'revisions', 'deployments', 'usage-node-series', 'node-traffic']) {
    assert.match(nodes, new RegExp(`\\['${key}'\\],\\n`), `移出后应刷新 ${key}`);
  }
});

test('机器列表只用来查看，不再有多选、批量退役或批量移出', () => {
  for (const removed of ['node-card-pick', 'beginSelect', 'retireAll', 'confirmRemoval', '移除选中', '退役下线']) {
    assert.ok(!nodes.includes(removed), `nodes.tsx 不应再包含 ${removed}`);
  }
});
