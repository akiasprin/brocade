import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const source = path => readFileSync(new URL(path, import.meta.url), 'utf8');
const deploy = source('../src/panes/deploy.tsx');
const agentRelease = source('../src/panes/agent-release.tsx');
const xrayRelease = source('../src/panes/xray-release.tsx');
const binaryRelease = source('../src/panes/binary-release.tsx');
const binaryStatus = source('../src/ui/binary-release-status.ts');
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
  assert.match(deploy, /className="cga-list"/);
  assert.match(deploy, /className=\{`cg-foot/);
  // Agent 与 Xray 用同一套词：升级范围、待升级 / 可升级 / 不可升级、批准。
  for (const [name, contents] of [
    ['agent-release.tsx', agentRelease],
    ['xray-release.tsx', xrayRelease],
  ]) {
    assert.match(contents, /<BinaryReleaseTab/, `${name} 未复用二进制发布视图`);
    assert.match(binaryRelease, /<ScopeToolbar/);
    assert.match(binaryRelease, /'可升级'/);
    assert.match(binaryStatus, /'待升级'/);
    assert.match(binaryRelease, /'不可升级'/);
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
  assert.match(styles, /\.cga-row\s*\{[^}]*grid-template-columns:\s*minmax\(220px, 300px\) minmax\(0, 1fr\) auto/s);
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

test('变更单：执行进度是时间线，产物是一行一个归属的清单，页头备注单行截取', () => {
  // 时间线：16 内距 + 54 时刻列 + 12 间距 + 10 半个图标，竖线穿过结果图标中心；机器格与阶段名同一左缘。
  assert.match(
    styles,
    /\.cgp-head,\s*\.cgp-group\s*\{[^}]*grid-template-columns:\s*54px 20px minmax\(0, 1fr\) auto 40px/s,
  );
  assert.match(styles, /\.cgp-line > li::before\s*\{[^}]*left:\s*91\.5px/s);
  assert.match(styles, /\.cgp-grid\s*\{[^}]*padding:\s*0 16px 6px 114px/s);
  assert.match(styles, /\.cgp-fail\s*\{[^}]*margin:\s*0 16px 6px 114px/s);
  assert.match(styles, /@container \(max-width: 700px\)\s*\{[\s\S]*?\.cgp-line > li::before\s*\{[^}]*left:\s*25\.5px/);
  assert.match(
    styles,
    /@media \(prefers-reduced-motion: reduce\)\s*\{\s*\.cg-lamp\.run,\s*\.cgp-step\.run \.cgp-node > i\s*\{/,
  );
  // 产物清单：变更单详情与创建变更单共用一套行与差异视图，差异区定高滚动；旧的文件树双栏已删除。
  assert.match(styles, /\.cga-code\.fg-code\s*\{[^}]*max-height:\s*520px/s);
  assert.equal(deploy.match(/<ArtifactList\b/g)?.length, 2, '产物记录与产物差异都应使用 ArtifactList');
  assert.doesNotMatch(deploy, /cg-files|cg-tree|cg-viewer|cgr-fail|collapseContext/);
  assert.doesNotMatch(
    styles,
    /\.cg-files\b|\.cg-tree\b|\.cg-viewer\b|\.cg-diff\b|\.cgr-fail\b|\.cg-artifact-unavailable/,
  );
  // 创建变更单的产物差异在第一次展开时才挂载：每份变更文件要拉两个修订的内容。
  assert.match(deploy, /\{artifactsShown && \(\s*<ArtifactChanges/);
  // 页头：身份区不再 flex: none 撑满，备注最宽 30em 后省略，修订范围单独一段不截。
  assert.match(styles, /\.cg-detail \.cg-head \.nd-page-identity\s*\{\s*flex:\s*0 1 auto;/);
  assert.match(styles, /\.cg-head-note\s*\{[^}]*max-width:\s*30em;[^}]*text-overflow:\s*ellipsis/s);
  assert.match(styles, /\.cg-head-range\s*\{\s*flex:\s*none;/);
  assert.match(deploy, /<span className="cg-head-note" title=\{note\}>/);
});

test('产物记录折叠后保留完整标题条', () => {
  // 公共折叠规则用 -11px 下外边距抵消卡片 11px 下内距；分区内距为 0，必须在其后归零，
  // 否则折叠卡只占 22px，标题条下半截被裁掉。两条规则优先级相同，由先后顺序决定。
  assert.match(
    deploy,
    /<details\s+className="panel config-panel cg-sec cg-disclosure cg-artifact-review"\s+open=\{expanded\}\s+onToggle=/,
  );
  assert.match(styles, /\.cg-sec\.panel\s*\{[^}]*padding:\s*0;/s);
  const shared = styles.search(/\.panel\.config-panel:not\(\[open\]\) > summary[^{]*\{[^}]*margin-bottom:\s*-11px;/s);
  const section = styles.search(/\.cg-sec\.panel:not\(\[open\]\) > summary\s*\{[^}]*margin-bottom:\s*0;/s);
  assert.notEqual(shared, -1, '公共折叠规则已改动，需重新核对分区标题条');
  assert.ok(section > shared, '分区折叠标题条的归零规则必须位于公共折叠规则之后');
});
