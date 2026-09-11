import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const styles = readFileSync(new URL('../src/styles.css', import.meta.url), 'utf8');

test('目标菜单脱离卡片裁切并保持监听摘要两行', () => {
  assert.match(styles, /\.external-target-menu\s*\{[^}]*position:\s*fixed;/s);
  assert.match(styles, /\.external-target-copy\s*\{[^}]*display:\s*grid;/s);
  assert.match(styles, /\.external-target-copy b,\s*\.external-target-copy small\s*\{[^}]*display:\s*block;/s);
});
