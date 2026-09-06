// 建链向导端口撞车的自检。跑法：node scripts/ports-check.mjs
//
// 端口撞了会让 Xray 无法启动，而配置在界面上看着仍然合理，因此必须在按下「创建」
// 之前拦住。模型 ID 已由 model-id.ts 独立生成和校验，不属于这份端口测试。
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

const dir = await mkdtemp(join(tmpdir(), 'brocade-ports-'));
const file = join(dir, 'ports.mjs');
await writeFile(file, bundle.outputFiles[0].text);
const { freePortAcross, occupiedPorts, portClash } = await import(pathToFileURL(file).href);

let failed = 0;
const eq = (name, got, want) => {
  const g = JSON.stringify(got);
  const w = JSON.stringify(want);
  if (g === w) return;
  failed += 1;
  console.log(`✗ ${name}\n    实际 ${g}\n    期望 ${w}`);
};

/* ══ 端口占用 ══ */
const ingress = (id, chain, node, port) => ({
  id,
  chain,
  node,
  bind: '0.0.0.0',
  port,
  wires: { vless: { kind: 'vless-reality', dest: '', server_names: [] }, hysteria2: null },
});
/* Hysteria 2 收在 UDP 上，跟同号的 TCP 不是一个口。
   监听口写在 `hysteria2.port` 里，不是外层那个 `ingress.port`——两条线共用一个号的年代
   这份夹具只给外层填了数，于是 UDP 那一支查出来永远是空的，测试也就一直在测空气。 */
const quicIngress = (id, chain, node, port) => ({
  ...ingress(id, chain, node, port),
  wires: {
    vless: null,
    hysteria2: { port, hop: null, bandwidth: {}, congestion: 'brutal', obfs: null, masquerade: { kind: 'not-found' } },
  },
});
const step = (chain, node, hopPort) => ({
  chain,
  node,
  accept: null,
  hop_in: hopPort ? { port: hopPort, security: { t: 'none' } } : null,
  rules: [],
});

const apps = [
  {
    id: 'app-a',
    label: 'A',
    chains: [{ id: 'chn-a1b2-c3d4', tenant: 't', name: '', spine: ['hk', 'jb'] }],
    ingresses: [ingress('ing-a1b2', 'chn-a1b2-c3d4', 'hk', 443)],
    steps: [step('chn-a1b2-c3d4', 'hk', null), step('chn-a1b2-c3d4', 'jb', 20000)],
    grants: [],
  },
  {
    id: 'app-b',
    label: 'B',
    chains: [
      { id: 'chn-e5f6-789a', tenant: 't', name: '', spine: ['hk'] },
      { id: 'chn-0a1b-2c3d', tenant: 't', name: '', spine: ['hk'] },
    ],
    /* 别的视图在同一台机器上占了口——端口是跨视图共享的。
       ing-0a1b 跟 ing-e5f6 同号，但一个 UDP 一个 TCP，互不相干。 */
    ingresses: [ingress('ing-e5f6', 'chn-e5f6-789a', 'hk', 444), quicIngress('ing-0a1b', 'chn-0a1b-2c3d', 'hk', 444)],
    steps: [step('chn-e5f6-789a', 'jb', 20001)],
    grants: [],
  },
];
const nodes = [
  { node_id: 'hk', wg_fake_tcp_port: 39743 },
  { node_id: 'jb', wg_fake_tcp_port: null },
];
const system = {
  nodes: [
    { id: 'hk', wireguard: { listen_port: 51820 } },
    { id: 'jb', wireguard: null },
  ],
};

