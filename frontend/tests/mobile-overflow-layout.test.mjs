import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const traffic = readFileSync(new URL('../src/user-traffic-realtime.tsx', import.meta.url), 'utf8');
const styles = readFileSync(new URL('../src/styles.css', import.meta.url), 'utf8');
const users = readFileSync(new URL('../src/panes/users.tsx', import.meta.url), 'utf8');
const shell = readFileSync(new URL('../src/forge/shell.tsx', import.meta.url), 'utf8');
const capacity = readFileSync(new URL('../src/panes/capacity.tsx', import.meta.url), 'utf8');

test('手机端菜单留在视口内并允许纵向滚动', () => {
  assert.match(
    styles,
    /@media \(max-width: 820px\)[\s\S]*?\.fg-menu\s*\{[\s\S]*?position:\s*fixed;[\s\S]*?top:\s*calc\(46px \+ var\(--safe-top\)\);[\s\S]*?right:\s*calc\(10px \+ var\(--safe-right\)\);[\s\S]*?max-width:\s*calc\(100vw - 20px - var\(--safe-left\) - var\(--safe-right\)\);[\s\S]*?overflow-y:\s*auto;/,
  );
  assert.match(
    styles,
    /@media \(max-width: 820px\)[\s\S]*?\.fg-menu\.nav-menu\s*\{[\s\S]*?width:\s*min\(232px,[\s\S]*?padding:\s*4px;/,
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

test('隧道连接表的长值单行省略，窄详情页头只留一层', () => {
  const value = styles.match(/\.tnd-kv-value\s*\{[^}]+\}/)?.[0] ?? '';
  assert.match(value, /min-width:\s*0/);
  assert.match(value, /text-overflow:\s*ellipsis/);
  assert.match(value, /white-space:\s*nowrap/);
  assert.match(
    styles,
    /@container \(max-width: 700px\)[\s\S]*?\.tnd-page\.nd-sheet > \.nd-paper > \.tnd-head\s*\{\s*grid-template-rows:\s*minmax\(58px, auto\);[\s\S]*?\.tnd-page\.nd-sheet > \.nd-paper > \.tnd-head > \.nd-acts\s*\{\s*grid-column:\s*2;\s*grid-row:\s*1;/,
  );
});

test('手机端不渲染桌面面包屑或额外返回条', () => {
  assert.match(shell, /\{!narrow && \(\s*<div className="fg-crumb">/);
  assert.doesNotMatch(shell, /ForgeMobileBack|fg-mobile-crumb/);
  assert.doesNotMatch(styles, /\.fg-mobile-crumb/);
});

test('手机端导航栏与内容面板保留呼吸距离', () => {
  assert.match(styles, /@media \(max-width: 420px\)[\s\S]*?\.forge\.narrow \.fg-brand\s*\{[\s\S]*?display:\s*none;/);
  assert.match(
    styles,
    /@media \(max-width: 820px\)[\s\S]*?\.fg-facebar \.fg-nv\s*\{[\s\S]*?gap:\s*4px;[\s\S]*?padding:\s*0 6px;/,
  );
  assert.match(
    styles,
    /@media \(max-width: 375px\)\s*\{\s*\.fg-facebar\s*\{\s*gap:\s*1px;\s*\}\s*\.fg-facebar \.fg-nv\s*\{\s*gap:\s*3px;\s*padding:\s*0 2px;/,
  );
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

test('机器实时卡使用小时柱状图，移除接入面网格及其工具行', () => {
  assert.match(traffic, /presentation=\{users \? 'bar' : 'line'\}/);
  assert.doesNotMatch(traffic, /NodeAccessMatrix|NODE_SORTS|rut-access/);
  assert.doesNotMatch(styles, /\.rut-row|\.rut-chip|\.rut-composition|\.realtime-user-traffic-dashboard/);
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

test('完整 UUID 仍可通过「更多」菜单复制，手机上随菜单宽度换行', () => {
  assert.match(
    users,
    /role="menuitem"\s+className="user-action-copy"[\s\S]*?label="复制 UUID"[\s\S]*?successLabel="UUID 已复制"/,
  );
  assert.doesNotMatch(users, /user-duuid/);
  assert.match(styles, /\.user-action-menu small\.mono\s*\{[^}]*overflow-wrap:\s*anywhere;/);
});

test('手机端图表和拓扑操作不会把关键控件推出屏幕', () => {
  assert.match(
    styles,
    /\.usage-page,\s*\.capacity-page\s*\{[\s\S]*?grid-template-columns:\s*minmax\(0, 1fr\);[\s\S]*?min-width:\s*0;[\s\S]*?\.usage-page > \*,\s*\.capacity-page > \*\s*\{[\s\S]*?min-width:\s*0;/,
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

test('手机端流量预测保留机器读数、风险和用户构成', () => {
  // 列表正文是自己的尺寸容器：行在窄宽度下改为三层，名称与结论、跑道、已用与预计都留在行内。
  assert.match(styles, /\.cap-body\s*\{\s*container-type:\s*inline-size;/);
  assert.match(
    styles,
    /@container \(max-width: 760px\)\s*\{[^@]*?\.cap-row\s*\{[^}]*?grid-template-areas:\s*'id verdict chev'\s*'run run run'\s*'used proj \.';/,
  );
  // 详情上下堆叠；用户占用保留四列读数，只去掉占比条。
  assert.match(
    styles,
    /@container \(max-width: 760px\)\s*\{[^@]*?\.cap-detail\s*\{\s*grid-template-columns:\s*minmax\(0, 1fr\);/,
  );
  assert.match(
    styles,
    /@container \(max-width: 760px\)\s*\{[^@]*?\.cap-user\s*\{\s*grid-template-columns:\s*minmax\(0, 1fr\) 46px 64px 64px;/,
  );
  // 标题栏只收起总台数，需要处理的计数保留。
  assert.match(
    styles,
    /@media \(max-width: 820px\)\s*\{\s*\.cap-rd \.opt,\s*\.cap-rd \.opt \+ \.dot\s*\{\s*display:\s*none;/,
  );
  assert.match(capacity, /<span className="opt">/);
});

test('页面内容容器不会通过动画变换成为固定弹窗的包含块', () => {
  const viewRule = [...styles.matchAll(/\.fg-view\s*\{[^}]+\}/g)].map(([rule]) => rule).join('\n');
  assert.match(viewRule, /display:\s*flow-root/);
  assert.doesNotMatch(viewRule, /animation|transform/);
  assert.doesNotMatch(styles, /motion-page-in|\.fg-topo\s*\{[^}]*animation/s);
});
