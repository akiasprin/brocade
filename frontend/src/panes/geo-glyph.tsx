import type { ReactNode } from 'react';
import type { GeodataKind } from '../api/geodata';
import { RegionFlag } from '../ui/region-flag';
import { hasSiteBrand, SiteIcon } from '../ui/site-icon';
import { isGeoSiteCollection } from './geo-menu-model';

const paths = {
  media: (
    <>
      <rect x="3" y="4" width="14" height="12" rx="2" />
      <path d="m8.5 7.5 4.5 2.5-4.5 2.5Z" />
    </>
  ),
  ads: (
    <>
      <path d="m3 8 10-4v12L3 12Z" />
      <path d="M3 8v4M5.5 12.5l1 3.5h3M16 7a4 4 0 0 1 0 6" />
    </>
  ),
  dev: <path d="m6.5 5-4 5 4 5M13.5 5l4 5-4 5M11.5 3.5l-3 13" />,
  games: (
    <>
      <path d="M6.5 6h7c2 0 3.5 3 4 6 .4 2.5-1.5 3.5-3 2l-2-2h-5l-2 2c-1.5 1.5-3.4.5-3-2 .5-3 2-6 4-6Z" />
      <path d="M5 8.5v3M3.5 10h3M13.5 9.5h.01M15.5 11.5h.01" />
    </>
  ),
  finance: <path d="m2.5 7 7.5-4 7.5 4ZM4 8v6M8 8v6M12 8v6M16 8v6M2.5 16h15" />,
  communication: (
    <>
      <path d="M3 4h14v9H9l-4 3v-3H3Z" />
      <path d="M6.5 8.5h7M6.5 6h4" />
    </>
  ),
  ecommerce: (
    <>
      <path d="M3 4h2l1.5 8h8l2-6H6" />
      <circle cx="8" cy="15.5" r="1" />
      <circle cx="14" cy="15.5" r="1" />
    </>
  ),
  tld: (
    <>
      <rect x="3" y="4" width="14" height="12" rx="2" />
      <path d="M3 7h14M6 5.5h.01M8 5.5h.01" />
      <path d="m8 10-1.5 1.5L8 13M12 10l1.5 1.5L12 13" />
    </>
  ),
  category: (
    <>
      <rect x="3" y="3" width="5" height="5" rx="1" />
      <rect x="12" y="3" width="5" height="5" rx="1" />
      <rect x="3" y="12" width="5" height="5" rx="1" />
      <rect x="12" y="12" width="5" height="5" rx="1" />
    </>
  ),
  private: (
    <>
      <rect x="4" y="8" width="12" height="9" rx="2" />
      <path d="M7 8V5.5a3 3 0 0 1 6 0V8M10 11v3" />
    </>
  ),
} satisfies Record<string, ReactNode>;

type CollectionGlyph = keyof typeof paths | 'ai' | 'ip';

function collectionFor(code: string): CollectionGlyph {
  if (code === 'private') return 'private';
  if (code.startsWith('geolocation-') || code.startsWith('tld-') || code === 'cn') return 'tld';
  if (/^category-(ai|ml|llm)/.test(code)) return 'ai';
  if (/^category-(entertainment|media|acg|emby|music|video|news|novel|olympiad)/.test(code)) return 'media';
  if (/^category-(ads|telemetry|consent)/.test(code)) return 'ads';
  if (
    /^category-(dev|container|web|browser|cdn|ddns|doh|httpdns|ipfs|network|stun|ntp|urlshortner|proxy|anticensorship|vpnservices)/.test(
      code,
    )
  )
    return 'dev';
  if (/^category-games?/.test(code)) return 'games';
  if (/^category-(finance|bank|bourse|insurance|payment|securities|cryptocurrency)/.test(code)) return 'finance';
  if (/^category-(communication|collaborate|forums|social|voip)/.test(code)) return 'communication';
  if (/^category-(ecommerce|shopping|retail|food|travel|flight|logistics)/.test(code)) return 'ecommerce';
  return 'category';
}

function CollectionIcon({ glyph, theme }: { glyph: CollectionGlyph; theme: boolean }) {
  return (
    <span className={`geo-collection-icon${theme ? ' theme' : ''}`} data-geo-glyph={glyph} aria-hidden="true">
      {glyph === 'ai' ? (
        <span className="geo-ai-text">AI</span>
      ) : glyph === 'ip' ? (
        <span className="geo-ip-text">IP</span>
      ) : (
        <svg
          viewBox="0 0 20 20"
          fill="none"
          stroke="currentColor"
          strokeWidth="1.35"
          strokeLinecap="round"
          strokeLinejoin="round"
          focusable="false"
        >
          {paths[glyph]}
        </svg>
      )}
    </span>
  );
}

function initialsFor(code: string) {
  const cleaned = code.replace(/[^a-z0-9]/gi, '');
  return (cleaned || code).slice(0, 3).toUpperCase();
}

function InitialsIcon({ code }: { code: string }) {
  return (
    <span
      className="geo-collection-icon initials"
      data-geo-glyph="initials"
      data-geo-initials={initialsFor(code)}
      aria-hidden="true"
    />
  );
}

export function GeoGlyph({ kind, code }: { kind: GeodataKind; code: string }) {
  const base = kind === 'geoip' ? code.replace(/^!+/, '') : code;
  if (kind === 'geoip') {
    if (/^[a-z]{2}$/i.test(base)) return <RegionFlag code={base} />;
    return <CollectionIcon glyph={base === 'private' ? 'private' : 'ip'} theme={true} />;
  }
  if (isGeoSiteCollection(base)) {
    const glyph = collectionFor(base);
    return <CollectionIcon glyph={glyph} theme={glyph !== 'tld'} />;
  }
  if (hasSiteBrand(base)) return <SiteIcon siteId={base} />;
  return <InitialsIcon code={base} />;
}
