import type { RealityFallbackLimits, RealityFallbackRateLimit } from './api';

export interface FallbackRateDraft {
  afterBytes: string;
  bytesPerSec: string;
  burstBytesPerSec: string;
}

export interface FallbackLimitDraft {
  mode: RealityFallbackLimits['mode'];
  upload: FallbackRateDraft;
  download: FallbackRateDraft;
}

const BALANCED = {
  upload: { after_bytes: 1_048_576, bytes_per_sec: 262_144, burst_bytes_per_sec: 524_288 },
  download: { after_bytes: 8_388_608, bytes_per_sec: 1_048_576, burst_bytes_per_sec: 2_097_152 },
} satisfies Record<'upload' | 'download', RealityFallbackRateLimit>;

const STRICT = {
  upload: { after_bytes: 262_144, bytes_per_sec: 65_536, burst_bytes_per_sec: 131_072 },
  download: { after_bytes: 1_048_576, bytes_per_sec: 262_144, burst_bytes_per_sec: 524_288 },
} satisfies Record<'upload' | 'download', RealityFallbackRateLimit>;

const draftRate = (rate: RealityFallbackRateLimit): FallbackRateDraft => ({
  afterBytes: String(rate.after_bytes),
  bytesPerSec: String(rate.bytes_per_sec),
  burstBytesPerSec: String(rate.burst_bytes_per_sec),
});

export function fallbackLimitDraft(policy: RealityFallbackLimits): FallbackLimitDraft {
  const rates = policy.mode === 'custom' ? policy : policy.mode === 'strict' ? STRICT : BALANCED;
  return {
    mode: policy.mode,
    upload: draftRate(rates.upload),
    download: draftRate(rates.download),
  };
}

const whole = (raw: string) => {
  if (!/^\d+$/.test(raw)) return null;
  const value = Number(raw);
  return Number.isSafeInteger(value) ? value : null;
};

const parsedRate = (draft: FallbackRateDraft): RealityFallbackRateLimit | null => {
  const after_bytes = whole(draft.afterBytes);
  const bytes_per_sec = whole(draft.bytesPerSec);
  const burst_bytes_per_sec = whole(draft.burstBytesPerSec);
  if (
    after_bytes === null ||
    bytes_per_sec === null ||
    burst_bytes_per_sec === null ||
    bytes_per_sec === 0 ||
    burst_bytes_per_sec < bytes_per_sec
  ) {
    return null;
  }
  return { after_bytes, bytes_per_sec, burst_bytes_per_sec };
};

export function fallbackLimitsFromDraft(draft: FallbackLimitDraft): RealityFallbackLimits | null {
  if (draft.mode !== 'custom') return { mode: draft.mode };
  const upload = parsedRate(draft.upload);
  const download = parsedRate(draft.download);
  return upload && download ? { mode: 'custom', upload, download } : null;
}
