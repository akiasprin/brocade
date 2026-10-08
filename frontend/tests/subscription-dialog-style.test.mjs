import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const styles = readFileSync(new URL('../src/styles.css', import.meta.url), 'utf8');
const viewport = readFileSync(new URL('../src/ui/viewport.ts', import.meta.url), 'utf8');
const start = styles.indexOf('/* 订阅与节点：同一个弹窗');
const end = styles.indexOf('@keyframes sub-sheet-out', start);
const dialog = start >= 0 && end > start ? styles.slice(start, end) : '';
const narrowMax = viewport.match(/export const NARROW_MAX = (\d+);/)?.[1];
const narrowAt = dialog.indexOf(`@media (max-width: ${narrowMax}px) {`);
const desktop = narrowAt > 0 ? dialog.slice(0, narrowAt) : '';
const narrow = narrowAt > 0 ? dialog.slice(narrowAt) : '';

test('订阅弹窗：筛选行上下各一条分隔线，正文不再另画上边线', () => {
  assert.ok(dialog, '订阅弹窗样式段存在');
  assert.match(
    desktop,
    /\.sub-dialog-filter\s*\{[^}]*border-top:\s*1px solid var\(--line\);[^}]*border-bottom:\s*1px solid var\(--line\);/,
  );
  assert.doesNotMatch(desktop, /\.sub-dialog-body\s*\{[^}]*border-top/);
  assert.doesNotMatch(desktop, /\.sub-dialog-head\s*\{[^}]*background/, '标题行不再画底色条');
});

test('订阅弹窗：节点地址是复制按钮，长 IPv6 只截主机、端口保留', () => {
  assert.match(desktop, /\.node-copy-host\s*\{[^}]*text-overflow:\s*ellipsis;/);
  assert.match(desktop, /\.node-copy-port\s*\{[^}]*flex:\s*none;/);
  assert.match(desktop, /\.node-copy:focus-visible\s*\{[^}]*outline:/);
  assert.doesNotMatch(styles, /\.node-insecure/, '「允许 insecure」开关已移进说明行');
});

test('订阅弹窗：窄屏与 useNarrow 同一断点，贴底成为面板，触屏目标放大', () => {
  assert.ok(narrowMax, 'ui/viewport.ts 定义 NARROW_MAX');
  assert.ok(narrow, `窄屏规则写在 @media (max-width: ${narrowMax}px)`);
  assert.match(narrow, /\.dialog-layer\.modal:has\(> \.sub-dialog\)\s*\{[^}]*place-items:\s*end stretch;/);
  assert.match(narrow, /\.dialog-surface\.sub-dialog\s*\{[^}]*border-radius:\s*16px 16px 0 0;/);
  assert.match(
    narrow,
    /\[data-motion-state='entering'\] > \.dialog-surface\.sub-dialog\s*\{\s*animation-name:\s*sub-sheet-in;/,
  );
  assert.match(narrow, /\.sub-dialog-foot\s*\{[^}]*var\(--safe-bottom\)/);
  assert.match(narrow, /\.sub-url > button,\s*\.sub-url > \.sub-url-copy\s*\{[^}]*height:\s*44px;/);
  assert.match(narrow, /\.node-copy\s*\{[^}]*height:\s*40px;/);
  // iOS 聚焦字号小于 16px 的表单控件会放大页面。
  assert.match(dialog, /\.sub-pick > select\s*\{[^}]*font-size:\s*16px;/);
});
