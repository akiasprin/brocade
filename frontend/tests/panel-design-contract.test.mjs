import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import test from 'node:test';

const source = path => readFileSync(new URL(path, import.meta.url), 'utf8');
const paneNames = readdirSync(new URL('../src/panes/', import.meta.url))
  .filter(name => name.endsWith('.tsx'))
  .sort();
const panes = new Map(paneNames.map(name => [name, source(`../src/panes/${name}`)]));
const styles = source('../src/styles.css');

const tagEndAfter = (contents, start) => {
  let braces = 0;
  let quote = null;
  for (let index = start; index < contents.length; index += 1) {
    const char = contents[index];
    if (quote !== null) {
      if (char === quote && contents[index - 1] !== '\\') quote = null;
      continue;
    }
    if (char === '"' || char === "'" || char === '`') quote = char;
    else if (char === '{') braces += 1;
    else if (char === '}') braces -= 1;
    else if (char === '>' && braces === 0) return index + 1;
  }
  return -1;
};

const panelDeclarations = () => {
  const declarations = [];
  for (const [file, contents] of panes) {
    for (const match of contents.matchAll(/className="([^"]+)"/g)) {
      const classes = match[1].split(/\s+/);
      if (!classes.includes('panel')) continue;
      const start = contents.lastIndexOf('<', match.index);
      const end = tagEndAfter(contents, start);
      assert.ok(start >= 0 && end > start, `${file}: 无法读取 panel 标签`);
      const tag = contents.slice(start, end);
      assert.match(tag, /^<(?:section|div|details|fieldset)\b/, `${file}: panel 必须使用标准容器元素`);
      declarations.push({ file, classes, tag, tail: contents.slice(end) });
    }
  }
  return declarations;
};

test('所有页面只能复用机器、线路和用户页的三种面板骨架', () => {
  const declarations = panelDeclarations();
  assert.ok(declarations.length > 30, '面板扫描异常，可能漏掉了 panes 下的页面');

  for (const { file, classes, tag } of declarations) {
    assert.equal(classes[0], 'panel', `${file}: panel 必须是面板的第一个骨架类`);
    assert.ok(
      Number(classes.includes('config-panel')) + Number(classes.includes('titled')) <= 1,
      `${file}: 一张面板不得同时叠加 config-panel 和 titled`,
    );
    assert.doesNotMatch(tag, /settings-panel/, `${file}: 不得为任何页面再造一套面板骨架`);
    assert.doesNotMatch(
      tag,
      /style=\{\{[^}]*\b(?:background|border|borderRadius|boxShadow|padding|font|color|overflow)\b/,
      `${file}: 面板外观不得用行内样式绕过公共骨架`,
    );
  }
});

test('所有面板标题固定使用 PanelTitle，列表页只使用 ListIcon 标题', () => {
  const delegatedHeaders = /^\s*(?:\{(?:renderHeader\(|head\})|<(?:MachineEgressDnsRules|NodeChainsSection)\b)/;

  for (const { file, classes, tail } of panelDeclarations()) {
    const content = tail.replace(/^(?:\s*\{\/\*[\s\S]*?\*\/\})*\s*/, '');
    const header = content.match(/^<(header|summary)>([\s\S]*?)<\/\1>/);
    if (!header) {
      if (classes.includes('config-panel') || classes.includes('titled')) {
        assert.match(content, delegatedHeaders, `${file}: 带色带的面板必须有公共标题或显式委托标题`);
      }
      continue;
    }

    assert.match(header[2], /<(?:PanelTitle|SettingsTitle|ListIcon)\b/, `${file}: 不得在面板里直接写另一种 h4 标题`);
    assert.doesNotMatch(header[2], /className="no"/, `${file}: 面板标题不得恢复页面私有编号`);
  }

  const settings = panes.get('settings.tsx');
  assert.match(settings, /function SettingsTitle[\s\S]*?<PanelTitle of=\{ICON_OF\[id\]\}>/);
});

test('配置控件与说明文字共用 6px 垂直间距', () => {
  assert.match(styles, /--field-note-gap:\s*6px;/);
  assert.match(styles, /\.setfld \.v\s*\{[^}]*row-gap:\s*var\(--field-note-gap\)/s);
  assert.match(styles, /\.fgrid \.v \.sub\s*\{[^}]*margin-top:\s*var\(--field-note-gap\)/s);
  assert.match(
    styles,
    /\.chain-face dd > :not\(\.note\) \+ \.note,[\s\S]*?margin-top:\s*var\(--field-note-gap\)/,
  );
});

const splitSelectors = selectorList => {
  const selectors = [];
  let depth = 0;
  let start = 0;
  for (let index = 0; index < selectorList.length; index += 1) {
    const char = selectorList[index];
    if (char === '(' || char === '[') depth += 1;
    else if (char === ')' || char === ']') depth -= 1;
    else if (char === ',' && depth === 0) {
      selectors.push(selectorList.slice(start, index));
      start = index + 1;
    }
  }
  selectors.push(selectorList.slice(start));
  return selectors.map(selector => selector.trim()).filter(Boolean);
};

const finalCompound = selector => {
  let depth = 0;
  for (let index = selector.length - 1; index >= 0; index -= 1) {
    const char = selector[index];
    if (char === ')' || char === ']') depth += 1;
    else if (char === '(' || char === '[') depth -= 1;
    else if (depth === 0 && (char === '>' || char === '+' || char === '~' || /\s/.test(char))) {
      return selector.slice(index + 1).trim();
    }
  }
  return selector.trim();
};

const isSkinProperty = property =>
  /^(?:--hue|background(?:-[\w-]+)?|border(?:-[\w-]+)?|box-shadow|padding(?:-[\w-]+)?|font(?:-[\w-]+)?|letter-spacing|text-transform|color|min-height|max-height|height|border-radius|overflow)$/.test(
    property,
  );

const staticClasses = contents => {
  const classes = new Set();
  for (const match of contents.matchAll(/className="([^"]+)"/g)) {
    for (const name of match[1].split(/\s+/)) {
      if (/^[A-Za-z_][\w-]*$/.test(name)) classes.add(name);
    }
  }
  return classes;
};

/* 机器页还会通过 rules/telemetry 渲染规则编辑器和观测卡，
 * 因此这两个子组件也是机器/线路/用户基准的一部分。 */
const referenceFiles = ['nodes.tsx', 'chains.tsx', 'users.tsx', 'rules.tsx', 'telemetry.tsx'];
const referenceClasses = new Set([
  'panel',
  'config-panel',
  'titled',
  'cardpage',
  'fg-sheet',
  'b-col',
  'duo',
  'col',
  'observed',
  'guard',
  'nd-tab-pair',
  'nd-sheet',
  'history-chart-card',
  'load-network',
  'load-metric',
  // 隧道列表明确复用三个基准列表页标题，与它们在同一条 CSS 规则中。
  'tunnel-list-panel',
]);
for (const file of referenceFiles) {
  for (const name of staticClasses(panes.get(file))) referenceClasses.add(name);
}

const panelHooksOutsideReferences = new Set();
for (const declaration of panelDeclarations()) {
  if (referenceFiles.includes(declaration.file)) continue;
  for (const name of declaration.classes.slice(1)) {
    if (name !== 'config-panel' && name !== 'titled') panelHooksOutsideReferences.add(`.${name}`);
  }
  const id = declaration.tag.match(/\bid="([^"]+)"/);
  if (id) panelHooksOutsideReferences.add(`#${id[1]}`);
}

