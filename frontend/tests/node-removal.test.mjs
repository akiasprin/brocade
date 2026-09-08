import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const api = readFileSync(new URL('../src/api.ts', import.meta.url), 'utf8');
const nodes = readFileSync(new URL('../src/panes/nodes.tsx', import.meta.url), 'utf8');

test('退役机器面板提供多选和移除全部，并只选择终态机器', () => {
  assert.match(nodes, />\s*多选\s*</);
  assert.match(nodes, /'移除全部'/);
  assert.match(nodes, /className="panel titled node-retired-panel">\s*<header>/);
  assert.match(nodes, /<Icon of="trash" size=\{13\} className="node-remove-button-icon" \/>/);
  assert.match(nodes, /node\.lifecycle_phase === 'retired' \|\| node\.lifecycle_phase === 'abandoned'/);
  assert.match(nodes, /confirmRemoval\(removable\.map\(node => node\.node_id\)\)/);
});

test('永久移除调用批量 DELETE 接口并明确清理关联线路', () => {
  assert.match(api, /api<RemoveRetiredNodesResult>\('\/nodes', '', \{/);
  assert.match(api, /method: 'DELETE'/);
  assert.match(api, /JSON\.stringify\(\{ node_ids: nodeIds \}\)/);
  assert.match(nodes, /所有包含这些机器的线路会一并删除/);
});
