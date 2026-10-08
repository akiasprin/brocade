import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const styles = readFileSync(new URL('../src/styles.css', import.meta.url), 'utf8');
const settings = readFileSync(new URL('../src/panes/settings.tsx', import.meta.url), 'utf8');

test('tunnel probe panels collapse instead of overflowing narrow screens', () => {
  assert.match(
    styles,
    /@container \(max-width: 520px\)[\s\S]*?\.tunnel-probe-actions\s*\{[\s\S]*?width:\s*100%;[\s\S]*?\.tunnel-probe-phases\s*\{[\s\S]*?grid-template-columns:\s*repeat\(2, minmax\(0, 1fr\)\);/,
  );
  assert.match(
    styles,
    /\.tunnel-probe-run\s*\{[\s\S]*?grid-template-columns:\s*8px minmax\(0, 1\.2fr\) minmax\(0, 0\.8fr\) auto;[\s\S]*?@container \(max-width: 520px\)[\s\S]*?\.tunnel-probe-run\s*\{[\s\S]*?grid-template-columns:\s*8px minmax\(0, 1fr\) auto;/,
  );
  assert.match(styles, /\.tunnel-probe-setting-facts code\s*\{[\s\S]*?overflow-wrap:\s*anywhere;/);
  assert.match(styles, /\.tunnel-probe-settings\s*\{[\s\S]*?width:\s*100%;[\s\S]*?box-sizing:\s*border-box;/);
});

test('runtime settings stay inside the settings two-column flow', () => {
  assert.match(settings, /<div className="duo settings-layout">/);
  assert.match(settings, /<div className="col">[\s\S]*?<VpngateIntelligenceSection[\s\S]*?<\/fieldset>[\s\S]*?<\/div>/);
  assert.match(settings, /<div className="col">[\s\S]*?<\/fieldset>[\s\S]*?<TunnelProbeSettingsSection[\s\S]*?<\/div>/);
  assert.match(styles, /\.tunnel-probe-settings\s*\{[\s\S]*?container-type:\s*inline-size;/);
  assert.match(styles, /\.vpngate-intelligence-settings\s*\{[\s\S]*?container-type:\s*inline-size;/);
  assert.match(
    styles,
    /@container \(max-width: 700px\)[\s\S]*?\.vpngate-intelligence-layout\s*\{[\s\S]*?grid-template-columns:\s*minmax\(0, 1fr\);/,
  );
});
