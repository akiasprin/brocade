import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { App } from './app';
import './styles.css';

/* 配置与观测使用不同的刷新语义。
 *
 * `snapshot`、`nodes`、`settings` 等查询构成正在编辑的配置快照。它们不能因为原生下拉菜单
 * 收起、窗口重新聚焦或网络恢复就自动换底稿；否则受控表单会在一次无关的重拉后把刚选的值
 * 写回旧值。配置只在进入页面、明确保存/还原以及代码定向 invalidate 时更新。
 *
 * 负载、流量、延迟、在线状态和执行进度需要实时更新，它们各自在 useQuery 上显式声明了
 * `refetchInterval`。关闭这里的隐式刷新不影响那些定时器，后台标签重新可见后也会恢复轮询。 */
const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 5_000,
      refetchOnWindowFocus: false,
      refetchOnReconnect: false,
    },
  },
});

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      <App />
    </QueryClientProvider>
  </StrictMode>,
);
