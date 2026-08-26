import { defineConfig } from 'vite';
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

export default defineConfig({
  plugins: [react()],
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
