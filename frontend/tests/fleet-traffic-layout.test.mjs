import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const css = readFileSync(new URL('../src/styles.css', import.meta.url), 'utf8');
const start = css.indexOf('/* ═══ 用量 · 机队实时流量');
const end = css.indexOf('/* ── 首次上线引导层', start);
assert.ok(start > 0 && end > start, '机队实时流量样式标记存在且顺序有效');
const styles = css.slice(start, end);

test('实时页签铺满连续纸，矩阵只有点阵与读数格两种固定尺寸，不随纸面拉伸', () => {
  assert.ok(start > 0 && end > start && styles.length > 0);
  assert.match(css, /\.usage-cockpit,\s*\.flt-cockpit\s*\{[^}]*grid-template-columns:\s*300px minmax\(0, 1fr\);/);
  assert.doesNotMatch(styles, /--flt-ledger-unit/);
  assert.match(styles, /--flt-col:\s*30px;/);
  assert.match(styles, /--flt-cell:\s*calc\(var\(--flt-col\) - 6px\);/);
  assert.match(styles, /--flt-gap:\s*8px;/);
  assert.doesNotMatch(styles, /cqw|--flt-col:\s*clamp|--flt-gap:\s*6px/);
  // 读数格固定 104 × 40，整张表放得下才用（组件写入 .reading），宽屏不放大。
  assert.match(styles, /\.flt-board\.reading\s*\{\s*--flt-col:\s*104px;\s*--flt-row:\s*40px;/);
  // 表格与其下的读数行、图例共用同一宽度变量，图例不被推到纸面最右侧。
  assert.match(styles, /\.flt-grid\s*\{\s*width:\s*var\(--flt-table-w\);/);
  assert.match(styles, /\.flt-under\s*\{[^}]*max-width:\s*max\(var\(--flt-table-w\), 560px\);/);
  // 手机宽度同样是矩阵，只收窄四周留白；流向列表已移除。
  assert.match(styles, /@container \(max-width: 680px\)\s*\{\s*\.flt-board\s*\{\s*padding:/);
  assert.doesNotMatch(css, /\.flt-(?:list|flows?|flow-[a-z]+|cv)\b/);
});

test('吸附列、准星和空闲格跟随页签纸面取色', () => {
  assert.match(styles, /\.flt-panel\s*\{[^}]*--flt-surface:\s*var\(--nd-paper-surface, var\(--sheet\)\);/);
  for (const selector of ['\\.flt-grid thead \\.flt-corner', '\\.flt-uhead']) {
    assert.match(
      styles,
      new RegExp(`${selector}\\s*\\{[^}]*position:\\s*sticky;[^}]*background:\\s*var\\(--flt-surface\\);`),
    );
  }
  assert.doesNotMatch(styles, /var\(--card\)/);
});

test('机队热力色阶保留四档，删除机器详情旧点阵的选择器', () => {
  for (const level of ['l1', 'l2', 'l3', 'l4']) {
    assert.match(css, new RegExp(`\\.flt-cell\\.${level},\\s*\\.flt-key \\.${level}\\s*\\{`));
  }
  assert.match(styles, /\.flt-cell\.idle,\s*\.flt-key \.idle\s*\{[^}]*color-mix\(in srgb, var\(--observe-1\)/);
  assert.match(styles, /\.flt-cell\.unknown,\s*\.flt-key \.unknown\s*\{[^}]*--flt-cell-ring:/);
});

test('斜排列名整条可点，用户头像保留自身的居中排版', () => {
  // 列名伸进右侧几列的表头格，表头格若接收指针，列名的大半段都点不到。
  assert.match(styles, /\.flt-grid thead th:not\(\.flt-corner\)\s*\{\s*pointer-events:\s*none;/);
  assert.match(styles, /\.flt-mlabel\s*\{\s*pointer-events:\s*auto;/);
  // 曾用 `button > span` 给名字块排版，头像也是按钮下的 span，被改成纵向 flex 后线稿贴顶。
  assert.doesNotMatch(styles, /button\s*>\s*span/);
  assert.match(styles, /\.flt-ubtn \.user-avatar\s*\{\s*width:\s*20px;\s*height:\s*20px;\s*\}/);
  for (const control of ['\\.flt-mlabel', '\\.flt-ubtn']) {
    assert.match(styles, new RegExp(`${control}:focus-visible[^{]*\\{[^}]*outline:\\s*2px solid var\\(--action\\)`));
  }
  assert.doesNotMatch(styles.replace(/@media \(hover: hover\)\s*\{[\s\S]*?\n\}/g, ''), /:hover\s*[,{]/);
});

test('机器用量弹层限制在视口内，正文独立滚动，旧机器流量页签样式已清除', () => {
  const traffic = readFileSync(new URL('../src/node-traffic.css', import.meta.url), 'utf8');
  assert.match(traffic, /\.node-usage-dialog\s*\{[^}]*max-height:\s*calc\(100dvh - 40px\);[^}]*min-height:\s*0;/);
  assert.match(traffic, /\.node-usage-header\s*\{[^}]*flex:\s*none;/);
  assert.match(traffic, /\.node-usage-body\s*\{[^}]*min-height:\s*0;[^}]*overflow-y:\s*auto;/);
  assert.doesNotMatch(css, /\.nd-tab-traffic/);
  assert.doesNotMatch(traffic, /\.nt-tab\b/);
  assert.doesNotMatch(traffic, /\.nts-(?:bar|title|note)\b/);
});
