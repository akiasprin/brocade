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
    /\.config-panel \.client-projection-family-grid\s*\{[\s\S]*?grid-template-columns:\s*repeat\(2, 220px\);[\s\S]*?justify-content:\s*start/,
  );
  assert.match(styles, /\.client-projection-family\s*\{[\s\S]*?width:\s*220px/);
  assert.match(styles, /\.client-projection-family \.ing-pj\s*\{[\s\S]*?width:\s*calc\(100% \+ 62px\)/);
  assert.doesNotMatch(source, /使用机器公网地址/);
  assert.doesNotMatch(source, /<em>:\{ingress\.port\}<\/em>/);
});
