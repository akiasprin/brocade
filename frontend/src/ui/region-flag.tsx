/** Two-letter ISO region code as a flag, cut out of the vector `flags.svg` sheet.
 *
 * It used to be the Regional Indicator emoji pair, which cost nothing to ship but needed a colour
 * emoji font on the viewer's machine: a Linux box without Noto Color Emoji drew every flag as two
 * boxed letters, and that is exactly where a console tends to be opened. The sheet and the cell
 * order below are generated together by `frontend/scripts/flags-sprite.mjs` — see its header for
 * why the flags are shipped rather than fetched from a CDN.
 *
 * An inline low-resolution sheet fills each flag immediately; the vector layer paints above it
 * when ready, so a slow image request never leaves an empty frame.
 *
 * The wrapper still owns the visual size. Cells are 3:2 and retain their SVG paths at every display size, so
 * the same two numbers place a flag correctly at either size and it never distorts. A code with no
 * cell renders nothing rather than an empty box.
 */
import { FLAG_COLS, FLAG_SHEET } from './flags';
import './flags-preview.css';

const CELLS = new Map<string, { col: number; row: number }>();
FLAG_SHEET.forEach((line, row) => {
  for (let col = 0; col * 2 < line.length; col += 1) CELLS.set(line.slice(col * 2, col * 2 + 2), { col, row });
});

// These flags carry their identifying motif in the hoist-side canton. A uniform center crop
// discards that motif; the square mark follows it instead. Centered motifs keep the default.
const SQUARE_FOCUS_X: Readonly<Record<string, number>> = {
  au: 0.25,
  cl: 0,
  cn: 0,
  cu: 0,
  gr: 0,
  lr: 0,
  my: 0,
  nz: 0.25,
  sg: 0,
  to: 0,
  tw: 0,
  us: 0,
  ws: 0,
};

export function RegionFlag({ code, square = false }: { code: string | null | undefined; square?: boolean }) {
  const normalized = code?.trim().toUpperCase() ?? '';
  // No shape check of its own: every key here is a real alpha-2 code, so a miss already covers the
  // empty string, a country the sheet does not carry, and the `PRIVATE`-style tags geoip.dat holds.
  const cell = CELLS.get(normalized.toLowerCase());
  if (!cell) return null;
  // Percentage positioning rather than pixels keeps both regular flags and the square node mark
  // independent of their rendered size. A square does not stretch a cell: each 3:2 cell is scaled
  // to 150% of the square width, then the extra half-width is cropped around its focal point. The
  // position includes that leading crop; col/(cols-1), correct for an uncropped cell, would
  // make every square after the first drift horizontally through the sprite.
  const focusX = SQUARE_FOCUS_X[normalized.toLowerCase()] ?? 0.5;
  const backgroundSizeX = square ? FLAG_COLS * 150 : FLAG_COLS * 100;
  const backgroundPositionX = square
    ? ((cell.col * 1.5 + focusX * 0.5) / (FLAG_COLS * 1.5 - 1)) * 100
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
