import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const source = path => readFileSync(new URL(path, import.meta.url), 'utf8');

const settings = source('../src/panes/settings.tsx');
const styles = source('../src/styles.css');

test('设置段只使用机器、链路和用户详情共用的 config-panel 骨架', () => {
  for (const reference of ['nodes.tsx', 'chains.tsx', 'users.tsx']) {
    assert.match(source(`../src/panes/${reference}`), /className="panel config-panel(?:\s|\")/);
  }

  const panels = [...settings.matchAll(/<section className="([^"]+)" id=(?:\{id\}|"set-[^"]+")>/g)].map(match =>
    match[1].split(/\s+/),
  );

  assert.equal(panels.length, 7, '设置页的公共段骨架或六个专用段发生了变化');
  for (const classes of panels) {
    assert.ok(classes.includes('panel'));
    assert.ok(classes.includes('config-panel'));
    assert.ok(!classes.includes('titled'), '不得恢复设置页原有的 titled 面板皮肤');
  }
  assert.doesNotMatch(settings, /settings-panel/, '不得为设置页再造专用面板类');
});

test('设置段标题固定使用与关键页面相同的 PanelTitle 图标标题', () => {
  assert.match(settings, /function SettingsTitle[\s\S]*?<PanelTitle of=\{ICON_OF\[id\]\}>/);
  assert.doesNotMatch(settings, /<span className="no">\{NO_OF/);
});

test('CSS 禁止按设置段重新定义面板材质或标题视觉', () => {
  assert.doesNotMatch(styles, /\.settings-panel\b/, '不得为设置页增加专用面板选择器');

  const skinProperty =
    /(?:^|[;\n])\s*(?:--hue|background(?:-[\w-]+)?|border(?:-[\w-]+)?|box-shadow|padding(?:-[\w-]+)?|margin(?:-[\w-]+)?|font(?:-[\w-]+)?|letter-spacing|text-transform|color|min-height|max-height|height|border-radius|overflow|position)\s*:/;
  const offenders = [...styles.matchAll(/([^{}]+)\{([^{}]*)\}/g)]
    .filter(
      ([, selector, declarations]) => /#set-[\w-]+|\[id\^=['"]set-/.test(selector) && skinProperty.test(declarations),
    )
    .map(([, selector]) => selector.replace(/\/\*[\s\S]*?\*\//g, '').trim());

  assert.deepEqual(offenders, [], '设置段不得按 id 覆盖背景、边框、间距、阴影或标题字体；应修改公共 config-panel');
});
