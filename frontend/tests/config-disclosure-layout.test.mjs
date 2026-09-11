import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const source = path => readFileSync(new URL(path, import.meta.url), 'utf8');

test('配置卡折叠统一使用同一摘要、状态和右侧开关布局', () => {
  const chains = source('../src/panes/chains.tsx');
  const nodes = source('../src/panes/nodes.tsx');
  const styles = source('../src/styles.css');

  assert.match(chains, /className="panel config-panel config-disclosure ingress-protocol-panel"/);
  assert.match(chains, /className="panel config-panel config-disclosure ingress-guard-panel"/);
  assert.match(nodes, /className="panel config-panel config-disclosure conn-card"/);
  assert.match(nodes, /className="panel config-panel config-disclosure node-log-retention"/);
  assert.doesNotMatch(chains, /ingress-protocol-(?:summary|toggle)/);

  assert.match(styles, /\.config-disclosure > summary\s*\{[^}]*align-items:\s*center;[^}]*min-width:\s*0;/s);
  assert.match(styles, /\.config-disclosure-summary\s*\{[^}]*text-overflow:\s*ellipsis;/s);
  assert.match(styles, /\.config-disclosure-toggle\s*\{[^}]*flex:\s*none;/s);
});
