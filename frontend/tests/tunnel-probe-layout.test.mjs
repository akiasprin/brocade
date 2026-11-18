import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const styles = readFileSync(new URL('../src/styles.css', import.meta.url), 'utf8');
const settings = readFileSync(new URL('../src/panes/settings.tsx', import.meta.url), 'utf8');

test('the tunnel detail probe panel collapses instead of overflowing narrow screens', () => {
  // Readings, records and the policy line rearrange inside the detail container, not the viewport.
  assert.match(
    styles,
    /\.tnd-body\.nd-paper-body\s*\{[\s\S]*?grid-template-columns:\s*minmax\(0, 1fr\) 320px;[\s\S]*?@container \(max-width: 1080px\)\s*\{\s*\.tnd-body\.nd-paper-body\s*\{\s*grid-template-columns:\s*minmax\(0, 1fr\);/,
  );

  assert.match(
    styles,
    /@container \(max-width: 700px\)[\s\S]*?\.tnd-metrics,\s*\.tnd-metrics\.is-3\s*\{\s*grid-template-columns:\s*repeat\(2, minmax\(0, 1fr\)\);[\s\S]*?\.tnd-route\s*\{\s*flex-basis:\s*100%;/,
  );
});

test('runtime settings stay inside the settings two-column flow', () => {
  assert.match(settings, /<div className="duo settings-layout">/);
  assert.match(settings, /<div className="col">[\s\S]*?<VpngateIntelligenceSection[\s\S]*?<\/fieldset>[\s\S]*?<\/div>/);
  assert.doesNotMatch(settings, /TunnelProbeSettingsSection|set-tunnel-probes/);
  assert.match(styles, /\.vpngate-intelligence-settings\s*\{[\s\S]*?container-type:\s*inline-size;/);
  assert.match(
    styles,
    /@container \(max-width: 700px\)[\s\S]*?\.vpngate-intelligence-grid\s*\{[\s\S]*?grid-template-columns:\s*repeat\(2, minmax\(0, 1fr\)\);/,
  );
});
