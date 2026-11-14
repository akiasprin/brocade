import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const css = readFileSync(new URL('../src/styles.css', import.meta.url), 'utf8');
const component = readFileSync(new URL('../src/user-site-usage.tsx', import.meta.url), 'utf8');
const styles = css.slice(css.indexOf('/* 用户详情「站点流量」'));

test('site usage lays the chart beside the ranking and stacks them on narrow cards', () => {
  assert.ok(styles.length > 0 && styles !== css);
  assert.match(styles, /\.site-usage\s*\{[^}]*container:\s*user-site-usage \/ inline-size;/);
  assert.match(styles, /\.site-usage-main\s*\{[^}]*grid-template-columns:\s*minmax\(0, 1fr\) 340px;/);
  assert.match(
    styles,
    /@container user-site-usage \(max-width: 860px\)[\s\S]*?\.site-usage-main\s*\{[^}]*grid-template-columns:\s*minmax\(0, 1fr\);/,
  );
  // 正文铺满卡片内距以外的区域，不在卡里再套一层底色
  assert.match(component, /className="site-usage-body panel-full-bleed-body"/);
  assert.match(css, /\.panel-full-bleed-body\s*\{[^}]*var\(--panel-pad-x\)/s);
  assert.doesNotMatch(styles, /\.site-usage-(item|row|detail)\s*\{[^}]*background:\s*var\(--block\)/);
});

test('the period filter sits at the end of the total row and spans narrow cards', () => {
  assert.match(styles, /\.site-usage-head > \.segsw\s*\{[^}]*margin-left:\s*auto;/);
  assert.match(
    styles,
    /@container user-site-usage \(max-width: 620px\)[\s\S]*?\.site-usage-head > \.segsw\s*\{[^}]*order:\s*-1;[^}]*width:\s*100%;/,
  );
});

test('detail rows keep the main domain whole and every control has a visible focus', () => {
  assert.match(styles, /\.site-usage-dsub\s*\{[^}]*text-overflow:\s*ellipsis;/);
  assert.match(
    styles,
    /\.site-usage-dmain\s*\{[^}]*min-width:\s*0;[^}]*overflow-wrap:\s*anywhere;[^}]*white-space:\s*normal;/,
  );
  assert.match(styles, /\.site-usage-target-row\s*\{[^}]*grid-template-columns:\s*minmax\(0, 1fr\) 70px;/);
  for (const control of ['button\\.site-usage-row', '\\.site-usage-detail-head', '\\.site-usage-more']) {
    assert.match(styles, new RegExp(`${control}:focus-visible\\s*\\{[^}]*outline:\\s*2px solid var\\(--action\\)`));
  }
  // 只检查本组件；后续追加的其他面板样式不属于站点流量的契约。
  assert.doesNotMatch(styles.replace(/@media \(hover: hover\)\s*\{[\s\S]*?\n\}/g, ''), /\.site-usage[^{}]*:hover\s*\{/);
});
