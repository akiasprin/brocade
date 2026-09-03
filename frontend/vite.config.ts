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

type MockPingTarget = {
  name: string;
  address: string;
  baseMs: number;
  phase: number;
};

const MOCK_PING_TARGETS: MockPingTarget[] = [
  { name: '深圳电信', address: 'icmp://202.96.134.33', baseMs: 28, phase: 0.2 },
  { name: '深圳移动', address: 'icmp://120.196.165.24', baseMs: 44, phase: 1.4 },
  { name: 'Cloudflare IPv6', address: 'icmp://[2606:4700:4700::1111]', baseMs: 62, phase: 2.5 },
  { name: '深圳电信', address: 'tcp://202.96.134.33:443', baseMs: 37, phase: 0.7 },
  { name: '深圳移动', address: 'tcp://120.196.165.24:443', baseMs: 58, phase: 1.9 },
  { name: 'Cloudflare', address: 'tcp://1.1.1.1:443', baseMs: 104, phase: 3.1 },
];

function mockPingStep(windowSecs: number): number {
  if (windowSecs <= 3_600) return 60;
  if (windowSecs <= 21_600) return 120;
  if (windowSecs <= 86_400) return 300;
  return 900;
}

function mockPingView(nodeId: string, requestedWindowSecs: number) {
  const windowSecs = Math.min(7 * 86_400, Math.max(900, requestedWindowSecs || 1_800));
  const step = mockPingStep(windowSecs);
  const count = Math.min(600, Math.floor(windowSecs / step) + 1);
  const end = Math.floor(Date.now() / 1_000 / step) * step;
  return {
    node_id: nodeId,
    targets: MOCK_PING_TARGETS.map((target, targetIndex) => ({
      name: target.name,
      address: target.address,
      samples: Array.from({ length: count }, (_, index) => {
        const at = end - (count - 1 - index) * step;
        // 每条线放一个超时。IPv6 目标另放一个未实际发包的缺口，以便确认两种状态没有混淆。
        const lossIndex = Math.max(2, count - 7 - targetIndex * 3);
        const gapIndex = target.address.includes('[') ? Math.max(1, Math.floor(count * 0.58)) : -1;
        const attempted = index !== gapIndex;
        const lost = index === lossIndex;
        const wave = Math.sin(index / 3.8 + target.phase) * 0.11 + Math.sin(index / 10 + target.phase) * 0.05;
        const spike = index === Math.floor(count * 0.72) ? target.baseMs * 0.45 : 0;
        const latencyMs = Math.max(0.18, target.baseMs * (1 + wave) + spike);
        return {
          probed_at_unix_secs: at,
          attempted,
          latency_us: !attempted || lost ? null : Math.round(latencyMs * 1_000),
        };
      }),
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
              targets: MOCK_PING_TARGETS.map(({ name, address }) => ({ name, address })),
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
        const prefix = '/ping-probe/nodes/';
        if (url.pathname.startsWith(prefix)) {
          const nodeId = decodeURIComponent(url.pathname.slice(prefix.length));
          const windowSecs = Number(url.searchParams.get('window_secs'));
          response.end(JSON.stringify(mockPingView(nodeId, windowSecs)));
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
