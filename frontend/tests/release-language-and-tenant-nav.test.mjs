import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const deploy = readFileSync(new URL('../src/panes/deploy.tsx', import.meta.url), 'utf8');
const settings = readFileSync(new URL('../src/panes/settings.tsx', import.meta.url), 'utf8');
const shell = readFileSync(new URL('../src/forge/shell.tsx', import.meta.url), 'utf8');
const state = readFileSync(new URL('../src/forge/state.ts', import.meta.url), 'utf8');
const legacyTopbar = readFileSync(new URL('../src/ui/topbar.tsx', import.meta.url), 'utf8');
const panes = readFileSync(new URL('../src/panes/index.tsx', import.meta.url), 'utf8');
const login = readFileSync(new URL('../src/ui/login.tsx', import.meta.url), 'utf8');
const users = readFileSync(new URL('../src/panes/users.tsx', import.meta.url), 'utf8');
const nodes = readFileSync(new URL('../src/panes/nodes.tsx', import.meta.url), 'utf8');
const provisionPreview = readFileSync(new URL('../src/preview/provision.tsx', import.meta.url), 'utf8');
const usage = readFileSync(new URL('../src/panes/usage.tsx', import.meta.url), 'utf8');
const tunnels = readFileSync(new URL('../src/panes/tunnels.tsx', import.meta.url), 'utf8');

test('发布界面统一使用灰度发布用语', () => {
  assert.doesNotMatch(deploy, /金丝雀/);
  assert.doesNotMatch(settings, /金丝雀/);
  assert.match(deploy, /灰度发布/);
});

test('发布预览不向用户暴露内部字段或实现措辞', () => {
  assert.doesNotMatch(deploy, /revision_id = \{target\}/);
  assert.doesNotMatch(deploy, /期间模型又动过/);
  assert.doesNotMatch(deploy, /相同修订无法展示/);
  assert.match(deploy, /创建前会再次确认配置/);
});

test('单租户阶段不在导航菜单暴露租户页', () => {
  const more = shell.match(/const MORE: Face\[\] = \[([\s\S]*?)\n\];/)?.[1] ?? '';
  const offNav = shell.match(/const OFF_NAV: Face\[\] = \[([\s\S]*?)\n\];/)?.[1] ?? '';
  assert.doesNotMatch(more, /key: 'tenants'/);
  assert.doesNotMatch(offNav, /key: 'tenants'/);
  const routable = state.match(/export const NAV_KEYS = \[([\s\S]*?)\n\]/)?.[1] ?? '';
  assert.doesNotMatch(routable, /'tenants'/);
  const tabs = legacyTopbar.match(/export const TABS: Tab\[\] = \[([\s\S]*?)\n\];/)?.[1] ?? '';
  assert.doesNotMatch(tabs, /key: 'tenants'/);
  assert.doesNotMatch(panes, /TenantsPane|case 'tab:tenants'/);
});

test('更多菜单项有图标，用户页更多入口只显示三点图标', () => {
  assert.match(shell, /className="fg-menu nav-menu"/);
  assert.match(shell, /className="fg-menu-icon"/);
  assert.match(nodes, /className="fg-menu action-menu"/);
  const userMore = users.match(/<button\s+className="btn user-dact user-dact-more"[\s\S]*?<\/button>/)?.[0] ?? '';
  assert.match(userMore, /aria-label="更多操作"/);
  assert.match(userMore, /of="more"/);
  assert.doesNotMatch(userMore, />\s*更多\s*</);
});

test('注册、资源创建、用量与身份区域不显示租户字段', () => {
  assert.doesNotMatch(login, /<span>根租户<\/span>/);
  assert.doesNotMatch(users, /归属租户/);
  assert.doesNotMatch(nodes, /<label>归属租户<\/label>/);
  assert.doesNotMatch(provisionPreview, /<label>归属租户<\/label>/);
  assert.doesNotMatch(usage, /租户：全部/);
  assert.doesNotMatch(shell, /who\.tenant_scope/);
  assert.doesNotMatch(legacyTopbar, /who\.tenant_scope/);
  assert.doesNotMatch(tunnels, /默认参数属于租户 WARP 资源/);
});
