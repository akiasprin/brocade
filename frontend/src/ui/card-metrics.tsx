import { useId } from 'react';
import { sparklinePaths } from './sparkline';

/** Compact resource rings use the same geometry as quota rings. */
export function CardRing({ share, tone, size = 13 }: { share: number | null; tone: string; size?: number }) {
  const percent = Math.min(100, Math.max(0, (share ?? 0) * 100));
  return (
    <svg
      className={`user-quota-ring node-card-ring ${tone}`}
      width={size}
      height={size}
      viewBox="0 0 12 12"
      aria-hidden="true"
    >
      <circle className="track" cx="6" cy="6" r="4.5" />
      {percent > 0 && (
        <circle
          className="value"
          cx="6"
          cy="6"
          r="4.5"
          pathLength="100"
          strokeDasharray={`${percent} ${100 - percent}`}
          transform="rotate(-90 6 6)"
        />
      )}
    </svg>
  );
}

/** Null values break history; freshness controls only the endpoint. */
export function CardComet({
  values,
  tone,
  current,
  floor = 5_000_000,
}: {
  values: readonly (number | null)[];
  tone: string;
  current: boolean;
  floor?: number;
}) {
  const gradient = `card-${useId().replace(/[^a-zA-Z0-9_-]/g, '')}`;
  if (values.filter(value => value !== null).length < 2)
    return <span className="node-card-comet empty" aria-hidden="true" />;
  const height = 46;
  const peak = Math.max(floor, ...values.map(value => value ?? 0)) * 1.08;
  const paths = sparklinePaths(values, peak, height);
  const span = values.length - 1;
  const last = values.at(-1);
  const top =
    last == null ? null : ((height - 0.5 - Math.min(1, Math.max(0, last) / peak) * (height - 3)) / height) * 100;
  return (
    <span className={`node-card-comet tone-${tone}`} aria-hidden="true">
      <svg viewBox={`0 0 ${span} ${height}`} preserveAspectRatio="none">
        <defs>
          <linearGradient id={`${gradient}-line`} gradientUnits="userSpaceOnUse" x1="0" x2={span} y1="0" y2="0">
            <stop offset="0" stopColor="currentColor" stopOpacity="0.45" />
            <stop offset="0.6" stopColor="currentColor" stopOpacity="0.75" />
            <stop offset="1" stopColor="currentColor" stopOpacity="1" />
          </linearGradient>
          <linearGradient id={`${gradient}-fill`} gradientUnits="userSpaceOnUse" x1="0" x2={span} y1="0" y2="0">
            <stop offset="0" stopColor="currentColor" stopOpacity="0.03" />
            <stop offset="1" stopColor="currentColor" stopOpacity="0.1" />
          </linearGradient>
        </defs>
        {paths.areas.map((path, index) => (
          <path key={`area-${index}`} d={path} fill={`url(#${gradient}-fill)`} />
        ))}
        {paths.curves.map((path, index) => (
          <path key={`line-${index}`} className="trail" d={path} stroke={`url(#${gradient}-line)`} />
        ))}
      </svg>
      {current && top !== null && <i className="node-card-comet-now" style={{ top: `${top}%` }} />}
    </span>
  );
}
