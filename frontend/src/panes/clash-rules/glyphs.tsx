/* 分组与去向的图形：彩色、透明底。自绘的几枚沿用生产图标的 16 网格与 1.5px 描边；品牌标识用生产 SiteIcon。 */
import type { CSSProperties, ReactNode } from 'react';
import { Icon } from '../../ui/icons';
import { RegionFlag } from '../../ui/region-flag';
import { SiteIcon } from '../../ui/site-icon';
import type { GlyphId, RuleKind, Target } from '../../clash-rules';
import { useRouting } from './context';

const LINE: Partial<Record<GlyphId, ReactNode>> = {
  rocket: (
    <>
      <path d="M8 1.9c2 1.5 3 3.7 3 6.2v2.7H5V8.1c0-2.5 1-4.7 3-6.2Z" />
      <path d="M5 8.9 3.2 10.5v2.3L5 11.6M11 8.9l1.8 1.6v2.3L11 11.6" />
      <circle cx="8" cy="6.6" r="1.15" />
      <path d="M6.9 13.1c.2.7.6 1.2 1.1 1.5.5-.3.9-.8 1.1-1.5" />
    </>
  ),
  fish: (
    <>
      <path d="M1.9 8c1.5-2.2 3.4-3.4 5.6-3.4 1.8 0 3.4.8 4.8 2.3l2.1-1.7v5.6l-2.1-1.7c-1.4 1.5-3 2.3-4.8 2.3-2.2 0-4.1-1.2-5.6-3.4Z" />
      <circle cx="4.9" cy="7.3" r=".6" fill="currentColor" stroke="none" />
    </>
  ),
  house: (
    <>
      <path d="M2.4 7.6 8 2.9l5.6 4.7" />
      <path d="M3.9 6.5v6.6h8.2V6.5" />
      <path d="M6.8 13.1V9.9h2.4v3.2" />
    </>
  ),
  globe: (
    <>
      <circle cx="8" cy="8" r="5.9" />
      <ellipse cx="8" cy="8" rx="2.5" ry="5.9" />
      <path d="M2.3 6.1h11.4M2.3 9.9h11.4" />
    </>
  ),
  direct: <path d="M2.4 8h10.6M9.4 4.4 13 8l-3.6 3.6" />,
};

const FILL: Partial<Record<GlyphId, ReactNode>> = {
  sparkle: (
    <>
      <path d="M6.6 2.6c.42 2.95 1.55 4.08 4.5 4.5-2.95.42-4.08 1.55-4.5 4.5-.42-2.95-1.55-4.08-4.5-4.5 2.95-.42 4.08-1.55 4.5-4.5Z" />
      <path d="M12.3 1.4c.17 1.12.7 1.65 1.8 1.8-1.1.17-1.63.7-1.8 1.8-.17-1.1-.7-1.63-1.8-1.8 1.1-.15 1.63-.68 1.8-1.8Z" />
      <path d="M12 10.3c.14.92.57 1.35 1.5 1.5-.93.14-1.36.57-1.5 1.5-.14-.93-.57-1.36-1.5-1.5.93-.15 1.36-.58 1.5-1.5Z" />
    </>
  ),
  microsoft: (
    <>
      <rect x="2" y="2" width="5.6" height="5.6" fill="#f25022" />
      <rect x="8.4" y="2" width="5.6" height="5.6" fill="#7fba00" />
      <rect x="2" y="8.4" width="5.6" height="5.6" fill="#00a4ef" />
      <rect x="8.4" y="8.4" width="5.6" height="5.6" fill="#ffb900" />
    </>
  ),
};

const BRAND: Partial<Record<GlyphId, string>> = {
  youtube: 'youtube',
  google: 'google',
  telegram: 'telegram',
  github: 'github',
  apple: 'apple',
  netflix: 'netflix',
  disney: 'disney',
  spotify: 'spotify',
  tiktok: 'tiktok',
  steam: 'steam',
  discord: 'discord',
  twitter: 'twitter',
  instagram: 'instagram',
  reddit: 'reddit',
  twitch: 'twitch',
  whatsapp: 'whatsapp',
};

export function Glyph({ of, size = 18 }: { of: GlyphId; size?: number }) {
  let body: ReactNode;
  if (of === 'cn') body = <RegionFlag code="CN" square />;
  else if (of === 'ban') body = <Icon of="ban" size={size} />;
  else if (of === 'bolt') body = <Icon of="bolt" size={size} />;
  else if (BRAND[of]) body = <SiteIcon siteId={BRAND[of]} />;
  else if (FILL[of])
    body = (
      <svg width={size} height={size} viewBox="0 0 16 16" fill="currentColor" aria-hidden="true">
        {FILL[of]}
      </svg>
    );
  else
    body = (
      <svg
        width={size}
        height={size}
        viewBox="0 0 16 16"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.5"
        strokeLinecap="round"
        strokeLinejoin="round"
        aria-hidden="true"
      >
        {LINE[of]}
      </svg>
    );
  return (
    <span className="clash-rule-glyph" data-glyph={of} style={{ '--clash-rule-glyph': `${size}px` } as CSSProperties}>
      {body}
    </span>
  );
}

export const targetGlyph = (target: Target): GlyphId | null =>
  target === 'select'
    ? 'rocket'
    : target === 'auto'
      ? 'bolt'
      : target === 'direct'
        ? 'direct'
        : target === 'reject'
          ? 'ban'
          : null;

/** 去向的图形：线路用旗，其余用分组图形。 */
export function TargetMark({ target, size = 16 }: { target: Target; size?: number }) {
  const { LINES } = useRouting();
  const line = LINES.find(line => line.id === target);
  if (line?.region)
    return (
      <span className="clash-rule-glyph clash-rule-flag" style={{ '--clash-rule-glyph': `${size}px` } as CSSProperties}>
        <RegionFlag code={line.region} />
      </span>
    );
  return <Glyph of={targetGlyph(target) ?? 'rocket'} size={size} />;
}

/** 自定义规则的类型小标：域名 / 关键词 / IP 段 / 进程。 */
export function KindMark({ kind }: { kind: RuleKind }) {
  const label = { domain: '域名', keyword: '关键词', ip: 'IP 段', process: '进程' }[kind];
  return (
    <span className="clash-rule-kind" data-kind={kind}>
      {label}
    </span>
  );
}
