/** Two-letter ISO region code as a flag, cut out of `flags.png`.
 *
 * It used to be the Regional Indicator emoji pair, which cost nothing to ship but needed a colour
 * emoji font on the viewer's machine: a Linux box without Noto Color Emoji drew every flag as two
 * boxed letters, and that is exactly where a console tends to be opened. The sheet and the cell
 * order below are generated together by `frontend/scripts/flags-sprite.mjs` — see its header for
 * why the flags are shipped rather than fetched from a CDN.
 *
 * The wrapper still owns the visual size. Cells are 3:2, the ratio of both boxes that use one, so
 * the same two numbers place a flag correctly at either size and it never distorts. A code with no
 * cell renders nothing rather than an empty box.
 */
import { FLAG_COLS, FLAG_SHEET } from './flags';

const CELLS = new Map<string, { col: number; row: number }>();
FLAG_SHEET.forEach((line, row) => {
  for (let col = 0; col * 2 < line.length; col += 1) CELLS.set(line.slice(col * 2, col * 2 + 2), { col, row });
});

export function RegionFlag({ code, square = false }: { code: string | null | undefined; square?: boolean }) {
  const normalized = code?.trim().toUpperCase() ?? '';
  // No shape check of its own: every key here is a real alpha-2 code, so a miss already covers the
  // empty string, a country the sheet does not carry, and the `PRIVATE`-style tags geoip.dat holds.
  const cell = CELLS.get(normalized.toLowerCase());
  if (!cell) return null;
  // Percentage positioning rather than pixels keeps both regular flags and the square node mark
  // independent of their rendered size. A square does not stretch a cell: each 3:2 cell is scaled
  // to 150% of the square width, then exactly 25% is cropped from either side. Its x position must
  // include that leading quarter-cell; col/(cols-1), which is correct for an uncropped cell, would
  // make every square after the first drift horizontally through the sprite.
  const backgroundSizeX = square ? FLAG_COLS * 150 : FLAG_COLS * 100;
  const backgroundPositionX = square
    ? ((cell.col * 1.5 + 0.25) / (FLAG_COLS * 1.5 - 1)) * 100
    : (cell.col / (FLAG_COLS - 1)) * 100;
  return (
    <span
      className={`geo-flag${square ? ' square' : ''}`}
      role="img"
      aria-label={`${normalized} 地区旗`}
      title={normalized}
      style={{
        backgroundSize: `${backgroundSizeX}% ${FLAG_SHEET.length * 100}%`,
        backgroundPosition: `${backgroundPositionX}% ${(cell.row / (FLAG_SHEET.length - 1)) * 100}%`,
      }}
    />
  );
}
