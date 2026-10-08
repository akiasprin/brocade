import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const app = readFileSync(new URL('../src/app.tsx', import.meta.url), 'utf8');
const preload = readFileSync(new URL('../src/panes/preload.ts', import.meta.url), 'utf8');
const panes = readFileSync(new URL('../src/panes/index.tsx', import.meta.url), 'utf8');
const shell = readFileSync(new URL('../src/forge/shell.tsx', import.meta.url), 'utf8');
const nodes = readFileSync(new URL('../src/panes/nodes.tsx', import.meta.url), 'utf8');
const chains = readFileSync(new URL('../src/panes/chains.tsx', import.meta.url), 'utf8');
const tunnels = readFileSync(new URL('../src/panes/tunnels.tsx', import.meta.url), 'utf8');
const vpngateProvider = readFileSync(new URL('../src/panes/vpngate-provider.tsx', import.meta.url), 'utf8');
const vpngate = readFileSync(new URL('../src/panes/vpngate.tsx', import.meta.url), 'utf8');
const usage = readFileSync(new URL('../src/panes/usage.tsx', import.meta.url), 'utf8');
const telemetry = readFileSync(new URL('../src/panes/telemetry.tsx', import.meta.url), 'utf8');
const observationCharts = readFileSync(new URL('../src/panes/node-observation-charts.tsx', import.meta.url), 'utf8');
const observeChart = readFileSync(new URL('../src/ui/observe-chart.ts', import.meta.url), 'utf8');
const loading = readFileSync(new URL('../src/ui/loading.tsx', import.meta.url), 'utf8');
const api = readFileSync(new URL('../src/api.ts', import.meta.url), 'utf8');

