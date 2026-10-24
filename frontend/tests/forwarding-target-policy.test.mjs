import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';

// 前端在提交前判断目标地址时用的网段，必须与服务端准入、数据面守卫共用的那份逐项一致；
// 两边各维护一份，这里防止只改了一边。
const rust = readFileSync(new URL('../../crates/brocade-core/src/forwarding.rs', import.meta.url), 'utf8');
const ts = readFileSync(new URL('../src/ui/forwarding-target.ts', import.meta.url), 'utf8');

const cidrs = (text, pattern) => {
  const match = pattern.exec(text);
  assert.ok(match, `找不到 ${pattern}`);
  return [...match[1].matchAll(/["']([^"']+)["']/g)].map(entry => entry[1]);
};

test('前端转发目标网段与 brocade-core BLOCKED_TARGET_CIDRS 逐项一致', () => {
  const server = cidrs(rust, /pub const BLOCKED_TARGET_CIDRS: &\[&str\] = &\[([\s\S]*?)\];/);
  const browser = cidrs(ts, /export const BLOCKED_TARGET_CIDRS = \[([\s\S]*?)\] as const;/);
  assert.ok(server.length > 10);
  assert.deepEqual(browser, server);
});

test('允许私网时可放行的范围与服务端一致', () => {
  const server = cidrs(rust, /pub const PRIVATE_TARGET_CIDRS: &\[&str\] = &\[([\s\S]*?)\];/);
  const browser = cidrs(ts, /export const PRIVATE_TARGET_CIDRS = \[([\s\S]*?)\] as const;/);
  assert.ok(server.length > 0);
  assert.deepEqual(browser, server);
});
