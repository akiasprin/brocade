// 「这一跳的口开在谁身上」的自检。跑法：node scripts/hop-listener-check.mjs
//
// 为什么单独有这么个东西：这条规则错了，界面上一切正常。常规几档是下游监听、上游
// 拨过去；反向那两档反过来——下游拨上游，口开在上游那台。写错的症状是编译报
// relay.no-hop-in，而它指着的是另一台机器，人会去那台上找一个本来就不该有的口。
//
// 去重那一条同样没有运行时症状：`hop_in` 在模型里挂在 `(chain, node)` 上，一台机器
// 在一条链上只有一个口。前一跳常规进来、后一跳反向出去的机器会被算中两次，分成两份
// 状态就是两处写同一行，谁后写谁赢——而两处显示的数还都是「对的」。
//
// ports.ts 是纯的（只有 import type），不需要后端也不需要浏览器。

import { build } from 'esbuild';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { dirname, join } from 'node:path';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';

const here = dirname(fileURLToPath(import.meta.url));

const bundle = await build({
  entryPoints: [join(here, '../src/panes/ports.ts')],
  bundle: true,
  format: 'esm',
  platform: 'neutral',
  mainFields: ['module', 'main'],
  conditions: ['import', 'default'],
  write: false,
  logLevel: 'silent',
});

const dir = await mkdtemp(join(tmpdir(), 'brocade-hoplistener-'));
const file = join(dir, 'ports.mjs');
await writeFile(file, bundle.outputFiles[0].text);
const { hopListener, hopListeners } = await import(pathToFileURL(file).href);

let failed = 0;
const eq = (name, got, want) => {
  const g = JSON.stringify(got);
  const w = JSON.stringify(want);
  if (g === w) return;
  failed += 1;
  console.log(`✗ ${name}\n    实际 ${g}\n    期望 ${w}`);
};

const SPINE = ['hk', 'sg', 'jp', 'us'];
/* 反向的跳号集合 → 主干上要开口的机器 */
const listeners = (...reverse) => hopListeners(SPINE, i => reverse.includes(i));

console.log('— 一跳一个口，开在监听的那台 —');

eq('全常规：每个下游各开一个，链头不开', listeners(), ['sg', 'jp', 'us']);
eq(
  '单跳链：hk → sg',
  hopListeners(['hk', 'sg'], () => false),
  ['sg'],
);
eq(
  '只有链头：一个口都不用开',
  hopListeners(['hk'], () => false),
  [],
);

console.log('— 反向把口挪到上游 —');

// 第 1 跳反向：sg 拨 hk，所以 hk 要开口，sg 不开。链头开口是合法的，编译器为这一档
// 专门放宽了「链头不配中转口」（ir/routing.rs），accept 它照旧会清掉。
eq('第一跳反向：口落到链头上', listeners(1), ['hk', 'jp', 'us']);
eq('中间一跳反向：口落到它上游', listeners(2), ['sg', 'us']);
eq('末跳反向：落地那台不开口', listeners(3), ['sg', 'jp']);
eq('全反向：末台不开，链头要开', listeners(1, 2, 3), ['hk', 'sg', 'jp']);

console.log('— 同一台机器只算一个口 —');

// sg 两头都沾：第 1 跳常规进来（它监听），第 2 跳反向出去（还是它监听）。
// 这是同一个 `hop_in` 行，出现两次就是两份状态写同一行。
eq('前一跳常规 + 后一跳反向落在同一台：只出现一次', listeners(2), ['sg', 'us']);
eq(
  '连着两跳反向落在同一台：只出现一次',
  hopListeners(['hk', 'sg', 'jp'], i => i === 1 || i === 2),
  ['hk', 'sg'],
);

console.log('— 单跳的判据本身 —');

eq('常规 = 下游', hopListener(SPINE, 2, false), 'jp');
eq('反向 = 上游', hopListener(SPINE, 2, true), 'sg');

await rm(dir, { recursive: true, force: true });

if (failed > 0) {
  console.log(`\n${failed} 条没过。`);
  process.exit(1);
}
console.log('\n全过了。');
