import {
  useEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
  type KeyboardEvent as ReactKeyboardEvent,
} from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  fetchNodes,
  fetchSnapshot,
  fetchVpngateCountryServers,
  fetchVpngateOverview,
  fetchVpngateRuntimes,
  requestVpngatePoolSwitch,
  VPNGATE_CONNECT_THRESHOLD_MAX_MS,
  VPNGATE_DEFAULT_PROBE_WORKERS,
  VPNGATE_MAX_PROBE_WORKERS,
  startVpngateSync,
  updateVpngateCatalogSettings,
  updateVpngateProbeNode,
  type NodeAgentStateItem,
  type SnapshotApp,
  type VpngateCatalogStatus,
  type VpngateCountrySummary,
  type VpngateDirectoryFilter,
  type VpngateDirectorySort,
  type VpngateIpNetwork,
  type VpngateIpScore,
  type VpngateRuntimeView,
  type VpngateServerPageRequest,
  type VpngateServerView,
  type VpngateSyncHistoryPoint,
} from '../api';
import { navigate } from '../forge/route';
import { palette } from '../forge/palette';
import { theme } from '../forge/theme';
import { useNodeRealtime } from '../node-realtime';
import { useSession } from '../session';
import { Confirm, Empty, ErrorBox, SegSwitch } from '../ui/bits';
import { FieldLoading } from '../ui/loading';
import { useNow } from '../ui/clock';
import { PanelTitle } from '../ui/icons';
import { RegionFlag } from '../ui/region-flag';
import { useServerForm } from '../ui/server-form';
import { vpngateNodeEligibility } from '../vpngate-capability';
import { freshVpngateReport } from '../vpngate-realtime';
import {
  isVpngateOutbound,
  isSelectableVpngateRegion,
  vpngateDirectoryServerCount,
  vpngateRegionName,
  vpngateServerIds,
  type VpngateOutbound,
} from '../vpngate-selection';
export { isVpngateOutbound, type VpngateOutbound } from '../vpngate-selection';

type EchartsCore = typeof import('echarts/core');
type EchartsInstance = ReturnType<EchartsCore['init']>;

let echartsPromise: Promise<EchartsCore> | undefined;

const loadEcharts = () =>
  (echartsPromise ??= Promise.all([
    import('echarts/core'),
    import('echarts/charts'),
    import('echarts/components'),
    import('echarts/renderers'),
  ]).then(([echarts, charts, components, renderers]) => {
    echarts.use([charts.LineChart, components.GridComponent, components.TooltipComponent, renderers.CanvasRenderer]);
    return echarts;
  }));

type VpngateTab = 'pool' | 'collection';
type CountrySort = 'region' | 'available' | 'candidate';

interface VpngatePoolReference {
  pool: VpngateOutbound;
  ruleCount: number;
  nodeCount: number;
}

const TABS: readonly { value: VpngateTab; label: string }[] = [
  { value: 'pool', label: '出口池' },
  { value: 'collection', label: '目录采集' },
];

const VPNGATE_DIRECTORY_PAGE_SIZE = 100;
const DEFAULT_DIRECTORY_REQUEST: VpngateServerPageRequest = {
  page: 1,
  page_size: VPNGATE_DIRECTORY_PAGE_SIZE,
  search: '',
  filter: 'all',
  sort: 'candidate',
};

const directoryQueryKey = (countryCode: string, request: VpngateServerPageRequest) =>
  request.page === 1 &&
  request.page_size === VPNGATE_DIRECTORY_PAGE_SIZE &&
  !request.search &&
  request.filter === 'all' &&
  request.sort === 'candidate'
    ? ['vpngate', 'country', countryCode]
    : ['vpngate', 'country', countryCode, request];

const compareCountriesByCandidate = (left: VpngateCountrySummary, right: VpngateCountrySummary) =>
  right.candidate_servers - left.candidate_servers ||
  right.measured_successful - left.measured_successful ||
  left.country_code.localeCompare(right.country_code);

const rate = (bitsPerSecond: number | null | undefined) => {
  if (bitsPerSecond == null) return '—';
  if (bitsPerSecond >= 1_000_000_000) return `${(bitsPerSecond / 1_000_000_000).toFixed(1)} Gbps`;
  if (bitsPerSecond >= 1_000_000) return `${(bitsPerSecond / 1_000_000).toFixed(1)} Mbps`;
  return `${Math.round(bitsPerSecond / 1_000)} Kbps`;
};

const dateTime = (unixSeconds: number | null | undefined) =>
  unixSeconds == null
    ? '—'
    : new Date(unixSeconds * 1000).toLocaleString('zh-CN', {
        month: '2-digit',
        day: '2-digit',
        hour: '2-digit',
        minute: '2-digit',
      });

const clockTime = (unixSeconds: number | null | undefined) =>
  unixSeconds == null
    ? '—'
    : new Date(unixSeconds * 1000).toLocaleTimeString('zh-CN', {
        hour: '2-digit',
        minute: '2-digit',
        hour12: false,
      });

const relativeTime = (unixSeconds: number, nowMilliseconds: number) => {
  const minutes = Math.max(0, Math.round((nowMilliseconds / 1000 - unixSeconds) / 60));
  if (minutes < 1) return '刚刚';
  if (minutes < 60) return `${minutes} 分钟前`;
  const hours = Math.floor(minutes / 60);
  return minutes % 60 ? `${hours} 小时 ${minutes % 60} 分钟前` : `${hours} 小时前`;
};

const providerLabel = { proxycheck: 'ProxyCheck', ffraud: 'FFraud', iplogs: 'IPLogs' } as const;
const networkTypeLabel = {
  datacenter: '机房',
  residential: '家宽',
  business: '商宽',
  mobile: '移动网络',
  relay: '转发网络',
  unknown: '类型未知',
} as const;

function IpScores({ scores }: { scores: VpngateIpScore[] }) {
  if (!scores.length) return <span className="st st-warn">等待拨测结果</span>;
  const shortLabel = { proxycheck: 'PC', ffraud: 'FF', iplogs: 'IL' } as const;
  const detail = scores
    .map(
      score =>
        `${providerLabel[score.provider]} ${score.score} / 100${score.country_code ? ` · ${vpngateRegionName(score.country_code)}` : ''}`,
    )
    .join('\n');
  return <span title={detail}>{scores.map(score => `${shortLabel[score.provider]} ${score.score}`).join(' · ')}</span>;
}

function IpNetwork({ networks }: { networks: VpngateIpNetwork[] }) {
  if (!networks.length) return <span className="muted">尚未拨测成功</span>;
  const kinds = [...new Set(networks.map(network => networkTypeLabel[network.network_type]))].join(' / ');
  const isps = [...new Set(networks.map(network => network.isp).filter((value): value is string => Boolean(value)))];
  const detail = networks
    .map(
      network =>
        `${providerLabel[network.provider]}：${network.isp ?? 'ISP 未知'} · ${networkTypeLabel[network.network_type]}`,
    )
    .join('\n');
  return (
    <span className="vpngate-network-intel" title={detail}>
      <b>{kinds}</b>
      <small>{isps.length ? isps.join(' / ') : 'ISP 未知'}</small>
    </span>
  );
}

function RuntimeNetworkSummary({ networks }: { networks: VpngateIpNetwork[] }) {
  if (!networks.length) return <small className="muted">网络待验证</small>;
  const kinds = [...new Set(networks.map(network => networkTypeLabel[network.network_type]))].join(' / ');
  const isps = [...new Set(networks.map(network => network.isp).filter((value): value is string => Boolean(value)))];
  const detail = networks
    .map(
      network =>
        `${providerLabel[network.provider]}：${network.isp ?? 'ISP 未知'} · ${networkTypeLabel[network.network_type]}`,
    )
    .join('\n');
  return (
    <small className="vpngate-runtime-network" title={detail}>
      {isps.length ? isps.join(' / ') : 'ISP 未知'} · {kinds}
    </small>
  );
}

