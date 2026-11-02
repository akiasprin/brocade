import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const styles = readFileSync(new URL('../src/styles.css', import.meta.url), 'utf8');
const nodes = readFileSync(new URL('../src/panes/nodes.tsx', import.meta.url), 'utf8');

test('machine cards use tabular body text without list latency', () => {
  assert.match(styles, /\.node-cardpage\s*\{[^}]*font-variant-numeric: tabular-nums;/s);
  assert.doesNotMatch(nodes, /TcpProbeLatest|fetchLatestNodePingProbes|NodeRosterRow/);
  assert.match(nodes, /<NodeCardComet/);
});

test('machine-detail toolbar uses the shared button and more icon conventions', () => {
  assert.match(nodes, /className="btn observe-range-trigger"/);
  assert.match(nodes, /<Icon of="calendar" size=\{14\} className="nd-tool-icon observe-range-clock" \/>/);
  assert.match(nodes, /className="btn fg-more"[\s\S]*?<Icon of="more" size=\{14\} className="nd-tool-icon" \/>/);
  assert.doesNotMatch(styles, /\.nd-page-head \.nd-acts \.btn\s*\{[^}]*border-radius:/s);
  assert.match(styles, /\.nd-tool-icon\s*\{[^}]*display: inline-flex;[^}]*line-height: 0;/s);
});
