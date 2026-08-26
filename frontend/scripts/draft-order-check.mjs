// 草稿队列的合并与排序。跑法：
//   node scripts/draft-order-check.mjs
//
// 验的是 draft.ts 里真的那个 DraftStore，不是仿制品。
//
// 为什么这件事值得单独验。 草稿是一串按顺序回放的操作，顺序错了不会报错，
// 只会在提交之后表现成模型不对：
// - 重复编辑必须留在原位——链要先存在才能往它上面写规则表，把后写的那条挪到
//   末尾就是把先决条件甩到自己后头去；
// - `prune_chain` 反过来必须挪到末尾——它算的是「这条链上谁没人指向」，那要
//   整条链的规则表都落到位才算得准。留在原位的话，第二轮保存的 put_step 会排在
//   它后面，于是它按一份还缺东西的链去摘，把人刚接上的机器摘掉。

import { build } from 'esbuild';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { dirname, join } from 'node:path';
import { writeFile, rm } from 'node:fs/promises';

/* DraftStore 的构造函数不碰 localStorage（init 才碰），但 commit 会写。立个假的。 */
const cell = new Map();
globalThis.localStorage = {
  getItem: k => (cell.has(k) ? cell.get(k) : null),
  setItem: (k, v) => cell.set(k, String(v)),
  removeItem: k => cell.delete(k),
};

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, '..');

const bundle = await build({
  stdin: {
    contents: `export { draft } from './src/draft';`,
    resolveDir: root,
    sourcefile: 'draft-entry.ts',
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

const file = join(root, '.draft-order-check.tmp.mjs');
await writeFile(file, bundle.outputFiles[0].text);
const { draft } = await import(pathToFileURL(file).href);

let failed = 0;
const check = (name, expected) => {
  /* 队列的形状：每条操作缩成一个短标签，顺序就是回放顺序。 */
  const actual = draft.ops().map(op => {
    switch (op.op) {
      case 'put_step':
        return `put:${op.node_id}`;
      case 'delete_step':
        return `del:${op.node_id}`;
      case 'prune_chain':
        return `prune:${op.chain_id}`;
      case 'upsert_chain':
        return `chain:${op.chain.id}`;
      default:
        return op.op;
    }
  });
  const ok = JSON.stringify(actual) === JSON.stringify(expected);
  console.log(
    `${ok ? '✓' : '✗'} ${name}${ok ? '' : `\n    是: ${JSON.stringify(actual)}\n    该: ${JSON.stringify(expected)}`}`,
  );
  if (!ok) failed++;
};

const put = node =>
  draft.push({
    op: 'put_step',
    app_id: 'a',
    chain_id: 'c1',
    node_id: node,
    step: { rules: [] },
  });
const prune = (chain = 'c1') => draft.push({ op: 'prune_chain', app_id: 'a', chain_id: chain });
const chain = id => draft.push({ op: 'upsert_chain', app_id: 'a', chain: { id, tenant_id: 't', name: id } });

console.log('— 一轮保存：规则表在前，清理在末尾 —');
{
  draft.clear();
  put('hk');
  put('sg');
  prune();
  check('put × N 之后跟一条 prune', ['put:hk', 'put:sg', 'prune:c1']);
}

console.log('\n— 第二轮保存：prune 要挪到新的 put 后面 —');
{
  draft.clear();
  put('hk');
  prune();
  put('sg');
  prune();
  check('prune 不留在原位，跟到末尾', ['put:hk', 'put:sg', 'prune:c1']);
}

console.log('\n— 重复编辑留在原位：先决条件不能被甩到后头 —');
{
  draft.clear();
  chain('c1');
  put('hk');
  chain('c1'); // 又改了一次链
  check('链的重复编辑仍排在规则表前面', ['chain:c1', 'put:hk']);
}
{
  draft.clear();
  put('hk');
  put('sg');
  put('hk'); // 又改了一次 hk
  check('规则表的重复编辑不改变相对顺序', ['put:hk', 'put:sg']);
}

console.log('\n— 两条链各自一条 prune —');
{
  draft.clear();
  put('hk');
  prune('c1');
  prune('c2');
  check('按链分键，互不合并', ['put:hk', 'prune:c1', 'prune:c2']);
}

console.log('\n— delete_step 跟 put_step 同键：后写的赢，位置不动 —');
{
  draft.clear();
  put('hk');
  put('sg');
  draft.push({ op: 'delete_step', app_id: 'a', chain_id: 'c1', node_id: 'hk' });
  check('先改后删收敛到删，仍在原位', ['del:hk', 'put:sg']);
}

draft.clear();
await rm(file, { force: true });
if (failed) {
  console.error(`\n${failed} 条不对。`);
  process.exit(1);
}
console.log('\n全过了。');
