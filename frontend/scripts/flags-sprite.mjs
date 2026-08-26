// 生成机器卡和链路卡上的国旗雪碧图。跑法：
//   cd frontend
//   npm i --no-save sharp@0.34.5 && node scripts/flags-sprite.mjs && npm rm --no-save sharp
//
// 产出两个文件，一起提交。它不挂在 `npm run build` 上，构建时不会重跑——只有换
// flag-icons 版本或者改格子尺寸时才需要手动跑一次：
//   src/ui/flags.png —— 16 列的雪碧图，一格 48×32
//   src/ui/flags.ts  —— 格子顺序，country-flag.tsx 按它算 background-position
//
// 从前这里是 Regional Indicator emoji（两个字母映射到 U+1F1E6 段），不占任何资源。
// 换掉的原因是它要系统装了彩色 emoji 字体才显示得出来——没装 Noto Color Emoji 的
// Linux 上每面旗都是两个方框字母，而控制面恰恰常开在这种机器上。
//
// 不外链 flagcdn 之类的 CDN。控制面是代理的控制面，按国家码逐个发图片请求等于把
// 「这个面板有几台机器、分布在哪些国家」持续讲给第三方听，URL 里就写着国家码。
// geoip.rs 拒绝为主机名做 DNS 解析是同一条理由，那边挡的是服务端，这边挡的是浏览器。
//
// 一格是 3:2 而不是源文件的 4:3：两个调用点的盒子（15×10 和 18×12）都是 3:2，生成
// 时居中裁掉一次，css 里就不必为每个尺寸各配一套 background-size。
//
// 只收 `^[a-z]{2}$` 的文件。flag-icons 还带 gb-eng、es-ct、arab 这类非 ISO 3166-1
// 的条目，而国家码这一路的两个来源——geoip.dat 和 cloudflare trace 的 loc——给的都是
// alpha-2，收进来的格子永远不会被引用，只是白占雪碧图的面积。
//
// 体积对照（257 面旗、48×32、16×17 格）：调色板 PNG 56K，webp q80 68K，无损 PNG 150K。
// 六倍放大逐面比过，调色板量化在这个尺寸上看不出来，取最小的那个。

import { execFileSync } from 'node:child_process';
import { mkdtempSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const FLAG_ICONS = '7.5.0'; // MIT，旗面本身是公有领域
const CELL_W = 48;
const CELL_H = 32;
const COLS = 16;

const here = dirname(fileURLToPath(import.meta.url));
const ui = join(here, '..', 'src', 'ui');

let sharp;
try {
  ({ default: sharp } = await import('sharp'));
} catch {
  console.error('缺 sharp。它只在生成这张图时用得上，没有必要进 package.json：\n  npm i --no-save sharp@0.34.5');
  process.exit(1);
}

// 不把 svg 源码 vendor 进仓库：这里要的是那张 png，几百个 svg 留在树里只会在每次
// grep 前端时挡路，而版本号钉死之后从 registry 取和从树里读是同一份东西。
const work = mkdtempSync(join(tmpdir(), 'brocade-flags-'));
try {
  const url = `https://registry.npmjs.org/flag-icons/-/flag-icons-${FLAG_ICONS}.tgz`;
  const response = await fetch(url);
  if (!response.ok) throw new Error(`下载 ${url}：HTTP ${response.status}`);
  const tarball = join(work, 'flag-icons.tgz');
  writeFileSync(tarball, Buffer.from(await response.arrayBuffer()));
  execFileSync('tar', ['xzf', tarball, '-C', work]);

  const svgDir = join(work, 'package', 'flags', '4x3');
  const codes = readdirSync(svgDir)
    .filter(name => /^[a-z]{2}\.svg$/.test(name))
    .map(name => name.slice(0, 2))
    .sort();
  const rows = Math.ceil(codes.length / COLS);

  const cells = await Promise.all(
    codes.map(async (code, index) => ({
      // density 只对 svg 的光栅化起作用，给高一点是因为 fit:'cover' 先按宽缩放到 48，
      // 默认 72dpi 下有些旗的徽章会先被栅格化成糊的再缩小。
      input: await sharp(join(svgDir, `${code}.svg`), { density: 600 })
        .resize(CELL_W, CELL_H, { fit: 'cover', position: 'center' })
        .png()
        .toBuffer(),
      left: (index % COLS) * CELL_W,
      top: Math.floor(index / COLS) * CELL_H,
    })),
  );

  const png = await sharp({
    create: {
      width: COLS * CELL_W,
      height: rows * CELL_H,
      channels: 4,
      background: { r: 0, g: 0, b: 0, alpha: 0 },
    },
  })
    .composite(cells)
    .png({ compressionLevel: 9, palette: true })
    .toBuffer();
  writeFileSync(join(ui, 'flags.png'), png);

  const sheet = Array.from({ length: rows }, (_, row) =>
    codes.slice(row * COLS, (row + 1) * COLS).join(''),
  );
  writeFileSync(
    join(ui, 'flags.ts'),
    [
      '// 由 frontend/scripts/flags-sprite.mjs 生成，别手改（改了下次生成也会被覆盖）。',
      '',
      `/** flags.png 的列数。一格 ${CELL_W}×${CELL_H}，3:2，与 \`.geo-flag\` 的两个尺寸同比。 */`,
      `export const FLAG_COLS = ${COLS};`,
      '',
      '/** 雪碧图里的格子顺序：一行字符串对应图上一行，每两个字符一个 ISO 3166-1 alpha-2 码。 */',
      'export const FLAG_SHEET = [',
      ...sheet.map(row => `  '${row}',`),
      '];',
      '',
    ].join('\n'),
  );

  console.log(
    `flags.png ${COLS * CELL_W}×${rows * CELL_H}，${(png.length / 1024).toFixed(1)}K，` +
      `${codes.length} 面旗排成 ${COLS}×${rows}`,
  );
} finally {
  rmSync(work, { recursive: true, force: true });
}
