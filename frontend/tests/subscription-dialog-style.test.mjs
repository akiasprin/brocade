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

test('订阅卡券：二维码白底不反色，撕线缺口用遮罩，地址行变长不撑宽列', () => {
  assert.match(desktop, /\.sub-qr\s*\{[^}]*background:\s*#fff;/);
  assert.doesNotMatch(dialog, /--sub-qr-(?:bg|fg)/, '二维码不再随主题换色');
  assert.match(desktop, /\.sub-pass-top\s*\{[^}]*\bmask:[^}]*at 0 100%[^}]*at 100% 100%/);
  assert.match(desktop, /\.sub-pass-code\s*\{[^}]*\bmask:[^}]*at 0 0,[^}]*at 100% 0,/);
  assert.match(desktop, /\.sub-pass-stack\s*\{[^}]*filter:\s*drop-shadow/, '投影沿缺口走，不用会被遮罩裁掉的 box-shadow');
  // 筛选参数让地址行变长时，网格列若按内容撑宽，居中的二维码会跟着右移。
  assert.match(desktop, /\.sub-pass-top,\s*\.sub-pass-code\s*\{[^}]*grid-template-columns:\s*minmax\(0, 1fr\);/);
  assert.match(desktop, /\.sub-pass-address > code\s*\{[^}]*text-overflow:\s*ellipsis;[^}]*white-space:\s*nowrap;/);
  assert.match(
    desktop,
    /@media \(prefers-reduced-motion: reduce\)\s*\{[^@]*\.sub-pass-stack\.swapped > \.sub-pass\s*\{\s*animation:\s*none;/,
  );
});

test('订阅弹窗：节点地址是复制按钮，长 IPv6 只截主机、端口保留', () => {
  assert.match(desktop, /\.node-copy-host\s*\{[^}]*text-overflow:\s*ellipsis;/);
  assert.match(desktop, /\.node-copy-port\s*\{[^}]*flex:\s*none;/);
  assert.match(desktop, /\.node-copy:focus-visible\s*\{[^}]*outline:/);
  assert.doesNotMatch(styles, /\.node-insecure/, '「允许 insecure」开关已移进说明行');
});

test('订阅弹窗：拨测状态灯沿用 .cg-lamp，失败原因写在地址下方，进度铺在按钮底色上', () => {
  // 状态灯的颜色只来自共享的 .cg-lamp；这里只留同宽的位置，让同列地址对齐。
  assert.match(desktop, /\.node-copy-lamp\s*\{[^}]*width:\s*7px;[^}]*flex:\s*none;/);
  assert.doesNotMatch(desktop, /\.node-copy-lamp[^{]*\{[^}]*background/);
  assert.match(desktop, /\.node-copy\.is-failed\s*\{[^}]*background:[^;]*var\(--err\)/);
  assert.match(desktop, /\.node-copy-note\s*\{[^}]*color:\s*var\(--err\);[^}]*white-space:\s*normal;/);
  assert.match(desktop, /\.node-copy\.has-note\s*\{[^}]*height:\s*auto;/);
  assert.match(desktop, /\.btn\.node-probe-progress\s*\{[^}]*background-size:\s*var\(--progress, 0%\) 100%;/);
  assert.match(desktop, /\.sub-dialog-foot > span\.node-probe-tally::before\s*\{[^}]*content:\s*none;/);
});

test('订阅弹窗：窄屏与 useNarrow 同一断点，贴底面板使用紧凑控件', () => {
  assert.ok(narrowMax, 'ui/viewport.ts 定义 NARROW_MAX');
  assert.ok(narrow, `窄屏规则写在 @media (max-width: ${narrowMax}px)`);
  assert.match(
    styles,
    /\.dialog-layer\s*\{[^}]*display:\s*flex;[^}]*align-items:\s*center;[^}]*justify-content:\s*center;/,
  );
  assert.match(
    narrow,
    /\.dialog-layer\.modal:has\(> \.sub-dialog\)\s*\{[^}]*align-items:\s*stretch;[^}]*justify-content:\s*flex-end;/,
  );
  assert.match(styles, /--mobile-control-h:\s*36px;/);
  assert.match(styles, /\.dialog-layer\s*\{[^}]*--ctl-h:\s*var\(--mobile-control-h\);/);
  assert.match(narrow, /\.dialog-surface\.sub-dialog\s*\{[^}]*border-radius:\s*16px 16px 0 0;/);
  assert.match(
    narrow,
    /\[data-motion-state='entering'\] > \.dialog-surface\.sub-dialog\s*\{\s*animation-name:\s*sub-sheet-in;/,
  );
  assert.match(narrow, /\.sub-dialog-foot\s*\{[^}]*var\(--safe-bottom\)/);
  // 订阅页签没有底栏：卡券正文自己让出底部安全区，操作按钮用紧凑控件高度。
  assert.match(narrow, /\.sub-dialog-subscription \.sub-subscription-body\s*\{[^}]*var\(--safe-bottom\)/);
  assert.match(narrow, /\.sub-pass-actions > \.btn\s*\{[^}]*min-height:\s*var\(--ctl-h\);/);
  assert.match(narrow, /\.node-copy\s*\{[^}]*height:\s*var\(--ctl-h\);/);
  assert.match(narrow, /\.node-copy-all,\s*\.node-probe-all,\s*\.node-probe-retry\s*\{[^}]*height:\s*var\(--ctl-h\);/);
  assert.match(narrow, /\.btn\.node-group-run\s*\{[^}]*min-height:\s*var\(--ctl-h\);/);
  // iOS 聚焦字号小于 16px 的表单控件会放大页面。
  assert.match(dialog, /\.sub-pick > select\s*\{[^}]*font-size:\s*16px;/);
});
