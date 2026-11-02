const sign = (value: number) => (value > 0 ? 1 : value < 0 ? -1 : 0);

/** Steffen tangents for unit-spaced points. Each is bounded by twice the smaller neighbouring
 * slope and is zero at a local extremum, so the cubic between two points stays between them. */
function tangents(ys: readonly number[]): number[] {
  const slope = ys.slice(1).map((y, index) => y - ys[index]);
  return ys.map((_, index) => {
    if (slope.length === 0) return 0;
    if (index === 0) return slope[0];
    if (index === ys.length - 1) return slope[index - 1];
    const before = slope[index - 1];
    const after = slope[index];
    return (sign(before) + sign(after)) * Math.min(Math.abs(before), Math.abs(after), Math.abs(before + after) / 4);
  });
}

const fixed = (value: number) => value.toFixed(2);

/** Paths for a compact trend drawn in a `0 0 (values.length - 1) height` viewBox with
 * `preserveAspectRatio="none"`. Each value sits at its index; `null` marks a missing point and
 * ends the current segment, so a collection gap is never drawn as data (see
 * `standards/frontend.md`). Values are clamped to `scale`.
 *
 * Within each collected segment:
 * - `bases`: a baseline at zero, so a quiet but collected stretch reads as zero, not as a gap.
 * - `curves`: each complete curve, including zero and sub-pixel runs.
 * - `areas`: the curve closed on the baseline.
 * - `lines`: the same curve over each run of non-zero values, extended to the zero on either side
 *   so a rise starts and ends on the baseline. A run is stroked only if it stands at least 1px
 *   above the baseline somewhere: a few KiB scattered over an idle hour would otherwise draw as
 *   short strokes along the baseline that read as a dashed line. A qualifying run is stroked in
 *   full, including its sub-pixel stretches; stroking only the visible part left the stroke
 *   ending in mid-air, so the foot of each rise looked cut off.
 * The curve is monotone cubic, so it never dips below the baseline or overshoots a peak. */
export function sparklinePaths(
  values: readonly (number | null)[],
  scale: number,
  height: number,
  top = 2.5,
): { bases: string[]; areas: string[]; curves: string[]; lines: string[] } {
  // The baseline is a 1px stroke on the bottom pixel row; zero sits on its centre.
  const zero = height - 0.5;
  const safeScale = scale > 0 ? scale : 1;
  const y = (value: number) => zero - Math.min(1, Math.max(0, value) / safeScale) * (zero - top);
  const segments: { start: number; values: number[]; ys: number[] }[] = [];
  let current: { start: number; values: number[]; ys: number[] } | null = null;
  values.forEach((value, index) => {
    if (value === null || !Number.isFinite(value)) {
      current = null;
      return;
    }
    if (!current) {
      current = { start: index, values: [], ys: [] };
      segments.push(current);
    }
    current.values.push(value);
    current.ys.push(y(value));
  });
  const bases: string[] = [];
  const areas: string[] = [];
  const curves: string[] = [];
  const lines: string[] = [];
  for (const { start, values: segment, ys } of segments) {
    const end = start + ys.length - 1;
    const m = tangents(ys);
    const to = (index: number) =>
      `C${fixed(start + index - 2 / 3)} ${fixed(ys[index - 1] + m[index - 1] / 3)} ${fixed(start + index - 1 / 3)} ${fixed(ys[index] - m[index] / 3)} ${start + index} ${fixed(ys[index])}`;
    bases.push(`M${start} ${zero}H${end}`);
    let area = `M${start} ${fixed(ys[0])}`;
    for (let index = 1; index < ys.length; index += 1) area += to(index);
    curves.push(area);
    areas.push(`${area}L${end} ${zero}L${start} ${zero}Z`);
    const drawn = segment.map(() => false);
    for (let first = 0; first < segment.length; first += 1) {
      if (segment[first] <= 0) continue;
      let last = first;
      while (last + 1 < segment.length && segment[last + 1] > 0) last += 1;
      if (ys.slice(first, last + 1).some(value => zero - value >= 1)) {
        for (let index = Math.max(0, first - 1); index <= Math.min(segment.length - 1, last + 1); index += 1) {
          drawn[index] = true;
        }
      }
      first = last;
    }
    let line = '';
    drawn.forEach((on, index) => {
      if (!on) return;
      line += index > 0 && drawn[index - 1] ? to(index) : `M${start + index} ${fixed(ys[index])}`;
    });
    if (line) lines.push(line);
  }
  return { bases, areas, curves, lines };
}
