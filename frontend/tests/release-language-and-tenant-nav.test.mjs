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

test('账户牌打开更多菜单，菜单项使用一致图标并标出展开与当前页状态', () => {
  assert.match(shell, /className="fg-menu nav-menu"/);
  assert.match(shell, /className="fg-menu-icon"/);
  // 宽窄屏共用一枚账户牌作为菜单入口，窄屏只留首字母牌。
  assert.match(shell, /className=\{`fg-who\$\{narrow \? ' compact' : ''\}/);
  assert.equal((shell.match(/\{accountButton\}/g) ?? []).length, 2);
  assert.match(shell, /aria-haspopup="menu"/);
  assert.match(shell, /aria-controls="forge-more-menu"/);
  assert.match(shell, /aria-expanded=\{more\}/);
  assert.doesNotMatch(shell, /className="fg-more-icon"|className="(?:fg-ico|btn fg-more) fg-more-trigger"/);
  assert.match(shell, /\{ key: 'topo', label: '拓扑', icon: 'topology' \}/);
  assert.match(shell, /aria-current=\{nav === f\.key \? 'page' : undefined\}/);
  assert.match(nodes, /className="fg-menu action-menu"/);
  const userMore = users.match(/<button\s+className="btn user-dact user-dact-more"[\s\S]*?<\/button>/)?.[0] ?? '';
  assert.match(userMore, /aria-label="更多操作"/);
  assert.match(userMore, /of="more"/);
  assert.doesNotMatch(userMore, />\s*更多\s*</);
});

test('移除链路与 MTU 独立页面及桌面和手机共用的菜单入口', () => {
  assert.doesNotMatch(shell, /LinksPane|panes\/links|key: 'links'|nav === 'links'/);
  assert.doesNotMatch(state, /'links'/);
  assert.doesNotMatch(shell, /链路与 MTU/);
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
  assert.match(shell, /<span className="fg-appearance-label">主题<\/span>/);
  assert.match(shell, /<small>\{selectedPaletteName\}<\/small>/);
  assert.match(shell, /aria-checked=\{paletteKey === option\.key\}/);
  const deferredAppearance =
    shell.match(/const closeMenuThenTransition = \(transition: \(\) => void\) => \{([\s\S]*?)\n\s*\};/)?.[1] ?? '';
  assert.match(deferredAppearance, /pendingAppearanceTransition\.current = transition;/);
  assert.match(deferredAppearance, /setMore\(false\);/);
  assert.match(
    shell,
    /if \(more \|\| menuPresence\.present\) return;[\s\S]*?pendingAppearanceTransition\.current = null;[\s\S]*?queueMicrotask\(transition\);/,
  );
  const themeOptions = [...shell.matchAll(/className="fg-theme-option"[\s\S]*?onClick=\{\(\) => \{([\s\S]*?)\}\}/g)];
  assert.equal(themeOptions.length, 2);
  for (const [, handler] of themeOptions) {
    assert.match(handler, /if \(themeKey === '(?:light|dark)'\) return setMore\(false\);/);
    assert.match(handler, /closeMenuThenTransition\(toggleTheme\);/);
  }
  const paletteHandler =
    shell.match(/className="fg-accdot"[\s\S]*?onClick=\{event => \{([\s\S]*?)\n\s*\}\}/)?.[1] ?? '';
  assert.match(paletteHandler, /if \(paletteKey === option\.key\) return setMore\(false\);/);
  assert.match(paletteHandler, /const origin = motionOriginFor\(/);
  assert.match(paletteHandler, /closeMenuThenTransition\(\(\) =>/);
  assert.match(styles, /button\.fg-theme-option\[aria-checked='true'\]/);
  // 选中的色点用一圈正文色标出，不靠颜色本身。
  assert.match(
    styles,
    /button\.fg-accdot\[aria-checked='true'\]\s*\{\s*box-shadow:\s*inset 0 0 0 1\.5px var\(--ink-2\);/,
  );
  // 外观两行与菜单项同一层级，不再套一层带底色的框。
  const appearance = styles.match(/\.fg-appearance\s*\{([^}]*)\}/)?.[1] ?? '';
  assert.doesNotMatch(appearance, /background|border/);
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
  assert.doesNotMatch(accountAction, /<small>/);
  assert.doesNotMatch(accountAction, /现在是公开访客/);
  // 账户名与角色放在菜单头，角色取产品名称；动作行只写动作。
  assert.match(shell, /role: ROLE_LABEL\[who\.role\]/);
  assert.match(shell, /<span className="fg-menu-id-name">\{account\.name\}<\/span>/);
  assert.match(shell, /<span className="fg-menu-id-role">\{account\.role\}<\/span>/);
  assert.doesNotMatch(accountAction, /ROLE_LABEL|fg-account-role/);
  assert.doesNotMatch(shell, /\{who\.role\}/);
});

test('产物与诊断只显示图标，与账户牌同为无框按钮', () => {
  const toggle = styles.match(/\.fg-tgl\s*\{([\s\S]*?)\n\}/)?.[1] ?? '';
  const who = styles.match(/\.fg-who\s*\{([\s\S]*?)\n\}/)?.[1] ?? '';
  const mobile = styles.match(/\s\.fg-ico\s*\{([\s\S]*?)\n\s+\}/)?.[1] ?? '';
  for (const [name, rule] of [
    ['fg-tgl', toggle],
    ['fg-who', who],
    ['fg-ico', mobile],
  ]) {
    assert.match(rule, /height:\s*28px/, `${name} height`);
    assert.match(rule, /border:\s*0/, `${name} has no border`);
    assert.match(rule, /background:\s*transparent/, `${name} has no surface`);
  }
  assert.match(toggle, /width:\s*30px/);
  // 名称只在视觉上隐藏，读屏仍读到「产物」「诊断」。
  assert.match(shell, /<span className="fg-tgl-label">产物<\/span>/);
  assert.match(shell, /<span className="fg-tgl-label">诊断<\/span>/);
  assert.match(styles, /\.fg-tgl-label\s*\{[^}]*clip-path:\s*inset\(50%\)/);
  // 诊断计数压在图标右上角。
  assert.match(styles, /\.fg-tgl \.fg-badge\s*\{[^}]*position:\s*absolute;[^}]*top:\s*-3px;[^}]*right:\s*-4px;/);
  assert.match(styles, /\.fg-tgl:disabled\s*\{[^}]*color:\s*var\(--ink-4\)/);
  assert.match(
    styles,
    /\.fg-tgl\[aria-pressed='true'\],\s*\.fg-tgl\[aria-expanded='true'\]\s*\{[^}]*var\(--action-wash\)/,
  );
  assert.match(styles, /\.fg-tgl-ic svg\s*\{[\s\S]*?stroke-width:\s*1\.25px/);
  assert.doesNotMatch(styles, /\.fg-top \.fg-more|\.fg-more-icon/);
  assert.match(shell, /title=\{artifacts \? '显示 \/ 隐藏产物栏' : '当前身份无权查看产物'\}/);
  assert.equal(
    (shell.match(/<Icon of="artifactFolder" size=\{1[45]\} className="fg-(?:tgl-ic|menu-icon)" \/>/g) ?? []).length,
    2,
  );
});

test('图标角标的文字对底色不低于 4.5', () => {
  const block = selector => {
    const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
    return styles.match(new RegExp(`^${escaped} \\{([^}]*)\\}`, 'm'))?.[1] ?? '';
  };
  const token = (body, name) => body.match(new RegExp(`${name}:\\s*(#[0-9a-f]{6});`))?.[1];
  const channels = hex => [1, 3, 5].map(i => parseInt(hex.slice(i, i + 2), 16) / 255);
  const luminance = hex => {
    const [r, g, b] = channels(hex).map(c => (c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4));
    return 0.2126 * r + 0.7152 * g + 0.0722 * b;
  };
  const contrast = (a, b) => {
    const [high, low] = [luminance(a), luminance(b)].sort((x, y) => y - x);
    return (high + 0.05) / (low + 0.05);
  };
  const dark = block(":root,\n[data-theme='dark']");
  const light = block(":root[data-theme='light'],\n[data-theme='light']");
  assert.match(styles, /\.fg-tgl \.fg-badge\.err\s*\{[^}]*background:\s*var\(--err\);[^}]*color:\s*#15171b;/);
  assert.match(styles, /:root\[data-theme='light'\] \.fg-tgl \.fg-badge\.err\s*\{[^}]*color:\s*#ffffff;/);
  assert.ok(contrast(token(dark, '--err'), '#15171b') >= 4.5, 'dark error badge');
  assert.ok(contrast(token(light, '--err'), '#ffffff') >= 4.5, 'light error badge');
  // 警告角标：--ink-3 底、--surface 字，各调色盘都要满足。
  const palettes = ['jinzi', 'dailan', 'songlv', 'oufen', 'xuanmo'];
  for (const [label, base, prefix] of [
    ['dark', dark, ":root[data-palette='"],
    ['light', light, ":root[data-theme='light'][data-palette='"],
  ]) {
    for (const key of palettes) {
      const own = block(`${prefix}${key}']`);
      const ink3 = token(own, '--ink-3') ?? token(base, '--ink-3');
      const surface = token(own, '--surface') ?? token(base, '--surface');
      assert.ok(contrast(ink3, surface) >= 4.5, `${label} ${key} warning badge ${ink3} on ${surface}`);
    }
  }
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
