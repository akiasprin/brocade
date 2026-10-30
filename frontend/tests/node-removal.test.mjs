import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const api = readFileSync(new URL('../src/api.ts', import.meta.url), 'utf8');
const nodes = readFileSync(new URL('../src/panes/nodes.tsx', import.meta.url), 'utf8');

// 多选里的退役与移除（各自的按钮、确认和移除中的状态）由 node-list.test.tsx 按交互验证；
// 这里守住「哪些机器可以移除」。
test('只有已停用的退役机器可以永久移除', () => {
  assert.match(
    nodes,
    /node\.retired_at !== null && \(node\.lifecycle_phase === 'retired' \|\| node\.lifecycle_phase === 'abandoned'\)/,
  );
  assert.match(nodes, /selectable=\{!n\.retired_at \|\| nodeRemovalReady\(n\)\}/);
  assert.match(nodes, /confirmRemoval\(pickedRetired\)/);
});

test('永久移除调用批量 DELETE 接口并明确清理关联线路', () => {
  assert.match(api, /api<RemoveRetiredNodesResult>\('\/nodes', '', \{/);
  assert.match(api, /method: 'DELETE'/);
  assert.match(api, /JSON\.stringify\(\{ node_ids: nodeIds \}\)/);
  assert.match(nodes, /所有包含这些机器的线路会一并删除/);
});
