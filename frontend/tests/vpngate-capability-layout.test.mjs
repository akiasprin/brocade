import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const styles = readFileSync(new URL('../src/styles.css', import.meta.url), 'utf8');

test('VPN Gate catalogue probe controls stay on the last card row', () => {
  assert.match(styles, /\.vpngate-capability-list article\s*\{[^}]*display:\s*flex;[^}]*flex-direction:\s*column;/s);
  assert.match(styles, /\.vpngate-capability-actions\s*\{[^}]*margin-top:\s*auto;[^}]*padding-top:\s*4px;/s);
});
