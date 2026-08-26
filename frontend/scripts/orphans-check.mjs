// 「改完之后谁没人指向」这条判据的样例。跑法：
//   node scripts/orphans-check.mjs
//
// 验的是 panes/rules.tsx 里的 `orphansAfter`——真函数，不是仿制品。
//
// 它的结果只拿去显示，不拿去删。 真删是服务端的事（store 的 prune_unreachable_tx，
// 同一套可达性判据，但它看得见整条链的落库现状）。所以算错的代价是提示错——说某台
// 落单了而其实没有，人会照着这句话去动一台好好的机器。
//
// 多张表一起改时，答案取决于喂进来的草稿有多全：只喂一张表的草稿，别的表用库里
// 的旧规则，那答案就是残缺的。从前这个函数是每张表各调各的，然后拿结果直接
// deleteStep——保存完编译报 relay.no-accept 就是这么来的（规则指着它，它的 step 被
// 另一张表摘了）。现在由 RuleDraftScope 把同一条链的草稿收齐再调一次。

import { build } from 'esbuild';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { dirname, join } from 'node:path';
import { writeFile, rm } from 'node:fs/promises';

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, '..');

const bundle = await build({
  stdin: {
    contents: `export { orphansAfter } from './src/panes/rules';`,
    resolveDir: root,
    sourcefile: 'orphans-entry.ts',
    loader: 'ts',
  },
  bundle: true,
  format: 'esm',
  platform: 'neutral',
  mainFields: ['module', 'main'],
  conditions: ['import', 'default'],
  write: false,
  logLevel: 'silent',
});

// 临时产物写在项目里而不是 /tmp：rules.tsx 拖着 react 一起被打包，
// 而 react 的解析要从 node_modules 那一层往上找。
const file = join(root, '.orphans-check.tmp.mjs');
await writeFile(file, bundle.outputFiles[0].text);
const { orphansAfter } = await import(pathToFileURL(file).href);

let failed = 0;
const check = (name, actual, expected) => {
  const ok = JSON.stringify([...actual].sort()) === JSON.stringify([...expected].sort());
  console.log(
    `${ok ? '✓' : '✗'} ${name}${ok ? '' : `\n    是: ${JSON.stringify(actual)}\n    该: ${JSON.stringify(expected)}`}`,
  );
  if (!ok) failed++;
};

/* 一台机器在一条链上的 step。只有 node 和 rules 参与这个判据。 */
const step = (node, ...to) => ({
  chain: 'c1',
  node,
  accept: null,
  hop_in: null,
  rules: to.map(t => ({ m: { t: 'any' }, a: { t: 'forward', to: t, dial: null } })),
});
const egress = node => ({
  chain: 'c1',
  node,
  accept: null,
  hop_in: null,
  rules: [{ m: { t: 'any' }, a: { t: 'egress' } }],
});
/* 一张表的草稿。多张就多写几对——这正是它跟从前那个单表签名的差别。 */
const draft = (...pairs) => new Map(pairs.map(([node, ...to]) => [node, step(node, ...to).rules]));
const cleared = node => [node];

console.log('— 主干 hk → sg → jp —');
{
  const steps = [step('hk', 'sg'), step('sg', 'jp'), egress('jp')];
  check('什么都没改', orphansAfter({ steps, root: 'hk' }), []);
  check('链头把转发删了：底下两台一起掉', orphansAfter({ steps, root: 'hk', drafts: draft(cleared('hk')) }), [
    'sg',
    'jp',
  ]);
  check('中间那台把转发删了：只掉末端', orphansAfter({ steps, root: 'hk', drafts: draft(cleared('sg')) }), ['jp']);
  check('末端删规则：谁都不掉', orphansAfter({ steps, root: 'hk', drafts: draft(cleared('jp')) }), []);
}

console.log('\n— 分叉：hk 同时指 sg 和 au —');
{
  const steps = [step('hk', 'sg', 'au'), egress('sg'), egress('au')];
  check('删掉指向 au 的那条', orphansAfter({ steps, root: 'hk', drafts: draft(['hk', 'sg']) }), ['au']);
  check('两条都删', orphansAfter({ steps, root: 'hk', drafts: draft(cleared('hk')) }), ['sg', 'au']);
}

console.log('\n— 两个上游都指向 my —');
{
  const steps = [step('hk', 'sg', 'my'), step('sg', 'my'), egress('my')];
  check('删掉其中一个上游：my 还有人指，不动它', orphansAfter({ steps, root: 'hk', drafts: draft(['hk', 'sg']) }), []);
  check(
    '两个上游一起删才掉——一次算完，不用分两遍',
    orphansAfter({ steps, root: 'hk', drafts: draft(['hk', 'sg'], cleared('sg')) }),
    ['my'],
  );
}

// 这一组是这个函数改签名的理由，也是那个 relay.no-accept 的成因。
// 同一次保存改了两张表：一张把 au 摘下来，另一张把 au 接到别处。只喂其中一张的
// 草稿，看不见另一张刚接上的那条边，au 就被判成落单——而从前这个错答案会被直接
// 拿去 deleteStep，于是 hk 的规则指着 au，au 的 step 没了。
console.log('\n— 多张表一起改：草稿必须收齐 —');
{
  /* hk → sg，sg 同时指 jp 和 au。改成 au 由 hk 直接接、sg 只留 jp。 */
  const steps = [step('hk', 'sg'), step('sg', 'jp', 'au'), egress('jp'), egress('au')];
  check(
    '两张表的草稿都给：au 被 hk 接住了，一台都不掉',
    orphansAfter({ steps, root: 'hk', drafts: draft(['hk', 'sg', 'au'], ['sg', 'jp']) }),
    [],
  );
  check(
    '只给 sg 那张（hk 用库里的旧规则）：au 被误判成落单',
    orphansAfter({ steps, root: 'hk', drafts: draft(['sg', 'jp']) }),
    ['au'],
  );
}

console.log('\n— 环：入度判据会漏，可达性不会 —');
{
  /* a↔b 互指，谁都不在链头的下游。按入度算两台都是 1，一台都删不掉。 */
  const steps = [egress('hk'), step('a', 'b'), step('b', 'a')];
  check('环上两台都不可达', orphansAfter({ steps, root: 'hk' }), ['a', 'b']);
}

console.log('\n— 兜底：图不完整就什么都不报 —');
{
  const steps = [step('hk', 'sg'), egress('sg')];
  check('不知道链头', orphansAfter({ steps, root: undefined, drafts: draft(cleared('hk')) }), []);
  check('一条 step 都没有', orphansAfter({ steps: [], root: 'hk', drafts: draft(cleared('hk')) }), []);
  check('链头自己永远保留', orphansAfter({ steps, root: 'hk', drafts: draft(cleared('hk')) }), ['sg']);
}

console.log('\n— 指向链外（那台还没有 step） —');
{
  const steps = [step('hk', 'outside'), egress('hk')];
  check('链外目标不算成员，不会被报成落单', orphansAfter({ steps, root: 'hk', drafts: draft(['hk', 'outside']) }), []);
}

await rm(file, { force: true });
if (failed) {
  console.error(`\n${failed} 条不对。`);
  process.exit(1);
}
console.log('\n全过了。');
