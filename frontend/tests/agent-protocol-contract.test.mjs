import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const rust = readFileSync(new URL('../../crates/brocade-deployment/src/protocol.rs', import.meta.url), 'utf8');
const core = readFileSync(new URL('../../crates/brocade-core/src/model.rs', import.meta.url), 'utf8');
const api = readFileSync(new URL('../src/api.ts', import.meta.url), 'utf8');

test('frontend and control plane expose the same Agent protocol version', () => {
  const rustVersion = rust.match(/pub const AGENT_PROTOCOL_VERSION: u32 = (\d+);/)?.[1];
  const frontendVersion = api.match(/export const AGENT_PROTOCOL_VERSION = (\d+);/)?.[1];
  const rustMinimum = rust.match(/pub const MIN_AGENT_PROTOCOL_VERSION: u32 = (\d+);/)?.[1];
  const frontendMinimum = api.match(/export const MIN_AGENT_PROTOCOL_VERSION = (\d+);/)?.[1];
  assert.ok(rustVersion, 'Rust Agent protocol constant is missing');
  assert.equal(frontendVersion, rustVersion);
  assert.ok(rustMinimum, 'Rust minimum Agent protocol constant is missing');
  assert.equal(frontendMinimum, rustMinimum);
});

test('frontend and compiler expose the same VPN Gate candidate bound', () => {
  const rustLimit = core.match(/pub const VPNGATE_MAX_CANDIDATES: u8 = (\d+);/)?.[1];
  const frontendLimit = api.match(/export const VPNGATE_MAX_CANDIDATES = (\d+);/)?.[1];
  assert.ok(rustLimit, 'Rust VPN Gate candidate bound is missing');
  assert.equal(frontendLimit, rustLimit);
});

test('frontend and runtime expose the same VPN Gate connection timeout ceiling', () => {
  const rustTimeout = core.match(/pub const VPNGATE_CONNECT_THRESHOLD_MAX_MS: u32 = ([\d_]+);/)?.[1];
  const frontendTimeout = api.match(/export const VPNGATE_CONNECT_THRESHOLD_MAX_MS = ([\d_]+);/)?.[1];
  assert.ok(rustTimeout, 'Rust VPN Gate connection timeout ceiling is missing');
  assert.equal(frontendTimeout?.replaceAll('_', ''), rustTimeout.replaceAll('_', ''));
});
