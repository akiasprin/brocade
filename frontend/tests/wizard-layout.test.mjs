import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const styles = readFileSync(new URL('../src/styles.css', import.meta.url), 'utf8');
const start = styles.indexOf('/* ═══ 建链向导 ═══');
const end = styles.indexOf('/* ═══ 标记删除 ═══', start);
const wizard = start >= 0 && end > start ? styles.slice(start, end) : '';

test('纳管与建链只保留机器详情式内层纸、不可点击的进度与底部操作条', () => {
  const paper = readFileSync(new URL('../src/ui/wizard-paper.tsx', import.meta.url), 'utf8');
  for (const file of ['nodes', 'chain-wizard']) {
    const source = readFileSync(new URL(`../src/panes/${file}.tsx`, import.meta.url), 'utf8');
    assert.match(source, /<WizardPaper/);
    assert.match(source, /<WizardPaperHeader/);
    assert.match(source, /<WizardFooter/);
    assert.doesNotMatch(source, /<WizardSummary/);
  }
  assert.match(paper, /nd-sheet nd-page pv-page/);
  assert.match(paper, /className="fg-sheet nd-paper"/);
  const nodes = readFileSync(new URL('../src/panes/nodes.tsx', import.meta.url), 'utf8');
  const panes = readFileSync(new URL('../src/panes/index.tsx', import.meta.url), 'utf8');
  assert.doesNotMatch(nodes, /bare \? <div className="fg-sheet">\{body\}<\/div>/);
  assert.doesNotMatch(panes, /variant: 'form', wrapInSheet:/);
  assert.match(paper, /panel config-panel/);
  assert.match(paper, /aria-current=.*current/);
  assert.match(styles, /\.pv-foot\s*\{[\s\S]*?position:\s*sticky;/);
  assert.match(styles, /\.pv-cmd pre\.code\s*\{[^}]*word-break:\s*normal;[^}]*overflow-wrap:\s*normal;/);
  assert.match(styles, /\.pv-cmd-word,\s*\.pv-flag\s*\{\s*white-space:\s*nowrap;/);
  const begin = styles.indexOf('/* ═══ 纳管 / 建链');
  const shared = styles.slice(begin, start);
  assert.doesNotMatch(shared, /#[0-9a-f]{3,8}\b|rgba?\(/i);
  assert.match(shared, /background:\s*var\(--sheet\)/);
  assert.match(shared, /@container \(max-width:\s*700px\)/);
  assert.match(shared, /\.pv-page\.nd-sheet\s*\{[\s\S]*?width:\s*auto;[\s\S]*?max-width:\s*none;/);
  assert.doesNotMatch(shared, /\.pv-page\.nd-sheet\s*\{[^}]*margin-inline:\s*0;/);
});

test('新建链接入协议复用详情页选择行，并始终一行一种协议', () => {
  const source = readFileSync(new URL('../src/panes/chain-wizard.tsx', import.meta.url), 'utf8');
  assert.match(source, /className=\{`protocol-choice wzp-card/);
  assert.match(styles, /\.pv-chain-fields \.wzp\s*\{\s*display:\s*grid;\s*grid-template-columns:\s*minmax\(0, 1fr\);/);
  assert.match(
    styles,
    /@container \(max-width:\s*700px\)[\s\S]*?\.pv-chain-fields \.wzp-card\s*\{\s*grid-template-columns:\s*16px minmax\(0, 1fr\) auto;/,
  );
});

test('窄屏地址说明不继承桌面横向占位为卡片高度', () => {
  assert.match(wizard, /@media \(max-width:\s*820px\)[\s\S]*?\.wz-hop \.wz-field-note\s*\{\s*flex:\s*0 0 auto;/);
});
