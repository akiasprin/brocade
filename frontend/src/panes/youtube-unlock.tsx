import { useRef, useState } from 'react';
import {
  YOUTUBE_UNLOCK_DETAILS_PROTOCOL_VERSION,
  type UnlockResult,
  type YoutubeDiagnostics,
  type YoutubePageResult,
} from '../api';
import { DialogClose, DialogLayer } from '../ui/dialog';
import { Icon } from '../ui/icons';
import { UnlockStatusMark, UNLOCK_REASONS } from './unlock-status';
import { dialFailureReason } from './unlock-dial';

const regionNames = new Intl.DisplayNames(['zh-Hans'], { type: 'region', style: 'short' });
const regionOf = (page: YoutubePageResult | undefined) => page?.privacy_region ?? page?.topbar_region;
const placeOf = (country: string | null | undefined) =>
  country && /^[A-Z]{2}$/.test(country) ? (regionNames.of(country) ?? country) : country;
const cdnReason = (code: UnlockResult['code']) =>
  code === 'unrecognized_response' ? '映射响应未识别' : UNLOCK_REASONS[code];

/** Older reports only measured Premium; they cannot establish ordinary YouTube availability. */
export function youtubeResult(result: UnlockResult | undefined, kind: 'webpage' | 'premium'): UnlockResult | null {
  const page = result?.youtube?.[kind];
  if (!result || !page) return kind === 'premium' ? (result ?? null) : null;
  return { ...result, status: page.status, code: page.code, region: regionOf(page) ?? null, client_dns: false };
}

export function youtubeLine(detail: YoutubeDiagnostics | null, result?: UnlockResult): string | null {
  if (!detail) return null;
  const parts: string[] = [];
  const cdn = detail.cdn;
  if (cdn.node) parts.push(`视频 CDN ${cdn.airport ?? cdn.country ?? cdn.node}`);
  else if (cdn.error) parts.push(`视频 CDN ${dialFailureReason(result, 'cdn') ?? cdnReason(cdn.error)}`);
  return parts.join(' · ') || null;
}

export function YoutubeMore({ result }: { result?: UnlockResult }) {
  const anchor = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  if (!result) return null;
  return (
    <>
      <button
        ref={anchor}
        type="button"
        className="unlock-info"
        aria-label="YouTube 分项"
        aria-expanded={open}
        onClick={() => setOpen(value => !value)}
      >
        <Icon of="info" size={14} />
      </button>
      {open && (
        <DialogLayer mode="popover" anchor={anchor} align="end" label="YouTube 分项" onClose={() => setOpen(false)}>
          <section className="dialog-surface unlock-yt" aria-label="YouTube 检测详情">
            <DialogClose className="sub-dialog-close unlock-yt-close" aria-label="关闭 YouTube 分项">
              <Icon of="close" size={14} />
            </DialogClose>
            <YoutubeUnlockDetails result={result} pending={false} />
          </section>
        </DialogLayer>
      )}
    </>
  );
}

function PageFact({
  name,
  service,
  page,
  fallback,
  dns,
  source,
}: {
  name: string;
  service: 'youtube' | 'youtube_premium';
  page?: YoutubePageResult;
  fallback?: UnlockResult;
  dns?: string | null;
  source?: UnlockResult;
}) {
  return (
    <div>
      <dt>{name}</dt>
      <dd>
        <UnlockStatusMark result={page ? { ...source, ...page } : fallback} service={service} />
        {page && (
          <span className="unlock-yt-dim">
            HTTP {page.http_status ?? '—'} · {regionOf(page) ?? '—'}
          </span>
        )}
        {dns && <small>{dns}</small>}
      </dd>
    </div>
  );
}

export function YoutubeUnlockDetails({ result, pending }: { result?: UnlockResult; pending: boolean }) {
  const detail = result?.youtube;
  const cdn = detail?.cdn;
  const pageRegion = detail?.webpage.page_region;
  const privacyRegion = detail?.webpage.privacy_region;
  const dns = (part: 'webpage' | 'premium' | 'cdn') => {
    const evidence = result?.dns_parts?.find(item => item.part === part);
    if (!evidence || (evidence.kind === 'unknown' && !evidence.paths.length)) return null;
    const label =
      evidence.kind === 'direct'
        ? evidence.paths.length
          ? '常规回退'
          : '常规解析'
        : evidence.kind === 'unknown'
          ? '路径已记录，常规族待确认'
          : ({ fallback: '回退', partial: '部分回退' } as const)[evidence.kind];
    return `${label}${evidence.exits.length ? ` · ${evidence.exits.join('、')}` : ''}`;
  };
  return (
    <div aria-busy={pending}>
      <h3>YouTube</h3>
      <dl>
        <PageFact name="YouTube 网页" service="youtube" page={detail?.webpage} dns={dns('webpage')} source={result} />
        <PageFact
          name="YouTube Premium"
          service="youtube_premium"
          page={detail?.premium}
          fallback={result}
          dns={dns('premium')}
          source={result}
        />
        <div>
          <dt>地区</dt>
          <dd>
            {detail ? (regionOf(detail.webpage) ?? '—') : (result?.region ?? '—')}
            <small>
              {detail
                ? privacyRegion
                  ? '来自隐私元数据'
                  : detail.premium.topbar_region
                    ? '来自页面 countryCode'
                    : '未返回明确地区'
                : '旧版采样地区'}
              {detail && regionOf(detail.webpage) !== regionOf(detail.premium)
                ? ` · 网页 ${regionOf(detail.webpage) ?? '—'}`
                : ''}
            </small>
          </dd>
        </div>
        <div>
          <dt>视频 CDN</dt>
          <dd>
            {cdn?.node ? placeOf(cdn.country) || cdn.airport || '位置未识别' : '—'}
            {cdn?.airport && cdn.country ? ` · ${cdn.airport}` : ''}
            {cdn?.node && <small className="youtube-unlock-cdn-node">{cdn.node}</small>}
            {cdn?.error && <small>{dialFailureReason(result, 'cdn') ?? cdnReason(cdn.error)}</small>}
            {cdn && cdn.elapsed_ms > 0 && <small>请求 {cdn.elapsed_ms} ms · 含完整链路</small>}
            <small>Google Video 映射参考</small>
            {dns('cdn') && <small>{dns('cdn')}</small>}
          </dd>
        </div>
        <div>
          <dt>页面配置地区</dt>
          <dd>
            {pageRegion ?? '—'}
            <small>
              GL · 仅页面配置
              {detail && detail.premium.page_region !== pageRegion
                ? ` · Premium ${detail.premium.page_region ?? '—'}`
                : ''}
            </small>
          </dd>
        </div>
        <div>
          <dt>隐私元数据 · 地区</dt>
          <dd>
            {privacyRegion ?? '—'}
            <small>
              仅取新响应地区码
              {detail && detail.premium.privacy_region !== privacyRegion
                ? ` · Premium ${detail.premium.privacy_region ?? '—'}`
                : ''}
            </small>
          </dd>
        </div>
      </dl>
      {result && !detail && (
        <p className="sub">
          当前结果未包含分项详情，请升级 Agent 至 v{YOUTUBE_UNLOCK_DETAILS_PROTOCOL_VERSION} 后重新检测。
        </p>
      )}
    </div>
  );
}
