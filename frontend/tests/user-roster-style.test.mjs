import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const styles = readFileSync(new URL('../src/styles.css', import.meta.url), 'utf8');
const start = styles.indexOf('/* 名册筛选是页面内导航');
const end = styles.indexOf('.user-row:focus-visible', start);
const rosterStyles = start >= 0 && end > start ? styles.slice(start, end) : '';

test('用户名册筛选使用独立标签而不是整条分段凹槽', () => {
  assert.match(rosterStyles, /\.user-roster-filter\s*\{[\s\S]*?gap:\s*4px;/);
  assert.match(rosterStyles, /\.user-roster-filter\s*\{[\s\S]*?border:\s*0;/);
  assert.match(rosterStyles, /\.user-roster-filter\s*\{[\s\S]*?background:\s*none;/);
  assert.match(rosterStyles, /\.user-roster-filter button i\s*\{[\s\S]*?min-width:\s*16px;/);
});

test('用户名册选中行使用中性卡面和弱边线且不再绘制高亮竖条', () => {
  assert.match(rosterStyles, /\.user-row\.picked,[\s\S]*?background:\s*var\(--block\);/);
  assert.match(rosterStyles, /\.user-row\.picked,[\s\S]*?box-shadow:[\s\S]*?var\(--action\) 18%/);
  assert.doesNotMatch(rosterStyles, /\.user-row\.picked::before/);
});

test('在线来源支持长 IPv6 和节点名称换行，地区在窄屏独占一行', () => {
  assert.match(styles, /\.user-presence-source > code\s*\{[^}]*overflow-wrap:\s*anywhere;/);
  assert.match(styles, /\.user-presence-source-nodes\s*\{[^}]*flex-wrap:\s*wrap;/);
  assert.match(styles, /\.user-presence-node-link:focus-visible\s*\{[^}]*outline:/);
  assert.match(styles, /\.user-presence-source-node-list\s*\{[^}]*flex-wrap:\s*wrap;/);
  assert.match(styles, /\.user-presence-source-nodes-toggle:focus-visible\s*\{[^}]*outline:/);
  assert.match(styles, /\.user-presence-location\s*\{[^}]*grid-column:\s*2 \/ -1;/);
  assert.doesNotMatch(styles, /\.user-presence-meta\s*\{[^}]*(text-overflow:\s*ellipsis|white-space:\s*nowrap)/);
});
