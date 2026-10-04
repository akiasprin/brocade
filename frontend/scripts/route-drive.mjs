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

const listeners = { popstate: [], hashchange: [], beforeunload: [], pagehide: [] };
const documentListeners = { scroll: [] };
const stack = [];
const states = [];
let idx = -1;
let pushes = 0;
let nextFrame = 1;
let frames = [];
let clock = 0;
let nextTimer = 1;
const timers = new Map();
const nativePerformanceNow = Object.getOwnPropertyDescriptor(performance, 'now');
Object.defineProperty(performance, 'now', { configurable: true, value: () => clock });
const advanceTime = milliseconds => {
  const end = clock + milliseconds;
  for (;;) {
    const next = [...timers].filter(([, timer]) => timer.at <= end).sort((a, b) => a[1].at - b[1].at)[0];
    if (!next) break;
    const [id, timer] = next;
    timers.delete(id);
    clock = timer.at;
    timer.callback();
  }
  clock = end;
};

const historyCalls = [];
let historyFailure = null;
let historyEngine = 'none';
let limiterStart = 0;
let limiterCount = 0;
const admitHistory = api => {
  historyCalls.push({ api, at: clock });
  if (historyFailure?.api === api) {
    if (historyFailure.mode === 'throw') throw new DOMException('History write rejected', 'SecurityError');
    return false;
  }
  if (historyEngine === 'none') return true;
  if (clock - limiterStart > 10_000) {
    limiterStart = clock;
    limiterCount = 0;
  }
  if (++limiterCount <= (historyEngine === 'webkit' ? 100 : 200)) return true;
  if (historyEngine === 'webkit') throw new DOMException('History write rate exceeded', 'SecurityError');
  return false;
};

class FakeHTMLElement {
  constructor(classes = []) {
    this.classes = new Set(classes);
    this.classList = { contains: name => this.classes.has(name) };
    this.dataset = {};
    this.scrollTop = 0;
    this.scrollHeight = 4_000;
    this.clientHeight = 600;
  }

  focus() {
    if (document.activeElement === this) return;
    document.activeElement = this;
  }
}

class FakePopStateEvent {
  constructor(state) {
    this.state = state;
  }
}

globalThis.HTMLElement = FakeHTMLElement;
globalThis.PopStateEvent = FakePopStateEvent;

const documentScroll = process.argv.includes('--document-scroll');
const desk = new FakeHTMLElement(['fg-desk']);
const rootScroller = new FakeHTMLElement();
const scroller = documentScroll ? rootScroller : desk;
globalThis.getComputedStyle = element => ({ overflowY: element === desk && documentScroll ? 'visible' : 'auto' });
const surface = new FakeHTMLElement(['fg-view']);
const card = new FakeHTMLElement();
let surfaceMounted = true;
const mutations = new Set();
globalThis.MutationObserver = class {
  constructor(callback) {
    this.callback = callback;
  }
  observe() {
    mutations.add(this.callback);
  }
  disconnect() {
    mutations.delete(this.callback);
  }
};
const rootStyle = { removeProperty() {}, setProperty() {} };
rootScroller.style = rootStyle;
globalThis.document = {
  activeElement: surface,
  documentElement: rootScroller,
  scrollingElement: rootScroller,
  querySelector: selector =>
    selector === '.fg-desk' ? desk : selector === '.fg-view, .fg-topo' && surfaceMounted ? surface : null,
  addEventListener: (type, fn) => documentListeners[type]?.push(fn),
  removeEventListener: (type, fn) => {
    const list = documentListeners[type];
    if (list) list.splice(list.indexOf(fn), 1);
  },
};

const normalizedHash = url => {
  if (typeof url !== 'string' || url === '') return win.location.hash;
  if (url.startsWith('#')) return url;
  return new URL(url, 'http://localhost/').hash;
};

const setLocation = url => {
  const hash = normalizedHash(url);
  win.location.hash = hash;
  win.location.href = `http://localhost/${hash}`;
};

