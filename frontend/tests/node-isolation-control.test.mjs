import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const api = readFileSync(new URL('../src/api.ts', import.meta.url), 'utf8');
const nodes = readFileSync(new URL('../src/panes/nodes.tsx', import.meta.url), 'utf8');

test('机器详情提供带确认的隔离入口并刷新机器与发布状态', () => {
  assert.match(
    api,
    /export const isolateNode = \(nodeId: string, acknowledgeUncertain = false\)[\s\S]*`\/nodes\/\$\{encodeURIComponent\(nodeId\)\}\/isolate`[\s\S]*acknowledge_uncertain: acknowledgeUncertain/,
  );
  assert.match(
    nodes,
    /!n\.operationally_isolated && n\.lifecycle_phase === 'active'[\s\S]*隔离机器[\s\S]*进行中的发布转为隔离待补偿/,
  );
  assert.match(
    nodes,
    /title=\{`隔离 \$\{n\.name \|\| id\}`\}[\s\S]*confirmLabel="确认隔离"[\s\S]*onConfirm=\{\(\) => isolate\.mutate\(\)\}/,
  );
  assert.match(
    nodes,
    /mutationFn: \(\) => isolateNode\(id, true\)[\s\S]*setConfirmIsolationFor\(null\);\s*refresh\(\);\s*qc\.invalidateQueries\(\{ queryKey: \['deployments'\] \}\)/,
  );
});
