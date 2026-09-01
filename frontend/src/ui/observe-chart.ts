/** Shared visual contract for machine-observation charts.
 *
 * CSS owns the real theme colors so canvas series and HTML legends stay identical. The fallback
 * arrays keep tests, print rendering, and partially loaded stylesheets categorical instead of
 * collapsing every series into the first color. */
export const OBSERVE_COLOR_VARS = [
  '--observe-1',
  '--observe-2',
  '--observe-3',
  '--observe-4',
  '--observe-5',
  '--observe-6',
  '--observe-7',
  '--observe-8',
  '--observe-9',
  '--observe-10',
] as const;

const DARK = [
  '#6d90c4',
  '#c88a5e',
  '#77a67d',
  '#c47b83',
  '#9789bd',
  '#bda65f',
  '#5c9ca0',
  '#b98aa8',
  '#7f8ab0',
  '#9c9384',
] as const;

const LIGHT = [
  '#4a6fa5',
  '#b06a3e',
  '#4f7d57',
  '#a85560',
  '#6f629c',
  '#94793a',
  '#3a7a7e',
  '#8f5f80',
  '#5a6690',
  '#736b5c',
] as const;

export function observeColors(themeName: string, resolve: (name: string) => string): string[] {
  const fallback = themeName === 'light' ? LIGHT : DARK;
  return OBSERVE_COLOR_VARS.map((name, index) => resolve(name) || fallback[index]);
}

/**
 * Theme-specific production fill:
 * - dark: soften toward the line luminance, normal blend, medium 0.18;
 * - light: unify toward the mockup's neutral blue-gray family, normal blend, light 0.10.
 */
export function observeAreaFill(hex: string, themeName: string): string {
  const value = Number.parseInt(hex.slice(1), 16);
  const rgb = [(value >> 16) & 255, (value >> 8) & 255, value & 255];
  if (themeName === 'light') {
    const neutral = [138, 146, 158];
    const unified = rgb.map((channel, index) => Math.round(channel + (neutral[index] - channel) * 0.62));
    return `rgba(${unified[0]},${unified[1]},${unified[2]},0.1)`;
  }
  const gray = Math.round(rgb[0] * 0.299 + rgb[1] * 0.587 + rgb[2] * 0.114);
  const softened = rgb.map(channel => Math.round(channel + (gray - channel) * 0.5));
  return `rgba(${softened[0]},${softened[1]},${softened[2]},0.18)`;
}

/** Seven time marks (six equal spans) keep the vertical grid useful without crowding narrow cards. */
export function observeTimeTick(index: number, count: number): boolean {
  if (count <= 1) return index === 0;
  const last = count - 1;
  for (let part = 0; part <= 6; part += 1) {
    if (index === Math.round((last * part) / 6)) return true;
  }
  return false;
}

export type ObserveValueAxis = { max: number; interval: number };

/**
 * Build a zero-based value axis from readable 1 / 2 / 2.5 / 5 × 10ⁿ steps.
 *
 * The ceiling is deliberately the tick *after* the observed peak, including when the peak lands
 * exactly on a tick. That keeps the trace away from the card edge and prevents raw sample maxima
 * such as 873.41 Mbps or 67.2 ms from becoming axis labels.
 */
export function observeValueAxis(peak: number, targetIntervals = 4): ObserveValueAxis {
  const safePeak = Number.isFinite(peak) && peak > 0 ? peak : 1;
  const intervals = Math.max(2, Math.floor(targetIntervals));
  const rawStep = safePeak / intervals;
  const magnitude = 10 ** Math.floor(Math.log10(rawStep));
  const fraction = rawStep / magnitude;
  const niceFraction = fraction <= 1.5 ? 1 : fraction <= 2.25 ? 2 : fraction <= 3.75 ? 2.5 : fraction <= 7.5 ? 5 : 10;
  const interval = niceFraction * magnitude;
  // A small epsilon makes an exact tick robust against floating point residue; it still advances
  // by one full interval as required.
  const completedTicks = Math.floor(safePeak / interval + Number.EPSILON * 8);
  const max = Number(((completedTicks + 1) * interval).toPrecision(12));
  return { max, interval };
}
