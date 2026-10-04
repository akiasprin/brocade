import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const styles = readFileSync(new URL('../src/styles.css', import.meta.url), 'utf8');

const tokens = selector => {
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const block = styles.match(new RegExp(`^${escaped} \\{([^}]*)\\}`, 'm'))?.[1];
  assert.ok(block, `${selector} block exists`);
  return new Map([...block.matchAll(/(--[\w-]+):\s*([^;]+);/g)].map(([, name, value]) => [name, value.trim()]));
};
const rule = selector => {
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const body = styles.match(new RegExp(`^${escaped} \\{([^}]*)\\}`, 'm'))?.[1];
  assert.ok(body, `${selector} rule exists`);
  return body;
};
const baseDark = tokens(":root,\n[data-theme='dark']");
const baseLight = tokens(":root[data-theme='light'],\n[data-theme='light']");

test('floating layers share one translucent material', () => {
  for (const t of [baseDark, baseLight]) {
    assert.equal(t.get('--float'), 'color-mix(in srgb, var(--surface) 64%, transparent)');
    assert.equal(t.get('--float-sheet'), 'color-mix(in srgb, var(--sheet) 64%, transparent)');
    assert.equal(t.get('--float-filter'), 'blur(20px) saturate(1.8)');
    assert.equal(t.get('--float-edge'), 'inset 0 1px 0 rgba(255, 255, 255, 0.1)');
    // 对话框本身半透明，遮罩只压暗 40%，否则背后的页面只剩一块暗色。
    assert.match(t.get('--scrim'), /^rgba\(\d+, \d+, \d+, 0\.4\)$/);
  }
  for (const selector of [
    '.fw',
    '.fg-menu',
    '.fg-pop',
    '.observe-range-menu',
    '.external-target-menu',
    '.cg-menu',
    '.topo-callout',
    '.sheet',
  ]) {
    const body = rule(selector);
    assert.match(body, /background: var\(--float\);/, `${selector} uses the floating material`);
    assert.match(body, /backdrop-filter: var\(--float-filter\);/, `${selector} blurs what is behind it`);
    assert.match(body, /box-shadow:\s*var\(--float-edge\),/, `${selector} keeps the top edge highlight`);
  }
  const dialog = rule('.dialog-surface');
  assert.match(dialog, /background: var\(--float-sheet\);/);
  assert.match(dialog, /backdrop-filter: var\(--float-filter\);/);
  assert.match(rule('.dialog-scrim'), /background: var\(--scrim\);[^}]*backdrop-filter: blur\(4px\);/s);
});

test('menus do not carry appearance-transition exceptions', () => {
  for (const selector of ['.fg-menu', '.observe-range-menu', '.external-target-menu', '.cg-menu']) {
    const body = rule(selector);
    assert.match(body, /background: var\(--float\);/, `${selector} uses the menu material`);
    assert.match(body, /-webkit-backdrop-filter: var\(--float-filter\);/, `${selector} supports WebKit blur`);
    assert.match(body, /backdrop-filter: var\(--float-filter\);/, `${selector} supports standard blur`);
    assert.match(body, /box-shadow:\s*var\(--float-edge\),/, `${selector} keeps the glass edge`);
  }
  assert.doesNotMatch(styles, /view-transition-(?:name|group|image-pair|old|new)\(account-menu\)/);
  assert.doesNotMatch(styles, /view-transition-name:\s*account-menu/);
});

test('papers and panels stay opaque without backdrop blur', () => {
  assert.match(rule('.fg-sheet'), /background: var\(--sheet\);/);
  assert.match(rule('.panel'), /background: var\(--card\);/);
  // 纸与面板在切页时播放入场动画，每帧都要重算背景模糊，实测掉到 34–42fps；
  // 容器带模糊还会成为 backdrop root，使容器内吸顶页头与菜单的模糊失效。
  const clean = styles.replace(/\/\*[\s\S]*?\*\//g, '');
  for (const [, selectors, body] of clean.matchAll(/([^{}]+)\{([^{}]*)\}/g)) {
    if (!/backdrop-filter/.test(body)) continue;
    for (const selector of selectors.split(',')) {
      const target = selector
        .trim()
        .split(/\s*[>+~\s]\s*/)
        .at(-1);
      assert.doesNotMatch(target, /\.(?:fg-sheet|nd-paper|panel|blk)\b/, `${selector.trim()} blurs a content surface`);
    }
  }
  // 顶栏随内容滚走且为实色，不参与半透明。
  assert.doesNotMatch(styles, /\.fg-top::before/);
  assert.match(rule('.fg-top'), /background: var\(--surface\);/);
});