function IntelligenceAge({
  verifiedAt,
  stale,
  historical,
}: {
  verifiedAt: number | null;
  stale: boolean;
  historical: boolean;
}) {
  if (verifiedAt == null) return null;
  const label = stale ? '情报已陈旧' : historical ? '上次成功情报' : '情报已验证';
  return (
    <small
      className={stale ? 'vpngate-intelligence-age warn' : 'vpngate-intelligence-age'}
    >{`${label} · ${dateTime(verifiedAt)}`}</small>
  );
}

function referencedVpngatePools(pools: VpngateOutbound[], apps: SnapshotApp[]): VpngatePoolReference[] {
  const references = new Map<string, { ruleCount: number; nodes: Set<string> }>();
  for (const app of apps) {
    for (const step of app.steps ?? []) {
      for (const rule of step.rules ?? []) {
        if (rule.a.t !== 'proxy') continue;
        const reference = references.get(rule.a.outbound) ?? { ruleCount: 0, nodes: new Set<string>() };
        reference.ruleCount += 1;
        reference.nodes.add(step.node);
        references.set(rule.a.outbound, reference);
      }
    }
  }
  return pools.flatMap(pool => {
    const reference = references.get(pool.id);
    return reference ? [{ pool, ruleCount: reference.ruleCount, nodeCount: reference.nodes.size }] : [];
  });
}

function catalogueState(status: VpngateCatalogStatus) {
  if (status.syncing) {
    return { label: '目录同步中', tone: 'warn' };
  }
  if (status.last_error_code) {
    return { label: '目录同步异常', tone: 'warn' };
  }
  if (!status.enabled) {
    return { label: '目录已暂停', tone: 'idle' };
  }
  return { label: '目录正常', tone: '' };
}

export function VpngatePage({ initialPool }: { initialPool?: VpngateOutbound }) {
  const { who } = useSession();
  const queryClient = useQueryClient();
  const systemAdmin = who.role === 'system-admin';
  const [tab, setTab] = useState<VpngateTab>('pool');
  const [countryChoice, setCountryChoice] = useState(
    initialPool && isSelectableVpngateRegion(initialPool.protocol.v.country_code)
      ? initialPool.protocol.v.country_code
      : '',
  );
  const [countrySearch, setCountrySearch] = useState('');
  const overview = useQuery({
    queryKey: ['vpngate'],
    queryFn: fetchVpngateOverview,
    retry: false,
    staleTime: 15_000,
    refetchInterval: 30_000,
  });
  const snapshot = useQuery({ queryKey: ['snapshot'], queryFn: fetchSnapshot });
  const nodes = useQuery({
    queryKey: ['nodes'],
    queryFn: () => fetchNodes(),
    enabled: tab === 'collection',
    retry: false,
    refetchInterval: 15_000,
  });
  const runtimes = useQuery({
    queryKey: ['vpngate', 'runtimes'],
    queryFn: fetchVpngateRuntimes,
    retry: false,
    refetchInterval: 15_000,
  });
  const pools = useMemo(
    () => (snapshot.data?.snapshot.external_outbounds ?? []).filter(isVpngateOutbound),
    [snapshot.data?.snapshot.external_outbounds],
  );
  const referencedPools = useMemo(
    () => referencedVpngatePools(pools, snapshot.data?.snapshot.apps ?? []),
    [pools, snapshot.data?.snapshot.apps],
  );
  // The tunnels page already has this snapshot. Use its first configured region as a cold-start
  // hint so the initial directory request can overlap the overview request; once the overview is
  // ready, its candidate-ranked default remains authoritative.
  const configuredCountryCode = useMemo(
    () => pools.find(pool => isSelectableVpngateRegion(pool.protocol.v.country_code))?.protocol.v.country_code ?? '',
    [pools],
  );
  const countries = useMemo(
    () => (overview.data?.countries ?? []).filter(country => isSelectableVpngateRegion(country.country_code)),
    [overview.data?.countries],
  );
  const defaultCountryCode = useMemo(
    () => [...countries].sort(compareCountriesByCandidate)[0]?.country_code ?? '',
    [countries],
  );
  const countryCode =
    countryChoice ||
    (initialPool && isSelectableVpngateRegion(initialPool.protocol.v.country_code)
      ? initialPool.protocol.v.country_code
      : '') ||
    defaultCountryCode ||
    configuredCountryCode;
  const country = countries.find(candidate => candidate.country_code === countryCode);
  const runtimeServers = useQuery({
    queryKey: directoryQueryKey(countryCode, DEFAULT_DIRECTORY_REQUEST),
    queryFn: () => fetchVpngateCountryServers(countryCode, DEFAULT_DIRECTORY_REQUEST),
    enabled: tab === 'pool' && Boolean(countryCode),
    retry: false,
    staleTime: 30_000,
    refetchInterval: 60_000,
  });
  const sync = useMutation({
    mutationFn: startVpngateSync,
    onSuccess: async () => queryClient.invalidateQueries({ queryKey: ['vpngate'] }),
  });

  const selectedPoolIds = new Set(
    referencedPools
      .filter(reference => reference.pool.protocol.v.country_code === countryCode)
      .map(reference => reference.pool.id),
  );
  const selectedRuntimes = (runtimes.data ?? []).filter(
    runtime => runtime.country_code === countryCode && selectedPoolIds.has(runtime.outbound_id),
  );
  const state = overview.data ? catalogueState(overview.data.status) : { label: '正在读取目录', tone: 'idle' };

  const onTabKeyDown = (event: ReactKeyboardEvent<HTMLButtonElement>) => {
    const current = TABS.findIndex(candidate => candidate.value === tab);
    let next: number;
    if (event.key === 'ArrowRight') next = (current + 1) % TABS.length;
    else if (event.key === 'ArrowLeft') next = (current - 1 + TABS.length) % TABS.length;
    else if (event.key === 'Home') next = 0;
    else if (event.key === 'End') next = TABS.length - 1;
    else return;
    event.preventDefault();
    const nextTab = TABS[next].value;
    setTab(nextTab);
    document.getElementById(`vpngate-tab-${nextTab}`)?.focus();
  };

  return (
    <div className="nd-sheet nd-page cg-page vpngate-page">
      <div className="fg-sheet nd-paper">
        <header className="nd-page-head cg-head vpngate-page-head">
          <div className="nd-page-identity">
            <span className="nd-idplate vpngate-title-icon">
              <span className="nd-idplate-clip">
                <RegionFlag code={countryCode} square />
              </span>
              <i className={`node-lamp ${state.tone}`} title={state.label} aria-label={state.label} />
            </span>
            <span className="nd-ident-text">
              <span className="nd-ident-row">
                <h1 className="nd-id nd-name">VPN Gate</h1>
              </span>
            </span>
          </div>

          <nav className="nd-tabs" role="tablist" aria-label="VPN Gate 页面">
            <div className="nd-tabs-seg">
              {TABS.map(item => (
                <button
                  id={`vpngate-tab-${item.value}`}
                  key={item.value}
                  type="button"
                  role="tab"
                  aria-selected={tab === item.value}
                  aria-controls={`vpngate-panel-${item.value}`}
                  tabIndex={tab === item.value ? 0 : -1}
                  onClick={() => setTab(item.value)}
                  onKeyDown={onTabKeyDown}
                >
                  {item.label}
                </button>
              ))}
            </div>
          </nav>

          <div className="nd-acts vpngate-head-actions">
            <button
              className="btn"
              type="button"
              disabled={!systemAdmin || sync.isPending || !overview.data || overview.data.status.syncing}
              onClick={() => sync.mutate()}
            >
              {sync.isPending ? '排队中…' : '立即同步'}
            </button>
          </div>
        </header>

        {overview.error && <ErrorBox error={overview.error} />}
        {sync.error && <ErrorBox error={sync.error} />}
        {snapshot.error && <ErrorBox error={snapshot.error} />}

        <div className="nd-paper-body vpngate-body">
          {tab === 'pool' ? (
            <section
              id="vpngate-panel-pool"
              role="tabpanel"
              aria-labelledby="vpngate-tab-pool"
              className="vpngate-pool-view"
            >
              <div className="vpngate-split">
                <div className="vpngate-side">
                  <CountryRail
                    countries={countries}
                    pools={referencedPools}
                    countryCode={countryCode}
                    search={countrySearch}
                    pending={overview.isPending}
                    onSearch={setCountrySearch}
                    onCountry={setCountryChoice}
                  />
                  <CountrySummary country={country} countryCode={countryCode} pending={overview.isPending} />
                </div>
                <CountryWorkspace
                  country={country}
                  countryCode={countryCode}
                  catalogPending={overview.isPending}
                  pools={referencedPools}
                  poolsPending={snapshot.isPending}
                  servers={runtimeServers.data?.items ?? []}
                  serversPending={runtimeServers.isPending}
                  runtimes={selectedRuntimes}
                  runtimesPending={runtimes.isPending}
                  runtimesError={runtimes.error}
                  systemAdmin={systemAdmin}
                  directorySearchEnabled={!who.masked_assets}
                />
              </div>
            </section>
          ) : overview.data ? (
            <CollectionPanel
              id="vpngate-panel-collection"
              status={overview.data.status}
              syncHistory={overview.data.sync_history ?? []}
              nodes={nodes.data?.nodes ?? []}
              nodesPending={nodes.isPending}
              nodesError={nodes.error}
              systemAdmin={systemAdmin}
            />
          ) : (
            <section
              id="vpngate-panel-collection"
              role="tabpanel"
              aria-labelledby="vpngate-tab-collection"
              className="vpngate-collection-view"
            >
              <div className="vpngate-collection-grid">
                <section className="panel titled cg-sec vpngate-card vpngate-collection-sync" aria-busy="true">
                  <header>
                    <PanelTitle of="observe">VPN Gate 目录同步</PanelTitle>
                  </header>
                  <p className="vpngate-empty">
                    <FieldLoading />
                  </p>
                </section>
              </div>
            </section>
          )}
        </div>
      </div>
    </div>
  );
}

