import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const styles = readFileSync(new URL('../src/styles.css', import.meta.url), 'utf8');
const nodes = readFileSync(new URL('../src/panes/nodes.tsx', import.meta.url), 'utf8');
const chains = readFileSync(new URL('../src/panes/chains.tsx', import.meta.url), 'utf8');

test('VPN Gate keeps its compact country rail on the right', () => {
  assert.match(styles, /\.vpngate-split\s*\{[^}]*grid-template-columns:\s*minmax\(0, 1fr\) 320px/s);
  assert.match(styles, /\.vpngate-side\s*\{[^}]*grid-column:\s*2/s);
  assert.match(styles, /\.vpngate-work\s*\{[^}]*grid-column:\s*1/s);
  assert.match(styles, /\.loading-skeleton-vpngate-rail\s*\{[^}]*grid-column:\s*2/s);
  assert.match(styles, /\.loading-skeleton-vpngate-work\s*\{[^}]*grid-column:\s*1/s);
});

test('WARP uses the same right-rail layout without changing other release details', () => {
  assert.match(
    styles,
    /\.wp-page\.cg-detail \.cg-body\.nd-paper-body\s*\{[^}]*grid-template-columns:\s*minmax\(0, 1fr\) 320px/s,
  );
  assert.match(styles, /\.wp-page\.cg-detail \.cg-aside\s*\{[^}]*grid-column:\s*2/s);
  assert.match(styles, /\.wp-page\.cg-detail \.cg-main\s*\{[^}]*grid-column:\s*1/s);
  assert.match(
    styles,
    /@container \(max-width: 1080px\)[\s\S]*?\.wp-page\.cg-detail \.cg-body\.nd-paper-body\s*\{[^}]*grid-template-columns:\s*minmax\(0, 1fr\)/,
  );
});

test('VPN Gate, WARP, and machine detail headers pin on desktop but scroll away on mobile', () => {
  assert.match(
    styles,
    /@media \(min-width: 821px\)\s*\{\s*\.nd-page-head\s*\{[^}]*position:\s*sticky;[^}]*top:\s*0;[^}]*background:\s*transparent;[^}]*backdrop-filter:\s*blur\(16px\) saturate\(1\.12\)/s,
  );
  assert.match(styles, /@media \(max-width: 820px\)\s*\{\s*\.nd-page-head\s*\{[^}]*position:\s*static;/s);
  assert.doesNotMatch(styles, /\.nd-node-detail > \.nd-paper > \.nd-page-head\s*\{[^}]*position:\s*sticky/s);
});

test('machine action menu escapes the sticky header backdrop root', () => {
  assert.match(
    nodes,
    /actsOpen\s*&&\s*createPortal\([\s\S]*?className="fg-menu action-menu"[\s\S]*?data-node-action-menu[\s\S]*?document\.body/,
  );
  assert.match(
    styles,
    /\.fg-menu\.action-menu\[data-node-action-menu\]\s*\{[^}]*position:\s*fixed;[^}]*right:\s*auto;[^}]*min-width:\s*0;[^}]*width:\s*min\(440px, calc\(100vw - 20px\)\);[^}]*max-height:\s*calc\(100dvh - 20px\);[^}]*overflow-y:\s*auto;/s,
  );
});

test('machine config keeps traffic below identity and DNS above certificates', () => {
  assert.match(
    nodes,
    /<PanelTitle of="identity">身份<\/PanelTitle>[\s\S]*?<TrafficAccountingCard node=\{n\} canEdit=\{system\} \/>[\s\S]*?<WgCard[\s\S]*?<div>\s*\{snapshot\.data && <DnsCard node=\{n\} canEdit=\{system\} onSaved=\{onSaved\} \/>\}[\s\S]*?\{snapshot\.data && <CertGroupCard node=\{n\}/,
  );

  const trafficCard = nodes.slice(
    nodes.indexOf('export function TrafficAccountingCard'),
    nodes.indexOf('export function LogRetentionCard'),
  );
  assert.match(trafficCard, /<div className="panel config-panel node-traffic-accounting">\s*<header>/);
  assert.doesNotMatch(trafficCard, /<(?:details|summary)\b/);
});

test('chain detail keeps one continuous two-column panel flow', () => {
  const detail = chains.slice(chains.indexOf('function ChainDetail('), chains.indexOf('type ChainRuleRow'));
  const grid = detail.slice(
    detail.indexOf('<div className="duo chain-config-grid">'),
    detail.indexOf('<section className="chain-detail-section chain-detail-section-rules">'),
  );
  const firstColumnStart = grid.indexOf('<div className="col">');
  const secondColumnStart = grid.indexOf('<div className="col">', firstColumnStart + 1);
  assert.ok(firstColumnStart >= 0 && secondColumnStart > firstColumnStart, '线路详情双栏标记缺失或顺序无效');

  const left = grid.slice(firstColumnStart, secondColumnStart);
  const right = grid.slice(secondColumnStart);
  assert.match(left, /<ProbeBanner item=\{probe\}/);
  assert.match(left, /<ConfigPanel title="入站"/);
  assert.match(left, /<ConfigPanel title="客户端配置"/);
  assert.match(left, /<IngressGuardBlock/);
  assert.match(right, /<ConfigPanel title="协议"/);
  assert.match(right, /title="VLESS · TLS \/ REALITY"/);
  assert.doesNotMatch(grid, /chain-access-grid|chain-protocol-grid|chain-detail-column/);
  assert.match(styles, /\.chain-config-grid\s*\{[^}]*margin:\s*0;[^}]*gap:\s*12px;/s);
});