const panelSkinOffenders = css => {
  const offenders = [];
  const clean = css.replace(/\/\*[\s\S]*?\*\//g, '');
  for (const rule of clean.matchAll(/([^{}]+)\{([^{}]*)\}/g)) {
    const properties = [...rule[2].matchAll(/(?:^|;)\s*([\w-]+)\s*:/g)].map(match => match[1]);
    if (!properties.some(isSkinProperty)) continue;

    for (let selector of splitSelectors(rule[1])) {
      selector = selector.replace(/^@[^\n]+/, '').trim();
      const target = finalCompound(selector);
      const targetsPanelShell = /\.panel\b/.test(selector) && /\.panel\b/.test(target);
      const targetsPanelTitle = /\.panel\b/.test(selector) && /^(?:header|summary|h4)(?:\b|:)/.test(target);
      const targetsOutsideHook = [...panelHooksOutsideReferences].some(hook => target.includes(hook));
      if (!targetsPanelShell && !targetsPanelTitle && !targetsOutsideHook) continue;

      const unknownClasses = [...selector.matchAll(/\.([A-Za-z_][\w-]*)/g)]
        .map(match => match[1])
        .filter(name => !referenceClasses.has(name));
      if (targetsOutsideHook || unknownClasses.length > 0) offenders.push(selector.replace(/\s+/g, ' '));
    }
  }
  return [...new Set(offenders)];
};

test('CSS 全局禁止任何页面或业务标记另造面板皮肤', () => {
  assert.deepEqual(panelSkinOffenders(styles), []);

  const poisoned = `${styles}\n.billing-page .panel { background: red; border-radius: 99px; }\n#set-cert { color: red; }`;
  assert.deepEqual(panelSkinOffenders(poisoned).slice(-2), ['.billing-page .panel', '#set-cert']);
});