function CountryRail({
  countries,
  pools,
  countryCode,
  search,
  pending,
  onSearch,
  onCountry,
}: {
  countries: VpngateCountrySummary[];
  pools: VpngatePoolReference[];
  countryCode: string;
  search: string;
  pending: boolean;
  onSearch: (value: string) => void;
  onCountry: (value: string) => void;
}) {
  const [sort, setSort] = useState<CountrySort>('candidate');
  const term = search.trim().toLocaleLowerCase('zh-CN');
  const filtered = useMemo(
    () =>
      countries
        .filter(country => {
          if (!term) return true;
          return [country.country_code, country.country_name, vpngateRegionName(country.country_code)].some(value =>
            value.toLocaleLowerCase('zh-CN').includes(term),
          );
        })
        .sort((left, right) => {
          const codeOrder = left.country_code.localeCompare(right.country_code);
          if (sort === 'available') {
            return right.measured_successful - left.measured_successful || codeOrder;
          }
          if (sort === 'candidate') {
            return compareCountriesByCandidate(left, right);
          }
          return codeOrder;
        }),
    [countries, sort, term],
  );
  return (
    <section
      className="panel config-panel cg-sec vpngate-country-rail"
      role="complementary"
      aria-label="地区"
      aria-busy={pending}
    >
      <header>
        <PanelTitle of="region">地区</PanelTitle>
        <span className="cg-meta">
          <b>
            {pending ? (
              <FieldLoading announce={false} />
            ) : term ? (
              `${filtered.length} / ${countries.length}`
            ) : (
              countries.length
            )}
          </b>
        </span>
      </header>
      <div className="vpngate-country-controls">
        <input
          className="f"
          type="search"
          value={search}
          placeholder="搜索"
          aria-label="搜索地区或代码"
          onChange={event => onSearch(event.target.value)}
        />
        <select
          className="f"
          aria-label="地区排序"
          value={sort}
          onChange={event => setSort(event.target.value as CountrySort)}
        >
          <option value="candidate">候选优先</option>
          <option value="available">可用优先</option>
          <option value="region">地区代码</option>
        </select>
      </div>
      <div className="vpngate-country-list">
        {filtered.map(country => {
          const configured = pools.filter(
            reference => reference.pool.protocol.v.country_code === country.country_code,
          ).length;
          const name = vpngateRegionName(country.country_code);
          return (
            <button
              type="button"
              key={country.country_code}
              aria-pressed={country.country_code === countryCode}
              aria-label={`${name}，${vpngateDirectoryServerCount(country)} 个目录节点，${country.measured_successful} 个可用节点，${country.candidate_servers} 个候选节点`}
              onClick={() => onCountry(country.country_code)}
            >
              <RegionFlag code={country.country_code} />
              <span className="vpngate-country-name">
                <b>
                  {name}
                  {configured > 0 && <i title={`线路已使用 ${configured} 个节点池`} />}
                </b>
                <small>{country.country_code}</small>
              </span>
              <span className="vpngate-country-count">
                {vpngateDirectoryServerCount(country)}
                <small className={country.measured_successful > 0 ? 'ok' : ''}>
                  可用 {country.measured_successful} · 候选 {country.candidate_servers}
                </small>
              </span>
            </button>
          );
        })}
        {filtered.length === 0 && (
          <p className="vpngate-country-empty">{pending ? <FieldLoading /> : '没有匹配的地区。'}</p>
        )}
      </div>
    </section>
  );
}

function CountrySummary({
  country,
  countryCode,
  pending,
}: {
  country?: VpngateCountrySummary;
  countryCode: string;
  pending: boolean;
}) {
  if (!country) {
    return pending ? (
      <section className="panel config-panel cg-sec vpngate-country-summary" aria-busy="true">
        <header>
          <PanelTitle of="outbound">{countryCode ? `${vpngateRegionName(countryCode)}出口池` : '地区概要'}</PanelTitle>
          {countryCode && (
            <span className="cg-meta">
              <RegionFlag code={countryCode} />
              <small>{countryCode}</small>
            </span>
          )}
        </header>
        <p className="vpngate-empty">
          <FieldLoading />
        </p>
      </section>
    ) : null;
  }
  return (
    <section className="panel config-panel cg-sec vpngate-country-summary">
      <header>
        <PanelTitle of="outbound">{vpngateRegionName(countryCode)}出口池</PanelTitle>
        <span className="cg-meta">
          <RegionFlag code={countryCode} />
          <small>{countryCode}</small>
        </span>
      </header>
      <dl className="cg-kv">
        <dt>目录节点</dt>
        <dd>{vpngateDirectoryServerCount(country)}</dd>
        <dt>候选节点</dt>
        <dd>{country.candidate_servers}</dd>
        <dt>实测可用</dt>
        <dd>{country.measured_successful}</dd>
      </dl>
      <footer>
        <button className="btn sm" type="button" onClick={() => navigate('chains')}>
          在线路中使用
        </button>
      </footer>
    </section>
  );
}

function CountryWorkspace({
  country,
  countryCode,
  catalogPending,
  pools,
  poolsPending,
  servers,
  serversPending,
  runtimes,
  runtimesPending,
  runtimesError,
  systemAdmin,
  directorySearchEnabled,
}: {
  country?: VpngateCountrySummary;
  countryCode: string;
  catalogPending: boolean;
  pools: VpngatePoolReference[];
  poolsPending: boolean;
  servers: VpngateServerView[];
  serversPending: boolean;
  runtimes: VpngateRuntimeView[];
  runtimesPending: boolean;
  runtimesError: unknown;
  systemAdmin: boolean;
  directorySearchEnabled: boolean;
}) {
  if (!country && !catalogPending) {
    return (
      <div className="vpngate-work">
        <Empty>目录还没有可选择的地区。</Empty>
      </div>
    );
  }
  if (!country) {
    return (
      <div className="vpngate-work">
        <section className="panel titled cg-sec vpngate-card vpngate-pool-runtime" aria-busy="true">
          <header>
            <PanelTitle of="chains">规则与承载机器</PanelTitle>
          </header>
          <p className="vpngate-empty">
            <FieldLoading />
          </p>
        </section>
        <section className="panel titled cg-sec vpngate-card vpngate-candidates" aria-busy="true">
          <header>
            <PanelTitle of="servers">目录节点</PanelTitle>
            <span className="cg-meta">
              <b>
                <FieldLoading announce={false} />
              </b>
            </span>
          </header>
          <p className="vpngate-empty">
            <FieldLoading />
          </p>
        </section>
      </div>
    );
  }
  const countryPools = pools.filter(reference => reference.pool.protocol.v.country_code === countryCode);
  return (
    <div className="vpngate-work">
      <PoolRuntimePanel
        pools={countryPools}
        poolsPending={poolsPending}
        servers={servers}
        runtimes={runtimes}
        runtimesPending={runtimesPending}
        runtimesError={runtimesError}
        systemAdmin={systemAdmin}
      />

      <section className="panel titled cg-sec vpngate-card vpngate-candidates" aria-busy={serversPending}>
        <header>
          <PanelTitle of="servers">目录节点</PanelTitle>
          <span className="cg-meta">
            <b>
              {serversPending ? (
                <FieldLoading announce={false} />
              ) : (
                `${servers.filter(server => server.active).length} 个候选`
              )}
            </b>
          </span>
        </header>
        <CandidateTable key={countryCode} countryCode={countryCode} searchEnabled={directorySearchEnabled} />
      </section>
    </div>
  );
}

