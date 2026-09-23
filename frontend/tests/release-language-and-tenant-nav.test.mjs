import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const deploy = readFileSync(new URL('../src/panes/deploy.tsx', import.meta.url), 'utf8');
const settings = readFileSync(new URL('../src/panes/settings.tsx', import.meta.url), 'utf8');
const shell = readFileSync(new URL('../src/forge/shell.tsx', import.meta.url), 'utf8');
const styles = readFileSync(new URL('../src/styles.css', import.meta.url), 'utf8');
const state = readFileSync(new URL('../src/forge/state.ts', import.meta.url), 'utf8');
const panes = readFileSync(new URL('../src/panes/index.tsx', import.meta.url), 'utf8');
const login = readFileSync(new URL('../src/ui/login.tsx', import.meta.url), 'utf8');
const users = readFileSync(new URL('../src/panes/users.tsx', import.meta.url), 'utf8');
const nodes = readFileSync(new URL('../src/panes/nodes.tsx', import.meta.url), 'utf8');
const provisionPreview = readFileSync(new URL('../src/preview/provision.tsx', import.meta.url), 'utf8');
const usage = readFileSync(new URL('../src/panes/usage.tsx', import.meta.url), 'utf8');
const tunnels = readFileSync(new URL('../src/panes/tunnels.tsx', import.meta.url), 'utf8');

test('配置发布使用操作者可理解的阶段名称', () => {
  assert.doesNotMatch(deploy, /金丝雀/);
  assert.doesNotMatch(settings, /金丝雀/);
  assert.match(deploy, /更新机器配置/);
  assert.match(deploy, /发布验证/);
  assert.match(deploy, /全量发布/);
  assert.match(deploy, /步骤 \{step\.step\}\/\{step\.steps\}/);
  assert.doesNotMatch(deploy, /`确认第 \$\{[^}]+\} 波`/);
});

test('发布预览不向用户暴露内部字段或实现措辞', () => {
  assert.doesNotMatch(deploy, /revision_id = \{target\}/);
  assert.doesNotMatch(deploy, /期间模型又动过/);
  assert.doesNotMatch(deploy, /相同修订无法展示/);
  assert.match(deploy, /创建时，系统会再次检查当前配置/);
  assert.match(deploy, /如果配置已更新，本次创建将取消，请重新预览/);
});

test('单租户阶段不在导航菜单暴露租户页', () => {
  const more = shell.match(/const MORE: Face\[\] = \[([\s\S]*?)\n\];/)?.[1] ?? '';
  const offNav = shell.match(/const OFF_NAV: Face\[\] = \[([\s\S]*?)\n\];/)?.[1] ?? '';
  assert.doesNotMatch(more, /key: 'tenants'/);
  assert.doesNotMatch(offNav, /key: 'tenants'/);
  const routable = state.match(/export const NAV_KEYS = \[([\s\S]*?)\n\]/)?.[1] ?? '';
  assert.doesNotMatch(routable, /'tenants'/);
  assert.doesNotMatch(panes, /TenantsPane|case 'tab:tenants'/);
});

test('隧道页位于线路前并接入主导航、路由和页面容器', () => {
  const nav = shell.match(/const NAV: Face\[\] = \[([\s\S]*?)\n\];/)?.[1] ?? '';
  const mobileMore = shell.match(/const MOBILE_MORE = ([^;]+);/)?.[1] ?? '';
  const routable = state.match(/export const NAV_KEYS = \[([\s\S]*?)\n\]/)?.[1] ?? '';
  assert.match(nav, /key: 'tunnels'/);
  assert.ok(nav.indexOf("key: 'tunnels'") < nav.indexOf("key: 'chains'"));
  assert.match(mobileMore, /f\.key === 'tunnels'/);
  assert.match(routable, /'tunnels'/);
  assert.match(panes, /TunnelsPane|case 'tab:tunnels'/);
});

test('更多菜单入口和菜单项使用一致图标，并明确展开与当前页状态', () => {
  assert.match(shell, /className="fg-menu nav-menu"/);
  assert.match(shell, /className="fg-menu-icon"/);
  assert.match(shell, /className="(?:fg-ico|btn fg-more) fg-more-trigger"/);
  assert.match(shell, /aria-haspopup="menu"/);
  assert.match(shell, /aria-controls="forge-more-menu"/);
  assert.match(shell, /aria-expanded=\{more\}/);
  assert.equal((shell.match(/<Icon of="menu" size=\{16\} className="fg-more-icon" \/>/g) ?? []).length, 2);
  assert.match(shell, /\{ key: 'topo', label: '拓扑', icon: 'topology' \}/);
  assert.match(shell, /\{ key: 'links', label: '链路与 MTU', icon: 'linkMeasure' \}/);
  assert.match(shell, /aria-current=\{nav === f\.key \? 'page' : undefined\}/);
  assert.match(nodes, /className="fg-menu action-menu"/);
  const userMore = users.match(/<button\s+className="btn user-dact user-dact-more"[\s\S]*?<\/button>/)?.[0] ?? '';
  assert.match(userMore, /aria-label="更多操作"/);
  assert.match(userMore, /of="more"/);
  assert.doesNotMatch(userMore, />\s*更多\s*</);
});

