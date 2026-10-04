import { defineConfig, type Plugin } from 'vite';
import react from '@vitejs/plugin-react';

// admin 面的 API 全挂在根路径上（http.rs 没有 /api 前缀）。
// 这里不再逐条列白名单——列表漏一条的后果是那条路径悄悄返回 index.html，
// api() 看 res.ok 为真就去 res.json()，撞上 HTML 抛 SyntaxError，
// 表现是「这个功能在开发态永远读不出来」，很难往代理上想（/auth 和 /revisions 就这么漏过）。
// 改成反过来：除了 Vite 自己的开发资源和 SPA 根，其余一律转给后端，加新路由不用动这里。
const FRONTEND_ONLY = ['$', '@.*', 'src/.*', 'node_modules/.*', 'assets/.*', 'favicon.*', '.*\\.hot-update\\..*'];
const TO_BACKEND = `^/(?!${FRONTEND_ONLY.join('|')})`;

const ADMIN_ORIGIN = process.env.BROCADE_ADMIN_ORIGIN ?? 'http://127.0.0.1:8080';
// 远程开发时浏览器通过 HTTP 打开 Vite，但被代理的生产控制面会签发 Secure session cookie。
// 浏览器会拒收那枚 cookie，表现为访客登录成功后立刻回到登录页。只在显式开启预览模式时
// 去掉代理响应中的 Secure；默认开发环境和生产部署均不改变 cookie 属性。
const INSECURE_PREVIEW_COOKIE = process.env.BROCADE_INSECURE_PREVIEW_COOKIE === '1';
const MOCK_PING = process.env.BROCADE_MOCK_PING === '1';

type MockPingFamily = 'ipv4' | 'ipv6';

type MockPingTarget = {
  name: string;
  kind: 'icmp' | 'tcp';
  ipv4: string | null;
  ipv6: string | null;
  baseMs: number;
  phase: number;
};

const MOCK_PING_TARGETS: MockPingTarget[] = [
  { name: '深圳电信', kind: 'icmp', ipv4: '202.96.134.33', ipv6: null, baseMs: 28, phase: 0.2 },
  { name: '深圳移动', kind: 'icmp', ipv4: '120.196.165.24', ipv6: null, baseMs: 44, phase: 1.4 },
  { name: 'Cloudflare', kind: 'icmp', ipv4: '1.1.1.1', ipv6: '2606:4700:4700::1111', baseMs: 62, phase: 2.5 },
  { name: '深圳电信', kind: 'tcp', ipv4: '202.96.134.33:443', ipv6: null, baseMs: 37, phase: 0.7 },
  { name: '深圳移动', kind: 'tcp', ipv4: '120.196.165.24:443', ipv6: null, baseMs: 58, phase: 1.9 },
  {
    name: 'Cloudflare',
    kind: 'tcp',
    ipv4: '1.1.1.1:443',
    ipv6: '[2606:4700:4700::1111]:443',
    baseMs: 104,
    phase: 3.1,
  },
];

function mockPingStep(windowSecs: number): number {
  if (windowSecs <= 3_600) return 60;
  if (windowSecs <= 21_600) return 120;
  if (windowSecs <= 86_400) return 300;
  return 900;
}

// 与 PingProbeEndpoint::series_address 一致：ICMP 的 IPv6 地址在序列标识里加方括号。
function mockSeriesAddress(target: MockPingTarget, family: MockPingFamily): string {
  const endpoint = target[family]!;
  return target.kind === 'icmp' && family === 'ipv6' ? `icmp://[${endpoint}]` : `${target.kind}://${endpoint}`;
}

function mockPingColumns(
  target: MockPingTarget,
  family: MockPingFamily,
  targetIndex: number,
  end: number,
  step: number,
  count: number,
) {
  const columns = {
    address: mockSeriesAddress(target, family),
    probed_at_unix_secs: [] as number[],
    attempted: [] as boolean[],
    latency_us: [] as Array<number | null>,
    skip_reason: [] as Array<string | null>,
  };
  // 每条线放一个超时。IPv6 另放一个未实际发包的缺口（无路由），以便确认两种状态没有混淆。
  const lossIndex = Math.max(2, count - 7 - targetIndex * 3 - (family === 'ipv6' ? 2 : 0));
  const gapIndex = family === 'ipv6' ? Math.max(1, Math.floor(count * 0.58)) : -1;
  const baseMs = target.baseMs * (family === 'ipv6' ? 1.08 : 1);
  for (let index = 0; index < count; index += 1) {
    const attempted = index !== gapIndex;
    const lost = index === lossIndex;
    const wave = Math.sin(index / 3.8 + target.phase) * 0.11 + Math.sin(index / 10 + target.phase) * 0.05;
    const spike = index === Math.floor(count * 0.72) ? baseMs * 0.45 : 0;
    const latencyMs = Math.max(0.18, baseMs * (1 + wave) + spike);
    columns.probed_at_unix_secs.push(end - (count - 1 - index) * step);
    columns.attempted.push(attempted);
    columns.latency_us.push(!attempted || lost ? null : Math.round(latencyMs * 1_000));
    columns.skip_reason.push(attempted ? null : 'no_route');
  }
  return columns;
}

