import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const styles = readFileSync(new URL('../src/styles.css', import.meta.url), 'utf8');

test('目标菜单脱离卡片裁切，监听摘要保持两行并可圈出引用子树', () => {
  assert.match(styles, /\.external-target-menu\s*\{[^}]*position:\s*fixed;/s);
  assert.match(styles, /\.external-target-copy\s*\{[^}]*display:\s*grid;/s);
  assert.match(styles, /\.external-target-copy b,\s*\.external-target-copy small\s*\{[^}]*display:\s*block;/s);
  assert.match(styles, /\.listener-reference-copy\s*\{[^}]*display:\s*grid;[^}]*flex:\s*1 1 auto;/s);
  assert.match(
    styles,
    /\.listener-map-node\.is-highlighted-subtree::before\s*\{[^}]*border:\s*1px dashed var\(--listener-ref\);/s,
  );
  assert.match(styles, /\.listener-reference-highlight-hitbox\s*\{[^}]*position:\s*absolute;[^}]*inset:\s*0;/s);
  assert.match(styles, /\.listener-reference-panel\.hop-target-panel\s*\{[^}]*--listener-panel-accent:/s);
});
