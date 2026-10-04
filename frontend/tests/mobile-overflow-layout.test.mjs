import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const styles = readFileSync(new URL('../src/styles.css', import.meta.url), 'utf8');
const users = readFileSync(new URL('../src/panes/users.tsx', import.meta.url), 'utf8');
const shell = readFileSync(new URL('../src/forge/shell.tsx', import.meta.url), 'utf8');

test('手机端菜单留在视口内并允许纵向滚动', () => {
  assert.match(
    styles,
    /@media \(max-width: 820px\)[\s\S]*?\.fg-menu\s*\{[\s\S]*?max-width:\s*calc\(100vw - 16px\);[\s\S]*?max-height:\s*calc\(100dvh - 60px - var\(--safe-top\) - var\(--safe-bottom\)\);[\s\S]*?overflow-y:\s*auto;/,
  );
  assert.match(
    styles,
    /@media \(max-width: 760px\)[\s\S]*?\.user-action-menu\s*\{[\s\S]*?position:\s*fixed;[\s\S]*?right:\s*12px;[\s\S]*?bottom:\s*max\(12px, env\(safe-area-inset-bottom, 0px\)\);[\s\S]*?width:\s*min\(256px, calc\(100vw - 24px\)\);[\s\S]*?max-height:\s*calc\(100dvh - 24px\);/,
  );
  assert.match(
    styles,
    /@media \(max-width: 520px\)[\s\S]*?\.observe-range-menu\s*\{[\s\S]*?position:\s*fixed;[\s\S]*?right:\s*12px;[\s\S]*?left:\s*12px;[\s\S]*?max-height:\s*calc\(100dvh - 24px\);/,
  );
});

test('隧道事实条按实际项目铺满并在窄容器换列', () => {
  const base = styles.match(/\.tunnel-strip\s*\{[^}]+\}/)?.[0] ?? '';
  assert.match(base, /grid-auto-flow:\s*column/);
  assert.match(base, /grid-auto-columns:\s*minmax\(0, 1fr\)/);
  assert.doesNotMatch(base, /repeat\(4,/);
  assert.match(
    styles,
    /@container \(max-width: 620px\)[\s\S]*?\.tunnel-strip\s*\{[\s\S]*?grid-auto-flow:\s*row;[\s\S]*?grid-template-columns:\s*repeat\(2, minmax\(0, 1fr\)\);/,
  );
  assert.match(
    styles,
    /@container \(max-width: 430px\)[\s\S]*?\.tunnel-strip\s*\{[\s\S]*?grid-template-columns:\s*minmax\(0, 1fr\);/,
  );
});

test('手机端不渲染桌面面包屑或额外返回条', () => {
  assert.match(shell, /\{!narrow && \(\s*<div className="fg-crumb">/);
  assert.doesNotMatch(shell, /ForgeMobileBack|fg-mobile-crumb/);
  assert.doesNotMatch(styles, /\.fg-mobile-crumb/);
});

test('手机端导航栏与内容面板保留呼吸距离', () => {
  assert.match(styles, /@media \(max-width: 820px\)[\s\S]*?\.fg-sheet\s*\{[\s\S]*?margin:\s*12px 8px;/);
  assert.match(
    styles,
    /\.nd-sheet,\s*\.cardpage,\s*\.loading-state\.loading-page\.sheeted:is\(\.loading-nodes, \.loading-detail\)\s*\{\s*margin:\s*12px 8px;/,
  );
});

test('手机端机器详情页签沿用页头底色，不额外切出一层背景', () => {
  const tabsRule = styles.match(/\.nd-sheet > \.nd-paper > \.nd-page-head > \.nd-tabs\s*\{[^}]+\}/)?.[0] ?? '';
  assert.match(tabsRule, /border-bottom:\s*1px solid var\(--line\)/);
  assert.doesNotMatch(tabsRule, /background(?:-color)?:/);
});

test('手机端表单控件可以收缩到所在字段宽度', () => {
  assert.match(styles, /input\.f\s*\{[\s\S]*?min-width:\s*0;[\s\S]*?max-width:\s*100%;/);
  assert.match(styles, /select\.f\s*\{[\s\S]*?min-width:\s*0;[\s\S]*?max-width:\s*100%;/);
  assert.match(
    styles,
    /\.toolbar\s*\{[\s\S]*?min-width:\s*0;[\s\S]*?max-width:\s*100%;[\s\S]*?\.toolbar > :is\(input\.f, select\.f, textarea\.f\)\s*\{[\s\S]*?max-width:\s*100%;/,
  );
  assert.match(
    styles,
    /@container \(max-width: 430px\)[\s\S]*?\.nd-tab-config \.fgrid \.k\s*\{[\s\S]*?width:\s*86px;[\s\S]*?\.wg-peer-picker\s*\{[\s\S]*?flex-wrap:\s*wrap;/,
  );
  assert.match(
    styles,
    /\.wz-fld :is\(input\.f, select\.f, textarea\.f\)\s*\{[\s\S]*?max-width:\s*100%;[\s\S]*?box-sizing:\s*border-box;/,
  );
  assert.match(
    styles,
    /@media \(max-width: 520px\)[\s\S]*?\.chain-face\.fill dd > \.toolbar:has\(> input\.f:nth-of-type\(2\)\) > input\.f\s*\{[\s\S]*?flex:\s*0 0 auto;/,
  );
  assert.match(
    styles,
    /\.user-new-row\s*\{[\s\S]*?width:\s*100%;[\s\S]*?min-width:\s*0;[\s\S]*?box-sizing:\s*border-box;[\s\S]*?overflow:\s*hidden;/,
  );
  assert.match(
    styles,
    /\.user-new-field > \.f\s*\{[\s\S]*?width:\s*100%;[\s\S]*?min-width:\s*0;[\s\S]*?max-width:\s*none;[\s\S]*?box-sizing:\s*border-box;/,
  );
});

test('用户 UUID 使用带状态反馈的紧凑复制按钮', () => {
  assert.match(
    users,
    /className="user-fcopy user-duuid-copy"[\s\S]*?label="复制 UUID"[\s\S]*?successLabel="UUID 已复制"[\s\S]*?iconOnly/,
  );
  assert.match(
    styles,
    /\.user-duuid-copy\s*\{[\s\S]*?width:\s*24px;[\s\S]*?height:\s*24px;[\s\S]*?border-radius:\s*7px;/,
  );
});

test('手机端图表和拓扑操作不会把关键控件推出屏幕', () => {
  assert.match(
    styles,
    /\.usage-page\s*\{[\s\S]*?grid-template-columns:\s*minmax\(0, 1fr\);[\s\S]*?min-width:\s*0;[\s\S]*?\.usage-page > \*\s*\{[\s\S]*?min-width:\s*0;/,
  );
  assert.match(
    styles,
    /@container \(max-width: 560px\)[\s\S]*?\.nd-throughput-block > \.load-network-cap\s*\{[\s\S]*?flex-wrap:\s*wrap;[\s\S]*?\.nd-throughput-block > \.load-network-cap > \.load-network-legend\s*\{[\s\S]*?flex:\s*1 1 100%;[\s\S]*?flex-wrap:\s*wrap;/,
  );
  assert.match(
    styles,
    /@media \(max-width: 820px\)[\s\S]*?\.fg-topo #viewbar2\s*\{[\s\S]*?grid-template-columns:\s*minmax\(0, 1fr\) auto auto;[\s\S]*?overflow:\s*visible;[\s\S]*?\.fg-topo \.slideseg\s*\{[\s\S]*?overflow-x:\s*auto;/,
  );
  assert.match(
    styles,
    /\.fg-sheet \.tbl:not\(\.cards\)\s*\{[\s\S]*?width:\s*max-content;[\s\S]*?min-width:\s*100%;[\s\S]*?max-width:\s*100%;[\s\S]*?box-sizing:\s*border-box;/,
  );
});

test('页面内容容器不会通过动画变换成为固定弹窗的包含块', () => {
  const viewRule = [...styles.matchAll(/\.fg-view\s*\{[^}]+\}/g)].map(([rule]) => rule).join('\n');
  assert.match(viewRule, /display:\s*flow-root/);
  assert.doesNotMatch(viewRule, /animation|transform/);
  assert.doesNotMatch(styles, /motion-page-in|\.fg-topo\s*\{[^}]*animation/s);
});