/** Same columnar shape as `/ping-probe/nodes/{id}/series`: one column set per configured family. */
function mockPingView(nodeId: string, requestedWindowSecs: number, requestedEnd?: number) {
  const windowSecs = Math.min(7 * 86_400, Math.max(900, requestedWindowSecs || 1_800));
  const step = mockPingStep(windowSecs);
  const count = Math.min(600, Math.floor(windowSecs / step) + 1);
  const end = Math.floor((requestedEnd ?? Date.now() / 1_000) / step) * step;
  return {
    node_id: nodeId,
    targets: MOCK_PING_TARGETS.map((target, targetIndex) => ({
      name: target.name,
      kind: target.kind,
      ipv4: target.ipv4 === null ? null : mockPingColumns(target, 'ipv4', targetIndex, end, step, count),
      ipv6: target.ipv6 === null ? null : mockPingColumns(target, 'ipv6', targetIndex, end, step, count),
    })),
  };
}

function pingMockPlugin(): Plugin {
  return {
    name: 'brocade-ping-mock',
    configureServer(server) {
      if (!MOCK_PING) return;
      server.config.logger.info('[brocade] Ping mock 已启用；仅拦截本地 /ping-probe/*');
      server.middlewares.use((request, response, next) => {
        const url = new URL(request.url ?? '/', 'http://brocade.local');
        if (!url.pathname.startsWith('/ping-probe/')) return next();

        response.setHeader('content-type', 'application/json; charset=utf-8');
        response.setHeader('cache-control', 'no-store');
        response.setHeader('x-brocade-ping-mock', '1');
        if (request.method !== 'GET') {
          response.statusCode = 409;
          response.end(JSON.stringify({ error: '本地 Ping mock 模式为只读，不会写入真实配置' }));
          return;
        }

        if (url.pathname === '/ping-probe/settings') {
          response.end(
            JSON.stringify({
              targets: MOCK_PING_TARGETS.map(({ name, kind, ipv4, ipv6 }) => ({ name, kind, ipv4, ipv6 })),
              interval_secs: 60,
              timeout_ms: 420,
            }),
          );
          return;
        }
        if (url.pathname === '/ping-probe/nodes') {
          response.end(JSON.stringify({ nodes: [] }));
          return;
        }
        if (url.pathname === '/ping-probe/nodes/latest') {
          response.end(JSON.stringify({ interval_secs: 60, nodes: [] }));
          return;
        }
        const prefix = '/ping-probe/nodes/';
        if (url.pathname.startsWith(prefix)) {
          const nodeId = decodeURIComponent(url.pathname.slice(prefix.length).replace(/\/series$/, ''));
          const start = Number(url.searchParams.get('start_unix_secs'));
          const end = Number(url.searchParams.get('end_unix_secs'));
          const fixed = Number.isFinite(start) && Number.isFinite(end) && end > start;
          const windowSecs = fixed ? end - start : Number(url.searchParams.get('window_secs'));
          response.end(JSON.stringify(mockPingView(nodeId, windowSecs, fixed ? end : undefined)));
          return;
        }
        next();
      });
    },
  };
}

export default defineConfig({
  plugins: [pingMockPlugin(), react()],
  server: {
    proxy: {
      [TO_BACKEND]: {
        target: ADMIN_ORIGIN,
        changeOrigin: true,
        configure(proxy) {
          if (!INSECURE_PREVIEW_COOKIE) return;
          proxy.on('proxyRes', response => {
            const cookies = response.headers['set-cookie'];
            if (cookies) response.headers['set-cookie'] = cookies.map(cookie => cookie.replace(/;\s*Secure/gi, ''));
          });
        },
      },
    },
  },
});
