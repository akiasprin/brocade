import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const styles = readFileSync(new URL('../src/styles.css', import.meta.url), 'utf8');
const start = styles.indexOf('/* 用户页手机形态');
const end = styles.indexOf('/* 旧版的标记网格', start);
const mobileUsers = start >= 0 && end > start ? styles.slice(start, end) : '';

test('手机用户名册为十行高的纵向滚动列表且不横向溢出', () => {
  assert.match(mobileUsers, /\.user-roster-options\s*\{[\s\S]*?max-height:\s*520px;/);
  assert.match(mobileUsers, /\.user-roster-options\s*\{[\s\S]*?overflow-x:\s*hidden;/);
  assert.match(mobileUsers, /\.user-roster-options\s*\{[\s\S]*?overflow-y:\s*auto;/);
  assert.match(mobileUsers, /\.user-row\s*\{[\s\S]*?width:\s*100%;/);
  assert.doesNotMatch(mobileUsers, /scroll-snap-type:\s*x|overscroll-behavior-x|flex-basis:/);
});
