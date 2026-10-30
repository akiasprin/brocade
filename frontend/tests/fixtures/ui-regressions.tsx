import { Suspense, useState } from 'react';
import { createRoot } from 'react-dom/client';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { ChainsPane, ChainRulesPanel } from '../../src/panes/chains';
import { NodesPane } from '../../src/panes/nodes';
import { TunnelsPane } from '../../src/panes/tunnels';
import { DeployPane } from '../../src/panes/deploy';
import { BinaryReleaseTab, useBinaryRelease } from '../../src/panes/binary-release';
import { LoadCard } from '../../src/panes/telemetry';
import { DiagTable } from '../../src/ui/diag-table';
import { ReverseHealthCard } from '../../src/reverse-health';
import { SessionProvider } from '../../src/session';
import { RuleDraftScope } from '../../src/panes/rules';
import { theme } from '../../src/forge/theme';
import { draft } from '../../src/draft';
import { ConsoleUpdate } from '../../src/ui/console-update';
import { lazy } from '../../src/ui/lazy';
import { useUnsavedChanges } from '../../src/ui/navigation-guard';
import type { Win } from '../../src/wm/store';
import { appId, pairs, nodes, snapshot, settings, compiled, report, now, warps } from './ui-regression-data';
import '../../src/styles.css';
const params = new URLSearchParams(location.search);
const scene = params.get('scene') ?? 'diagnostics';
if (theme.snapshot() !== (params.get('theme') === 'light' ? 'light' : 'dark')) theme.toggle();
draft.init('ui-regressions');
const client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
const deployment = {
  id: 100,
  revision_id: 7,
  status: 'running',
  activation_status: 'waiting',
  settlement_status: 'debt',
  active: true,
  base_revision_id: 6,
  created_at: new Date(now).toISOString(),
  wave_progress: [],
  targets: nodes.map(node => ({
    node_id: node.node_id,
    status: 'pending',
    wave: 0,
    disruptive: true,
    desired_structure: { actions: [] },
    error: null,
  })),
};
const data: [readonly unknown[], unknown][] = [
  [['nodes'], { nodes }],
  [['snapshot'], snapshot],
  [['snapshot', 'committed'], snapshot],
  [['settings'], settings],
  [['revisions'], { current_revision: 7, revisions: [] }],
  [['compile', 7], compiled],
  [['e2e-probes'], { chains: [] }],
  [['deployments'], { deployments: [] }],
  [['deployments', 'runtime'], { deployments: [] }],
  [['deployment', 100, true], deployment],
  [['deployment-settings'], { xray_handoff_beta: true }],
  [
    ['binary-releases', 'xray'],
    {
      available: {
        component: 'xray',
        version: 'a'.repeat(40),
        build_id: 'a'.repeat(64),
        source: 'embedded',
        artifacts: [{ arch: 'x86_64', sha256: 'a'.repeat(64) }],
      },
      current: null,
      legacy_approval: null,
    },
  ],
  [['certs'], { groups: [], nodes: [], domain: null, sealing_available: true }],
];
for (const [key, value] of data) client.setQueryData(key, value);
const responses: Record<string, unknown> = {
  '/model/snapshot': snapshot,
  '/nodes/agent-state': { nodes },
  '/settings': settings,
  '/revisions?limit=50': { current_revision: 7, revisions: [] },
  '/compile/7': compiled,
  '/probes/e2e?format=columnar-v1': { chains: [] },
  '/deployments?limit=50': { deployments: [] },
  '/certs': { groups: [], nodes: [] },
};
// Every request stays inside this deterministic fixture; mutations are synthetic and never reach a Console.
window.fetch = async (input, init) => {
  const path = String(input);
  if (init?.method && init.method !== 'GET')
    return Response.json({
      node_id: decodeURIComponent(path.split('/')[2]),
      token: 'TEST_ONLY',
      token_prefix: 'test-new',
      install_command: `TEST_ONLY_NOT_EXECUTABLE ${path.split('/')[2]}`,
    });
  if (path.startsWith('/load/nodes/')) return Response.json({ ...report, node_id: path.split('/')[3] });
  if (path.startsWith('/usage/node-series')) return Response.json({ nodes: [], since: '', month_start: '' });
  if (path.startsWith('/ping-probe/')) return Response.json({ node_id: 'node-0', targets: [] });
  if (path.startsWith('/realtime/')) return Response.json({ nodes: [] });
  if (path.includes('/deployment-settings')) return Response.json({ xray_handoff_beta: true });
  if (path.startsWith('/deployments/100')) return Response.json(deployment);
  if (path === '/deployments/active') return Response.json({ deployments: [] });
  if (path.startsWith('/binary-releases/xray')) return Response.json(client.getQueryData(['binary-releases', 'xray']));
  if (path.includes('probe-capability')) return Response.json({});
  if (path in responses) return Response.json(responses[path]);
  return Response.json({ error: `Unexpected fixture request: ${path}` }, { status: 404 });
};
class FixtureSource extends EventTarget {
  constructor() {
    super();
    queueMicrotask(() =>
      this.dispatchEvent(
        new MessageEvent('sample', {
          data: JSON.stringify({
            node_id: 'node-0',
            received_at_unix_millis: now - 30_000,
            sample: {
              sampled_at_unix_millis: now - 30_000,
              reverse_health: {
                boot_id: 'fixture',
                sequence: 1,
                sampled_at_unix_ms: now - 30_000,
                events: [],
                workers: Array.from({ length: 80 }, (_, i) => ({
                  worker_id: `w-${i}`,
                  pair: `rev:portal:${appId}/${pairs[0].chainId}>node-1`,
                  role: 'portal',
                  state: 'ready',
                  reason: '',
                  active_sessions: 0,
                  affected_sessions: 0,
                  control_queue_depth: 0,
                  queue_delay_ms: 0,
                  scheduler_lag_ms: 0,
                  ack_age_ms: 0,
                  rtt_ms: 10,
                  probes: 1,
                  acks: 1,
                  timeouts: 0,
                })),
              },
            },
          }),
        }),
      ),
    );
  }
  close() {}
}
window.EventSource = FixtureSource as unknown as typeof EventSource;
function Binary() {
  const release = useBinaryRelease('xray');
  return <BinaryReleaseTab release={release} editable editing onEditingChange={() => {}} />;
}
const FailingModule = lazy(() => import('./ui-regressions-module'));
function Recovery() {
  const [open, setOpen] = useState(false);
  const [value, setValue] = useState('');
  const clear = useUnsavedChanges(value !== '', '未保存的测试输入');
  return (
    <>
      <ConsoleUpdate reloadDelayMs={50} />
      <input aria-label="保留中的输入" value={value} onChange={e => setValue(e.target.value)} />
      <button
        onClick={() => {
          clear();
          setValue('');
        }}
      >
        完成编辑
      </button>
      <button onClick={() => setOpen(true)}>打开子层</button>
      {open && (
        <Suspense fallback={<p>加载中</p>}>
          <FailingModule />
        </Suspense>
      )}
    </>
  );
}
function Preview() {
  const [index, setIndex] = useState(0);
  const win: Win = {
    id: 9001,
    key: `tab:${scene}`,
    title: '回归验证',
    x: 0,
    y: 0,
    w: 1000,
    h: 800,
    z: 1,
    min: false,
    home: 'desk',
    data: {
      drill:
        scene === 'nodes'
          ? { p: 'node', id: nodes[index].node_id, tab: 'config' }
          : scene === 'install'
            ? { p: 'install', node: nodes[index].node_id, step: 2 }
            : scene === 'warps'
              ? { p: 'warp', id: warps[index].id }
              : scene === 'deploy'
                ? { p: 'detail', id: 100 }
                : { p: 'chain', app: appId, chain: pairs[index].chainId },
    },
  };
  return (
    <div className={`forge${innerWidth < 900 ? ' narrow' : ''}`}>
      <main className="fg-desk" style={{ padding: 16 }}>
        {['nodes', 'install', 'chains', 'warps'].includes(scene) && (
          <nav>
            <button onClick={() => setIndex(0)}>切换 A</button>
            <button onClick={() => setIndex(1)}>切换 B</button>
          </nav>
        )}
        {scene === 'diagnostics' ? (
          <div className="fg-pop" style={{ position: 'relative', top: 0, right: 0, width: '100%', maxWidth: 760 }}>
            <DiagTable
              names={{ node: () => '东京机器', chain: () => '线路 A' }}
              diagnostics={Array.from({ length: 48 }, (_, i) => ({
                level: i % 2 ? 'warn' : 'error',
                code: i % 2 ? 'rule.unknown-external-outbound' : 'ingress.vless-encryption-options',
                location: `app:${appId}/chain:${pairs[0].chainId}/node:node-0/${'path'.repeat(30)}`,
                message: `诊断详情 ${i} https://diagnostic.example.test/trace?query=${'abcdefghij'.repeat(100)}`,
              }))}
            />
          </div>
        ) : scene === 'reverse' ? (
          <ReverseHealthCard
            nodeId="node-0"
            nodeName={id => nodes.find(n => n.node_id === id)?.name ?? id}
            chainName={() => '跨区域备用线路'}
          />
        ) : scene === 'rules' ? (
          <section className="panel config-panel rule-sheet-card node-chain-sheet">
            <RuleDraftScope>
              <ChainRulesPanel
                appId={appId}
                chain={snapshot.snapshot.apps[0].chains[0]}
                spine={nodes.map(n => n.node_id)}
                steps={snapshot.snapshot.apps[0].steps.filter(step => step.chain === pairs[0].chainId)}
                nodes={nodes}
                onRemove={() => {}}
              />
            </RuleDraftScope>
          </section>
        ) : scene === 'telemetry' ? (
          <LoadCard report={report} />
        ) : scene === 'binary' ? (
          <Binary />
        ) : scene === 'deploy' ? (
          <DeployPane win={win} />
        ) : scene === 'nodes' || scene === 'install' ? (
          <NodesPane win={win} bare />
        ) : scene === 'warps' ? (
          <TunnelsPane win={win} />
        ) : scene === 'recovery' ? (
          <Recovery />
        ) : (
          <ChainsPane win={win} />
        )}
      </main>
    </div>
  );
}
createRoot(document.getElementById('root')!).render(
  <QueryClientProvider client={client}>
    <SessionProvider
      value={{
        who: {
          operator_id: 'ui-regressions',
          role: 'system-admin',
          tenant_scope: null,
          token_prefix: null,
          masked_assets: false,
        },
        initial: { node_count: 12, chain_group_count: [] },
      }}
    >
      <Preview />
    </SessionProvider>
  </QueryClientProvider>,
);