test('外观控制分开呈现明暗模式与当前色调', () => {
  assert.match(shell, /className="fg-appearance" role="group" aria-label="外观"/);
  assert.match(shell, /className="fg-theme-switch" role="group" aria-label="明暗模式"/);
  assert.match(shell, /className="fg-tone-list" role="group" aria-label="界面色调"/);
  assert.equal((shell.match(/className="fg-theme-option"/g) ?? []).length, 2);
  assert.match(shell, /aria-checked=\{themeKey === 'light'\}/);
  assert.match(shell, /aria-checked=\{themeKey === 'dark'\}/);
  assert.match(shell, /<Icon of="sun" size=\{12\} className="fg-theme-option-icon" \/>/);
  assert.match(shell, /<Icon of="moon" size=\{12\} className="fg-theme-option-icon" \/>/);
  assert.match(shell, /<small>\{selectedPaletteName\}<\/small>/);
  assert.match(shell, /aria-checked=\{paletteKey === option\.key\}/);
  assert.match(shell, /paletteKey === option\.key && <Icon of="check"/);
  assert.match(styles, /button\.fg-theme-option\[aria-checked='true'\]/);
  assert.match(styles, /button\.fg-accdot\[aria-checked='true'\]/);
  assert.doesNotMatch(shell, /<span[^>]*>\s*配色\s*<\/span>/);
});

test('账户入口使用单行文案和产品角色名称', () => {
  const accountClass = shell.indexOf('className="fg-menu-item fg-account-action"');
  const accountAction = shell.slice(
    shell.lastIndexOf('<button', accountClass),
    shell.indexOf('</button>', accountClass),
  );
  assert.match(shell, /const ROLE_LABEL: Record<AdminRole, string>/);
  assert.match(shell, /'system-admin': '系统管理员'/);
  assert.match(accountAction, /\{isPublic\(who\) \? '登录' : '退出登录'\}/);
  assert.match(accountAction, /className="fg-account-role">\{ROLE_LABEL\[who\.role\]\}/);
  assert.doesNotMatch(accountAction, /<small>/);
  assert.doesNotMatch(accountAction, /现在是公开访客/);
  assert.match(styles, /\.fg-account-role\s*\{[\s\S]*?white-space:\s*nowrap/);
  assert.doesNotMatch(shell, /\{who\.role\}/);
});

test('产物、诊断和更多入口共用轻量按钮表面', () => {
  const toggle = styles.match(/\.fg-tgl\s*\{([\s\S]*?)\n\}/)?.[1] ?? '';
  const more = styles.match(/\.fg-top \.fg-more\s*\{([\s\S]*?)\n\}/)?.[1] ?? '';
  const mobile = styles.match(/\s\.fg-ico\s*\{([\s\S]*?)\n\s+\}/)?.[1] ?? '';
  assert.match(toggle, /height:\s*28px/);
  assert.match(toggle, /border:\s*1px solid var\(--line-soft\)/);
  assert.match(toggle, /background:\s*color-mix\(in srgb, var\(--block\) 72%, transparent\)/);
  assert.match(toggle, /box-shadow:/);
  assert.match(more, /height:\s*28px/);
  assert.match(more, /border-color:\s*var\(--line-soft\)/);
  assert.match(more, /background:\s*color-mix\(in srgb, var\(--block\) 72%, transparent\)/);
  assert.match(more, /box-shadow:/);
  assert.match(styles, /\.fg-tgl:disabled\s*\{[\s\S]*?background:\s*color-mix/);
  assert.match(styles, /\.fg-tgl-ic svg\s*\{[\s\S]*?stroke-width:\s*1\.25px/);
  assert.match(styles, /\.fg-more-icon svg\s*\{[\s\S]*?stroke-width:\s*1\.15px/);
  assert.match(mobile, /height:\s*28px/);
  assert.match(mobile, /border:\s*1px solid var\(--line-soft\)/);
  assert.match(shell, /title=\{artifacts \? '显示 \/ 隐藏产物栏' : '当前身份无权查看产物'\}/);
  assert.equal(
    (shell.match(/<Icon of="artifactFolder" size=\{1[34]\} className="fg-(?:tgl-ic|menu-icon)" \/>/g) ?? []).length,
    2,
  );
});

test('注册、资源创建、用量与身份区域不显示租户字段', () => {
  assert.doesNotMatch(login, /<span>根租户<\/span>/);
  assert.doesNotMatch(users, /归属租户/);
  assert.doesNotMatch(nodes, /<label>归属租户<\/label>/);
  assert.doesNotMatch(provisionPreview, /<label>归属租户<\/label>/);
  assert.doesNotMatch(usage, /租户：全部/);
  assert.doesNotMatch(shell, /who\.tenant_scope/);
  assert.doesNotMatch(tunnels, /默认参数属于租户 WARP 资源/);
});