test('登录入口与页面功能保持异步边界', () => {
  assert.doesNotMatch(app, /import \{ ForgeShell \} from '\.\/forge\/shell'/);
  assert.match(app, /forgeShellModule \?\?= import\('\.\/forge\/shell'\)/);
  assert.match(app, /lazy\(\(\) => loadForgeShell\(\)/);
  assert.match(app, /loadForgeShell\(\)[\s\S]*preloadPaneForHash\(window\.location\.hash\)/);
  assert.doesNotMatch(app, /SessionRestoring/);
  assert.match(app, /<Suspense fallback=\{<div id="stage" \/>\}>/);

  for (const module of ['nodes', 'chains', 'tunnels', 'users', 'deploy', 'usage', 'settings', 'password']) {
    assert.match(preload, new RegExp(`${module}: \\(\\) => import\\('\\./${module}'\\)`));
  }
  assert.match(preload, /topo: \(\) => import\('\.\.\/topo\/canvas'\)/);

  for (const module of ['nodes', 'chains', 'tunnels', 'deploy', 'users', 'usage', 'settings', 'password', 'inspect']) {
    assert.doesNotMatch(panes, new RegExp(`from './${module}'`));
    assert.match(panes, new RegExp(`import\\('./${module}'\\)`));
  }

  assert.doesNotMatch(shell, /panes\/links/);
  assert.doesNotMatch(preload, /import\('\.\/links'\)/);
  assert.doesNotMatch(shell, /from '\.\.\/topo\/canvas'/);
  assert.match(shell, /import\('\.\.\/topo\/canvas'\)/);

  assert.doesNotMatch(nodes, /from 'echarts\//);
  assert.match(nodes, /import\('\.\/node-observation-charts'\)/);
});

test('代码分包与首次 API 请求共用一个持续加载边界', () => {
  assert.match(panes, /<LoadingBoundary fallback=\{fallback\}/);
  assert.doesNotMatch(panes, /<LoadingBoundary[^>]*\bkey=/);
  assert.match(panes, /routeKey=\{paneLoadingRouteKey\(win\.key, win\.data\)\}/);
  assert.match(shell, /<LoadingBoundary fallback=\{<Loading variant="canvas" \/>\} variant="canvas">/);
  assert.match(usage, /monthly\.isPending[\s\S]*?return <Loading variant="usage" \/>/);
  assert.match(usage, /placeholderData: previous => previous/);
});

test('机器详情先显示真实框架，各观测面板在自己的位置渐进读取', () => {
  assert.match(nodes, /const initialUsage = useQuery\(/);
  assert.match(nodes, /const initialPing = useQuery\(/);
  assert.match(nodes, /const observationModules = useNodeObservationModules\(\)/);
  assert.match(
    nodes,
    /const observationBusy =[\s\S]*observationModules\.status === 'pending'[\s\S]*load\.isPending[\s\S]*initialUsage\.isPending[\s\S]*initialPing\.isPending/,
  );
  assert.match(nodes, /if \(nodes\.isPending\) return <Loading variant="detail"/);
  assert.doesNotMatch(nodes, /deployments\.isPending[\s\S]*return <Loading variant="detail"/);
  assert.doesNotMatch(nodes, /snapshot\.isPending[\s\S]*return <Loading variant="detail"/);
  assert.doesNotMatch(nodes, /initialObservationPending/);
  assert.match(nodes, /tab === 'observed' && \([\s\S]*aria-busy=\{observationBusy/);
  assert.match(nodes, /observationModules\.status === 'ready' \? \([\s\S]*<LoadCardFor[\s\S]*<ObservationKpisState/);
  assert.match(nodes, /<ThroughputPanelState[\s\S]*<PingProbePanelState/);
  assert.match(nodes, /<RuntimeCard[\s\S]*loadPending=\{load\.isPending\}/);
  assert.match(nodes, /import \{ FieldLoading, PanelLoading \} from '\.\.\/ui\/loading'/);
  assert.match(vpngate, /import \{ FieldLoading \} from '\.\.\/ui\/loading'/);
  assert.doesNotMatch(vpngate, /PanelLoading/);
  assert.doesNotMatch(nodes, /function ObservationLoadingMark\(/);
  assert.match(nodes, /state === 'pending' && ChartLoading/);
  assert.match(loading, /export function PanelLoading\(/);
  assert.match(loading, /export function FieldLoading\(/);
  assert.match(loading, /role=\{announce \? 'status' : undefined\}/);
  assert.match(loading, /LOADING_SPINNER_RADIUS \* 2/);
  assert.match(loading, /export const LOADING_TEXT = '加载中…'/);
  assert.match(observeChart, /spinnerRadius: LOADING_SPINNER_RADIUS/);
  assert.match(telemetry, /chart\.showLoading\('default', observeLoadingOptions\(cv\)\)/);
  assert.match(observationCharts, /chart\.showLoading\('default', observeLoadingOptions\(resolve\)\)/);
  assert.match(nodes, /<LoadCardFor[\s\S]*?observationModules=\{observationModules\.modules\}/);
  assert.match(nodes, /<ThroughputPanel[\s\S]*?observationModules=\{observationModules\.modules\}/);
  assert.match(nodes, /<PingProbePanel[\s\S]*?observationModules=\{observationModules\.modules\}/);
});

test('机器详情只按明确导航意图预取，并由滚动窗口补齐预取后的时间', () => {
  assert.match(nodes, /export async function prefetchNodeDetailData\(/);
  assert.match(nodes, /loadNodeObservationModules\(\)/);
  assert.match(nodes, /queryClient\.prefetchQuery\(\{[\s\S]*nodeLoadRangeQuery\(nodeId, range\)/);
  assert.match(nodes, /queryClient\.prefetchQuery\(\{[\s\S]*nodeUsageRangeQuery\(nodeId, range\)/);
  assert.match(nodes, /queryClient\.prefetchQuery\(nodePingRangeQuery\(nodeId, range\)\)/);
  assert.match(nodes, /onMouseEnter=\{schedulePrepare\}/);
  assert.match(nodes, /onFocus=\{event =>/);
  assert.match(nodes, /onPointerDown=\{event =>/);
  assert.match(nodes, /window\.setTimeout\(prepareNow, 120\)/);
  assert.doesNotMatch(nodes, /items\.map\([^)]*prefetchNodeDetailData/);
  assert.match(nodes, /after ten seconds it is stale[\s\S]*complete moving window again/);

  assert.match(app, /initialNodeDetailFromHash\(hash\)/);
  assert.match(app, /import\('\.\/panes\/nodes'\)[\s\S]*prefetchNodeDetailData/);
  assert.match(preload, /export function initialNodeDetailFromHash\(hash: string\): string \| null/);
});

test('机器详情首屏剥离深度对象，深度图立即挂载并按十项指标并发读取', () => {
  assert.match(nodes, /fetchNodeLoadOverview\(nodeId, startUnixSecs, endUnixSecs\)/);
  assert.match(api, /\/load\/nodes\/\$\{encodeURIComponent\(nodeId\)\}\/overview/);
  assert.match(api, /\/ping-probe\/nodes\/\$\{encodeURIComponent\(nodeId\)\}\/series/);
  assert.match(telemetry, /const DETAIL_METRIC_GROUPS/);
  assert.match(telemetry, /const METRICS_PER_BATCH = 10/);
  assert.match(telemetry, /function planMetricBatches/);
  assert.match(telemetry, /fetchNodeLoadMetrics\(/);
  assert.match(telemetry, /queries: plan\.batches\.map/);
  assert.match(telemetry, /const readyViews = queries\.flatMap/);
  assert.match(telemetry, /const metricStates = plan\.groupBatchIndices\.map/);
  assert.doesNotMatch(telemetry, /loadedBatches/);
  assert.match(telemetry, /const sampledSeries = downsampleKpiSeries\(series, valueOf\)/);
  assert.match(
    telemetry,
    /data: observeSeriesData\(times, lineSeries\.values, \{ windowStartsUnixSecs: windowStarts \}\)/,
  );
  assert.doesNotMatch(telemetry, /data: downsample|data: sampledSeries|sampling:/);
});

test('机器详情的配置与规则页保留框架，并把快照等待限制在当前页签', () => {
  assert.match(nodes, /function NodeDetailTabState\(/);
  assert.match(nodes, /tab === 'config' && !snapshot\.data[\s\S]*<NodeDetailTabState/);
  assert.match(nodes, /function NodeRuleCardState\(/);
  assert.match(nodes, /tab === 'config'[\s\S]*<CertGroupCard[\s\S]*<LogRetentionCard/);
  assert.doesNotMatch(nodes, /<Suspense fallback=\{null\}>/);
  assert.match(
    nodes,
    /function MachineEgressDnsRules\([\s\S]*if \(!snapshotReady\) return <NodeRuleCardState kind="dns"[\s\S]*<Suspense fallback=\{<NodeRuleCardState kind="dns" \/>\}>[\s\S]*<LazyMachineEgressDnsRules/,
  );
  assert.match(
    nodes,
    /function NodeChainsSection\([\s\S]*if \(!snapshotReady\) return <NodeRuleCardState kind="chains"[\s\S]*<Suspense fallback=\{<NodeRuleCardState kind="chains" \/>\}>/,
  );
  assert.match(
    nodes,
    /tab === 'chains'[\s\S]*<MachineEgressDnsRules[\s\S]*snapshotReady=\{!!snapshot\.data\}[\s\S]*<NodeChainsSection[\s\S]*snapshotReady=\{!!snapshot\.data\}/,
  );
  assert.match(nodes, /<ChainRulesPanel[\s\S]*loadingFallback=\{null\}/);
  assert.match(chains, /loadingFallback === undefined \? <Loading variant="editor" \/> : loadingFallback/);
});

test('机器列表先返回基础数据，再加载有界的轻量遥测', () => {
  assert.equal(nodes.match(/enabled: nodes\.isSuccess/g)?.length, 3);
  assert.match(nodes, /fetchNodeNicListWindows\(LIST_NIC_WINDOWS\)/);
  assert.match(nodes, /queryFn: fetchNodeTraffic/);
  assert.doesNotMatch(nodes, /本月业务|本期网卡/);
  assert.match(nodes, /<NicMonthTotal[\s\S]*?<small>本月<\/small>/);
  assert.doesNotMatch(nodes, /nicTotalTracked|本期 \{exactBytes\(nicTotal\.total_bytes\)\}/);
  assert.doesNotMatch(nodes, /fetchNodeLoadListWindows\(LIST_NIC_WINDOWS\)/);
  assert.match(nodes, /Math\.max\(query\.state\.data\?\.interval_secs \?\? 10, 10\) \* 1_000/);
  assert.match(api, /\/load\/nodes\/nic\?windows=\$\{windows\}/);
});

test('机器和线路首屏数据与代码分包并行，线路拨测也与主数据并行读取', () => {
  assert.match(app, /draft\.init\(session\.who\.operator_id\)[\s\S]*const pane = initialPaneFromHash\(hash\)/);
  assert.match(app, /prefetchInitialRouteData\(queryClient, window\.location\.hash, bootstrap\)/);
  assert.match(app, /pane === 'nodes'[\s\S]*queryKey: \['nodes'\]/);
  assert.match(app, /pane === 'chains'[\s\S]*queryKey: \['nodes'\][\s\S]*queryKey: \['snapshot'\]/);
  assert.match(
    chains,
    /queryKey: \['e2e-probes'\][\s\S]*queryFn: \(\) => fetchE2eProbes\(\)[\s\S]*refetchInterval: 60_000/,
  );
  assert.doesNotMatch(chains, /enabled: snapshot\.isSuccess && nodeList\.isSuccess/);
});

test('隧道总览预取自身数据，详情图表不进入总览代码分包', () => {
  assert.match(app, /pane === 'tunnels'[\s\S]*queryKey: \['snapshot'\]/);
  assert.doesNotMatch(app, /queryKey: \['tenants'\]/);
  assert.doesNotMatch(tunnels, /fetchTenants|queryKey: \['tenants'\]/);
  assert.match(tunnels, /from '\.\/vpngate-provider'/);
  assert.match(tunnels, /lazy\(\(\) => import\('\.\/vpngate'\)/);
  assert.doesNotMatch(tunnels, /from '\.\/vpngate'/);
  assert.match(tunnels, /const loadTunnelEditor = \(\) => import\('\.\/rules'\)/);
  assert.doesNotMatch(tunnels, /from '\.\/rules'/);
  assert.doesNotMatch(vpngateProvider, /echarts|from '\.\/vpngate'/);
  assert.doesNotMatch(vpngate, /^import .* from 'echarts\//m);
  assert.match(vpngate, /import\('echarts\/core'\)/);
  assert.match(vpngate, /defaultCountryCode \|\|[\s\S]*configuredCountryCode/);
  assert.doesNotMatch(vpngate, /if \(overview\.isPending\) return/);
});
