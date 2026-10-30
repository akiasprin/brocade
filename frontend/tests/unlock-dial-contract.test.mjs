import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

test('every structured Xray dial category is understood by Agent policy and frontend explanations', () => {
  const go = readFileSync(new URL('../../components/xray-core/transport/internet/probe_dial.go', import.meta.url), 'utf8');
  const policy = readFileSync(new URL('../../crates/brocade-deployment/src/unlock_probe/dial.rs', import.meta.url), 'utf8');
  const ui = readFileSync(new URL('../src/panes/unlock-dial.ts', import.meta.url), 'utf8');
  const codes = [...new Set([...go.matchAll(/return "([a-z0-9_]+)"/g)].map(match => match[1]))].filter(code => code !== 'connect_error');
  assert.ok(codes.length >= 10, 'the classifier must expose actual structured categories');
  for (const code of codes) {
    assert.ok(policy.includes(`"${code}"`), `Agent does not understand ${code}`);
    assert.ok(ui.includes(`${code}:`), `frontend does not explain ${code}`);
  }
});
