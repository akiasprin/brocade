import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const source = path => readFileSync(new URL(path, import.meta.url), 'utf8');
const deploy = source('../src/panes/deploy.tsx');
const agentRelease = source('../src/panes/agent-release.tsx');
const xrayRelease = source('../src/panes/xray-release.tsx');
const cockpit = source('../src/panes/deploy-cockpit.tsx');
const styles = source('../src/styles.css');

test('发布页面使用 mockup 的页签 + 驾驶舱结构，不保留旧 dp 命名体系', () => {
  for (const [name, contents] of [
    ['deploy.tsx', deploy],
    ['agent-release.tsx', agentRelease],
    ['xray-release.tsx', xrayRelease],
    ['styles.css', styles],
  ]) {
    assert.doesNotMatch(contents, /(?:^|[^\w])dp-/, `${name} 仍然包含旧发布页结构`);
  }

  // 发布页照机器详情页分页签（连续纸 + 页头页签），每个页签照用量页的驾驶舱：读数栏 + 列表。
  // 软件版本不再是入口加抽屉，Agent / Xray 各占一个页签。
  assert.match(deploy, /className="nd-sheet nd-page cgc-page"/);
  assert.match(deploy, /<div className="nd-tabs" role="tablist" aria-label="发布页签">/);
  assert.match(deploy, /className="usage-cockpit cgc-cockpit" role="tabpanel"/);
  assert.match(cockpit, /className="usage-ledger cgc-ledger"/);
  assert.match(cockpit, /className=\{`cgc-table\$\{editing \? ' editing' : ''\}`\}/);
  assert.match(deploy, /className="cgf-well"/);
  assert.match(deploy, /className="cgf-track"/);
  assert.doesNotMatch(deploy, /SoftwareLauncher|SoftwareDrawer|cgf-soft|cardpage cg-flow|panel titled cgf/);
  assert.doesNotMatch(deploy, /cg-duo|cg-overview|cg-history|cgo-tabs|cgo-row/);
  assert.match(deploy, /HISTORY_PREVIEW_COUNT = 6/);
  assert.doesNotMatch(deploy, /当前修订 <b>|待发布 <b>/);
  assert.match(deploy, /className="nd-sheet nd-page cg-page"/);
  assert.match(deploy, /className="nd-sheet nd-page cg-page cg-detail"/);
  assert.match(deploy, /className="nd-paper-body cg-body"/);
  assert.match(deploy, /className="cgr-waves"/);
  assert.match(deploy, /className="cg-files/);
  assert.match(deploy, /className=\{`cg-foot/);
  // Agent 与 Xray 用同一套词：升级范围、待升级 / 可升级 / 不可升级、批准。
  for (const [name, contents] of [
    ['agent-release.tsx', agentRelease],
    ['xray-release.tsx', xrayRelease],
  ]) {
    assert.doesNotMatch(contents, /替换|持续批准范围|Xray 二进制|独立于配置修订/, `${name} 仍有旧文案`);
    assert.match(contents, /<ScopeToolbar/, `${name} 没有升级范围工具条`);
    assert.match(contents, /'可升级'/, `${name} 没有「可升级」`);
    assert.match(contents, /'待升级'/, `${name} 没有「待升级」`);
    assert.match(contents, /'不可升级'/, `${name} 没有「不可升级」`);
  }
});

test('发布页是一条流水，变更单复用全站单栏宽度', () => {
  // 时间轴：时刻列 40px、结果图标 20px，竖线穿过图标中心。
  assert.match(styles, /\.cgf-row\s*\{[^}]*grid-template-columns:\s*40px 20px minmax\(0, 1fr\) auto auto/s);
  assert.match(styles, /\.cgf-list > li::before\s*\{[^}]*left:\s*75\.5px/s);
  assert.match(styles, /\.cgc-page\s*\{[^}]*--cgf-well:/s);
  assert.doesNotMatch(styles, /\.cg-duo|\.cg-overview|\.cgo-tabs\b|\.cgo-row\b/);
  assert.doesNotMatch(styles, /\.cgf-software|\.cgf-soft\b|\.cgf-ready|\.cg-software\b|\.ag-t\b/);
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
  assert.match(styles, /\.cg-disclosure > summary::after\s*\{[^}]*display:\s*none;/s);
  // 驾驶舱铺满纸面内距；机器表各行定宽对齐，窄宽度改为一台两行。
  assert.match(styles, /\.usage-cockpit\.cgc-cockpit\s*\{[^}]*margin:\s*-18px -20px -22px/s);
  assert.match(
    styles,
    /\.cgc-th,\s*\.cgc-tr\s*\{[^}]*grid-template-columns:\s*minmax\(170px, 1\.3fr\) minmax\(0, 0\.8fr\) minmax\(0, 0\.8fr\) 124px 78px/s,
  );
  assert.match(
    styles,
    /@container \(max-width: 700px\)\s*\{[\s\S]*?\.cgc-tr,\s*\.cgc-table\.editing \.cgc-tr\s*\{[^}]*grid-template-areas:/,
  );
  assert.doesNotMatch(deploy, /执行前 → 本次目标/);
  assert.match(deploy, /<summary className="btn" aria-label="更多操作">\s*<Icon of="more" size=\{16\} \/>/s);
  assert.doesNotMatch(deploy, />\s*•••\s*</);
});
