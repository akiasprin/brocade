import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const styles = readFileSync(new URL('../src/styles.css', import.meta.url), 'utf8');
const declarations = selector => {
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  return styles.match(new RegExp(`^${escaped} \\{([^}]*)\\}`, 'm'))?.[1] ?? '';
};

test('rule loading stays content-sized and follows the card left edge', () => {
  const rule = declarations('.node-rule-card-state');
  assert.ok(rule);
  assert.match(rule, /justify-content:\s*flex-start;/);
  assert.doesNotMatch(rule, /(?:min-|max-)?height\s*:/);
});

test('shared panel progress leaves width and alignment to its owning surface', () => {
  const indicator = declarations('.loading-mark-panel');
  assert.doesNotMatch(indicator, /(?:min-|max-)?width\s*:|justify-content\s*:/);
  const chart = declarations('.node-observation-reading');
  assert.match(chart, /height:\s*216px;/);
  assert.match(chart, /justify-content:\s*center;/);
});
