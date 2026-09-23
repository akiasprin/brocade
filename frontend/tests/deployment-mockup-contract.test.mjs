import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const source = path => readFileSync(new URL(path, import.meta.url), 'utf8');
const deploy = source('../src/panes/deploy.tsx');
const agentRelease = source('../src/panes/agent-release.tsx');
const xrayRelease = source('../src/panes/xray-release.tsx');
const styles = source('../src/styles.css');

test('发布页面使用 mockup 的单一结构，不保留旧 dp 命名体系', () => {
  for (const [name, contents] of [
    ['deploy.tsx', deploy],
    ['agent-release.tsx', agentRelease],
    ['xray-release.tsx', xrayRelease],
    ['styles.css', styles],
  ]) {
    assert.doesNotMatch(contents, /(?:^|[^\w])dp-/, `${name} 仍然包含旧发布页结构`);
  }

  // 发布页是一块面板一条流水：抬头 + 软件读数行 + 时间轴，没有第二块列表面板。
  assert.match(deploy, /className="cardpage cg-flow"/);
  assert.match(deploy, /className="panel titled cgf" data-page-title="true"/);
  assert.match(deploy, /className="cgf-soft"/);
  assert.match(deploy, /className="cgf-well"/);
  assert.match(deploy, /className="cgf-track"/);
  assert.doesNotMatch(deploy, /cg-duo|cg-overview|cg-history|cgo-tabs|cgo-row/);
  assert.match(deploy, /HISTORY_PREVIEW_COUNT = 12/);
  assert.doesNotMatch(deploy, /当前修订 <b>|待发布 <b>/);
  assert.match(deploy, /className="nd-sheet nd-page cg-page"/);
  assert.match(deploy, /className="nd-sheet nd-page cg-page cg-detail"/);
  assert.match(deploy, /className="nd-paper-body cg-body"/);
  assert.match(deploy, /className="cgr-waves"/);
  assert.match(deploy, /className="cg-files/);
  assert.match(deploy, /className=\{`cg-foot/);
  assert.match(agentRelease, /<PanelTitle of="agent">Agent 版本<\/PanelTitle>/);
  assert.doesNotMatch(agentRelease, /持续批准范围/);
  assert.match(xrayRelease, /<PanelTitle of="xray">Xray 版本<\/PanelTitle>/);
  assert.doesNotMatch(xrayRelease, /Xray 二进制|独立于配置修订/);
});

test('发布页是一条流水，变更单复用全站单栏宽度', () => {
  // 时间轴：时刻列 40px、结果图标 20px，竖线穿过图标中心。
  assert.match(
    styles,
    /\.cgf-row\s*\{[^}]*grid-template-columns:\s*40px 20px minmax\(0, 1fr\) auto auto/s,
  );
  assert.match(styles, /\.cgf-list > li::before\s*\{[^}]*left:\s*75\.5px/s);
  assert.match(styles, /\.cg-flow \.panel\.titled\s*\{[^}]*--cgf-well:/s);
  assert.doesNotMatch(styles, /\.cg-duo|\.cg-overview|\.cgo-tabs\b|\.cgo-row\b/);
  assert.doesNotMatch(styles, /\.cg-page\.nd-sheet\s*\{[^}]*\b(?:width|margin):/s);
  assert.doesNotMatch(styles, /\.fg-sheet\s*>\s*\.wp-page\s*\{/);
  assert.match(styles, /\.cg-body\.nd-paper-body\s*\{[^}]*grid-template-columns:\s*minmax\(0, 1fr\)/s);
  assert.match(styles, /\.cg-aside\s*\{[^}]*position:\s*static;[^}]*grid-row:\s*1/s);
  assert.match(
    styles,
    /\.cg-detail \.cg-body\.nd-paper-body\s*\{[^}]*grid-template-columns:\s*minmax\(0, 1fr\) 320px/s,
  );
  assert.match(styles, /\.cg-detail \.cg-aside\s*\{[^}]*position:\s*sticky;[^}]*top:\s*76px;[^}]*grid-column:\s*2/s);
  assert.match(
    styles,
    /@container \(max-width: 1080px\)\s*\{[\s\S]*?\.cg-detail \.cg-body\.nd-paper-body\s*\{[^}]*grid-template-columns:\s*minmax\(0, 1fr\)/,
  );
  assert.match(styles, /\.cg-files\s*\{[^}]*grid-template-columns:\s*216px minmax\(0, 1fr\)/s);
  assert.match(deploy, /<summary className="btn" aria-label="更多操作">\s*<Icon of="more" size=\{16\} \/>/s);
  assert.doesNotMatch(deploy, />\s*•••\s*</);
});
