import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const styles = readFileSync(new URL('../src/styles.css', import.meta.url), 'utf8');
const chains = readFileSync(new URL('../src/panes/chains.tsx', import.meta.url), 'utf8');

test('clickable cards do not expose sticky hover rules to touch pointers', () => {
  assert.doesNotMatch(styles, /^(?:\.ncard|\.chain-card):hover/m);
  assert.match(styles, /@media \(hover: hover\) and \(pointer: fine\)\s*\{\s*\.ncard:hover\s*\{[^}]+\}\s*\}/s);
  assert.match(styles, /@media \(hover: hover\) and \(pointer: fine\)\s*\{\s*\.chain-card:hover\s*\{[^}]+\}\s*\}/s);
});

test('touch cards retain immediate pressed feedback', () => {
  assert.match(styles, /\.ncard:active\s*\{[^}]*border-color:/s);
  assert.match(styles, /\.chain-card:active\s*\{[^}]*border-color:/s);
});

test('chain cards reject compatibility mouse dragging on coarse touch surfaces', () => {
  assert.match(styles, /\.chain-card\s*\{[^}]*touch-action: manipulation;/s);
  assert.match(chains, /matchMedia\('\(hover: hover\) and \(pointer: fine\)'\)\.matches/);
  assert.doesNotMatch(chains, /className="order-grip chain-order-grip"/);
});
