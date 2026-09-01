import type { BrandingSettings } from '../api';

/** Custom raster mark when configured; otherwise the built-in woven Brocade mark. */
export function BrandIcon({ branding, className = 'fg-logo' }: { branding: BrandingSettings; className?: string }) {
  if (branding.icon_data_url) {
    return <img className={className} src={branding.icon_data_url} alt="" />;
  }
  return (
    <svg className={className} viewBox="0 0 24 24" aria-hidden="true" fill="none">
      <g stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" strokeLinejoin="round">
        <path d="M8.5,5V19" strokeDasharray="1.7 3.6 8.7" />
        <path d="M15.5,5V19" strokeDasharray="8.7 3.6 1.7" />
        <path d="M5,8.5H19" strokeDasharray="8.7 3.6 1.7" />
        <path d="M5,15.5H19" strokeDasharray="1.7 3.6 8.7" />
      </g>
    </svg>
  );
}
