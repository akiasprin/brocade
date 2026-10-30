import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

test('all generated country and Fastly mappings have a bundled flag and traceable evidence', () => {
  const data = JSON.parse(readFileSync(new URL('../src/ui/unlock-region-data.json', import.meta.url), 'utf8'));
  const sources = JSON.parse(readFileSync(new URL('../scripts/unlock-region-data.sources.json', import.meta.url), 'utf8'));
  const sheet = readFileSync(new URL('../src/ui/flags.ts', import.meta.url), 'utf8').split('export const FLAG_SHEET = [')[1];
  const flags = new Set([...sheet.matchAll(/'([a-z]+)'/g)].flatMap(([, row]) => row.match(/../g)));
  assert.ok(Object.keys(data.iso3).length >= 249);
  assert.ok(sources.fastly_pops >= 100);
  assert.equal(Object.keys(data.fastly).length, sources.fastly_pops);
  assert.deepEqual(sources.unmapped_pops, []);
  for (const table of [data.iso3, data.fastly]) {
    for (const [code, region] of Object.entries(table)) {
      assert.match(code, /^[A-Z]{3}$/);
      assert.ok(flags.has(region.toLowerCase()), `${code}: missing ${region} flag`);
    }
  }
  for (const code of Object.keys(data.fastly)) assert.ok(sources.evidence[code]);
  for (const source of Object.values(sources.sources)) assert.match(source.sha256, /^[a-f0-9]{64}$/);
});
