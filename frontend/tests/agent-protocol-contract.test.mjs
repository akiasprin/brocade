import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import { DEFAULT_TLS_FINGERPRINT } from '../src/reality.ts';

const rust = readFileSync(new URL('../../crates/brocade-deployment/src/protocol.rs', import.meta.url), 'utf8');
const core = readFileSync(new URL('../../crates/brocade-core/src/model.rs', import.meta.url), 'utf8');
const api = readFileSync(new URL('../src/api.ts', import.meta.url), 'utf8');

test('compiler, database and frontend agree on the new TLS fingerprint default', () => {
  const rustDefault = core.match(/pub const DEFAULT_TLS_FINGERPRINT: &str = "([^"]+)";/)?.[1];
  const schema = readFileSync(new URL('../../crates/brocade-store/migrations/0001_init.sql', import.meta.url), 'utf8');
  const databaseDefault = schema.match(/reality_fingerprint TEXT DEFAULT '([^']+)'/)?.[1];
  assert.ok(rustDefault, 'compiler TLS fingerprint default is missing');
  assert.ok(databaseDefault, 'database TLS fingerprint default is missing');
  assert.equal(rustDefault, 'safari');
  assert.equal(DEFAULT_TLS_FINGERPRINT, rustDefault);
  assert.equal(databaseDefault, rustDefault);
});

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

test('frontend and Console agree on the first Agent with forwarding target policies', () => {
  const rustVersion = rust.match(/pub const FORWARDING_TARGET_POLICY_PROTOCOL_VERSION: u32 = (\d+);/)?.[1];
  const frontendVersion = api.match(/export const FORWARDING_TARGET_POLICY_PROTOCOL_VERSION = (\d+);/)?.[1];
  assert.ok(rustVersion, 'Rust forwarding target policy protocol constant is missing');
  assert.ok(frontendVersion, 'Frontend forwarding target policy protocol constant is missing');
  assert.equal(frontendVersion, rustVersion);
});

test('frontend and compiler expose the same VPN Gate candidate bound', () => {
  const rustLimit = core.match(/pub const VPNGATE_MAX_CANDIDATES: u8 = (\d+);/)?.[1];
  const frontendLimit = api.match(/export const VPNGATE_MAX_CANDIDATES = (\d+);/)?.[1];
  assert.ok(rustLimit, 'Rust VPN Gate candidate bound is missing');
  assert.equal(frontendLimit, rustLimit);
});

test('frontend and Agent agree on the first protocol with user-rate reports', () => {
  const rustVersion = rust.match(/pub const REALTIME_USER_TRAFFIC_PROTOCOL_VERSION: u32 = (\d+);/)?.[1];
  const frontendVersion = api.match(/export const REALTIME_USER_TRAFFIC_PROTOCOL_VERSION = (\d+);/)?.[1];
  assert.ok(rustVersion, 'Rust user-rate protocol constant is missing');
  assert.ok(frontendVersion, 'Frontend user-rate protocol constant is missing');
  assert.equal(frontendVersion, rustVersion);
});

test('frontend and runtime expose the same VPN Gate connection timeout ceiling', () => {
  const rustTimeout = core.match(/pub const VPNGATE_CONNECT_THRESHOLD_MAX_MS: u32 = ([\d_]+);/)?.[1];
  const frontendTimeout = api.match(/export const VPNGATE_CONNECT_THRESHOLD_MAX_MS = ([\d_]+);/)?.[1];
  assert.ok(rustTimeout, 'Rust VPN Gate connection timeout ceiling is missing');
  assert.equal(frontendTimeout?.replaceAll('_', ''), rustTimeout.replaceAll('_', ''));
});

test('frontend and Store agree on explicit handoff restart fallback capability', () => {
  const rustVersion = rust.match(/pub const XRAY_HANDOFF_RESTART_FALLBACK_PROTOCOL_VERSION: u32 = (\d+);/)?.[1];
  const frontendVersion = api.match(/export const XRAY_HANDOFF_RESTART_FALLBACK_PROTOCOL_VERSION = (\d+);/)?.[1];
  assert.ok(rustVersion, 'Rust handoff fallback protocol constant is missing');
  assert.ok(frontendVersion, 'Frontend handoff fallback protocol constant is missing');
  assert.equal(frontendVersion, rustVersion);
});
