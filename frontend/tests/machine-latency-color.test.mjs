import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const styles = readFileSync(new URL('../src/styles.css', import.meta.url), 'utf8');
const nodes = readFileSync(new URL('../src/panes/nodes.tsx', import.meta.url), 'utf8');

test('machine-card latency stays neutral when a probe sample is stale', () => {
  assert.match(styles, /\.nc-tcp-latest\s*\{[^}]*color: var\(--ink-4\);/s);
  assert.doesNotMatch(styles, /\.nc-tcp-latest\.stale\s*\{[^}]*color:/s);
});

test('machine-card status, region and name use one consistent gap', () => {
  assert.match(styles, /\.nc-status-slot\s*\{[^}]*margin-right: 9px;/s);
  assert.match(styles, /\.nc-region-flag\s*\{[^}]*margin-right: 9px;/s);
  assert.doesNotMatch(styles, /\.nc-status-slot\.with-region\s*\{/s);
});

test('machine-card monthly usage suffix is vertically centered with its value', () => {
  assert.match(styles, /\.nc-foot\s*\{[^}]*align-items: center;/s);
  assert.match(styles, /\.nc-foot \.lst-sum\s*\{[^}]*display: inline-flex;[^}]*align-items: center;/s);
  assert.match(styles, /\.nc-foot \.lst-sum small\s*\{[^}]*font: 10\.5px\/1 var\(--sans\);/s);
});

test('machine-detail toolbar uses the shared button and more icon conventions', () => {
  assert.match(nodes, /className="btn observe-range-trigger"/);
  assert.match(nodes, /<Icon of="calendar" size=\{14\} className="nd-tool-icon observe-range-clock" \/>/);
  assert.match(nodes, /className="btn fg-more"[\s\S]*?<Icon of="more" size=\{14\} className="nd-tool-icon" \/>/);
  assert.doesNotMatch(styles, /\.nd-page-head \.nd-acts \.btn\s*\{[^}]*border-radius:/s);
  assert.match(styles, /\.nd-tool-icon\s*\{[^}]*display: inline-flex;[^}]*line-height: 0;/s);
});