const flushFrames = onFrame => {
  let frame = 0;
  while (frames.length > 0) {
    const current = frames;
    frames = [];
    current.forEach(({ callback }) => callback());
    frame++;
    onFrame?.(frame);
  }
};

const fire = (type, event = {}) => listeners[type]?.forEach(fn => fn(event));
const traverse = (delta, onFrame) => {
  const next = idx + delta;
  if (next < 0 || next >= stack.length) return;
  idx = next;
  setLocation(stack[idx]);
  fire('popstate', new FakePopStateEvent(states[idx]));
  // Browsers also emit hashchange when a history traversal changes the fragment. The router must
  // treat that pair as one movement, especially while it is undoing a rejected dirty-form back.
  fire('hashchange');
  flushFrames(onFrame);
};

const win = {
  location: { hash: '', href: 'http://localhost/' },
  history: {
    get state() {
      return idx < 0 ? null : states[idx];
    },
    scrollRestoration: 'auto',
    pushState(state, _t, url) {
      if (!admitHistory('push')) return;
      stack.length = idx + 1;
      states.length = idx + 1;
      stack.push(normalizedHash(url));
      states.push(state);
      idx++;
      setLocation(url);
      pushes++;
    },
    replaceState(state, _t, url) {
      if (!admitHistory('replace')) return;
      if (idx < 0) {
        stack.push(normalizedHash(url));
        states.push(state);
        idx = 0;
      } else {
        stack[idx] = normalizedHash(url);
        states[idx] = state;
      }
      setLocation(url);
    },
    back: () => traverse(-1),
    go: delta => traverse(delta),
  },
  addEventListener: (type, fn) => listeners[type]?.push(fn),
  removeEventListener: (type, fn) => {
    const l = listeners[type];
    if (l) l.splice(l.indexOf(fn), 1);
  },
  /* 桌面形态：窄屏分支会跳过几何，跟路由无关但 wm 要用 */
  matchMedia: () => ({ matches: false, addEventListener() {}, removeEventListener() {} }),
  requestAnimationFrame: callback => {
    const id = nextFrame++;
    frames.push({ id, callback });
    return id;
  },
  cancelAnimationFrame: id => {
    frames = frames.filter(frame => frame.id !== id);
  },
  setTimeout: (callback, delay = 0) => {
    const id = nextTimer++;
    timers.set(id, { at: clock + delay, callback });
    return id;
  },
  clearTimeout: id => timers.delete(id),
  confirm: () => true,
  innerWidth: 1440,
  innerHeight: 900,
};
globalThis.window = win;

const back = onFrame => {
  if (idx <= 0) throw new Error('已经在栈底了');
  traverse(-1, onFrame);
};
const forward = () => {
  if (idx >= stack.length - 1) throw new Error('已经在栈顶了');
  traverse(1);
};
const rememberScroll = value => {
  scroller.scrollTop = value;
  documentListeners.scroll.forEach(fn => fn({ target: documentScroll ? document : scroller }));
  flushFrames();
};

/* ══ 载入真模块 ══ */

