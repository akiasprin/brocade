import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const rust = readFileSync(new URL('../../crates/brocade-deployment/src/protocol.rs', import.meta.url), 'utf8');
const api = readFileSync(new URL('../src/api.ts', import.meta.url), 'utf8');

test('frontend and control plane expose the same Agent protocol version', () => {
  const rustVersion = rust.match(/pub const AGENT_PROTOCOL_VERSION: u32 = (\d+);/)?.[1];
  const frontendVersion = api.match(/export const AGENT_PROTOCOL_VERSION = (\d+);/)?.[1];
  assert.ok(rustVersion, 'Rust Agent protocol constant is missing');
  assert.equal(frontendVersion, rustVersion);
});
