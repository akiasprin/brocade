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

test('用量与额度：读数栏沿用用量页写法，线路行不铺底色，窄卡把读数栏移到上方', () => {
  const users = readFileSync(new URL('../src/panes/users.tsx', import.meta.url), 'utf8');
  const ledger = users.slice(users.indexOf('function UserUsageLedger('), users.indexOf('function QuotaReset('));
  assert.match(ledger, /className="usage-split"/);
  assert.match(ledger, /className="usage-io"/);
  assert.doesNotMatch(users, /留空表示不限。/);

  assert.match(styles, /\.qta-t > i\s*\{[^}]*background:\s*var\(--data\);/);
  assert.match(styles, /\.qta-r\.warn \.qta-t > i\s*\{[^}]*background:\s*var\(--gold\);/);
  assert.match(styles, /\.qta-r\.over \.qta-t > i\s*\{[^}]*background:\s*var\(--err\);/);
  assert.doesNotMatch(styles, /\.qta-r(?:\.over|\.warn)?\s*\{[^}]*background:/);
  assert.match(styles, /\.panel\.config-panel\.user-usage-card\s*\{[^}]*container:\s*user-usage \/ inline-size;/);
  assert.match(
    styles,
    /@container user-usage \(max-width: 620px\)\s*\{[\s\S]*?\.user-usage\s*\{[^}]*grid-template-columns:\s*minmax\(0, 1fr\);[\s\S]*?\.qta-r\s*\{[^}]*grid-template-areas:/,
  );
});

test('在线接入时间线：IPv6 不截断，接入可换行，窄卡改为两行', () => {
  assert.doesNotMatch(styles, /\.user-presence-ip > code\s*\{[^}]*(text-overflow:\s*ellipsis|white-space:\s*nowrap)/);
  assert.match(styles, /\.user-presence-access\s*\{[^}]*flex-wrap:\s*wrap;/);
  assert.match(styles, /\.user-presence-node-link:focus-visible\s*\{[^}]*outline:/);
  assert.match(styles, /\.user-presence-more:focus-visible\s*\{[^}]*outline:/);
  assert.match(styles, /\.user-presence-sub\s*\{\s*display:\s*contents;/);
  assert.match(
    styles,
    /@container user-presence \(max-width: 620px\)\s*\{[\s\S]*?\.user-presence-row\s*\{[^}]*grid-template-areas:/,
  );
  assert.match(styles, /\.user-presence-mark\s*\{[^}]*background:\s*var\(--ok\);/);
});
