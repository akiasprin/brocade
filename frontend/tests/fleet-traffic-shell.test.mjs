import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const shell = readFileSync(new URL('../src/forge/shell.tsx', import.meta.url), 'utf8');
const traffic = readFileSync(new URL('../src/ui/fleet-traffic.tsx', import.meta.url), 'utf8');
const styles = readFileSync(new URL('../src/styles.css', import.meta.url), 'utf8');

test('桌面面包屑左侧保留页面标题、右侧显示流量，手机端不显示该区域', () => {
  assert.equal(shell.match(/<FleetTrafficMeter \/>/g)?.length, 1);
  assert.match(
    shell,
    /function ForgeCrumb[\s\S]*?segs\.length === 0 \? \(\s*<span className="cur">\{LABEL\[nav\]\}<\/span>/,
  );
  assert.match(shell, /<span className="fg-crumb-right">\s*<FleetTrafficMeter \/>/);
  assert.match(shell, /\{!narrow && \(\s*<div className="fg-crumb">/);
  assert.doesNotMatch(shell, /ForgeMobileBack|fg-mobile-crumb|ft-mobile-crumb/);
  assert.doesNotMatch(styles, /\.fg-mobile-crumb|\.ft-mobile-crumb/);
});

test('流量条由机队 SSE 驱动秒级读数，历史兜底只读取每台机器的最新窗口', () => {
  assert.match(traffic, /FLEET_TRAFFIC_WINDOWS = 1/);
  assert.match(traffic, /FLEET_TRAFFIC_REFETCH_MS = 30_000/);
  assert.match(traffic, /fetchNodeNicListWindows\(FLEET_TRAFFIC_WINDOWS\)/);
  assert.match(traffic, /new EventSource\('\/realtime\/nodes\/events'/);
  assert.match(traffic, /FLEET_REALTIME_DISPLAY_MS = 1_000/);
  assert.match(traffic, /setInterval\([\s\S]*?new Map\(pendingNodes\)[\s\S]*?FLEET_REALTIME_DISPLAY_MS/);
  assert.match(traffic, /rx_bytes_per_sec \* 8/);
  assert.match(traffic, /tx_bytes_per_sec \* 8/);
  assert.doesNotMatch(traffic, /峰值|近 1 小时/);
});
