import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const source = readFileSync(new URL('../src/panes/chains.tsx', import.meta.url), 'utf8');
const styles = readFileSync(new URL('../src/styles.css', import.meta.url), 'utf8');

test('client mappings group protocols and place IPv4 and IPv6 side by side', () => {
  assert.match(source, /activeProjectionProtocols\(ingress\)\.map\(protocol/);
  assert.match(source, /className="client-projection-group"/);
  assert.match(source, /className="kv form2 chain-face client-projection-family-grid"/);
  assert.match(source, /className="client-projection-family"/);
  assert.match(
    styles,
    /\.config-panel \.client-projection-family-grid\s*\{[^}]*grid-template-columns:\s*repeat\(2, minmax\(0, 242px\)\);[^}]*justify-content:\s*start/,
  );
  assert.doesNotMatch(source, /使用机器公网地址/);
  assert.doesNotMatch(source, /<em>:\{ingress\.port\}<\/em>/);
});

test('conversion controls and editors share the form alignment without extending hit areas into the next family', () => {
  assert.match(styles, /\.client-projection-groups\s*\{[^}]*container:\s*client-projection \/ inline-size/);
  assert.match(
    styles,
    /\.config-panel \.client-projection-family-grid\s*\{[^}]*padding-left:\s*var\(--projection-control-offset\)/,
  );
  assert.match(styles, /\.client-projection-editor\s*\{[^}]*grid-template-columns:\s*minmax\(0, 1fr\) 68px/);
  assert.doesNotMatch(styles, /\.client-projection-editor\s*\{[^}]*(?:margin-left|width:\s*calc)/);
  assert.match(
    styles,
    /@container client-projection \(max-width: 440px\)\s*\{\s*\.config-panel \.client-projection-family-grid\s*\{\s*grid-template-columns:\s*minmax\(0, 1fr\)/,
  );
});
