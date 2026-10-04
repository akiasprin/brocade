import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const styles = readFileSync(new URL('../src/styles.css', import.meta.url), 'utf8');

test('KPI sparklines keep a fine, non-scaling stroke with rounded joins', () => {
  const rule = styles.match(/^\.kpi-spark-line \{([^}]+)\}/m)?.[1];
  assert.ok(rule, 'KPI sparkline style exists');
  assert.match(rule, /stroke-width:\s*1\.2;/);
  assert.match(rule, /vector-effect:\s*non-scaling-stroke;/);
  assert.match(rule, /stroke-linecap:\s*round;/);
  assert.match(rule, /stroke-linejoin:\s*round;/);
});
