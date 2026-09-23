import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const styles = readFileSync(new URL('../src/styles.css', import.meta.url), 'utf8');

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
