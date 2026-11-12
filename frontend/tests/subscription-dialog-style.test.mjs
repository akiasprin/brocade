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

// Node layout contracts are exercised against rendered Chromium/WebKit views in
// visual/node-links.spec.ts and visual/subscription-layout.spec.ts. Retired DOM/CSS
// selectors must not constrain the replacement layout.
test('订阅卡券：二维码白底不反色，撕线缺口用遮罩，地址行变长不撑宽列', () => {
  assert.match(desktop, /\.sub-qr\s*\{[^}]*background:\s*#fff;/);
  assert.doesNotMatch(dialog, /--sub-qr-(?:bg|fg)/, '二维码不再随主题换色');
  assert.match(desktop, /\.sub-pass-top\s*\{[^}]*\bmask:[^}]*at 0 100%[^}]*at 100% 100%/);
  assert.match(desktop, /\.sub-pass-code\s*\{[^}]*\bmask:[^}]*at 0 0,[^}]*at 100% 0,/);
  assert.match(
    desktop,
    /\.sub-pass-stack\s*\{[^}]*filter:\s*drop-shadow/,
    '投影沿缺口走，不用会被遮罩裁掉的 box-shadow',
  );
  // 筛选参数让地址行变长时，网格列若按内容撑宽，居中的二维码会跟着右移。
  assert.match(desktop, /\.sub-pass-top,\s*\.sub-pass-code\s*\{[^}]*grid-template-columns:\s*minmax\(0, 1fr\);/);
  assert.match(desktop, /\.sub-pass-address > code\s*\{[^}]*text-overflow:\s*ellipsis;[^}]*white-space:\s*nowrap;/);
  assert.match(
    desktop,
    /@media \(prefers-reduced-motion: reduce\)\s*\{[^@]*\.sub-pass-stack\.swapped > \.sub-pass\s*\{\s*animation:\s*none;/,
  );
});