const runtimeConnectTimedOut = (runtime: VpngateRuntimeView) =>
  runtime.latest_probe_status === 'failed' &&
  runtime.latest_error_detail?.includes('OpenVPN initialization timed out') === true;

const runtimeConnectRank = (runtime: VpngateRuntimeView) =>
  runtimeConnectTimedOut(runtime) ? 2 : (runtime.latest_connect_ms ?? 0) > 0 ? 0 : 1;

const runtimeConnectLabel = (runtime: VpngateRuntimeView) => {
  if (runtimeConnectTimedOut(runtime)) {
    return `超时 ${VPNGATE_CONNECT_THRESHOLD_MAX_MS / 1_000} 秒`;
  }
  return (runtime.latest_connect_ms ?? 0) > 0 ? `${runtime.latest_connect_ms} ms` : '待更新';
};

function RuntimeCard({
  runtime,
  servers,
  systemAdmin,
}: {
  runtime: VpngateRuntimeView;
  servers: VpngateServerView[];
  systemAdmin: boolean;
}) {
  const now = useNow();
  const queryClient = useQueryClient();
  const realtime = useNodeRealtime(runtime.node_id);
  const fresh = freshVpngateReport(realtime);
  const report = fresh ?? realtime.last?.event.sample.vpngate;
  const pool = report?.pools.find(value => value.outbound_id === runtime.outbound_id);
  const backends = report?.backends.filter(value => value.outbound_id === runtime.outbound_id) ?? [];
  const active = backends.find(value => value.role === 'active' && value.slot === pool?.active_slot);
  const standby = backends.find(value => value.role === 'standby');
  const stale = Boolean(pool) && !fresh;
  const selectedServerId = pool ? (active?.server_id ?? null) : runtime.selected_server_id;
  const selectedServer = selectedServerId ? servers.find(server => server.id === selectedServerId) : undefined;
  const selectedHostname = selectedServerId
    ? (selectedServer?.hostname ??
      (selectedServerId === runtime.selected_server_id ? runtime.selected_hostname : null) ??
      selectedServerId)
    : null;
  // Measurements belong to the machine + pool + backend, not the shared catalogue. Hide the
  // previous backend's evidence until the live-aware runtime endpoint returns the matching row.
  const evidence = selectedServerId === runtime.selected_server_id ? runtime : null;
  // A catalogue sample is not proof of this machine's current exit. It is still useful while the
  // selected Agent sample is in transit, provided the UI labels its different scope explicitly.
  const candidateReference = !evidence?.latest_exit_ip && selectedServer?.latest_exit_ip ? selectedServer : null;
  const displayEvidence = evidence?.latest_exit_ip ? evidence : candidateReference;
  const liveServerId = fresh && pool ? selectedServerId : undefined;
  useEffect(() => {
    if (liveServerId !== undefined) void queryClient.invalidateQueries({ queryKey: ['vpngate', 'runtimes'] });
  }, [liveServerId, queryClient]);
  const [confirming, setConfirming] = useState<{ id: string; name: string } | null>(null);
  const switching = useMutation({
    mutationFn: (serverId: string) => requestVpngatePoolSwitch(runtime.node_id, runtime.outbound_id, serverId),
    onSuccess: async () => {
      setConfirming(null);
      await queryClient.invalidateQueries({ queryKey: ['vpngate', 'runtimes'] });
    },
  });
  const failed = pool
    ? stale || pool.state !== 'healthy'
    : Boolean(runtime.latest_error_code) || runtime.runtime_status !== 'running';
  const switchFailed = runtime.switch_status === 'failed' || Boolean(switching.error);
  const switchPending = runtime.switch_status === 'pending' || switching.isPending;
  const cooldownSeconds = Math.max(0, (runtime.switch_cooldown_until_unix_secs ?? 0) - Math.floor(now / 1_000));
  const cooldownLabel = `${String(Math.floor(cooldownSeconds / 60)).padStart(2, '0')}:${String(
    cooldownSeconds % 60,
  ).padStart(2, '0')}`;
  const footerText = switchPending
    ? '正在切换节点…'
    : switchFailed
      ? `切换失败 · ${runtime.switch_error_detail ?? (switching.error instanceof Error ? switching.error.message : '请重试')}`
      : cooldownSeconds > 0 && runtime.switch_status === 'applied'
        ? `${runtime.switch_previous_hostname ?? '旧节点'}冷却 ${cooldownLabel}`
        : evidence?.latest_error_code
          ? `${evidence.latest_error_code} · ${evidence.latest_error_detail ?? '拨测失败'}`
          : `最后拨测 ${dateTime(evidence?.latest_probed_at_unix_secs)}`;
  const showStatus =
    switchPending ||
    switchFailed ||
    cooldownSeconds > 0 ||
    Boolean(evidence?.latest_error_code) ||
    (!pool && runtime.runtime_status !== 'running');
  const backendLabel = { starting: '启动中', healthy: '健康', unhealthy: '不可用', backoff: '退避中' };
  return (
    <>
      <div className={`vpngate-runtime-entry${failed || switchFailed ? ' warn' : ''}`}>
        <article className="vpngate-runtime-row">
          <div className="vpngate-runtime-machine">
            <span className={`node-lamp ${failed ? 'warn' : 'ok'}`} aria-hidden />
            <span>
              <b>{runtime.node_name}</b>
              <small className="mono">{runtime.node_id}</small>
            </span>
          </div>
          <div className="vpngate-runtime-address">
            <strong className="mono">
              {displayEvidence?.latest_exit_ip ?? (selectedServerId ? '出口待更新' : '尚无出口')}
            </strong>
            <small>
              {displayEvidence?.latest_exit_country_code
                ? vpngateRegionName(displayEvidence.latest_exit_country_code)
                : '出口待验证'}
              {candidateReference ? ' · 候选参考' : ''}
            </small>
          </div>
          <div className="vpngate-runtime-candidate">
            <small>
              {pool
                ? stale
                  ? '上次主用 · 数据已过期'
                  : `主用 · ${active ? backendLabel[active.state] : '等待候选'}`
                : '上次上报主用'}
            </small>
            <b title={selectedHostname ?? undefined}>{selectedHostname ?? '尚未选中节点'}</b>
            <RuntimeNetworkSummary networks={displayEvidence?.latest_ip_networks ?? []} />
          </div>
          <div className="vpngate-runtime-metric vpngate-runtime-probed">
            <b>{dateTime(displayEvidence?.latest_probed_at_unix_secs)}</b>
            <small>{candidateReference ? '参考拨测' : '最后拨测'}</small>
          </div>
          <div className="vpngate-runtime-metric vpngate-runtime-risk">
            <b>
              <IpScores scores={displayEvidence?.latest_ip_scores ?? []} />
            </b>
            <IntelligenceAge
              verifiedAt={displayEvidence?.intelligence_verified_at_unix_secs ?? null}
              stale={displayEvidence?.intelligence_stale ?? false}
              historical={displayEvidence?.latest_probe_status === 'failed'}
            />
          </div>
          <div className="vpngate-runtime-metric">
            <b className={evidence && runtimeConnectTimedOut(evidence) ? 'warn' : ''}>
              {displayEvidence
                ? (displayEvidence.latest_connect_ms ?? 0) > 0
                  ? `${displayEvidence.latest_connect_ms} ms`
                  : evidence
                    ? runtimeConnectLabel(evidence)
                    : '待更新'
                : '待更新'}
            </b>
            <small>建连</small>
          </div>
          <div className="vpngate-runtime-metric vpngate-runtime-throughput">
            <b>{rate(displayEvidence?.latest_download_bps)}</b>
            <small>单流性能</small>
          </div>
          <div className="vpngate-runtime-action">
            {systemAdmin && runtime.automatic_pool && (
              <button
                className="btn primary sm"
                type="button"
                disabled={!selectedServerId || stale || switchPending}
                onClick={() =>
                  selectedServerId &&
                  setConfirming({ id: selectedServerId, name: selectedHostname ?? selectedServerId })
                }
              >
                {switchPending ? '切换中…' : '切换节点'}
              </button>
            )}
          </div>
        </article>
        {pool && (
          <div className={`vpngate-runtime-paths${stale ? ' stale' : ''}`}>
            <span className="vpngate-runtime-standby">
              <span className={`node-lamp ${stale ? '' : standby?.state === 'healthy' ? 'ok' : 'warn'}`} aria-hidden />
              <span>{stale ? '上次备用' : '备用'}</span>
              <b className="mono">
                {standby
                  ? (servers.find(server => server.id === standby.server_id)?.hostname ?? standby.server_id)
                  : '等待补位'}
              </b>
              {standby && <span>{backendLabel[standby.state]}</span>}
            </span>
            <span className="vpngate-runtime-failovers" title="计数自本次 Agent 启动">
              主备切换 {pool.failovers} 次
            </span>
            <span>
              {stale
                ? '实时数据已过期'
                : pool.state === 'failing_over'
                  ? '正在主备切换'
                  : `就绪备用 ${pool.ready_standbys} · 候选 ${pool.candidate_count}`}
            </span>
          </div>
        )}
        {showStatus && (
          <div className="vpngate-runtime-status" title={footerText}>
            <span className={`node-lamp ${failed || switchFailed ? 'warn' : 'ok'}`} aria-hidden />
            <span>{footerText}</span>
          </div>
        )}
      </div>
      {confirming && (
        <Confirm
          title="切换 VPN Gate 节点"
          body={
            <p>
              当前：{confirming.name}。成功后旧节点冷却 10 分钟，无可用替代时保持现状。
              {confirming.id !== selectedServerId && <span className="warn">主用节点已变化，请关闭后重新确认。</span>}
            </p>
          }
          confirmLabel={switching.isPending ? '切换中…' : '确认切换'}
          danger={false}
          confirmDisabled={switching.isPending || stale || confirming.id !== selectedServerId}
          onConfirm={() => switching.mutate(confirming.id)}
          onCancel={() => setConfirming(null)}
        />
      )}
    </>
  );
}

