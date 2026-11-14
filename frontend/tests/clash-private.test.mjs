// Production browser parser and client extension export regression tests; no proxy connections.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import vm from 'node:vm';
import { mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { build } from 'esbuild';

const bundle = await build({
  entryPoints: ['src/panes/clash-rules/private-export.ts'],
  bundle: true,
  platform: 'browser',
  format: 'esm',
  write: false,
  logLevel: 'silent',
});
const temporary = mkdtempSync(path.join(tmpdir(), 'brocade-private-check-'));
const moduleFile = path.join(temporary, 'private-export.mjs');
writeFileSync(moduleFile, bundle.outputFiles[0].text);
let parser;
try {
  parser = await import(pathToFileURL(moduleFile));
} finally {
  rmSync(temporary, { recursive: true, force: true });
}
const { parsePrivateNode, parsePrivateNodes, privateChainScript: renderChainScript } = parser;
const PRIVATE_SAMPLE = 'socks5://demo:demo-password@exit.example.invalid:1080#我的美国出口';
const PRIVATE_SAMPLES = [
  `vless://11111111-1111-4111-8111-111111111111@us-exit.example.invalid:443?security=reality&type=tcp&flow=xtls-rprx-vision&sni=www.example.invalid&pbk=${'A'.repeat(43)}&sid=0123456789abcdef&fp=chrome#我的美国Reality出口`,
  'hy2://demo-password@jp-exit.example.invalid:443?sni=jp-exit.example.invalid&obfs=salamander&obfs-password=demo-obfs#我的日本Hy2出口',
];
const PRIVATE_SELECTION = { id: 'select', yaml: '🚀 节点选择' };
const privateChainScript = (nodes, front, service, exitMode = 'select') =>
  renderChainScript(
    nodes,
    {
      name:
        front === 'auto'
          ? '⚡ 自动选择'
          : front === 'select'
            ? '🚀 节点选择'
            : front === 'line:hk1'
              ? '🇭🇰 香港 01'
              : '🇺🇸 美国 01',
      kind: front.startsWith('line:') ? 'line' : 'group',
      selection: front === 'select',
    },
    service,
    '🚀 节点选择',
    exitMode,
  );
const ai = { name: 'AI 服务', yaml: '🤖 AI 服务' };
const fixture = () => ({
  proxies: [{ name: '🇭🇰 香港 01', type: 'socks5', server: 'hk.example.invalid', port: 1080 }],
  'proxy-groups': [
    { name: '🚀 节点选择', type: 'select', proxies: ['⚡ 自动选择', '🇭🇰 香港 01'] },
    { name: '⚡ 自动选择', type: 'url-test', proxies: ['🇭🇰 香港 01'] },
    { name: ai.yaml, type: 'select', proxies: ['🚀 节点选择'] },
  ],
  rules: [
    'DOMAIN-SUFFIX,example.com,DIRECT',
    'RULE-SET,openai,🤖 AI 服务',
    'IP-CIDR,1.1.1.0/24,🤖 AI 服务,no-resolve',
    'RULE-SET,youtube,📹 油管视频',
    'MATCH,🐟 漏网之鱼',
  ],
});
const execute = (script, config) => {
  const context = vm.createContext({});
  vm.runInContext(script, context, { timeout: 1000 });
  return context.main(config);
};

test('SOCKS5 credentials are decoded locally, including Unicode and IPv6', () => {
  const node = parsePrivateNode('socks5://alice:p%40ss%3Aword@[2001:db8::1]:1080#%E7%A7%81%E6%9C%89%E5%87%BA%E5%8F%A3');
  assert.equal(node.server, '2001:db8::1');
  assert.equal(node.password, 'p@ss:word');
  assert.equal(node.name, '私有出口');
});
test('HTTPS preserves TLS and Trojan uses the credential before @ as password', () => {
  assert.deepEqual(parsePrivateNode('https://user:pass@exit.example.invalid'), {
    name: '我的自建出口',
    type: 'http',
    server: 'exit.example.invalid',
    port: 443,
    username: 'user',
    password: 'pass',
    tls: true,
    sni: 'exit.example.invalid',
  });
  const node = parsePrivateNode('trojan://p%40ss@exit.example.invalid:443?sni=tls.example.invalid#US');
  assert.equal(node.password, 'p@ss');
  assert.equal(node.sni, 'tls.example.invalid');
});
test('malformed or unsupported links fail without reflecting credentials in errors', () => {
  for (const value of [
    '',
    'not-a-link',
    'vless://SECRET@host:443',
    'socks5://u:SECRET@host',
    'socks5://u@host:1080',
    'socks5://u:SECRET@host:1080?unsupported=1',
    'socks5://host:1080\nsocks5://other:1080',
    'trojan://host:443',
  ]) {
    assert.throws(
      () => parsePrivateNode(value),
      error => !error.message.includes('SECRET'),
    );
  }
});
for (const [front, expected] of [
  ['select', '🚀 节点选择'],
  ['auto', '⚡ 自动选择'],
  ['line:hk1', '🇭🇰 香港 01'],
]) {
  test(`export preserves original proxies and wires the private exit through ${front}`, () => {
    const input = fixture();
    const before = JSON.stringify(input);
    const result = execute(privateChainScript(parsePrivateNode(PRIVATE_SAMPLE), front, ai), input);
    assert.equal(JSON.stringify(input), before);
    assert.equal(result.proxies.length, 2);
    assert.equal(result.proxies[1]['dialer-proxy'], expected);
    assert.equal(result.proxies[1].password, 'demo-password');
    assert.equal(result.rules[0], input.rules[0]);
    assert.equal(result.rules[1], 'RULE-SET,openai,链式代理');
    assert.equal(result.rules[2], 'IP-CIDR,1.1.1.0/24,链式代理,no-resolve');
    assert.equal(result.rules[3], input.rules[3]);
    assert.equal(result.rules[4], input.rules[4]);
    assert.equal(result['proxy-groups'].find(group => group.name === '链式代理').proxies[0], result.proxies[1].name);
    assert(
      !result['proxy-groups'].find(group => group.name === '🚀 节点选择').proxies.includes(result.proxies[1].name),
    );
  });
}
test('missing front, disabled module and duplicate installation fail before mutation', () => {
  const script = privateChainScript(parsePrivateNode(PRIVATE_SAMPLE), 'line:us1', ai);
  const input = fixture();
  const before = JSON.stringify(input);
  assert.throws(() => execute(script, input), /找不到所选前置线路/);
  assert.equal(JSON.stringify(input), before);
  const noRules = { ...fixture(), rules: ['MATCH,DIRECT'] };
  assert.throws(
    () => execute(privateChainScript(parsePrivateNode(PRIVATE_SAMPLE), 'select', ai), noRules),
    /没有启用的规则/,
  );
  const installed = execute(privateChainScript(parsePrivateNode(PRIVATE_SAMPLE), 'select', ai), fixture());
  assert.throws(
    () => execute(privateChainScript(parsePrivateNode(PRIVATE_SAMPLE), 'select', ai), installed),
    /名称冲突/,
  );
});
test('a proxy named like a built-in group cannot hijack the selected client strategy', () => {
  const input = fixture();
  input.proxies.push({ name: '🚀 节点选择', type: 'socks5', server: 'other.example.invalid', port: 1080 });
  input['proxy-groups'][0].name = '🚀 节点选择 · Brocade 2';
  const result = execute(privateChainScript(parsePrivateNode(PRIVATE_SAMPLE), 'select', ai), input);
  assert.equal(result.proxies.at(-1)['dialer-proxy'], '🚀 节点选择 · Brocade 2');
});
test('node names are data, and exports perform no network or browser storage operations', () => {
  const node = { ...parsePrivateNode(PRIVATE_SAMPLE), name: '\"; globalThis.injected = true; //' };
  const script = privateChainScript(node, 'select', ai);
  const context = vm.createContext({});
  vm.runInContext(script, context, { timeout: 1000 });
  assert.equal(context.injected, undefined);
  assert(!/fetch\(|XMLHttpRequest|localStorage|sessionStorage|sendBeacon/.test(script));
  assert.equal(context.main(fixture()).proxies[1].server, 'exit.example.invalid');
});

test('multiple exits remain parallel choices and receive distinct names', () => {
  const nodes = PRIVATE_SAMPLES.map(parsePrivateNode);
  nodes[1].name = nodes[0].name;
  const input = fixture();
  const result = execute(privateChainScript(nodes, 'auto', ai), input);
  assert.equal(result.proxies.length, 3);
  assert.notEqual(result.proxies[1].name, result.proxies[2].name);
  assert.equal(result.proxies[1]['dialer-proxy'], '⚡ 自动选择');
  assert.equal(result.proxies[2]['dialer-proxy'], '⚡ 自动选择');
  assert.equal(result['proxy-groups'].find(group => group.name === '链式代理').type, 'select');
  assert.equal(result['proxy-groups'].find(group => group.name === '链式代理').proxies.length, 2);
});
test('automatic exit choice tests the generated chains while keeping platform auto untouched', () => {
  const input = fixture();
  const before = JSON.stringify(input);
  const result = execute(privateChainScript(PRIVATE_SAMPLES.map(parsePrivateNode), 'auto', ai, 'url-test'), input);
  const chain = result['proxy-groups'].find(group => group.name === '链式代理');
  assert.equal(chain.type, 'url-test');
  assert.equal(chain.interval, 300);
  assert.equal(chain.proxies.length, 2);
  assert.equal(JSON.stringify(input), before);
  assert.deepEqual(
    result['proxy-groups'].find(group => group.name === '⚡ 自动选择'),
    input['proxy-groups'][1],
  );
});
test('apply to node selection adds one private chain choice without rewriting routing rules', () => {
  const input = fixture();
  const before = JSON.stringify(input);
  const result = execute(privateChainScript(PRIVATE_SAMPLES.map(parsePrivateNode), 'auto', PRIVATE_SELECTION), input);
  assert.equal(JSON.stringify(input), before);
  assert.deepEqual(result.rules, input.rules);
  assert.equal(result['proxy-groups'].find(group => group.name === '🚀 节点选择').proxies[0], '链式代理');
  assert.deepEqual(
    Array.from(result['proxy-groups'].find(group => group.name === '🚀 节点选择').proxies.slice(1)),
    input['proxy-groups'][0].proxies,
  );
  assert.deepEqual(
    result['proxy-groups'].find(group => group.name === '⚡ 自动选择'),
    input['proxy-groups'][1],
  );
  assert.equal(result['proxy-groups'].find(group => group.name === '链式代理').proxies.length, 2);
});
test('chain group appears immediately before node selection while other groups keep order', () => {
  for (const service of [ai, PRIVATE_SELECTION]) {
    const input = fixture();
    input['proxy-groups'].unshift({ name: '已有前置组', type: 'select', proxies: ['🇭🇰 香港 01'] });
    const before = JSON.stringify(input);
    const result = execute(privateChainScript(PRIVATE_SAMPLES.map(parsePrivateNode), 'auto', service), input);
    assert.deepEqual(
      Array.from(result['proxy-groups'], group => group.name),
      ['已有前置组', '链式代理', '🚀 节点选择', '⚡ 自动选择', '🤖 AI 服务'],
    );
    assert.equal(JSON.stringify(input), before);
  }
});
test('chain order also handles the collision-renamed node selection group', () => {
  const input = fixture();
  input['proxy-groups'][0].name = '🚀 节点选择 · Brocade 2';
  const result = execute(privateChainScript(PRIVATE_SAMPLES.map(parsePrivateNode), 'auto', ai), input);
  assert.equal(result['proxy-groups'][0].name, '链式代理');
  assert.equal(result['proxy-groups'][1].name, '🚀 节点选择 · Brocade 2');
});
test('exported proxy names use the exact two-digit chain naming format', () => {
  const result = execute(
    privateChainScript(PRIVATE_SAMPLES.map(parsePrivateNode), 'auto', PRIVATE_SELECTION),
    fixture(),
  );
  assert.equal(result.proxies[1].name, '链式代理 01 | 我的美国Reality出口');
  assert.equal(result.proxies[2].name, '链式代理 02 | 我的日本Hy2出口');
  assert.deepEqual(Array.from(result['proxy-groups'][0].proxies), [
    '链式代理 01 | 我的美国Reality出口',
    '链式代理 02 | 我的日本Hy2出口',
  ]);
  assert(!JSON.stringify(result).includes('本地出口'));
});
test('node selection rejects a direct loop before export', () => {
  assert.throws(
    () => privateChainScript(PRIVATE_SAMPLES.map(parsePrivateNode), 'select', PRIVATE_SELECTION),
    /循环代理/,
  );
});
test('node selection rejects indirect front dependencies without mutating config', () => {
  const input = fixture();
  input['proxy-groups'][1].proxies.push('🚀 节点选择');
  const before = JSON.stringify(input);
  assert.throws(
    () => execute(privateChainScript(PRIVATE_SAMPLES.map(parsePrivateNode), 'auto', PRIVATE_SELECTION), input),
    /循环代理/,
  );
  assert.equal(JSON.stringify(input), before);
  const proxyLoop = fixture();
  proxyLoop.proxies[0]['dialer-proxy'] = '🚀 节点选择';
  assert.throws(
    () => execute(privateChainScript(parsePrivateNode(PRIVATE_SAMPLE), 'line:hk1', PRIVATE_SELECTION), proxyLoop),
    /循环代理/,
  );
});
test('empty exit lists cannot produce an unusable export', () => {
  assert.throws(() => privateChainScript([], 'auto', PRIVATE_SELECTION), /至少一个/);
});

const USER_ID = '11111111-1111-4111-8111-111111111111';
const encode = value => Buffer.from(value).toString('base64url');
test('VLESS Reality preserves flow, SNI, public key, short ID and fingerprint', () => {
  const node = parsePrivateNode(PRIVATE_SAMPLES[0]);
  assert.equal(node.type, 'vless');
  assert.equal(node.uuid, USER_ID);
  assert.equal(node.flow, 'xtls-rprx-vision');
  assert.equal(node.servername, 'www.example.invalid');
  assert.equal(node['reality-opts']['public-key'], 'A'.repeat(43));
  assert.equal(node['reality-opts']['short-id'], '0123456789abcdef');
  assert.equal(node['client-fingerprint'], 'chrome');
});
test('VLESS WebSocket TLS preserves host, path, ALPN and early data', () => {
  const node = parsePrivateNode(
    `vless://${USER_ID}@ws.example.invalid:443?security=tls&type=ws&path=%2Fproxy%3Fa%3D1&host=cdn.example.invalid&sni=tls.example.invalid&alpn=h2%2Chttp%2F1.1&ed=2048&eh=Sec-WebSocket-Protocol#WS`,
  );
  assert.equal(node.network, 'ws');
  assert.equal(node['ws-opts'].path, '/proxy?a=1');
  assert.equal(node['ws-opts'].headers.Host, 'cdn.example.invalid');
  assert.equal(node['ws-opts']['max-early-data'], 2048);
  assert.deepEqual(node.alpn, ['h2', 'http/1.1']);
});
for (const [kind, query, expected] of [
  ['grpc', 'serviceName=example-service', { 'grpc-opts': { 'grpc-service-name': 'example-service' } }],
  [
    'h2',
    'host=a.example.invalid%2Cb.example.invalid&path=%2Fh2',
    { 'h2-opts': { host: ['a.example.invalid', 'b.example.invalid'], path: '/h2' } },
  ],
  [
    'xhttp',
    'mode=stream-up&host=cdn.example.invalid&path=%2Fxhttp',
    { 'xhttp-opts': { mode: 'stream-up', host: 'cdn.example.invalid', path: '/xhttp' } },
  ],
])
  test(`VLESS ${kind} preserves transport fields`, () => {
    const node = parsePrivateNode(`vless://${USER_ID}@exit.example.invalid:443?security=tls&type=${kind}&${query}`);
    assert.equal(node.network, kind);
    for (const [key, value] of Object.entries(expected)) assert.deepEqual(node[key], value);
  });
test('Trojan WebSocket preserves TLS and transport rather than becoming a TCP proxy', () => {
  const node = parsePrivateNode(
    'trojan://p%3Aa%40ss@exit.example.invalid:443?type=ws&path=%2Ftrojan&host=cdn.example.invalid&sni=tls.example.invalid',
  );
  assert.equal(node.password, 'p:a@ss');
  assert.equal(node.network, 'ws');
  assert.equal(node.sni, 'tls.example.invalid');
  assert.equal(node['ws-opts'].path, '/trojan');
});
test('VMess Base64 JSON handles UTF-8 labels and TLS WebSocket fields', () => {
  const node = parsePrivateNode(
    `vmess://${encode(JSON.stringify({ v: '2', ps: '我的 VMess 出口', add: 'vm.example.invalid', port: '443', id: USER_ID, aid: '0', scy: 'auto', net: 'ws', type: 'none', host: 'cdn.example.invalid', path: '/vmess', tls: 'tls', sni: 'tls.example.invalid' }))}`,
  );
  assert.equal(node.name, '我的 VMess 出口');
  assert.equal(node.alterId, 0);
  assert.equal(node.cipher, 'auto');
  assert.equal(node.servername, 'tls.example.invalid');
  assert.equal(node['ws-opts'].headers.Host, 'cdn.example.invalid');
});
test('VMess gRPC service name is retained', () => {
  const node = parsePrivateNode(
    `vmess://${encode(JSON.stringify({ add: 'vm.example.invalid', port: 443, id: USER_ID, net: 'grpc', path: 'grpcService', tls: 'tls' }))}`,
  );
  assert.equal(node['grpc-opts']['grpc-service-name'], 'grpcService');
});
test('Shadowsocks SIP002 handles Base64URL credentials and passwords containing colons', () => {
  const node = parsePrivateNode(`ss://${encode('aes-256-gcm:pass:word')}@ss.example.invalid:8388#SS`);
  assert.equal(node.cipher, 'aes-256-gcm');
  assert.equal(node.password, 'pass:word');
});
test('Shadowsocks 2022 plain credentials preserve Base64 key punctuation', () => {
  const node = parsePrivateNode('ss://2022-blake3-aes-256-gcm:YWJj%2B%2F%3D@ss.example.invalid:8388#SS2022');
  assert.equal(node.cipher, '2022-blake3-aes-256-gcm');
  assert.equal(node.password, 'YWJj+/=');
});
test('legacy Shadowsocks full Base64 handles IPv6 endpoints', () => {
  const node = parsePrivateNode(`ss://${encode('chacha20-ietf-poly1305:pass:word@[2001:db8::2]:8388')}#IPv6`);
  assert.equal(node.server, '2001:db8::2');
  assert.equal(node.port, 8388);
  assert.equal(node.password, 'pass:word');
});
test('Shadowsocks standard Base64 credentials containing slash remain user info', () => {
  const credential = Buffer.from('aes-256-gcm:密码\uffff').toString('base64');
  const node = parsePrivateNode(`ss://${credential}@ss.example.invalid:8388`);
  assert.equal(node.password, '密码\uffff');
});
test('Shadowsocks obfs and v2ray-plugin parameters are converted explicitly', () => {
  const base = `ss://${encode('aes-128-gcm:password')}@ss.example.invalid:8388/?plugin=`;
  const obfs = parsePrivateNode(base + encodeURIComponent('obfs-local;obfs=tls;obfs-host=cover.example.invalid'));
  assert.equal(obfs.plugin, 'obfs');
  assert.deepEqual(obfs['plugin-opts'], { mode: 'tls', host: 'cover.example.invalid' });
  const ws = parsePrivateNode(base + encodeURIComponent('v2ray-plugin;tls;host=cdn.example.invalid;path=/ws'));
  assert.equal(ws.plugin, 'v2ray-plugin');
  assert.equal(ws['plugin-opts'].tls, true);
  assert.equal(ws['plugin-opts'].path, '/ws');
});
test('Hysteria2 alias handles auth, obfs and port hopping', () => {
  const node = parsePrivateNode(
    'hy2://user:pass@hy.example.invalid:443,5000-6000/?sni=tls.example.invalid&obfs=salamander&obfs-password=obfs-secret&insecure=1#Hy2',
  );
  assert.equal(node.type, 'hysteria2');
  assert.equal(node.password, 'user:pass');
  assert.equal(node.ports, '443,5000-6000');
  assert.equal(node.obfs, 'salamander');
  assert.equal(node['obfs-password'], 'obfs-secret');
  assert.equal(node['skip-cert-verify'], true);
});
test('AnyTLS preserves password and session settings and rejects unsupported Reality', () => {
  const node = parsePrivateNode(
    'anytls://pass%3Aword@any.example.invalid?sni=tls.example.invalid&idle-session-timeout=90&min-idle-session=2',
  );
  assert.equal(node.type, 'anytls');
  assert.equal(node.password, 'pass:word');
  assert.equal(node['idle-session-timeout'], 90);
  assert.equal(node['min-idle-session'], 2);
  assert.throws(() => parsePrivateNode('anytls://pass@any.example.invalid?security=reality'), /不支持 AnyTLS/);
});
test('TUIC v5 preserves QUIC settings and explicit false values', () => {
  const node = parsePrivateNode(
    `tuic://${USER_ID}:password@tuic.example.invalid:443?sni=tls.example.invalid&congestion_control=bbr&udp_relay_mode=native&reduce_rtt=0&disable_sni=false&alpn=h3`,
  );
  assert.equal(node.uuid, USER_ID);
  assert.equal(node.password, 'password');
  assert.equal(node['congestion-controller'], 'bbr');
  assert.equal(node['udp-relay-mode'], 'native');
  assert.equal(node['reduce-rtt'], false);
  assert.equal(node['disable-sni'], false);
});
test('SOCKS alias accepts Base64 basic authentication', () => {
  const node = parsePrivateNode(`socks://${encode('alice:pass:word')}@socks.example.invalid:1080`);
  assert.equal(node.username, 'alice');
  assert.equal(node.password, 'pass:word');
});
test('multi-line and Base64 subscriptions import several protocols locally', () => {
  const links = PRIVATE_SAMPLES.join('\n') + '\n' + PRIVATE_SAMPLE;
  const nodes = parsePrivateNodes(links);
  assert.deepEqual(
    nodes.map(node => node.type),
    ['vless', 'hysteria2', 'socks5'],
  );
  assert.deepEqual(parsePrivateNodes(encode(links)), nodes);
});
test('YAML native imports preserve WireGuard, SSH and Snell credentials and nested data', () => {
  const nodes = parsePrivateNodes(
    `proxies:\n  - name: 我的 WG\n    type: wireguard\n    server: wg.example.invalid\n    port: 51820\n    private-key: demo-private-key\n    public-key: demo-public-key\n    ip: 10.0.0.2\n    reserved: [1, 2, 3]\n  - name: 我的 SSH\n    type: ssh\n    server: ssh.example.invalid\n    port: 22\n    username: alice\n    private-key: |\n      demo-line-1\n      demo-line-2\n  - name: 我的 Snell\n    type: snell\n    server: snell.example.invalid\n    port: 443\n    psk: demo-psk\n    version: 4\n`,
  );
  assert.equal(nodes.length, 3);
  assert.deepEqual(nodes[0].reserved, [1, 2, 3]);
  assert.equal(nodes[1]['private-key'], 'demo-line-1\ndemo-line-2\n');
  assert.equal(nodes[2].psk, 'demo-psk');
});
test('JSON native imports preserve advanced XHTTP and TLS options in the exported file', () => {
  const node = {
    name: '原生 XHTTP',
    type: 'vless',
    server: 'xh.example.invalid',
    port: 443,
    uuid: USER_ID,
    tls: true,
    'xhttp-opts': {
      path: '/xhttp',
      mode: 'auto',
      'download-settings': { server: 'download.example.invalid', port: 443 },
      'reuse-settings': { 'max-concurrency': '16-32' },
    },
    'client-fingerprint': 'chrome',
  };
  const parsed = parsePrivateNodes(JSON.stringify([node]));
  assert.deepEqual(parsed, [node]);
  const output = execute(privateChainScript(parsed, 'auto', ai), fixture());
  assert.deepEqual(JSON.parse(JSON.stringify(output.proxies.at(-1)['xhttp-opts'])), node['xhttp-opts']);
});
test('a full Clash config extracts proxies without applying unrelated rules or settings', () => {
  const nodes = parsePrivateNodes(
    JSON.stringify({
      proxies: [{ name: 'HTTP', type: 'http', server: 'proxy.example.invalid', port: 8080 }],
      rules: ['MATCH,REJECT'],
      'allow-lan': false,
    }),
  );
  assert.equal(nodes.length, 1);
  assert.equal(nodes[0].type, 'http');
  assert.equal(nodes[0].rules, undefined);
});
test('batch parsing fails atomically, identifies the row and never reflects credentials', () => {
  assert.throws(
    () => parsePrivateNodes(PRIVATE_SAMPLE + '\n' + 'vless://VERY_SECRET@host:443'),
    error => error.message.startsWith('第 2 条') && !error.message.includes('VERY_SECRET'),
  );
});
test('unsupported URI parameters are named but their values remain private', () => {
  assert.throws(
    () => parsePrivateNode(`vless://${USER_ID}@exit.example.invalid:443?security=tls&type=xhttp&extra=SECRET_VALUE`),
    error => error.message.includes('extra') && !error.message.includes('SECRET_VALUE'),
  );
});
test('dangerous YAML tags, aliases, duplicate keys and prototype fields are rejected as data errors', () => {
  for (const value of [
    '!!js/function function(){}',
    'a: &a [*a]',
    'type: http\ntype: socks5\nserver: host\nport: 80',
    '{"type":"http","server":"host","port":80,"__proto__":{"polluted":true}}',
    'type: http\nserver: host\nport: 80\nother: .nan',
  ])
    assert.throws(() => parsePrivateNodes(value));
  assert.equal({}.polluted, undefined);
});
test('native nodes with an existing dialer and malformed credentials fail explicitly', () => {
  assert.throws(() => parsePrivateNodes('type: http\nserver: host\nport: 80\ndialer-proxy: OTHER'), /已设置前置/);
  assert.throws(() => parsePrivateNodes('type: vless\nserver: host\nport: 443'), /认证字段/);
  assert.throws(() => parsePrivateNodes('type: tuic\nserver: host\nport: 443'), /认证字段/);
});
test('URI security and transport errors cannot silently become a different proxy', () => {
  for (const query of [
    'security=none&sni=cover.example.invalid',
    'security=reality&sni=cover.example.invalid&pbk=short',
    'security=tls&type=quic',
    'security=tls&type=ws&ed=wrong',
    'security=tls&sni=a&sni=b',
  ])
    assert.throws(() => parsePrivateNode(`vless://${USER_ID}@exit.example.invalid:443?${query}`));
});
test('unsupported protocol/transport combinations fail instead of falling back to TCP', () => {
  assert.throws(() => parsePrivateNode('trojan://password@exit.example.invalid:443?type=h2'), /不支持此协议与传输/);
  assert.throws(
    () =>
      parsePrivateNode(
        `vmess://${encode(JSON.stringify({ add: 'exit.example.invalid', port: 443, id: USER_ID, net: 'xhttp', tls: 'tls' }))}`,
      ),
    /不支持此协议与传输/,
  );
});
