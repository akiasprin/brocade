import type { UnlockResult } from '../api';
import { dialFailureReason } from './unlock-dial';
export type Tone = 'ok' | 'part' | 'no' | 'info' | 'unknown' | 'wait';
type VerdictService = UnlockResult['service'] | 'youtube';
export const UNLOCK_REASONS = {
  web_available: '服务网页可访问',
  region_unsupported: '服务返回地区限制',
  access_denied: '服务拒绝访问',
  originals_only: '测试片目不可访问，可能仅支持自制剧',
  web_only: '网页端通过，App 端访问受限',
  app_only: 'App 端通过，网页端访问受限',
  timeout: '请求超时',
  network_error: '网络连接失败',
  no_ipv6_dns: '无 IPv6 地址解析',
  no_ipv4_dns: '无 IPv4 地址解析',
  no_ipv6_egress: '无 IPv6 出站',
  unrecognized_response: '未识别服务响应，可能需要验证或服务页面已变化',
} as const;

export function Glyph({ tone }: { tone: Tone }) {
  return (
    <svg className="unlock-glyph" data-tone={tone} viewBox="0 0 16 16" width="15" height="15" aria-hidden="true">
      {tone === 'ok' && (
        <>
          <circle cx="8" cy="8" r="7.2" className="fill" />
          <path d="M4.9 8.3 7.1 10.4 11.2 5.9" className="mark" />
        </>
      )}
      {tone === 'no' && (
        <>
          <circle cx="8" cy="8" r="7.2" className="fill" />
          <path d="M5.7 5.7 10.3 10.3 M10.3 5.7 5.7 10.3" className="mark" />
        </>
      )}
      {tone === 'part' && (
        <>
          <circle cx="8" cy="8" r="6.4" className="ring" />
          <path d="M8 1.6A6.4 6.4 0 0 0 8 14.4Z" className="fill" />
        </>
      )}
      {tone === 'unknown' && (
        <>
          <circle cx="8" cy="8" r="6.4" className="ring" />
          <path d="M5.4 8h5.2" className="dash" />
        </>
      )}
      {tone === 'info' && (
        <>
          <circle cx="8" cy="8" r="6.4" className="ring" />
          <circle cx="8" cy="4.8" r="0.75" className="fill" />
          <path d="M8 7.3v3.5" className="dash" />
        </>
      )}
      {tone === 'wait' && <circle cx="8" cy="8" r="6" className="spin" />}
    </svg>
  );
}

export interface Verdict {
  tone: Tone;
  /** Short verdict shown everywhere: 可用 / 仅自制剧 / 地区不支持 / 访问受限 / 检测失败. */
  label: string;
  /** A qualifier that changes how the verdict is used, shown next to it (client DNS). */
  note: string | null;
  /** Why the service answered that way; the failure reason is shown, the rest is a tooltip. */
  reason: string;
}

const FAILURE: Record<string, string> = {
  timeout: '请求超时',
  network_error: '网络连接失败',
  unrecognized_response: '未识别服务响应',
};

export function verdictOf(
  result: Pick<UnlockResult, 'status' | 'code' | 'client_dns'> &
    Partial<Pick<UnlockResult, 'service' | 'connection_observations' | 'connection_requests'>>,
  service: VerdictService | undefined = result.service,
): Verdict {
  if ((service === 'youtube' || service === 'youtube_premium') && result.status === 'unsupported_region') {
    return {
      tone: 'info',
      label: '提示 · 地区不支持',
      note: null,
      reason:
        service === 'youtube_premium'
          ? 'Premium 订阅地区限制；普通 YouTube 和视频 CDN 独立显示检测结果'
          : 'YouTube 返回地区限制；Premium 和视频 CDN 独立显示检测结果',
    };
  }
  if (result.service === 'crunchyroll' && result.status === 'available') {
    return {
      tone: 'ok',
      label: '可用',
      note: null,
      reason: '匿名服务接口可访问；地区来自服务响应，不保证具体片目版权',
    };
  }
  if (result.client_dns)
    return {
      tone: 'ok',
      label: '可用',
      note: '客户端 DNS',
      reason: '将域名交给链路时请求失败，入口解析 IPv4 后经同一链路访问成功',
    };
  switch (result.status) {
    case 'available':
      return { tone: 'ok', label: '可用', note: null, reason: '服务网页可访问' };
    case 'partial':
      return result.code === 'originals_only'
        ? { tone: 'part', label: '仅自制剧', note: null, reason: '测试片目不可访问，可能仅支持自制剧' }
        : result.code === 'web_only'
          ? { tone: 'part', label: '仅网页可用', note: null, reason: '网页端通过，App 端访问受限' }
          : { tone: 'part', label: '仅 App 可用', note: null, reason: 'App 端通过，网页端访问受限' };
    case 'unsupported_region':
      return { tone: 'no', label: '地区不支持', note: null, reason: '服务返回地区限制' };
    case 'restricted':
      return { tone: 'no', label: '访问受限', note: null, reason: '服务拒绝访问或要求验证' };
    case 'skipped':
      return { tone: 'unknown', label: '已跳过', note: null, reason: UNLOCK_REASONS[result.code] };
    case 'failed':
      return {
        tone: 'unknown',
        label: '检测失败',
        note: null,
        reason:
          (['network_error', 'timeout'].includes(result.code)
            ? dialFailureReason(
                result,
                service === 'youtube' ? 'webpage' : service === 'youtube_premium' ? 'premium' : 'service',
              )
            : null) ??
          FAILURE[result.code] ??
          '检测失败',
      };
  }
}

export function UnlockStatusMark({
  result,
  pending = false,
  service,
}: {
  result?: Pick<UnlockResult, 'status' | 'code' | 'client_dns'>;
  pending?: boolean;
  service?: VerdictService;
}) {
  const verdict = result
    ? verdictOf(result, service)
    : {
        tone: pending ? ('wait' as const) : ('unknown' as const),
        label: pending ? '等待结果' : '未提供',
        reason: '',
        note: null,
      };
  return (
    <span className="unlock-status" data-tone={verdict.tone} title={verdict.reason}>
      <Glyph tone={verdict.tone} />
      <span>
        {verdict.label}
        {verdict.note ? `（${verdict.note}）` : ''}
      </span>
      {(result?.status === 'failed' || result?.status === 'skipped') && <small>{verdict.reason}</small>}
    </span>
  );
}