function CandidateTable({ countryCode, searchEnabled }: { countryCode: string; searchEnabled: boolean }) {
  const [search, setSearch] = useState('');
  const [querySearch, setQuerySearch] = useState('');
  const [filter, setFilter] = useState<VpngateDirectoryFilter>('all');
  const [sort, setSort] = useState<VpngateDirectorySort>('candidate');
  const [page, setPage] = useState(1);
  useEffect(() => {
    const timer = window.setTimeout(() => setQuerySearch(search.trim()), 250);
    return () => window.clearTimeout(timer);
  }, [search]);
  const request = useMemo<VpngateServerPageRequest>(
    () => ({
      page,
      page_size: VPNGATE_DIRECTORY_PAGE_SIZE,
      search: searchEnabled ? querySearch : '',
      filter,
      sort,
    }),
    [filter, page, querySearch, searchEnabled, sort],
  );
  const directory = useQuery({
    queryKey: directoryQueryKey(countryCode, request),
    queryFn: () => fetchVpngateCountryServers(countryCode, request),
    retry: false,
    staleTime: 30_000,
    refetchInterval: 60_000,
    placeholderData: previous => previous,
  });
  const servers = directory.data?.items ?? [];
  const total = directory.data?.total ?? 0;
  const pageCount = Math.max(1, Math.ceil(total / VPNGATE_DIRECTORY_PAGE_SIZE));
  const currentPage = Math.min(directory.data?.page ?? page, pageCount);
  const pageStart = (currentPage - 1) * VPNGATE_DIRECTORY_PAGE_SIZE;
  const pageEnd = Math.min(pageStart + servers.length, total);
  const resetFilters = () => {
    setSearch('');
    setQuerySearch('');
    setFilter('all');
    setSort('candidate');
    setPage(1);
  };

  if (!directory.isPending && !directory.error && total === 0 && !querySearch && filter === 'all') {
    return (
      <div className="vpngate-empty">
        <b>这个地区还没有目录节点</b>
        <span>下一次完整目录同步后会自动补充；已发现的历史服务器不会因快照波动消失。</span>
      </div>
    );
  }
  return (
    <div className="vpngate-directory-table">
      <div className="vpngate-directory-tools">
        <input
          className="f"
          type="search"
          value={search}
          placeholder="搜索节点、IP、ISP"
          aria-label="搜索目录节点"
          disabled={!searchEnabled}
          title={searchEnabled ? undefined : '当前身份下目录字段已脱敏，不能按原始内容搜索'}
          onChange={event => {
            setSearch(event.target.value);
            setPage(1);
          }}
        />
        <select
          className="f"
          aria-label="目录节点筛选"
          value={filter}
          onChange={event => {
            setFilter(event.target.value as VpngateDirectoryFilter);
            setPage(1);
          }}
        >
          <option value="all">全部节点</option>
          <option value="candidate">候选节点</option>
          <option value="successful">有成功样本</option>
          <option value="failed">最近失败</option>
          <option value="pending">等待拨测</option>
          <option value="current">当前目录</option>
          <option value="retained">历史保留</option>
        </select>
        <select
          className="f"
          aria-label="目录节点排序"
          value={sort}
          onChange={event => {
            setSort(event.target.value as VpngateDirectorySort);
            setPage(1);
          }}
        >
          <option value="candidate">候选规则排序</option>
          <option value="download">全局下载高到低</option>
          <option value="connect">全局建连短到长</option>
          <option value="catalog">目录质量高到低</option>
          <option value="samples">成功样本多到少</option>
          <option value="recent">最近拨测优先</option>
          <option value="hostname">节点名称</option>
        </select>
        <span className="vpngate-directory-range" aria-live="polite">
          {directory.isPending ? (
            <FieldLoading announce={false} />
          ) : (
            <>
              {total ? `${pageStart + 1}–${pageEnd} / ${total}` : '0 / 0'}
              <small>每页 {VPNGATE_DIRECTORY_PAGE_SIZE}</small>
            </>
          )}
        </span>
      </div>
      {directory.error ? (
        <ErrorBox error={directory.error} />
      ) : directory.isPending ? (
        <p className="vpngate-empty">
          <FieldLoading />
        </p>
      ) : total === 0 ? (
        <div className="vpngate-empty vpngate-directory-filter-empty">
          <b>没有匹配的目录节点</b>
          <span>调整搜索或筛选条件后再试。</span>
          <button className="btn sm" type="button" onClick={resetFilters}>
            清除筛选
          </button>
        </div>
      ) : (
        <div className="vpngate-table-wrap">
          <table className="vpngate-server-table">
            <thead>
              <tr className="group">
                <th scope="col" rowSpan={2}>
                  节点
                </th>
                <th scope="colgroup" colSpan={2}>
                  VPN Gate 目录数据
                </th>
                <th scope="colgroup" colSpan={5}>
                  Brocade 全局实测
                </th>
              </tr>
              <tr>
                <th scope="col">线路质量</th>
                <th scope="col" className="num">
                  会话
                </th>
                <th scope="col">全局质量</th>
                <th scope="col">IP 风险</th>
                <th scope="col">ISP / 类型</th>
                <th scope="col">验证出口</th>
                <th scope="col">样本</th>
              </tr>
            </thead>
            <tbody>
              {servers.map((server, index) => (
                // Readonly responses intentionally collapse provider hostnames to the same masked
                // value. Keep the server-side order in the key so every evidence row still renders.
                <tr key={`${server.id}/${pageStart + index}`} className={server.active ? 'active' : ''}>
                  <td className="vpngate-host">
                    <b>{server.hostname}</b>
                    <small className="mono">{server.ip}</small>
                    {server.candidate_rank != null && (
                      <span className="st">
                        {server.active ? '候选' : '候补'} #{server.candidate_rank}
                        {server.pareto_layer != null ? ` · L${server.pareto_layer}` : ''}
                      </span>
                    )}
                  </td>
                  <td className="vpngate-metric-cell">
                    <span>{rate(server.catalog_speed_bps)}</span>
                    <small>{server.ping_ms == null ? '延迟 —' : `延迟 ${server.ping_ms} ms`}</small>
                  </td>
                  <td className="num">{server.vpn_sessions}</td>
                  <td className="vpngate-metric-cell">
                    <span>{rate(server.global_download_bps)}</span>
                    <small>
                      {server.global_connect_ms == null ? '全局建连 —' : `全局建连 ${server.global_connect_ms} ms`}
                    </small>
                  </td>
                  <td className="vpngate-risk-cell">
                    <IpScores scores={server.latest_ip_scores} />
                    <IntelligenceAge
                      verifiedAt={server.intelligence_verified_at_unix_secs}
                      stale={server.intelligence_stale}
                      historical={server.latest_probe_status === 'failed'}
                    />
                  </td>
                  <td>
                    <IpNetwork networks={server.latest_ip_networks} />
                  </td>
                  <td className="vpngate-exit-cell">
                    <span className="mono">{server.latest_exit_ip ?? '—'}</span>
                    <small>
                      {server.latest_ip_scores.length
                        ? [
                            ...new Set(
                              server.latest_ip_scores
                                .map(score => score.country_code)
                                .filter(countryCode => countryCode.length === 2),
                            ),
                          ]
                            .map(vpngateRegionName)
                            .join(' / ')
                        : '未验证'}
                    </small>
                  </td>
                  <td className="vpngate-evidence-cell">
                    <b>{server.successful_samples}</b>
                    <small>
                      {server.latest_probe_status === 'failed'
                        ? `${server.latest_error_code ?? '拨测失败'} · 上次成功 ${dateTime(
                            server.latest_successful_probed_at_unix_secs,
                          )}`
                        : `${server.measured_nodes} 台机器 · ${dateTime(server.latest_probed_at_unix_secs)}`}
                    </small>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      {pageCount > 1 && (
        <footer className="vpngate-directory-pagination" aria-label="目录节点分页">
          <label>
            第
            <select
              className="f"
              aria-label="目录节点页码"
              value={currentPage}
              onChange={event => setPage(Number(event.target.value))}
            >
              {Array.from({ length: pageCount }, (_, index) => (
                <option key={index + 1} value={index + 1}>
                  {index + 1}
                </option>
              ))}
            </select>
            / {pageCount} 页
          </label>
          <div>
            <button
              className="btn sm"
              type="button"
              disabled={currentPage === 1}
              onClick={() => setPage(currentPage - 1)}
            >
              上一页
            </button>
            <button
              className="btn sm"
              type="button"
              disabled={currentPage === pageCount}
              onClick={() => setPage(currentPage + 1)}
            >
              下一页
            </button>
          </div>
        </footer>
      )}
    </div>
  );
}

function VpngateCapabilityPanel({
  nodes,
  pending,
  error,
  systemAdmin,
}: {
  nodes: NodeAgentStateItem[];
  pending: boolean;
  error: unknown;
  systemAdmin: boolean;
}) {
  const queryClient = useQueryClient();
  const [showUnavailable, setShowUnavailable] = useState(false);
  const selection = useMutation({
    mutationFn: ({ nodeId, enabled, workers }: { nodeId: string; enabled: boolean; workers: number }) =>
      updateVpngateProbeNode(nodeId, enabled, workers),
    onSuccess: result => {
      queryClient.setQueryData<{ nodes: NodeAgentStateItem[] }>(['nodes'], current =>
        current
          ? {
              nodes: current.nodes.map(node =>
                node.node_id === result.node_id
                  ? {
                      ...node,
                      vpngate_probe_enabled: result.enabled,
                      vpngate_probe_workers: result.workers,
                    }
                  : node,
              ),
            }
          : current,
      );
    },
  });
  const rank = ({
    node,
    eligibility,
  }: {
    node: NodeAgentStateItem;
    eligibility: ReturnType<typeof vpngateNodeEligibility>;
  }) => {
    if (node.vpngate_probe_enabled) return eligibility.eligible ? 1 : 0;
    return eligibility.eligible ? 2 : 3;
  };
  const rows = nodes
    .map(node => ({ node, eligibility: vpngateNodeEligibility(node) }))
    .sort(
      (left, right) =>
        rank(left) - rank(right) ||
        Number(Boolean(right.node.vpngate_probe_data_stale)) - Number(Boolean(left.node.vpngate_probe_data_stale)) ||
        left.node.name.localeCompare(right.node.name),
    );
  const selectedRows = rows.filter(row => row.node.vpngate_probe_enabled);
  const hiddenRows = rows.filter(row => rank(row) === 3);
  const visibleRows = showUnavailable ? rows : rows.filter(row => rank(row) < 3);
  const totalWorkers = selectedRows
    .filter(row => row.eligibility.eligible)
    .reduce((sum, { node }) => {
      const capacity = Math.max(
        1,
        Math.min(VPNGATE_MAX_PROBE_WORKERS, node.runtime_versions?.vpngate_catalog_probe_workers ?? 1),
      );
      return sum + Math.max(1, Math.min(capacity, node.vpngate_probe_workers ?? VPNGATE_DEFAULT_PROBE_WORKERS));
    }, 0);
  const brokenSelected = selectedRows.filter(
    row => !row.eligibility.eligible || row.node.vpngate_probe_data_stale,
  ).length;
  return (
    <section className="panel titled cg-sec vpngate-card vpngate-probe-section" aria-busy={pending}>
      <header>
        <PanelTitle of="nodes">拨测机器</PanelTitle>
        <span className="cg-meta">
          {pending ? (
            <FieldLoading announce={false} />
          ) : (
            <>
              <span>
                参与 <b>{selectedRows.length}</b> 台
              </span>
              <span>
                并发 <b>{totalWorkers}</b>
              </span>
              {brokenSelected > 0 && (
                <span>
                  <b className="warn">{brokenSelected}</b> 台异常
                </span>
              )}
            </>
          )}
        </span>
      </header>
      <div className="vpngate-capability-body">
        <p className="vpngate-collection-scope-note">
          这里只选择执行 VPN Gate OpenVPN 目录拨测的机器，与设置中的情报执行 Agent 独立。
        </p>
        {pending ? (
          <p className="vpngate-empty">
            <FieldLoading />
          </p>
        ) : error ? (
          <ErrorBox error={error} />
        ) : rows.length === 0 ? (
          <p className="vpngate-empty">当前权限范围内还没有 Agent 机器。</p>
        ) : visibleRows.length === 0 ? (
          <p className="vpngate-empty">没有满足拨测条件的机器。</p>
        ) : (
          <div className="vpngate-capability-list">
            {visibleRows.map(({ node, eligibility }) => {
              const availableWorkers = Math.max(
                1,
                Math.min(VPNGATE_MAX_PROBE_WORKERS, node.runtime_versions?.vpngate_catalog_probe_workers ?? 1),
              );
              const configuredWorkers = Math.max(
                1,
                Math.min(availableWorkers, node.vpngate_probe_workers ?? VPNGATE_DEFAULT_PROBE_WORKERS),
              );
              const capability = eligibility.version?.match(/^OpenVPN\s+\S+/)?.[0] ?? eligibility.reason;
              const probeDataStale = node.vpngate_probe_enabled && Boolean(node.vpngate_probe_data_stale);
              const lastProbeDataAt = node.vpngate_probe_reported_at
                ? new Date(node.vpngate_probe_reported_at).toLocaleString('zh-CN')
                : null;
              const staleWarning = lastProbeDataAt
                ? `拨测长时间无有效数据更新；最后有效数据：${lastProbeDataAt}`
                : '拨测长时间无有效数据更新；启用后尚未收到有效数据';
              const workerOptions = [...new Set([1, 2, 4, 8, 16, 32, 64, 128, configuredWorkers])]
                .filter(workers => workers <= availableWorkers)
                .sort((left, right) => left - right);
              return (
                <article
                  className={
                    node.vpngate_probe_enabled
                      ? eligibility.eligible && !probeDataStale
                        ? 'selected'
                        : 'selected warn'
                      : eligibility.eligible
                        ? undefined
                        : 'unavailable'
                  }
                  key={node.node_id}
                  aria-label={node.name}
                >
                  {probeDataStale && (
                    <span
                      className="vpngate-probe-stale-warning"
                      role="img"
                      aria-label={`${node.name}：${staleWarning}`}
                      title={staleWarning}
                    >
                      !
                    </span>
                  )}
                  <div className="vpngate-capability-head">
                    <RegionFlag code={node.public_ipv4_country} />
                    <b>{node.name}</b>
                    <span
                      className={`cg-lamp ${node.vpngate_probe_enabled ? (eligibility.eligible && !probeDataStale ? 'ok' : 'warn') : 'idle'}`}
                      aria-hidden
                    />
                  </div>
                  <small title={eligibility.version ?? eligibility.reason}>
                    {node.node_id} · {capability}
                  </small>
                  {(node.vpngate_probe_enabled || eligibility.eligible) && (
                    <div className="vpngate-capability-actions">
                      {node.vpngate_probe_enabled && (
                        <select
                          className="f vpngate-capability-workers"
                          aria-label={`${node.name} 的目录拨测并发数`}
                          title="目录拨测并发数"
                          value={configuredWorkers}
                          disabled={!systemAdmin || selection.isPending}
                          onChange={event =>
                            selection.mutate({
                              nodeId: node.node_id,
                              enabled: true,
                              workers: Number(event.currentTarget.value),
                            })
                          }
                        >
                          {workerOptions.map(workers => (
                            <option value={workers} key={workers}>
                              {workers} 并发
                            </option>
                          ))}
                        </select>
                      )}
                      <button
                        className="btn sm"
                        type="button"
                        disabled={
                          !systemAdmin || selection.isPending || (!node.vpngate_probe_enabled && !eligibility.eligible)
                        }
                        aria-label={`${node.vpngate_probe_enabled ? '停用' : '启用'} ${node.name} 的 VPN Gate 目录拨测`}
                        onClick={() =>
                          selection.mutate({
                            nodeId: node.node_id,
                            enabled: !node.vpngate_probe_enabled,
                            workers: configuredWorkers,
                          })
                        }
                      >
                        {node.vpngate_probe_enabled ? '移出' : '加入'}
                      </button>
                    </div>
                  )}
                </article>
              );
            })}
          </div>
        )}
        {selection.error && <ErrorBox error={selection.error} />}
      </div>
      {!pending && !error && hiddenRows.length > 0 && (
        <button
          className="vpngate-capability-more"
          type="button"
          aria-expanded={showUnavailable}
          onClick={() => setShowUnavailable(current => !current)}
        >
          {showUnavailable ? '隐藏' : '显示'}不满足条件的 {hiddenRows.length} 台
        </button>
      )}
    </section>
  );
}

function PoolRuntimePanel({
  pools,
  poolsPending,
  servers,
  runtimes,
  runtimesPending,
  runtimesError,
  systemAdmin,
}: {
  pools: VpngatePoolReference[];
  poolsPending: boolean;
  servers: VpngateServerView[];
  runtimes: VpngateRuntimeView[];
  runtimesPending: boolean;
  runtimesError: unknown;
  systemAdmin: boolean;
}) {
  const orderedRuntimes = [...runtimes].sort((left, right) => runtimeConnectRank(left) - runtimeConnectRank(right));
  return (
    <section
      className="panel titled cg-sec vpngate-card vpngate-usage-section"
      aria-busy={poolsPending || runtimesPending}
    >
      <header>
        <PanelTitle of="chains">规则与承载机器</PanelTitle>
      </header>
      {poolsPending ? (
        <p className="vpngate-empty">
          <FieldLoading />
        </p>
      ) : pools.length === 0 ? (
        <div className="vpngate-empty compact">
          <span>当前未被线路规则使用，因此没有承载机器</span>
        </div>
      ) : (
        <div className="vpngate-pool-list">
          {pools.map(({ pool, ruleCount, nodeCount }) => {
            const ids = vpngateServerIds(pool);
            const poolRuntimes = orderedRuntimes.filter(runtime => runtime.outbound_id === pool.id);
            return (
              <section className="vpngate-pool-runtime-group" key={pool.id}>
                <header className="vpngate-pool-head">
                  <RegionFlag code={pool.protocol.v.country_code} />
                  <span>
                    <b>{pool.name}</b>
                    <small className="mono">
                      {pool.id} · {ids.length ? `手动池 ${ids.length} 个节点` : '地区自动池'} · 候选上限{' '}
                      {pool.protocol.v.max_candidates}
                    </small>
                  </span>
                  <span className="vpngate-pool-summary">
                    {ruleCount} 条规则 · {nodeCount} 台机器
                  </span>
                </header>
                {runtimesPending ? (
                  <p className="vpngate-empty">
                    <FieldLoading />
                  </p>
                ) : runtimesError ? null : poolRuntimes.length === 0 ? (
                  <div className="vpngate-empty compact">
                    <span>暂无承载机器</span>
                  </div>
                ) : (
                  <div className="vpngate-runtime-list">
                    {poolRuntimes.map(runtime => (
                      <RuntimeCard
                        key={`${runtime.node_id}/${runtime.outbound_id}`}
                        runtime={runtime}
                        servers={servers}
                        systemAdmin={systemAdmin}
                      />
                    ))}
                  </div>
                )}
              </section>
            );
          })}
        </div>
      )}
      {runtimesError ? <ErrorBox error={runtimesError} /> : null}
    </section>
  );
}

function CollectionPanel({
  id,
  status,
  syncHistory,
  nodes,
  nodesPending,
  nodesError,
  systemAdmin,
}: {
  id: string;
  status: VpngateCatalogStatus;
  syncHistory: VpngateSyncHistoryPoint[];
  nodes: NodeAgentStateItem[];
  nodesPending: boolean;
  nodesError: unknown;
  systemAdmin: boolean;
}) {
  const queryClient = useQueryClient();
  const now = useNow();
  const { form, setForm } = useServerForm({
    enabled: status.enabled,
    interval: String(status.interval_secs),
  });
  const dirty = form.enabled !== status.enabled || form.interval !== String(status.interval_secs);
  const save = useMutation({
    mutationFn: () => updateVpngateCatalogSettings({ enabled: form.enabled, interval_secs: Number(form.interval) }),
    onSuccess: async () => queryClient.invalidateQueries({ queryKey: ['vpngate'] }),
  });
  const lastSync = syncHistory.at(-1);
  const lastSuccess = lastSync
    ? `上次成功 ${clockTime(lastSync.finished_at_unix_secs)} · ${relativeTime(lastSync.finished_at_unix_secs, now)}`
    : '尚无成功记录';
  const syncState = status.syncing
    ? {
        tone: 'run',
        lamp: 'run',
        label: '正在同步',
        copy: lastSuccess,
        due: null,
      }
    : status.last_error_code
      ? {
          tone: 'err',
          lamp: 'err',
          label: '同步失败',
          copy: `${lastSuccess}，目录保持该次快照`,
          due: `重试 ${clockTime(status.next_sync_at_unix_secs)}`,
        }
      : !status.enabled
        ? {
            tone: '',
            lamp: 'idle',
            label: '自动同步已暂停',
            copy: lastSync ? `目录保持 ${clockTime(lastSync.finished_at_unix_secs)} 的快照` : '尚无成功记录',
            due: null,
          }
        : {
            tone: '',
            lamp: 'ok',
            label: '同步正常',
            copy: lastSuccess,
            due: `下次同步 ${clockTime(status.next_sync_at_unix_secs)}`,
          };
  return (
    <section id={id} role="tabpanel" aria-labelledby="vpngate-tab-collection" className="vpngate-collection-view">
      <div className="vpngate-collection-grid">
        <section className="panel titled cg-sec vpngate-card vpngate-collection-sync">
          <header>
            <PanelTitle of="observe">VPN Gate 目录同步</PanelTitle>
            <span className="cg-meta">
              <span>
                快照 <b>{status.current_servers.toLocaleString('zh-CN')}</b>
              </span>
              <span>
                目录 <b>{status.retained_servers.toLocaleString('zh-CN')}</b>
              </span>
              <span>
                观测 <b>{status.retained_observations.toLocaleString('zh-CN')}</b>
              </span>
              <span>
                本轮首次发现 <b>{lastSync?.first_seen_servers?.toLocaleString('zh-CN') ?? '—'}</b>
              </span>
            </span>
          </header>
          <p className="vpngate-collection-scope-note">
            这里只管理 VPN Gate 目录的同步周期和快照；上游目录采集 Agent 在设置「情报任务」中统一选择。
          </p>
          <div className={`cgr-notice vpngate-collection-status ${syncState.tone}`} role="status">
            <span className={`cg-lamp ${syncState.lamp}`} aria-hidden />
            <span className="vpngate-collection-status-text">
              <b>{syncState.label}</b>
              {status.last_error_code && <code>{status.last_error_code}</code>}
              <span>{syncState.copy}</span>
            </span>
            {syncState.due && <span className="vpngate-collection-due">{syncState.due}</span>}
            {status.last_error_code && status.last_error_detail && (
              <p className="vpngate-collection-error-detail">{status.last_error_detail}</p>
            )}
          </div>

          <CollectionTrendChart history={syncHistory} />

          <div className="vpngate-collection-settings">
            <div className="vpngate-collection-controls">
              <label>
                自动同步
                <SegSwitch
                  checked={form.enabled}
                  disabled={!systemAdmin}
                  off="暂停"
                  on="启用"
                  ariaLabel="VPN Gate 自动同步"
                  onChange={enabled => setForm(current => ({ ...current, enabled }))}
                />
              </label>
              <label>
                同步间隔
                <select
                  className="f"
                  value={form.interval}
                  disabled={!systemAdmin}
                  onChange={event => setForm(current => ({ ...current, interval: event.target.value }))}
                >
                  <option value="300">5 分钟</option>
                  <option value="900">15 分钟</option>
                  <option value="1800">30 分钟</option>
                  <option value="3600">1 小时</option>
                </select>
              </label>
              <span className="vpngate-collection-commit">
                {save.isSuccess && !dirty && <span className="vpngate-collection-saved">已保存</span>}
                <button
                  className="btn primary"
                  type="button"
                  disabled={!systemAdmin || !dirty || save.isPending}
                  onClick={() => save.mutate()}
                >
                  {save.isPending ? '保存中…' : '保存'}
                </button>
              </span>
            </div>
            <div className="vpngate-collection-source" title={status.source_url}>
              来源<code>{status.source_url}</code>
            </div>
          </div>
          {save.error && <ErrorBox error={save.error} />}
        </section>

        <VpngateCapabilityPanel nodes={nodes} pending={nodesPending} error={nodesError} systemAdmin={systemAdmin} />
      </div>
    </section>
  );
}

function CollectionTrendChart({ history }: { history: VpngateSyncHistoryPoint[] }) {
  const elementRef = useRef<HTMLDivElement>(null);
  const [chart, setChart] = useState<EchartsInstance | null>(null);
  const themeName = useSyncExternalStore(theme.subscribe, theme.snapshot);
  const paletteName = useSyncExternalStore(palette.subscribe, palette.snapshot);
  const values = useMemo(() => history.map(point => point.current_servers), [history]);
  const latest = history.at(-1);
  const earliest = history.at(0);
  const minimum = values.length ? Math.min(...values) : 0;
  const maximum = values.length ? Math.max(...values) : 0;
  const delta = values.length > 1 ? values[values.length - 1] - values[values.length - 2] : 0;
  const aria = latest
    ? `最近 ${history.length} 次成功同步的快照节点数：${values[0]} 到 ${latest.current_servers}`
    : '尚无成功同步走势';

  useEffect(() => {
    const element = elementRef.current;
    if (!element) return;
    let disposed = false;
    let instance: EchartsInstance | null = null;
    let observer: ResizeObserver | null = null;
    void loadEcharts().then(echarts => {
      if (disposed) return;
      instance = echarts.init(element, null, { renderer: 'canvas' });
      observer =
        typeof ResizeObserver === 'undefined'
          ? null
          : new ResizeObserver(() => {
              instance?.resize();
            });
      observer?.observe(element);
      setChart(instance);
    });
    return () => {
      disposed = true;
      observer?.disconnect();
      instance?.dispose();
    };
  }, []);

  useEffect(() => {
    if (!chart) return;
    const css = getComputedStyle(document.documentElement);
    const color = (name: string) => css.getPropertyValue(name).trim();
    const dataColor = color('--data');
    const ink = color('--ink');
    const ink3 = color('--ink-3');
    const line = color('--line');
    const card = color('--card');
    const padding = Math.max(2, (maximum - minimum) * 0.25);
    chart.setOption(
      {
        animation: false,
        grid: { left: 2, right: 2, top: 8, bottom: 2, containLabel: false },
        textStyle: { fontFamily: 'ui-monospace, SFMono-Regular, Menlo, monospace' },
        tooltip: {
          trigger: 'axis',
          backgroundColor: card,
          borderColor: line,
          borderWidth: 1,
          padding: [6, 8],
          textStyle: { color: ink3, fontSize: 11 },
          formatter: (params: unknown) => {
            const [entry] = params as { dataIndex: number; value: number }[];
            const point = history[entry.dataIndex];
            return `${clockTime(point.finished_at_unix_secs)}<br/><b style="color:${ink}">${entry.value.toLocaleString('zh-CN')} 个节点</b>`;
          },
        },
        xAxis: {
          type: 'category',
          show: false,
          boundaryGap: false,
          data: history.map(point => point.finished_at_unix_secs),
        },
        yAxis: {
          type: 'value',
          show: false,
          min: Math.max(0, minimum - padding),
          max: maximum + padding,
        },
        series: [
          {
            type: 'line',
            data: values,
            smooth: 0.22,
            showSymbol: true,
            symbol: 'circle',
            symbolSize: (_value: number, params: { dataIndex: number }) =>
              params.dataIndex === values.length - 1 ? 6 : 0,
            lineStyle: { color: dataColor, width: 1.5, cap: 'round', join: 'round' },
            itemStyle: { color: dataColor, borderColor: card, borderWidth: 1.5 },
            areaStyle: { color: dataColor, opacity: 0.09 },
            emphasis: { disabled: true },
          },
        ],
      },
      true,
    );
  }, [chart, history, maximum, minimum, themeName, paletteName, values]);

  return (
    <div className="vpngate-collection-trend">
      <div ref={elementRef} className="vpngate-collection-chart" role="img" aria-label={aria} />
      <div className="vpngate-collection-axis">
        <span>{clockTime(earliest?.finished_at_unix_secs)}</span>
        <span>
          最近 {history.length} 次同步 · 快照节点 {minimum.toLocaleString('zh-CN')}–{maximum.toLocaleString('zh-CN')}
        </span>
        <span>
          <b>{latest?.current_servers.toLocaleString('zh-CN') ?? '—'}</b>{' '}
          {delta === 0 ? '±0' : delta > 0 ? `+${delta}` : delta}
        </span>
      </div>
    </div>
  );
}
