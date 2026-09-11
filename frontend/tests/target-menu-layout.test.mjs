import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const styles = readFileSync(new URL('../src/styles.css', import.meta.url), 'utf8');
const chains = readFileSync(new URL('../src/panes/chains.tsx', import.meta.url), 'utf8');
const rules = readFileSync(new URL('../src/panes/rules.tsx', import.meta.url), 'utf8');

test('目标菜单脱离卡片裁切，监听编辑保持紧凑且不再显示决策图', () => {
  assert.match(styles, /\.external-target-menu\s*\{[^}]*position:\s*fixed;/s);
  assert.match(
    styles,
    /\.external-target-menu > button\.external-target-new\s*\{[^}]*grid-template-columns:\s*minmax\(0,\s*1fr\) auto;[^}]*padding:\s*5px 7px;/s,
  );
  assert.match(styles, /\.external-target-copy\s*\{[^}]*display:\s*grid;/s);
  assert.match(styles, /\.external-target-copy b,\s*\.external-target-copy small\s*\{[^}]*display:\s*block;/s);
  assert.match(styles, /\.listener-reference-copy\s*\{[^}]*display:\s*grid;[^}]*flex:\s*1 1 auto;/s);
  assert.match(styles, /\.listener-reference-panel\.hop-target-panel\s*\{[^}]*--listener-panel-accent:/s);
  assert.match(styles, /\.hop-target-facts\s*\{[^}]*flex-wrap:\s*nowrap;/s);
  assert.match(
    styles,
    /\.hop-target-panel \.hop-target-facts > \.hopfld\s*\{[^}]*display:\s*inline-flex;[^}]*align-items:\s*center;/s,
  );
  assert.match(
    styles,
    /@container rules \(max-width:\s*820px\)[\s\S]*\.listener-reference-row,\s*\.hop-target-row\s*\{[^}]*grid-template-columns:\s*minmax\(0,\s*1fr\);/s,
  );
  assert.match(
    styles,
    /@container rules \(max-width:\s*820px\)[\s\S]*\.listener-reference-facts,\s*\.hop-target-row \.hop-target-facts\s*\{[^}]*display:\s*grid;[^}]*grid-template-columns:\s*minmax\(0,\s*1fr\);/s,
  );
  assert.match(rules, /className="hopfld hop-target-mux"/);
  assert.match(rules, /className="listener-reference-mux"/);
  assert.doesNotMatch(rules, /发布会重启受影响的 Xray/);
  assert.doesNotMatch(rules, /<span>↗<\/span>/);
  assert.doesNotMatch(chains, /<section className="listener-map">/);
  assert.doesNotMatch(rules, /listener-reference-highlight-hitbox/);
});
