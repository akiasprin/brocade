import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const styles = readFileSync(new URL('../src/styles.css', import.meta.url), 'utf8');
const chains = readFileSync(new URL('../src/panes/chains.tsx', import.meta.url), 'utf8');

test('clickable cards do not expose sticky hover rules to touch pointers', () => {
  const css = styles.replace(/\/\*[\s\S]*?\*\//g, '');
  const pointerMedia = /@media \(hover: hover\) and \(pointer: fine\)\s*\{[\s\S]*?^\}/gm;
  const pointerRules = css.match(pointerMedia) ?? [];
  assert.match(pointerRules.join('\n'), /:is\(\.node-card, \.chain-card\):hover/);
  assert.doesNotMatch(
    css.replace(pointerMedia, ''),
    /:is\(\.node-card, \.chain-card\):hover/,
    'every card hover selector, including conditional fills, must require a fine hover pointer',
  );
});

test('touch cards retain immediate pressed feedback', () => {
  assert.match(styles, /:is\(\.node-card, \.chain-card\):is\(:active,[^}]*box-shadow:/s);
});

test('chain cards reject compatibility mouse dragging on coarse touch surfaces', () => {
  assert.match(styles, /\.chain-card\s*\{[^}]*touch-action: manipulation;/s);
  assert.match(chains, /matchMedia\('\(hover: hover\) and \(pointer: fine\)'\)\.matches/);
  assert.doesNotMatch(chains, /className="order-grip chain-order-grip"/);
});
