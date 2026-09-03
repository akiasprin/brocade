/** Shared visual contract for machine-observation charts.
 *
 * Every trace comes from the categorical observation palette, including the primary trace. This
 * keeps multi-series charts balanced instead of forcing their first line to a separate KPI color.
 * CSS owns the real colors so canvas series and HTML legends stay identical. */
export const OBSERVE_SERIES_COLOR_VARS = [
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

/* Fallbacks for the CSS tokens of the same name; keep both lists in sync with styles.css.
   Rosé Pine Moon / Dawn, with slots 6, 8 and 10 filled in — the palette ships only six accents. */
const DARK = [
  '#3e8fb0', // pine
  '#e99cd3', // 紫红, added
  '#8bbe95', // 绿, added
  '#7da1e3', // 靛, added
  '#ea9a97', // rose
  '#9ccfd8', // foam
  '#c4a7e7', // iris
  '#f6c177', // gold
  '#eb6f92', // love
  '#908caa', // subtle
] as const;

const LIGHT = [
  '#286983', // pine
  '#b66fa2', // 紫红, added
  '#5b8e66', // 绿, added
  '#4d70b2', // 靛, added
  '#d7827e', // rose
  '#56949f', // foam
  '#907aa9', // iris
  '#ea9d34', // gold
  '#b4637a', // love
  '#797593', // subtle
] as const;

export function observeColors(themeName: string, resolve: (name: string) => string): string[] {
  const fallback = themeName === 'light' ? LIGHT : DARK;
  return OBSERVE_SERIES_COLOR_VARS.map((name, index) => resolve(name) || fallback[index]);
}

/** The paper an area fill lands on — `--card` in each theme. The overlay fill is expressed as a
 * tint of it, which is the whole mechanism behind the bound described on `observeAreaFill`. */
const PAPER = { light: [255, 255, 255], dark: [31, 32, 35] } as const;

/**
 * Share of the series hue kept in an overlay fill; the rest is paper. Alpha compositing converges
 * to the fill color itself, so N overlapping fills land on exactly this tint however large N
 * grows — it is the constant the stack settles on, and the only number that decides how far from
 * the paper that constant sits.
 *
 * One ratio serves both themes because PAPER differs: 12% lands the constant at L* 95.4–96.0 on
 * white and L* 15.3–17.9 on the dark card, which in both themes is the narrow band between the
 * paper and the split lines (L* 93.3 light, L* 18.5 dark). The grid therefore still reads through
 * a filled region. Raising it pushes the fill past the grid and the chart starts looking pressed
 * down (light) or hazy (dark) — that was the 0.22 / 0.34 this replaced.
 */
const OVERLAY_TINT = 0.12;
const OVERLAY_ALPHA = { light: 0.62, dark: 0.55 } as const;
/** Alpha of a stacked band. Bands never overlap, so this one is flat and keeps the full hue. */
const BAND_INK = { light: 0.28, dark: 0.34 } as const;

/**
 * Share of an overlay fill's alpha still standing at the baseline. Every unstacked trace fills
 * down to the same zero line, so the bottom of the plot is where they pile up — thinning them
 * there takes the weight out of the pile while each trace keeps its own hue at full strength just
 * under its own line, which is where the fill actually identifies anything.
 *
 * Not zero. A fill that fades to nothing stops being a fill and the trace loses its footing; the
 * point is a lighter bottom, not an absent one. At 0.55 the baseline sits about 1 L* (light) and
 * 2 L* (dark) off the region under the line — a hint of depth, not a ramp.
 */
const OVERLAY_BASE_FADE = 0.55;

/** ECharts accepts this object form wherever it accepts a color; `graphic.LinearGradient` builds
 * the same shape. Spelling it out keeps this module free of an echarts import. */
export type ObserveFill =
  | string
  | {
      type: 'linear';
      x: number;
      y: number;
      x2: number;
      y2: number;
      colorStops: { offset: number; color: string }[];
    };

/**
 * Area fill for an observation trace. Two modes, because the two kinds of area behave differently
 * under overlap:
 *
 * - `stacked` bands sit on top of each other in y and never overlap, so they take a flat fill at
 *   an alpha high enough to read as a composition.
 * - unstacked traces all fill down to the same zero line, so N of them overlap there and their
 *   alphas compound. The old fill was a full-strength color at alpha 0.10, whose composite
 *   converges to that color: four traces already reached L* 87 on white and sixteen reached L* 66,
 *   the gray-black floor. Filling with a *tint* of the series color instead makes the same
 *   compositing converge to the tint, so the floor is a constant — `count` only sets how fast the
 *   stack reaches it, never how far it goes. See OVERLAY_TINT for where that constant sits.
 *
 * A visible fill on white is necessarily darker than white, and on the dark card necessarily
 * lighter than it, so a stack can only move away from the paper — the reachable best is to stop
 * moving, close to it. Blend modes do not help: `lighten` and `screen` are inert on white paper
 * (it is already the per-channel maximum), and `darken` holds the lightness flat only by fixing it
 * darker than the split lines, which then stop reading through the fill.
 *
 * Unstacked fills also thin toward the zero line — see OVERLAY_BASE_FADE — so the pile-up at the
 * bottom carries less weight than the band right under each line.
 */
export function observeAreaFill(
  hex: string,
  themeName: string,
  options: { stacked?: boolean; count?: number } = {},
): ObserveFill {
  const value = Number.parseInt(hex.slice(1), 16);
  const rgb = [(value >> 16) & 255, (value >> 8) & 255, value & 255];
  const key = themeName === 'light' ? 'light' : 'dark';
  if (options.stacked) return `rgba(${rgb[0]},${rgb[1]},${rgb[2]},${BAND_INK[key]})`;
  const paper = PAPER[key];
  const tinted = rgb.map((channel, index) => Math.round(channel * OVERLAY_TINT + paper[index] * (1 - OVERLAY_TINT)));
  // Fewer traces earn a more present fill; more traces reach the same floor more gently.
  const alpha = Math.max(0.1, OVERLAY_ALPHA[key] / Math.sqrt(Math.max(1, options.count ?? 1)));
  const at = (share: number) => `rgba(${tinted[0]},${tinted[1]},${tinted[2]},${Number((alpha * share).toFixed(4))})`;
  // Offsets run over the filled shape, whose top is the series' own peak and whose bottom is the
  // zero line every unstacked trace shares.
  return {
    type: 'linear',
    x: 0,
    y: 0,
    x2: 0,
    y2: 1,
    colorStops: [
      { offset: 0, color: at(1) },
      { offset: 1, color: at(OVERLAY_BASE_FADE) },
    ],
  };
}

/**
 * `areaStyle` for one trace. Every observation chart goes through here so the fill rule is decided
 * in one place rather than per caller.
 */
export function observeAreaStyle(
  hex: string,
  themeName: string,
  options: { stacked?: boolean; count?: number } = {},
): { color: ObserveFill; opacity: number } {
  return { color: observeAreaFill(hex, themeName, options), opacity: 1 };
}

/**
 * Full-size observation traces echo the 1.8px round KPI sparkline at an optically lighter 1.5px.
 * Keeping this here makes throughput, ping, and expanded history use one stroke contract.
 */
export function observeSeriesLine(color: string) {
  return { color, width: 1.5, cap: 'round' as const, join: 'round' as const };
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

/**
 * Engineering L-frame shared by every observation chart. Without it the series floated as thin
 * sparklines; a solid axis line plus short outward ticks give the plot the instrument face the
 * observe page asks for. Split lines (the grid) and labels stay with the caller — only the frame
 * and its ticks are shared, so all four charts read as one instrument.
 *
 * Callers pass ink-3, not the ink-4 this started on. The frame reads as thin at 1px against a
 * grid that sits only 6 L* off the card, and raising its contrast (L* 45 → 60) gives it presence
 * without spending a second pixel — at 2px the frame becomes the heaviest stroke in the chart,
 * heavier than the 1.5px series, which puts the chrome in front of the data.
 */
export function observeAxisLine(strong: string) {
  return { show: true, lineStyle: { color: strong, width: 1 } };
}

/**
 * Short outward major tick. Takes the same ink and width as `observeAxisLine` — a tick thinner or
 * paler than the axis it stands on shows the mismatch at the join. `interval` is only meaningful
 * on a category axis, where it pins ticks under the sparse time labels instead of drawing one per
 * window; time and value axes place their own ticks and ignore it.
 */
export function observeAxisTick(strong: string, interval?: (index: number) => boolean) {
  const tick = { show: true, length: 4, lineStyle: { color: strong, width: 1 } };
  return interval ? { ...tick, alignWithLabel: true, interval } : tick;
}

/**
 * Faint minor ticks between the majors — the fine graduations of the instrument. Only time and
 * value axes accept them (a category axis rejects minor ticks), so this stays on the time-axis
 * charts' horizontal frame.
 */
export function observeMinorTick(soft: string, splitNumber = 4) {
  return { show: true, splitNumber, length: 2, lineStyle: { color: soft } };
}

/** Round wall-clock steps a time axis is allowed to snap its major ticks to (milliseconds). */
const TIME_STEPS_MS = [
  30_000, 60_000, 120_000, 300_000, 600_000, 900_000, 1_200_000, 1_800_000, 3_600_000, 7_200_000, 10_800_000,
  21_600_000, 43_200_000, 86_400_000,
];

/**
 * Pick a major-tick interval (ms) for a time axis so it lands ~6 divisions instead of letting
 * ECharts choose — its `splitNumber` hint is loose on a time axis and readily overshoots to 15
 * crowded labels. The returned step is a round wall-clock value (½m, 1m, 2m, 5m, …), so ticks fall
 * on clock boundaries and the vertical grid stays sparse.
 */
export function observeTimeInterval(spanMs: number, targetDivisions = 6): number {
  const target = spanMs / Math.max(2, targetDivisions);
  return TIME_STEPS_MS.find(step => step >= target) ?? TIME_STEPS_MS[TIME_STEPS_MS.length - 1];
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

/** One unit for a whole value axis: named once in the card's title bar, stripped from every tick. */
export type ObserveAxisUnit = {
  /** What the title bar prints, as in `网卡流量 (Mb/s)`. */
  name: string;
  /** One tick, bare — no unit, since the title bar already carries it. */
  text: (value: number) => string;
};

const BPS_UNITS: readonly (readonly [number, string])[] = [
  [1e9, 'Gb/s'],
  [1e6, 'Mb/s'],
  [1e3, 'kb/s'],
  [1, 'b/s'],
] as const;

/**
 * The one rule that picks a throughput unit, for axes and for readings alike.
 *
 * `pick` chooses the rung: the largest whose quotient is at least 1. For a reading that is the
 * value; for an axis it is the tick interval, because the interval is the smallest quantity the
 * axis has to spell out and so decides whether the labels need decimals — a 250 Mb/s step under a
 * Gb/s heading reads 0 / 0.25 / 0.50 / 0.75 / 1.00, under Mb/s it reads 0 / 250 / 500 / 750 / 1000.
 *
 * `span` then decides whether to step down one rung: a single-digit span means the unit is too
 * coarse for what is being shown. `3 Mb/s` is 3.4 Mb/s rounded — a 13% haircut, and every value
 * between 1 and 10 Mb/s takes one of that order — while `3400 kb/s` takes none worth naming. For a
 * reading `span` is the value; for an axis it is the ceiling, so an axis topping out at 4 Mb/s
 * labels its ticks 1000 / 2000 / 3000 / 4000 kb/s rather than 1 / 2 / 3 / 4.
 *
 * Below the top rung the quotient therefore lands in 1000–9999: throughput reads as three or four
 * digits, never a decimal, and the unit is stable across the whole card.
 */
function bpsRung(pick: number, span: number): { divisor: number; name: string; ceiling: boolean } {
  const found = BPS_UNITS.findIndex(([step]) => pick / step >= 1);
  const rung = found === -1 ? BPS_UNITS.length - 1 : found;
  // b/s is the bottom of the ladder; there is nothing below it to step down to.
  const stepDown = rung < BPS_UNITS.length - 1 && span / BPS_UNITS[rung][0] < 10;
  const index = stepDown ? rung + 1 : rung;
  const [divisor, name] = BPS_UNITS[index];
  return { divisor, name, ceiling: index === 0 };
}

/**
 * The rung a single throughput reading belongs on; `bps()` in panes/telemetry.tsx formats it.
 *
 * `ceiling` says the ladder ran out above — Gb/s with nothing larger to move to. It is the one
 * place the step-down cannot rescue the reading, so the formatter spends a decimal there instead:
 * a whole-fleet total of 12.4 Gb/s rounds to `12 Gb/s` otherwise, losing 3%.
 */
export function observeBpsRung(value: number): { divisor: number; name: string; ceiling: boolean } {
  return bpsRung(value, value);
}

/**
 * Unit for a throughput axis: one unit for every tick, named once in the title bar.
 *
 * This replaced `bps()` on the axis, which chose per tick and so put `1.00 Gb/s` and `500 Mb/s` on
 * the same axis — a label column jumping between three and nine characters, with `containLabel`
 * reserving room for the longest. `bps()` still formats the tooltip and the legend: those are
 * readings, not a scale. Both now go through `bpsRung`, so a card's axis and its legend can differ
 * by at most the rung their own magnitudes earn, never by the ad-hoc rounding they used before.
 */
export function observeBpsUnit(axis: ObserveValueAxis): ObserveAxisUnit {
  const { divisor, name } = bpsRung(axis.interval, axis.max);
  return {
    name,
    text: value => {
      const scaled = value / divisor;
      // Integers cover every 1 / 2 / 5 × 10ⁿ step; only the 2.5 step lands on a half.
      return Number.isInteger(scaled) ? String(scaled) : String(Number(scaled.toFixed(2)));
    },
  };
}

/** Latency axes never change unit — see `observeMsUnit` — so a card can name it without an axis. */
export const OBSERVE_MS_UNIT = 'ms';

/**
 * The same for latency, where the ladder has a single rung: `ui/ping-probe.ts` prints milliseconds
 * only, because microsecond precision means nothing on the links these charts watch. Only the tick
 * precision varies with the interval; the name never does.
 */
export function observeMsUnit(interval: number): ObserveAxisUnit {
  return {
    name: OBSERVE_MS_UNIT,
    text: value => (interval >= 1 ? String(Math.round(value)) : String(Number(value.toFixed(1)))),
  };
}