// 占用要分协议看。一个号在 TCP 上和在 UDP 上是两个口，内核也允许两边各绑一份：
// wg 和 Hysteria 2 只吃 UDP，中转口、phantun 的伪 TCP 口和别的接入面只吃 TCP。
// 合起来查会报出根本不存在的冲突——「51820 被 WireGuard 占着」，而 WireGuard 压根不在 TCP 上。
const taken = occupiedPorts(apps, nodes, system);
const takenUdp = occupiedPorts(apps, nodes, system, undefined, 'udp');
const portsOn = node => [...(taken.get(node) ?? new Map()).keys()].sort((a, b) => a - b);
const udpPortsOn = node => [...(takenUdp.get(node) ?? new Map()).keys()].sort((a, b) => a - b);
eq('hk 上被占的 TCP 口', portsOn('hk'), [443, 444, 39743]);
eq('hk 上被占的 UDP 口', udpPortsOn('hk'), [444, 51820]);
eq('jb 上被占的口', portsOn('jb'), [20000, 20001]);
eq('接入面算进去', taken.get('hk').get(443), '线路 app-a 的接入面 ing-a1b2 VLESS');
eq('跨视图的接入面也算', taken.get('hk').get(444), '线路 app-b 的接入面 ing-e5f6 VLESS');
eq('WireGuard 只算在 UDP 上', takenUdp.get('hk').get(51820), 'WireGuard');
eq('WireGuard 不占 TCP', taken.get('hk').has(51820), false);
eq('hy2 接入面只算在 UDP 上', takenUdp.get('hk').get(444), '线路 app-b 的接入面 ing-0a1b');
eq('hy2 接入面不占 TCP', taken.get('hk').get(444), '线路 app-b 的接入面 ing-e5f6 VLESS');
eq('phantun 算进去（它就是 TCP）', taken.get('hk').get(39743), 'phantun 伪 TCP 口');
eq('phantun 不占 UDP', takenUdp.get('hk').has(39743), false);
eq('中转口算进去', taken.get('jb').get(20000), '线路 app-a 链 chn-a1b2-c3d4 的中转口');
eq('中转口不占 UDP', udpPortsOn('jb'), []);

/* ══ 默认值避让 ══ */
eq('接入面默认跳过 443/444', freePortAcross(taken, ['hk'], 443), 445);
eq('中转口默认跳过 20000/20001', freePortAcross(taken, ['jb'], 20000), 20002);
/* 中转口对 spine[1..] 是同一个数，只在其中一台上空着不算数 */
eq('多台一起看', freePortAcross(taken, ['hk', 'jb'], 20000), 20002);
eq('没有中转节点时原样返回', freePortAcross(taken, [], 20000), 20000);
eq('干净机器就是起点', freePortAcross(new Map(), ['zz'], 443), 443);

eq('撞了要说出占用者', portClash(taken, ['hk'], 443), 'hk 的 443 已经被线路 app-a 的接入面 ing-a1b2 VLESS占着');
eq('没撞是 null', portClash(taken, ['hk'], 445), null);
eq(
  '多台里任一台撞都算',
  portClash(taken, ['hk', 'jb'], 20000),
  'jb 的 20000 已经被线路 app-a 链 chn-a1b2-c3d4 的中转口占着',
);

// ══ 改已有接入面的端口：得先把它自己摘出去 ══
// 不摘的话它占着的正是它当前那个口，编辑框一打开就报「跟自己撞了」，保存键永远是灰的。
const editing = occupiedPorts(apps, nodes, system, 'ing-a1b2');
eq('摘掉自己之后 443 空出来', portClash(editing, ['hk'], 443), null);
eq('别人占的 444 照样拦', portClash(editing, ['hk'], 444), 'hk 的 444 已经被线路 app-b 的接入面 ing-e5f6 VLESS占着');
// wg 的口在 TCP 上不拦（它不在 TCP 上），在 UDP 上照拦。这两条是一起的：
// 少了前一条，一个合法的 TCP 接入面被挡住；少了后一条，一个真的会绑不上的口被放行。
eq('wg 的口不拦 TCP 接入面', portClash(editing, ['hk'], 51820), null);
eq(
  'wg 的口照拦 UDP 接入面',
  portClash(occupiedPorts(apps, nodes, system, 'ing-a1b2', 'udp'), ['hk'], 51820),
  'hk 的 51820 已经被WireGuard占着',
);
eq('不摘的话跟自己撞', portClash(taken, ['hk'], 443), 'hk 的 443 已经被线路 app-a 的接入面 ing-a1b2 VLESS占着');

/* ══ 系统层取不到时不能炸 ══ */
const noSystem = occupiedPorts(apps, nodes, undefined);
eq(
  '没有编译结果时 wg 口查不到',
  occupiedPorts(apps, nodes, undefined, undefined, 'udp').get('hk')?.has(51820) ?? false,
  false,
);
eq('没有编译结果时接入面照算', noSystem.get('hk').get(443), '线路 app-a 的接入面 ing-a1b2 VLESS');
eq('system 是别的形状也不炸', occupiedPorts([], [], { nodes: 'nope' }).size, 0);

await rm(dir, { recursive: true, force: true });

if (failed) {
  console.error(`\n${failed} 项不通过`);
  process.exit(1);
}
console.log('ports: 全部通过');
