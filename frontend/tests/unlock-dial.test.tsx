import { expect, it } from 'vitest';
import { verdictOf } from '../src/panes/unlock-status';
import { dialFailureReason } from '../src/panes/unlock-dial';
import { youtubeResult, youtubeLine } from '../src/panes/youtube-unlock';
import { youtubeSplitResult } from './fixtures/youtube-unlock-data';
import { withDialFailure } from './fixtures/unlock-dial-data';
import { summarizeChainUnlock } from '../src/panes/chain-unlock-summary';
import type { ChainUnlockProbeItem, NodeUnlockProbe, UnlockResult } from '../src/api';

const original: UnlockResult = { service: 'gemini', status: 'available', code: 'web_available', region: 'TW' };
it.each([
  ['network_unreachable', 'IPv6 出站网络不可达'],
  ['host_unreachable', 'IPv6 目标不可达'],
  ['connection_refused', 'IPv6 连接被拒绝'],
  ['source_unavailable', 'IPv6 出站绑定地址不可用'],
  ['source_family_mismatch', 'IPv6 出站绑定地址族不匹配'],
  ['permission_denied', 'IPv6 出站连接被系统拒绝'],
  ['connect_timeout', 'IPv6 出站连接超时'],
  ['connection_reset', 'IPv6 出站连接被重置'],
])('shows actual exit %s independently from DNS and regional restrictions', (error, reason) => {
  const result = withDialFailure(original, 'unreachable');
  for (const record of result.connection_observations!) record.error = error;
  expect(verdictOf(result)).toMatchObject({ label: '检测失败', reason });
  for (const record of result.connection_observations!) record.family = 'v4';
  expect(verdictOf(result).reason).toBe(reason.replace('IPv6', 'IPv4'));
});

it('does not infer a failure from incomplete, duplicate, successful or legacy observations', () => {
  for (const change of [
    (r: UnlockResult) => { r.connection_requests!.service = 3; },
    (r: UnlockResult) => { r.connection_observations![0].complete = false; },
    (r: UnlockResult) => { r.connection_observations![1].id = r.connection_observations![0].id; },
    (r: UnlockResult) => { r.connection_observations![0].connected = true; },
    (r: UnlockResult) => { r.connection_observations![0].error = 'connect_error'; },
  ]) {
    const result = withDialFailure(original, 'unreachable'); change(result);
    expect(dialFailureReason(result, 'service')).toBeNull();
    expect(verdictOf(result).reason).toBe('网络连接失败');
  }
});

it('keeps YouTube webpage, Premium and CDN dial explanations separate', () => {
  const result = withDialFailure(youtubeSplitResult, 'unreachable');
  for (const record of result.connection_observations!) record.error = record.part === 'premium' ? 'connection_refused' : record.part === 'cdn' ? 'connect_timeout' : 'network_unreachable';
  expect(verdictOf(youtubeResult(result, 'webpage')!, 'youtube').reason).toBe('IPv6 出站网络不可达');
  expect(verdictOf(youtubeResult(result, 'premium')!, 'youtube_premium').reason).toBe('IPv6 连接被拒绝');
  expect(youtubeLine(result.youtube!, result)).toBe('视频 CDN IPv6 出站连接超时');
  const mixed = withDialFailure(youtubeSplitResult, 'mixed');
  expect(verdictOf(youtubeResult(mixed, 'webpage')!, 'youtube')).toMatchObject({ label: '已跳过', reason: '无 IPv6 出站' });
  expect(verdictOf(youtubeResult(mixed, 'premium')!, 'youtube_premium').tone).toBe('ok');
});

it('uses IPv4 for proven absent IPv6 egress and preserves an actual IPv6 route failure', () => {
  const job: NodeUnlockProbe = { id: 'job', node_id: 'entry-v4', family: 'v4', state: 'complete', created_at: 1, completed_at: 2, results: [original], previous_results: [] };
  for (const [scenario, tone] of [['no-egress', 'ok'], ['unreachable', 'part']] as const) {
    const v6 = { ...job, family: 'v6' as const, results: [withDialFailure(original, scenario)] };
    const item: ChainUnlockProbeItem = { app_id: 'app', chain_id: 'chain', capabilities: { family_mode: 'split', ipv6_available: false, ipv6_reason: null }, probes: { v4: job, v6, auto: null } };
    const cell = summarizeChainUnlock(item).cells.find(cell => cell.service === 'gemini')!;
    expect(cell.verdict.tone).toBe(tone);
    expect(cell.title).toContain(scenario === 'no-egress' ? '无 IPv6 出站' : 'IPv6 出站网络不可达');
  }
});
