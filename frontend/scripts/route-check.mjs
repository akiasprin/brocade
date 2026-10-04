// 地址编解码的自检。跑法：node scripts/route-check.mjs
//
// 为什么单独有这么个东西：路由的错法都是安静的——多一层、少一段、数字回来变成
// 字符串，编译器一个都拦不住，界面上表现为「后退之后这一页是空的」，而那时候
// 已经很难看出是编解码的锅。这里直接喂样例、对着断言跑，不需要后端也不需要浏览器。
//
// route.ts 里那两个函数是纯的，但它同一个模块里 import 了 wm / forge（读
// localStorage、matchMedia）。那些在模块加载时都有 typeof 保护，所以打包进 node
// 直接跑得起来；真要哪天跑不起来了，说明有人往顶层加了副作用，那也该知道。

import { build } from 'esbuild';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { dirname, join } from 'node:path';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';

const here = dirname(fileURLToPath(import.meta.url));

const bundle = await build({
  entryPoints: [join(here, '../src/forge/route.ts')],
  bundle: true,
  format: 'esm',
  platform: 'neutral',
  mainFields: ['module', 'main'],
  conditions: ['import', 'default'],
  write: false,
  logLevel: 'silent',
});

const dir = await mkdtemp(join(tmpdir(), 'brocade-route-'));
const file = join(dir, 'route.mjs');
await writeFile(file, bundle.outputFiles[0].text);
const { serialize, parse } = await import(pathToFileURL(file).href);

let failed = 0;
const eq = (a, b) => JSON.stringify(a) === JSON.stringify(b);

/* loc → 地址 → loc，两头都要对得上 */
function trip(name, loc, hash, back = loc) {
  const got = serialize(loc);
  if (got !== hash) {
    console.error(`✗ ${name}\n    写出来: ${got}\n    该是:   ${hash}`);
    failed++;
    return;
  }
  const round = parse(got);
  if (!eq(round, back)) {
    console.error(`✗ ${name} 读回来对不上\n    读回:   ${JSON.stringify(round)}\n    该是:   ${JSON.stringify(back)}`);
    failed++;
    return;
  }
  console.log(`✓ ${name}  ${hash}`);
}

/* 只验一个方向：人手改出来的地址 */
function reads(name, hash, expected) {
  const got = parse(hash);
  if (!eq(got, expected)) {
    console.error(`✗ ${name}\n    读回:   ${JSON.stringify(got)}\n    该是:   ${JSON.stringify(expected)}`);
    failed++;
    return;
  }
  console.log(`✓ ${name}  ${hash} → ${JSON.stringify(got)}`);
}

console.log('— 往返 —');
trip('面的根', { nav: 'nodes' }, '#/nodes');
trip('列表等于根', { nav: 'nodes', drill: { p: 'list' } }, '#/nodes', { nav: 'nodes' });
trip('机器详情', { nav: 'nodes', drill: { p: 'node', id: 'hk-01' } }, '#/nodes/node/hk-01');
trip('没下钻的面', { nav: 'settings' }, '#/settings');
trip('不走 wm 的面', { nav: 'topo' }, '#/topo');
trip('链路详情两段', { nav: 'chains', drill: { p: 'chain', app: 'app-1', chain: 'c-a' } }, '#/chains/chain/app-1/c-a');
trip('建链向导保留所属分组', { nav: 'chains', drill: { p: 'new', app: 'app-1' } }, '#/chains/new/app-1');
trip(
  'WARP 详情带资源类型',
  { nav: 'tunnels', drill: { p: 'warp', id: 'warp-8f3a-2d71' } },
  '#/tunnels/warp/warp-8f3a-2d71',
);
trip(
  '自定义隧道详情带资源类型',
  { nav: 'tunnels', drill: { p: 'custom', id: 'custom-8f3a-2d71' } },
  '#/tunnels/custom/custom-8f3a-2d71',
);
trip('VPN Gate 总页进入浏览器历史', { nav: 'tunnels', drill: { p: 'vpngate' } }, '#/tunnels/vpngate');
trip(
  'VPN Gate 国家选择进入浏览器历史',
  { nav: 'tunnels', drill: { p: 'vpngate', country: 'VN' } },
  '#/tunnels/vpngate?country=VN',
);
trip(
  'VPN Gate 资源进入浏览器历史',
  { nav: 'tunnels', drill: { p: 'vpngate', id: 'vpngate-8f3a-2d71' } },
  '#/tunnels/vpngate/vpngate-8f3a-2d71',
);
trip('用户详情由用户名定位', { nav: 'users', drill: { p: 'user', id: 'alice' } }, '#/users/user/alice');