const bundle = await build({
  stdin: {
    contents: `
      export { navigate, navigateInPlace, returnTo, startRouting } from './src/forge/route';
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
const { navigate, navigateInPlace, returnTo, startRouting, forge, wm } = await import(pathToFileURL(file).href);

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
setLocation('#/links');
startRouting(nav => LABEL[nav] ?? nav);
flushFrames();

console.log(`— ${documentScroll ? '触屏文档' : '桌面工作区'}滚动 · 开局 —`);
check('地址被写上了', win.location.hash, '#/nodes');
check('没白留一条历史', stack.length, 1);
check('旧链路页面启动时回到机器列表', forge.snapshot().nav, 'nodes');
check(
  '不为旧链路页面创建窗口',
  wm.snapshot().wins.some(win => win.key === 'tab:links'),
  false,
);

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
setLocation('#/chains/chain/app-1/c-a');
fire('hashchange');
check('nav 跟过去', forge.snapshot().nav, 'chains');
check('下钻跟过去', drillOf('chains'), { p: 'chain', app: 'app-1', chain: 'c-a' });

console.log('\n— 用户深链恢复用户名 —');
setLocation('#/users/user/alice');
fire('hashchange');
check('用户深链切到用户面', forge.snapshot().nav, 'users');
check('用户深链恢复身份', drillOf('users'), { p: 'user', id: 'alice' });

console.log('\n— 深链进向导 —');
setLocation('#/nodes/provision');
fire('hashchange');
check('落在第一步', drillOf('nodes'), { p: 'provision', step: 1 });

console.log('\n— 机器建好之后那几屏挂在 node_id 上 —');
goDrill('nodes', { p: 'install', node: 'hk-01', step: 2, result: { revision_id: 9 } });
check('装机进地址', win.location.hash, '#/nodes/install/hk-01');
check('步号不进地址', stack[idx], '#/nodes/install/hk-01');
setLocation('#/nodes/install/hk-01');
fire('hashchange');
check('同地址的重复事件不抹掉短命步骤', drillOf('nodes'), {
  p: 'install',
  node: 'hk-01',
  step: 2,
  result: { revision_id: 9 },
});

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

console.log('\n— 新导航与返回的滚动语义 —');
navigate('users');
flushFrames();
rememberScroll(260);
navigate('chains');
flushFrames();
check('新页面从顶部开始', scroller.scrollTop, 0);
rememberScroll(404);
const pushesBeforeFreshNavigation = pushes;
navigate('users');
flushFrames();
check('顶栏命中上一页仍创建新导航', pushes - pushesBeforeFreshNavigation, 1);
check('顶栏不会恢复上一页的中段位置', scroller.scrollTop, 0);
const backPaintScrolls = [];
back(() => backPaintScrolls.push(scroller.scrollTop));
check('浏览器后退恢复线路页位置', scroller.scrollTop, 404);
check('浏览器后退在第一帧绘制前恢复位置', backPaintScrolls[0], 404);
const pushesBeforeReturn = pushes;
returnTo('users');
check('显式返回不新增历史', pushes - pushesBeforeReturn, 0);
check('显式返回恢复用户页位置', scroller.scrollTop, 260);

console.log('\n— 用户选择回到顶部，只有历史返回恢复原位置 —');
navigate('users', { p: 'user', id: 'alice' });
flushFrames();
rememberScroll(318);
navigate('users', { p: 'user', id: 'bob' });
scroller.scrollHeight = 620;
const userSelectionPaints = [];
flushFrames(() => userSelectionPaints.push(scroller.scrollTop));
check('用户详情第一帧绘制前回到顶部', userSelectionPaints, [0]);
check('新用户的历史位置为顶部', win.history.state.brocadeRoute.scrollTop, 0);
scroller.scrollHeight = 4_000;
[...mutations].forEach(callback => callback());
advanceTime(4_000);
check('详情随后变长不再恢复上一位用户的位置', scroller.scrollTop, 0);
back();
check('后退恢复原用户与滚动位置', [drillOf('users').id, scroller.scrollTop], ['alice', 318]);
forward();
check('前进到新用户仍停在顶部', [drillOf('users').id, scroller.scrollTop], ['bob', 0]);

console.log('\n— VPN Gate 国家选择 —');
navigate('tunnels', { p: 'vpngate' });
flushFrames();
rememberScroll(318);
navigateInPlace('tunnels', { p: 'vpngate', country: 'JP' });
flushFrames();
check('VPN Gate 原位选择仍保留滚动位置', scroller.scrollTop, 318);
check('国家进入地址', win.location.hash, '#/tunnels/vpngate?country=JP');
navigateInPlace('tunnels', { p: 'vpngate', country: 'VN' });
check('切换国家创建历史', win.location.hash, '#/tunnels/vpngate?country=VN');
back();
check('后退还原上一个国家', drillOf('tunnels'), { p: 'vpngate', country: 'JP' });
forward();
check('前进还原下一个国家', drillOf('tunnels'), { p: 'vpngate', country: 'VN' });
setLocation('#/tunnels/vpngate?country=KR');
fire('hashchange');
check('刷新地址可恢复国家', drillOf('tunnels'), { p: 'vpngate', country: 'KR' });

console.log('\n— 滚动不消耗浏览器导航配额 —');
for (const engine of ['webkit', 'chromium']) {
  navigate('chains');
  flushFrames();
  advanceTime(11_000);
  historyEngine = engine;
  limiterStart = clock;
  limiterCount = 0;
  const callsBeforeScroll = historyCalls.length;
  for (let frame = 0; frame < 600; frame++) {
    rememberScroll((frame + 1) % 2_000);
    advanceTime(1000 / 60);
  }
  card.focus();
  check(`${engine} 连续滚动十秒不写历史`, historyCalls.length - callsBeforeScroll, 0);
  check(`${engine} 滚动后点击成功`, navigate('chains', { p: 'chain', app: 'demo', chain: 'last' }), true);
  flushFrames();
  check(
    `${engine} 页面与 URL 一起进入详情`,
    [drillOf('chains').p, win.location.hash],
    ['chain', '#/chains/chain/demo/last'],
  );
  back();
  check(`${engine} 后退只恢复滚动`, [scroller.scrollTop, document.activeElement === card], [600, false]);
  historyEngine = 'none';
}

console.log('\n— 防抖前直接后退，前进仍恢复最新内存位置 —');
navigate('nodes');
flushFrames();
rememberScroll(731);
check('防抖之前历史仍是旧位置', win.history.state.brocadeRoute.scrollTop, 0);
back();
advanceTime(2_000);
forward();
check('前进读取尚未持久化的最新位置', scroller.scrollTop, 731);

console.log('\n— 异步列表恢复期间不覆盖目标位置 —');
navigate('chains');
flushFrames();
rememberScroll(940);
card.focus();
navigate('nodes');
flushFrames();
scroller.scrollHeight = 800;
surfaceMounted = false;
back();
surfaceMounted = true;
[...mutations].forEach(callback => callback());
check('列表未加载时只临时夹到可滚动高度', scroller.scrollTop, 200);
rememberScroll(200);
advanceTime(1_000);
scroller.scrollHeight = 4_000;
[...mutations].forEach(callback => callback());
check('列表加载后仍恢复完整目标位置', scroller.scrollTop, 940);
check('完成恢复后清理观察器', mutations.size, 0);

console.log('\n— 延迟保存不会跨页，重复位置不再写入 —');
rememberScroll(864);
navigate('users');
flushFrames();
const callsAfterNavigation = historyCalls.length;
advanceTime(2_000);
check('旧防抖回调已取消', historyCalls.length, callsAfterNavigation);
check('旧列表位置没有写进新页面', win.history.state.brocadeRoute.scrollTop, 0);
rememberScroll(205);
advanceTime(2_000);
const callsAfterSave = historyCalls.length;
rememberScroll(205);
advanceTime(2_000);
check('滚动未变时不重复保存', historyCalls.length, callsAfterSave);

console.log('\n— 频繁短滚动也限制持久化频率 —');
const callsBeforeShortScrolls = historyCalls.length;
for (let step = 0; step < 30; step++) {
  rememberScroll(300 + step);
  advanceTime(400);
}
const shortScrollWrites = historyCalls.slice(callsBeforeShortScrolls).filter(call => call.api === 'replace');
check('十二秒短滚动最多写入十三次', shortScrollWrites.length <= 13, true);
check(
  '短滚动写入至少间隔一秒',
  shortScrollWrites.every((call, i) => i === 0 || call.at - shortScrollWrites[i - 1].at >= 1_000),
  true,
);
advanceTime(2_000);
check('尾部防抖最终保存最新位置', win.history.state.brocadeRoute.scrollTop, 329);

console.log('\n— 保存位置失败不能阻断导航 —');
const nativeWarn = console.warn;
const warnings = [];
console.warn = (...args) => warnings.push(args);
for (const mode of ['throw', 'ignore']) {
  navigate('chains');
  flushFrames();
  rememberScroll(942);
  card.focus();
  historyFailure = { api: 'replace', mode };
  advanceTime(2_000);
  check(`${mode} 保存失败后点击仍成功`, navigate('chains', { p: 'chain', app: 'demo', chain: 'last' }), true);
  flushFrames();
  check(`${mode} 保存失败不影响导航地址`, win.location.hash, '#/chains/chain/demo/last');
  returnTo('chains');
  check(`${mode} 返回仍恢复内存滚动位置`, [scroller.scrollTop, document.activeElement === card], [942, false]);
  historyFailure = null;
}

console.log('\n— 导航写入失败保持页面、地址和索引一致 —');
for (const mode of ['throw', 'ignore']) {
  navigate('chains');
  flushFrames();
  rememberScroll(415);
  const oldIndex = win.history.state.brocadeRoute.index;
  const oldLength = stack.length;
  historyFailure = { api: 'push', mode };
  check(`${mode} 导航报告失败`, navigate('nodes', { p: 'node', id: 'hk-01' }), false);
  flushFrames();
  check(
    `${mode} 失败保留原页面、地址、位置和历史`,
    [forge.snapshot().nav, win.location.hash, scroller.scrollTop, stack.length, win.history.state.brocadeRoute.index],
    ['chains', '#/chains', 415, oldLength, oldIndex],
  );
  // 旧页面直接修改 store 的入口也必须在 push 被拒绝后对齐地址。
  forge.setNav('nodes');
  check(`${mode} store 同步失败恢复原路由`, [forge.snapshot().nav, win.location.hash], ['chains', '#/chains']);
  historyFailure = null;
  check(`${mode} 配额恢复后可再次导航`, navigate('nodes', { p: 'node', id: 'hk-01' }), true);
  flushFrames();
  check(`${mode} 失败没有消耗应用历史索引`, win.history.state.brocadeRoute.index, oldIndex + 1);
  back();
  check(`${mode} 重试后仍可后退到原位置`, scroller.scrollTop, 415);
}
check('历史写入失败有诊断信息', warnings.length > 0, true);
console.warn = nativeWarn;

console.log('\n— 新历史分支不能复用废弃的同索引缓存 —');
navigate('nodes');
flushFrames();
rememberScroll(888);
back();
navigate('users');
flushFrames();
advanceTime(2_000);
navigate('deploy');
flushFrames();
back();
check('新分支从自己的顶部恢复', scroller.scrollTop, 0);

console.log('\n— 离开文档补存位置，重新启动可恢复 —');
rememberScroll(517);
card.focus();
fire('pagehide');
check(
  'pagehide 只保存最新滚动',
  [win.history.state.brocadeRoute.scrollTop, Object.hasOwn(win.history.state.brocadeRoute, 'focusKey')],
  [517, false],
);
scroller.scrollTop = 0;
document.activeElement = surface;
startRouting(nav => LABEL[nav] ?? nav);
flushFrames();
check('重新启动从历史快照只恢复滚动', [scroller.scrollTop, document.activeElement === card], [517, false]);

console.log('\n— 安全区弹性回弹与嵌套面板滚动 —');
rememberScroll(-32);
advanceTime(1_000);
check('回弹不会保存负的历史位置', win.history.state.brocadeRoute.scrollTop, 0);
rememberScroll(210);
documentListeners.scroll.forEach(fn => fn({ target: card }));
advanceTime(1_000);
check('嵌套面板滚动不覆盖页面位置', win.history.state.brocadeRoute.scrollTop, 210);

if (nativePerformanceNow) Object.defineProperty(performance, 'now', nativePerformanceNow);
else delete performance.now;
await rm(dir, { recursive: true, force: true });

if (failed) {
  console.error(`\n${failed} 条不对。`);
  process.exit(1);
}
console.log('\n全过了。');
