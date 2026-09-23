import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const source = path => readFileSync(new URL(path, import.meta.url), 'utf8');
const route = source('../src/forge/route.ts');
const shell = source('../src/forge/shell.tsx');
const styles = source('../src/styles.css');
const nodes = source('../src/panes/nodes.tsx');
const chains = source('../src/panes/chains.tsx');
const tunnels = source('../src/panes/tunnels.tsx');
const users = source('../src/panes/users.tsx');
const deploy = source('../src/panes/deploy.tsx');
const previewProvision = source('../src/preview/provision.tsx');

test('每条历史记录恢复滚动，但不重新聚焦上次打开的列表卡片', () => {
  assert.match(route, /interface RouteHistoryState[\s\S]*scrollTop: number;/);
  assert.doesNotMatch(route, /focusKey/);
  assert.match(route, /history\.scrollRestoration = 'manual'/);
  assert.match(route, /scroller\.scrollTop = Math\.min\(state\.scrollTop, maxScroll\)/);
  assert.match(route, /surface\.focus\(\{ preventScroll: true \}\)/);
  assert.doesNotMatch(route, /findRouteFocus/);
  assert.match(styles, /\.fg-desk\s*\{[^}]*overflow-anchor:\s*none/s);
  assert.doesNotMatch(styles, /\[data-route-focus\]:focus-visible/);

  assert.match(nodes, /data-route-focus=\{`node:\$\{node\.node_id\}`\}/);
  assert.match(chains, /data-route-focus=\{`chain:\$\{key\}`\}/);
  assert.match(tunnels, /data-route-focus=\{`tunnel:\$\{tunnel\.tenant\}:\$\{tunnel\.id\}`\}/);
  assert.match(users, /data-route-focus=\{`user:\$\{key\}`\}/);
  assert.match(deploy, /focusKey=\{`deployment:\$\{item\.id\}`\}/);
  assert.match(deploy, /data-route-focus=\{focusKey\}/);
});

test('窄屏不重复显示面包屑，进入新页面后焦点仍落在主内容', () => {
  assert.match(shell, /\{!narrow && \(\s*<div className="fg-crumb">/);
  assert.doesNotMatch(shell, /ForgeMobileBack|fg-mobile-crumb/);
  assert.match(shell, /role="main" aria-label=\{`\$\{LABEL\[nav\]\}内容`\} tabIndex=\{-1\}/);
});

test('普通页面顶栏随唯一的工作区滚动容器滚动', () => {
  assert.match(shell, /<div className=\{`fg-desk\$\{nav === 'topo' \? ' is-topo' : ''\}`\}>\s*<TopBar/);
  assert.equal((shell.match(/<TopBar/g) ?? []).length, 1);
  assert.match(styles, /\.fg-desk\s*\{[^}]*display:\s*flex;[^}]*flex-direction:\s*column;[^}]*overflow:\s*auto;/s);
  assert.match(styles, /\.fg-desk\s*\{[^}]*background:\s*var\(--surface\);/s);
  assert.doesNotMatch(styles, /\.fg-desk\s*\{[^}]*background-image:\s*var\(--grid-image\)/s);
  assert.match(styles, /\.fg-desk\.is-topo\s*\{[^}]*overflow:\s*hidden;/s);
  assert.match(
    styles,
    /\.fg-desk > \.fg-view\s*\{[^}]*flex:\s*1 0 auto;[^}]*background-color:\s*var\(--ground\);[^}]*background-image:\s*var\(--grid-image\)/s,
  );
  assert.doesNotMatch(styles, /\.fg-top\s*\{[^}]*position:\s*(?:fixed|sticky)/s);
  assert.doesNotMatch(styles, /\.fg-top::before/);
});

test('建链向导是可刷新、可返回的正式下钻位置', () => {
  assert.match(route, /\{ seg: 'new', fields: \[\{ name: 'app' \}\] \}/);
});

test('机器详情页签实现完整的键盘和 ARIA 关系', () => {
  assert.match(nodes, /role="tablist" aria-label=/);
  assert.equal((nodes.match(/role="tab"/g) ?? []).length, 3);
  assert.equal((nodes.match(/aria-controls=\{panelId\(/g) ?? []).length, 3);
  assert.equal((nodes.match(/role="tabpanel"/g) ?? []).length, 4);
  assert.equal((nodes.match(/aria-labelledby=\{tabId\(/g) ?? []).length, 3);
  assert.match(nodes, /function NodeDetailTabState[\s\S]*?role="tabpanel"[\s\S]*?aria-labelledby=\{tabId\}/);
  assert.match(nodes, /event\.key === 'ArrowRight' \|\| event\.key === 'ArrowDown'/);
  assert.match(nodes, /event\.key === 'ArrowLeft' \|\| event\.key === 'ArrowUp'/);
  assert.match(nodes, /event\.key === 'Home'/);
  assert.match(nodes, /event\.key === 'End'/);
});

test('导航统一中止外观快照并检查未保存修改', () => {
  assert.match(route, /if \(!confirmDiscardChanges\(\)\) return false;\s*cancelVisualTransition\(\)/);
  assert.match(route, /const restore =[\s\S]*?!approvedTraversal && !confirmDiscardChanges\(\)/);
  assert.match(nodes, /confirmDiscardChanges\(`node-tab:\$\{id\}:\$\{activeTab\}`\)/);
});

test('纳管完成后去发布只执行一次跨页面导航', () => {
  assert.doesNotMatch(nodes, /navigate\('deploy',[\s\S]{0,160}go\(\{ p: 'list' \}\)/);
  assert.doesNotMatch(previewProvision, /navigate\('deploy',[\s\S]{0,160}go\(\{ p: 'list' \}\)/);
});

test('用户主从页桌面原位切换，窄屏进入独立详情', () => {
  assert.match(users, /narrow \? navigate\('users', d\) : navigateInPlace\('users', d\)/);
  assert.match(users, /user-detail-route/);
  assert.match(styles, /@media \(max-width: 820px\)[\s\S]*\.user-split\.user-detail-route > \.user-split-roster/);
});
