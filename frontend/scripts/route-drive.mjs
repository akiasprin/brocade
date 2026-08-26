// 把路由接上一个假的浏览器历史栈，真的走一遍前进后退。跑法：
// node scripts/route-drive.mjs
//
// route-check.mjs 验的是编解码，这里验的是活的部分，也是真出事的地方：
// - 状态一变，地址有没有跟着走；
// - 后退键回来，forge / wm 有没有被还原；
// - 还原会再次惊动 forge 和 wm 的订阅者，会不会反过来又 push 一条 —— 那是自己
//   拿自己的历史喂自己，表现为后退键按下去纹丝不动。这个只有跑起来才看得见。
//
// 用的是 route.ts 里那两个真的 store，不是仿制品。

import { build } from 'esbuild';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { dirname, join } from 'node:path';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, '..');

/* ══ 假浏览器。必须在模块加载之前立好：forge 的构造函数会读 localStorage ══ */

const store = new Map();
globalThis.localStorage = {
  getItem: k => (store.has(k) ? store.get(k) : null),
  setItem: (k, v) => store.set(k, String(v)),
  removeItem: k => store.delete(k),
};

const listeners = { popstate: [], hashchange: [] };
const stack = [];
let idx = -1;
let pushes = 0;

const win = {
  location: { hash: '' },
  history: {
    pushState(_s, _t, url) {
      stack.length = idx + 1;
      stack.push(url);
      idx++;
      win.location.hash = url;
      pushes++;
    },
    replaceState(_s, _t, url) {
      if (idx < 0) {
        stack.push(url);
        idx = 0;
      } else {
        stack[idx] = url;
      }
      win.location.hash = url;
    },
  },
  addEventListener: (type, fn) => listeners[type]?.push(fn),
  removeEventListener: (type, fn) => {
    const l = listeners[type];
    if (l) l.splice(l.indexOf(fn), 1);
  },
  /* 桌面形态：窄屏分支会跳过几何，跟路由无关但 wm 要用 */
  matchMedia: () => ({ matches: false, addEventListener() {}, removeEventListener() {} }),
  innerWidth: 1440,
  innerHeight: 900,
};
globalThis.window = win;

const fire = type => listeners[type].forEach(fn => fn());
const back = () => {
  if (idx <= 0) throw new Error('已经在栈底了');
  idx--;
  win.location.hash = stack[idx];
  fire('popstate');
};
const forward = () => {
  if (idx >= stack.length - 1) throw new Error('已经在栈顶了');
  idx++;
  win.location.hash = stack[idx];
  fire('popstate');
};

/* ══ 载入真模块 ══ */

