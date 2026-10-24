import { useState } from 'react';
import { createRoot } from 'react-dom/client';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { Confirm, ErrorBox } from '../../src/ui/bits';
import { SourceAccessRollout } from '../../src/panes/user-source-access';
import type { SourceAccessNode } from '../../src/api';
import { theme } from '../../src/forge/theme';
import '../../src/styles.css';
const params = new URLSearchParams(location.search);
if (theme.snapshot() !== (params.get('theme') === 'light' ? 'light' : 'dark')) theme.toggle();
const message = '诊断请求失败 https://diagnostic.example.test/request?trace=' + 'a'.repeat(128);
const nodes: SourceAccessNode[] = Array.from({ length: 64 }, (_, i) => ({
  node_id: 'coverage-node-' + i,
  phase: i % 4 === 0 ? 'error' : i % 4 === 1 ? 'unsupported' : i % 4 === 2 ? 'pending' : 'applied',
  error: i % 4 === 0 ? message : null,
}));
const client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
client.setQueryData(['nodes'], {
  nodes: nodes.map((n, i) => ({
    ...n,
    name: '跨区域高性能加密专线 / Tokyo Singapore edge ' + i + ' / ' + 'ABCDEFGHIJKLMNOPQRSTUVWXYZ'.repeat(2),
  })),
});
function Preview() {
  const [confirm, setConfirm] = useState(false),
    [done, setDone] = useState(false);
  return (
    <QueryClientProvider client={client}>
      <main style={{ padding: 16 }}>
        <button className="btn" onClick={() => setConfirm(true)}>
          打开确认
        </button>
        <span style={{ float: 'right' }}>
          <SourceAccessRollout nodes={nodes} />
        </span>
        <p>{done ? '已执行合成动作' : '共享组件夹具；未连接业务写入'}</p>
        {params.has('error') && (
          <section className="panel config-panel">
            <header>受控长错误文本</header>
            <ErrorBox error={new Error(message)} />
          </section>
        )}
        {confirm && (
          <Confirm
            title="确认这批机器的操作？"
            body={
              <ol>
                {Array.from({ length: 80 }, (_, i) => (
                  <li key={i}>机器 {i + 1} · 香港 / 东京 / Singapore · 等待确认该机器对应的变更和当前连接处理策略</li>
                ))}
              </ol>
            }
            confirmLabel="确认合成动作"
            onCancel={() => setConfirm(false)}
            onConfirm={() => {
              setDone(true);
              setConfirm(false);
            }}
          />
        )}
      </main>
    </QueryClientProvider>
  );
}
createRoot(document.getElementById('root')!).render(<Preview />);
