import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const shell = readFileSync(new URL('../src/forge/shell.tsx', import.meta.url), 'utf8');
const traffic = readFileSync(new URL('../src/ui/fleet-traffic.tsx', import.meta.url), 'utf8');
const source = readFileSync(new URL('../src/fleet-realtime.ts', import.meta.url), 'utf8');
const styles = readFileSync(new URL('../src/styles.css', import.meta.url), 'utf8');

test('流量和状态在桌面顶栏通知之前，移除面包屑行与均值标签', () => {
  assert.equal(shell.match(/<FleetTrafficMeter \/>/g)?.length, 1);
  assert.match(shell, /<span className="fg-topstat">\s*<FleetTrafficMeter \/>/);
  assert.match(shell, /fg-topstat[\s\S]*?<MachineNotifications/);
  assert.doesNotMatch(shell, /ForgeCrumb|fg-crumb|ForgeMobileBack|fg-mobile-crumb/);
  assert.doesNotMatch(styles, /\.fg-crumb|--shell-crumb-h/);
  assert.doesNotMatch(traffic, /30 秒均值/);
  assert.match(traffic, /最近 30 秒窗口均值/);
});

test('流量条由机队 SSE 驱动秒级读数，历史兜底只读取每台机器的最新窗口', () => {
  assert.match(traffic, /FLEET_TRAFFIC_WINDOWS = 1/);
  assert.match(traffic, /FLEET_TRAFFIC_REFETCH_MS = 30_000/);
  assert.match(traffic, /fetchNodeNicListWindows\(FLEET_TRAFFIC_WINDOWS\)/);
  assert.match(source, /new EventSource\('\/realtime\/nodes\/events'/);
  assert.match(source, /useMetricValue\(\s*nodes,\s*'fleet'/);
  assert.match(source, /useNow\(\)/);
  assert.doesNotMatch(traffic, /setInterval\(/);
  assert.match(traffic, /rx_bytes_per_sec \* 8/);
  assert.match(traffic, /tx_bytes_per_sec \* 8/);
  assert.doesNotMatch(traffic, /峰值|近 1 小时/);
});