const bundle = await build({
  stdin: {
    contents: `
      export { navigate, startRouting } from './src/forge/route';
      export { forge } from './src/forge/state';
      export { wm } from './src/wm/store';
    `,
    resolveDir: root,
    sourcefile: 'drive-entry.ts',
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

const dir = await mkdtemp(join(tmpdir(), 'brocade-drive-'));
const file = join(dir, 'drive.mjs');
await writeFile(file, bundle.outputFiles[0].text);
const { navigate, startRouting, forge, wm } = await import(pathToFileURL(file).href);

/* ══ 断言 ══ */

let failed = 0;
const LABEL = { nodes: '机器', chains: '应用', deploy: '发布', users: '用户' };

function check(name, actual, expected) {
  const ok = JSON.stringify(actual) === JSON.stringify(expected);
  console.log(
    `${ok ? '✓' : '✗'} ${name}${ok ? '' : `\n    是: ${JSON.stringify(actual)}\n    该: ${JSON.stringify(expected)}`}`,
  );
  if (!ok) failed++;
}

const drillOf = nav => wm.snapshot().wins.find(w => w.key === `tab:${nav}`)?.data.drill;
const goDrill = (nav, drill) => {
  const w = wm.snapshot().wins.find(x => x.key === `tab:${nav}`) ?? wm.open(`tab:${nav}`, LABEL[nav] ?? nav);
  wm.setData(w.id, { ...w.data, drill });
};

wm.init('op-1');
startRouting(nav => LABEL[nav] ?? nav);

console.log('— 开局 —');
check('地址被写上了', win.location.hash, '#/nodes');
check('没白留一条历史', stack.length, 1);

console.log('\n— 状态动，地址跟着动 —');
forge.setNav('chains');
check('切面进了地址', win.location.hash, '#/chains');
goDrill('nodes', { p: 'node', id: 'hk-01' });
check('别的面下钻不动地址', win.location.hash, '#/chains');
forge.setNav('nodes');
check('切回来带着下钻', win.location.hash, '#/nodes/node/hk-01');
goDrill('nodes', { p: 'list' });
check('退回列表', win.location.hash, '#/nodes');

console.log('\n— 不是导航的动作不留脚印 —');
const before = pushes;
wm.fitHeight(wm.snapshot().wins.find(w => w.key === 'tab:nodes').id, 600);
forge.setDiag(true);
forge.setDiag(false);
check('量高和气泡都没 push', pushes - before, 0);

console.log('\n— 后退 —');
/* 此刻栈：#/nodes → #/chains → #/nodes/node/hk-01 → #/nodes */
check('栈的形状', stack, ['#/nodes', '#/chains', '#/nodes/node/hk-01', '#/nodes']);
const pushesBeforeBack = pushes;
back();
check('退到了机器详情', win.location.hash, '#/nodes/node/hk-01');
check('nav 还原', forge.snapshot().nav, 'nodes');
check('下钻还原', drillOf('nodes'), { p: 'node', id: 'hk-01' });
check('还原没有反过来 push', pushes - pushesBeforeBack, 0);

back();
check('再退到应用面', win.location.hash, '#/chains');
check('nav 还原成 chains', forge.snapshot().nav, 'chains');

console.log('\n— 前进 —');
forward();
check('前进回机器详情', win.location.hash, '#/nodes/node/hk-01');
check('nav 是 nodes', forge.snapshot().nav, 'nodes');
check('下钻还在', drillOf('nodes'), { p: 'node', id: 'hk-01' });

console.log('\n— 退到底再往前推新的一条 —');
while (idx > 0) back();
check('回到栈底', win.location.hash, '#/nodes');
forge.setNav('deploy');
goDrill('deploy', { p: 'detail', id: 12 });
check('新分支', win.location.hash, '#/deploy/detail/12');
check('旧的前进历史被截断', stack, ['#/nodes', '#/deploy', '#/deploy/detail/12']);
back();
check('退回发布列表', win.location.hash, '#/deploy');
check('drill 回到 list', drillOf('deploy'), { p: 'list' });

console.log('\n— 手改地址栏 —');
win.location.hash = '#/chains/chain/app-1/c-a';
fire('hashchange');
check('nav 跟过去', forge.snapshot().nav, 'chains');
check('下钻跟过去', drillOf('chains'), { p: 'chain', app: 'app-1', chain: 'c-a' });

console.log('\n— 深链进向导 —');
win.location.hash = '#/nodes/provision';
fire('hashchange');
check('落在第一步', drillOf('nodes'), { p: 'provision', step: 1 });

console.log('\n— 机器建好之后那几屏挂在 node_id 上 —');
goDrill('nodes', { p: 'install', node: 'hk-01', step: 2, result: { revision_id: 9 } });
check('装机进地址', win.location.hash, '#/nodes/install/hk-01');
check('步号不进地址', stack[idx], '#/nodes/install/hk-01');
win.location.hash = '#/nodes/install/hk-01';
fire('hashchange');
check('深链还原到安装命令那屏', drillOf('nodes'), { p: 'install', step: 4, node: 'hk-01' });

console.log('\n— 点导航 = 回这一面的首页 —');
navigate('nodes', { p: 'node', id: 'hk-01' });
check('带 drill 的导航停在那一层', win.location.hash, '#/nodes/node/hk-01');
check('drill 落进了窗', drillOf('nodes'), { p: 'node', id: 'hk-01' });

const pushesBeforeTabs = pushes;
navigate('chains');
check('切去应用面', win.location.hash, '#/chains');
navigate('nodes');
check('点回机器落在列表，不是上次那台', win.location.hash, '#/nodes');
check('窗里的下钻跟着重置', drillOf('nodes'), { p: 'list' });
check('一次点击只留一条历史', pushes - pushesBeforeTabs, 2);

back();
check('后退回应用面', win.location.hash, '#/chains');
back();
check('再退一步回到刚才那台机器', win.location.hash, '#/nodes/node/hk-01');
check('下钻被还原', drillOf('nodes'), { p: 'node', id: 'hk-01' });

console.log('\n— 跨面跳转：切过去，并停在那一层 —');
navigate('deploy', { p: 'plan', revision: 42 });
check('nav 真的切了', forge.snapshot().nav, 'deploy');
check('地址只写到 plan 这一段', win.location.hash, '#/deploy/plan');
check('不进地址的字段留在窗里', drillOf('deploy'), { p: 'plan', revision: 42 });

console.log('\n— 已经在这一面，再点一次 —');
navigate('deploy');
check('回到发布列表', drillOf('deploy'), { p: 'list' });
check('地址回到面的根', win.location.hash, '#/deploy');

console.log('\n— 面包屑不留上一个位置 —');
const deployWin = wm.snapshot().wins.find(w => w.key === 'tab:deploy');
wm.setData(deployWin.id, { ...deployWin.data, crumb: [{ label: '#12' }] });
navigate('deploy', { p: 'detail', id: 7 });
check('导航把 crumb 清空，交给面板重写', wm.snapshot().wins.find(w => w.key === 'tab:deploy').data.crumb, []);

await rm(dir, { recursive: true, force: true });

if (failed) {
  console.error(`\n${failed} 条不对。`);
  process.exit(1);
}
console.log('\n全过了。');