console.log('\n— 类型要还原 —');
trip('发布详情的 id 是数字', { nav: 'deploy', drill: { p: 'detail', id: 12 } }, '#/deploy/detail/12');
{
  const back = parse('#/deploy/detail/12');
  const ok = typeof back?.drill?.id === 'number';
  console.log(`${ok ? '✓' : '✗'} id 读回来是 ${typeof back?.drill?.id}`);
  if (!ok) failed++;
}

console.log('\n— 短命字段不许进地址 —');
trip('幂等键和修订不进地址', { nav: 'deploy', drill: { p: 'plan', key: 'abc123', revision: 7 } }, '#/deploy/plan', {
  nav: 'deploy',
  drill: { p: 'plan' },
});
trip(
  '向导只留入口，回来是第一步',
  { nav: 'nodes', drill: { p: 'provision', step: 4, result: { node: {} } } },
  '#/nodes/provision',
  { nav: 'nodes', drill: { p: 'provision', step: 1 } },
);

console.log('\n— 带斜杠的 id 要能活下来 —');
trip(
  '租户取完整路径',
  { nav: 'chains', drill: { p: 'chain', app: 'acme/sub', chain: 'c-a' } },
  '#/chains/chain/acme%2Fsub/c-a',
);
console.log('\n— 坏地址不许炸出坏状态 —');
reads('不认识的面', '#/nonsense', null);
reads('已移除的链路与 MTU 页面不可路由', '#/links', null);
reads('空地址', '#/', null);
reads('什么都没有', '', null);
reads('不认识的下钻', '#/nodes/bogus', { nav: 'nodes' });
reads('详情少了 id', '#/deploy/detail', { nav: 'deploy' });
reads('机器详情少了 id', '#/nodes/node', { nav: 'nodes' });
reads('WARP 详情少了资源 id', '#/tunnels/warp', { nav: 'tunnels' });
reads('自定义详情少了资源 id', '#/tunnels/custom', { nav: 'tunnels' });
reads('旧的通用隧道地址已禁用', '#/tunnels/tunnel/warp-8f3a-2d71', { nav: 'tunnels' });
reads('旧的租户隧道地址已禁用', '#/tunnels/tunnel/platform/warp-8f3a-2d71', { nav: 'tunnels' });
reads('资源类型与 id 前缀不一致', '#/tunnels/warp/custom-8f3a-2d71', { nav: 'tunnels' });
reads('链式代理地址暂不开放', '#/tunnels/front/app-main/front-asia', { nav: 'tunnels' });
reads('用户详情少了用户名', '#/users/user', { nav: 'users' });
reads('用户详情多出路径段', '#/users/user/acme/alice', { nav: 'users' });
reads('机器详情多出路径段', '#/nodes/node/hk-01/extra', { nav: 'nodes' });

console.log('\n— 缺字段的 loc 退回面的根 —');
trip('detail 没带 id', { nav: 'deploy', drill: { p: 'detail' } }, '#/deploy', { nav: 'deploy' });

await rm(dir, { recursive: true, force: true });

if (failed) {
  console.error(`\n${failed} 条不对。`);
  process.exit(1);
}
console.log('\n全过了。');
