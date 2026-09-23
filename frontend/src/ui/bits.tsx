import {
  createContext,
  Suspense,
  useCallback,
  useContext,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from 'react';
import type { ConsoleInitialData } from '../api';
import { useNow } from './clock';
import { DialogClose, DialogLayer } from './dialog';
import { Icon, type IconName } from './icons';

/* 发布状态标签的配色：对应状态机，failed-dirty 使用最高对比度的红色 */
const STATUS_CLASS: Record<string, string> = {
  succeeded: 'st-succeeded',
  running: 'st-running',
  converging: 'st-running',
  dispatched: 'st-dispatched',
  planned: 'st-pending',
  pending: 'st-pending',
  deferred: 'st-gold',
  superseded: 'st-skipped',
  skipped: 'st-skipped',
  canceled: 'st-canceled',
  halted: 'st-halted',
  failed: 'st-halted',
  'failed-recovered': 'st-halted',
  'failed-dirty': 'st-failed-dirty',
};

// 状态标签的文案。协议中的取值是英文状态机名（succeeded / failed-dirty 等），界面显示中文——
// 本页其他内容均为中文，中间出现一串小写英文会被理解为内部代码。
//
// 这是全站唯一一份译名。发布列表此前另有一份，导致同一个 `running` 在列表中
// 显示为「推送中」而在详情的波次表中显示为其他文案——两个页面对应同一条发布，
// 会被理解为两种状态。修改文案时修改此处。
//
// 原始取值写入 title：查询日志、核对服务端返回、与 brocade-store 中的状态机对应时
// 需要该英文字符串。两者同时保留。
export const STATUS_TEXT: Record<string, string> = {
  succeeded: '成功',
  running: '推送中',
  converging: '收敛中',
  dispatched: '已下发',
  planned: '待推',
  pending: '待推',
  deferred: '隔离待补偿',
  superseded: '已被替代',
  skipped: '跳过',
  canceled: '已取消',
  halted: '已中止',
  failed: '失败',
  'failed-recovered': '失败已回滚',
  'failed-dirty': '失败未回滚',
};

export function Status({ value }: { value: string }) {
  return (
    <span className={`st ${STATUS_CLASS[value] ?? ''}`} title={value}>
      {STATUS_TEXT[value] ?? value}
    </span>
  );
}

export function Empty({ children }: { children: ReactNode }) {
  return <p className="note empty-compact">{children}</p>;
}

export function EmptyState({
  icon,
  title,
  children,
  action,
}: {
  icon: IconName;
  title: string;
  children: ReactNode;
  action?: ReactNode;
}) {
  return (
    <div className="empty-state">
      <span className="empty-state-mark">
        <Icon of={icon} size={25} />
      </span>
      <b>{title}</b>
      <p>{children}</p>
      {action}
    </div>
  );
}

export type LoadingVariant =
  | 'panel'
  | 'chart-panel'
  | 'table-panel'
  | 'control'
  | 'rows'
  | 'metrics'
  | 'table'
  | 'chart'
  | 'code'
  | 'tree'
  | 'editor'
  | 'nodes'
  | 'chains'
  | 'tunnels'
  | 'users'
  | 'usage'
  | 'usage-metrics'
  | 'links'
  | 'settings'
  | 'deploy'
  | 'detail'
  | 'chain-detail'
  | 'config-detail'
  | 'vpngate'
  | 'form'
  | 'plan'
  | 'deployment'
  | 'canvas';

// Product loading policy: keep a routed surface blank until its initial module and API dependencies
// have settled, then mount the complete page in one frame. The legacy skeleton implementation
// remains available only to focused tests; production must not paint a loading prompt or placeholder
// content that could be mistaken for real data.
const PREVIEW_LOADING_SKELETON = import.meta.env.DEV && import.meta.env.VITE_PREVIEW_LOADING_SKELETON === '1';

type LoadingRegistry = {
  variant: LoadingVariant;
  register(): () => void;
};

const LoadingRegistryContext = createContext<LoadingRegistry | null>(null);

function LoadingBoundaryReady({ onReady }: { onReady: () => void }) {
  useLayoutEffect(() => {
    onReady();
  }, [onReady]);
  return null;
}

/**
 * Owns one uninterrupted loading surface for a route. Nested Loading components only register their
 * pending lifetime, so a lazy import can hand off to API reads without exposing partial content.
 * Once every dependency is ready, the complete content mounts in one frame. That reveal is one-way
 * within a routed surface: later refetches and parameter changes keep
 * the real surface mounted, while a different routeKey starts a new lifecycle without remounting
 * its parent pane. The guard keeps the two surfaces from painting on top of each other during the
 * handoff.
 */
type LoadingBoundaryProps = {
  variant: LoadingVariant;
  fallback: ReactNode;
  children: ReactNode;
  /** A new routed surface gets its own one-way reveal lifecycle. */
  routeKey?: string;
};

type LoadingBoundaryState = {
  routeKey: string;
  moduleReady: boolean;
  pendingCount: number;
  revealed: boolean;
};

const DEFAULT_LOADING_ROUTE = '';

export function LoadingBoundary({
  variant,
  fallback,
  children,
  routeKey = DEFAULT_LOADING_ROUTE,
}: LoadingBoundaryProps) {
  const registrations = useRef(new Map<string, Set<symbol>>());
  const [state, setState] = useState<LoadingBoundaryState>({
    routeKey,
    moduleReady: false,
    pendingCount: 0,
    revealed: false,
  });

  const register = useCallback(() => {
    const token = Symbol('loading');
    const routeRegistrations = registrations.current.get(routeKey) ?? new Set<symbol>();
    routeRegistrations.add(token);
    registrations.current.set(routeKey, routeRegistrations);
    setState(current => {
      if (current.routeKey !== routeKey) {
        return { routeKey, moduleReady: false, pendingCount: routeRegistrations.size, revealed: false };
      }
      return current.revealed ? current : { ...current, pendingCount: routeRegistrations.size };
    });
    return () => {
      routeRegistrations.delete(token);
      if (routeRegistrations.size === 0) registrations.current.delete(routeKey);
      setState(current => {
        if (current.routeKey !== routeKey || current.revealed) return current;
        // StrictMode can run the matching setup again before this state updater is applied. Read
        // the current route set instead of the set captured by the old cleanup, or that replay is
        // mistaken for a completed request and permanently reveals an empty surface.
        const pendingCount = registrations.current.get(routeKey)?.size ?? 0;
        return {
          ...current,
          pendingCount,
          revealed: current.moduleReady && pendingCount === 0,
        };
      });
    };
  }, [routeKey]);
  const registry = useMemo<LoadingRegistry>(() => ({ variant, register }), [register, variant]);
  const markModuleReady = useCallback(
    () =>
      setState(current => {
        const pendingCount = registrations.current.get(routeKey)?.size ?? 0;
        if (current.routeKey !== routeKey) {
          return { routeKey, moduleReady: true, pendingCount, revealed: pendingCount === 0 };
        }
        if (current.revealed || current.moduleReady) return current;
        return { ...current, moduleReady: true, pendingCount, revealed: pendingCount === 0 };
      }),
    [routeKey],
  );

  useLayoutEffect(() => {
    setState(current =>
      current.routeKey === routeKey
        ? current
        : {
            routeKey,
            moduleReady: false,
            pendingCount: registrations.current.get(routeKey)?.size ?? 0,
            revealed: false,
          },
    );
  }, [routeKey]);

  const pending = state.routeKey !== routeKey || (!state.revealed && (!state.moduleReady || state.pendingCount > 0));

  return (
    <>
      {pending && fallback}
      {pending && <span className="loading-boundary-guard" aria-hidden="true" />}
      <LoadingRegistryContext.Provider value={registry}>
        <Suspense fallback={null}>
          {children}
          <LoadingBoundaryReady onReady={markModuleReady} />
        </Suspense>
      </LoadingRegistryContext.Provider>
      <span className="loading-boundary-end" aria-hidden="true" />
    </>
  );
}

const skeletonRange = (length: number) => Array.from({ length }, (_, index) => index);

function SkeletonRows({ count = 4 }: { count?: number }) {
  return (
    <div className="loading-skeleton-rows">
      {skeletonRange(count).map(row => (
        <div className="loading-skeleton-row" key={row}>
          <i className="loading-skeleton-dot" />
          <span>
            <i className="loading-bar" />
            <i className="loading-bar faint" />
          </span>
          <i className="loading-skeleton-chip" />
        </div>
      ))}
    </div>
  );
}

function SkeletonControl() {
  return (
    <div className="loading-skeleton-control">
      <span>
        <i className="loading-bar" />
        <i className="loading-bar faint" />
      </span>
      <i className="loading-skeleton-input" />
      <i className="loading-skeleton-chip" />
    </div>
  );
}

function SkeletonPanel({ rows = 4, fields = false }: { rows?: number; fields?: boolean }) {
  return (
    <section className={`loading-skeleton-panel${fields ? ' fields' : ''}`}>
      <div className="loading-skeleton-panel-head">
        <i className="loading-skeleton-title-icon" />
        <span className="loading-skeleton-title-copy">
          <i className="loading-bar title" />
          <i className="loading-bar hint" />
        </span>
        <i className="loading-skeleton-head-stat" />
        <i className="loading-skeleton-head-action" />
      </div>
      {fields ? (
        <div className="loading-skeleton-fields">
          {skeletonRange(rows).map(row => (
            <div className="loading-skeleton-field" key={row}>
              <i className="loading-bar label" />
              <i className="loading-skeleton-input" />
              <i className="loading-bar help" />
            </div>
          ))}
        </div>
      ) : (
        <SkeletonRows count={rows} />
      )}
    </section>
  );
}

function SkeletonFramedPanel({ kind }: { kind: 'chart' | 'table' }) {
  return (
    <section className={`loading-skeleton-panel loading-skeleton-framed-${kind}`}>
      <div className="loading-skeleton-panel-head">
        <i className="loading-skeleton-title-icon" />
        <span className="loading-skeleton-title-copy">
          <i className="loading-bar title" />
          <i className="loading-bar hint" />
        </span>
        <i className="loading-skeleton-head-stat" />
        <i className="loading-skeleton-head-action" />
      </div>
      {kind === 'chart' ? <SkeletonChart /> : <SkeletonTable />}
    </section>
  );
}

function SkeletonTable({ rows = 5 }: { rows?: number }) {
  return (
    <div className="loading-skeleton-table">
      <div className="loading-skeleton-table-head">
        {skeletonRange(5).map(column => (
          <i className="loading-bar" key={column} />
        ))}
      </div>
      {skeletonRange(rows).map(row => (
        <div className="loading-skeleton-table-row" key={row}>
          <i className="loading-skeleton-dot" />
          <i className="loading-bar" />
          <i className="loading-bar" />
          <i className="loading-bar" />
          <i className="loading-skeleton-chip" />
        </div>
      ))}
    </div>
  );
}

function SkeletonChart() {
  return (
    <div className="loading-skeleton-chart">
      <div className="loading-skeleton-chart-head">
        <i className="loading-bar" />
        <span />
        <i className="loading-skeleton-chip" />
        <i className="loading-skeleton-chip" />
      </div>
      <div className="loading-skeleton-plot">
        <i />
      </div>
      <div className="loading-skeleton-chart-legend">
        <i />
        <span className="loading-bar" />
        <i />
        <span className="loading-bar" />
      </div>
    </div>
  );
}

function SkeletonCode() {
  const widths = ['74%', '52%', '88%', '63%', '79%', '46%', '70%', '57%'];
  return (
    <div className="loading-skeleton-code">
      {widths.map((width, row) => (
        <div key={row}>
          <i className="loading-skeleton-code-number" />
          <i className="loading-bar" style={{ width }} />
        </div>
      ))}
    </div>
  );
}

function SkeletonEditor() {
  return (
    <section className="loading-skeleton-panel loading-skeleton-editor">
      <div className="loading-skeleton-editor-toolbar">
        <i className="loading-bar title" />
        <i className="loading-bar hint" />
        <span />
        <i className="loading-skeleton-head-action" />
      </div>
      <SkeletonTable rows={4} />
      <div className="loading-skeleton-editor-footer">
        <i className="loading-bar" />
        <span />
        <i className="loading-skeleton-chip" />
        <i className="loading-skeleton-head-action" />
      </div>
    </section>
  );
}

function SkeletonPageHead() {
  return (
    <div className="loading-skeleton-page-head">
      <i className="loading-skeleton-page-icon" />
      <span>
        <i className="loading-bar title" />
        <i className="loading-bar subtitle" />
      </span>
      <i className="loading-skeleton-page-status" />
    </div>
  );
}

function SkeletonNodeCards({ count = 11 }: { count?: number }) {
  return (
    <div className="loading-skeleton-node-grid">
      {skeletonRange(count).map(card => (
        <article className="loading-skeleton-node-card" key={card}>
          <div className="loading-skeleton-node-head">
            <i className="loading-skeleton-dot" />
            <i className="loading-bar" />
            <i className="loading-bar tiny" />
          </div>
          <div className="loading-skeleton-node-trace">
            <i />
          </div>
          <div className="loading-skeleton-node-foot">
            <i className="loading-bar value" />
            <i className="loading-bar tiny" />
            <i className="loading-skeleton-chip" />
          </div>
        </article>
      ))}
    </div>
  );
}

function SkeletonChainCards({ count }: { count: number }) {
  return (
    <div className="chain-card-grid">
      {skeletonRange(count).map(card => (
        <article className="chain-card loading-skeleton-chain-card" key={card}>
          <div className="chain-card-head">
            <span className="chain-status-slot">
              <i className="loading-skeleton-dot" />
            </span>
            <span className="chain-card-flag">
              <i className="loading-skeleton-title-icon" />
            </span>
            <span className="chain-card-title">
              <i className="loading-bar title" />
              <i className="loading-bar faint" />
            </span>
            <span className="chain-card-latency">
              <i className="loading-bar tiny" />
              <i className="loading-bar value" />
            </span>
          </div>
          <div className="chain-card-path">
            <i className="loading-skeleton-chip" />
            <i className="loading-bar path-link" />
            <i className="loading-skeleton-chip" />
          </div>
          <div className="history-plot chain-latency-plot loading-skeleton-chain-trace">
            <i />
          </div>
          <div className="chain-card-foot">
            <i className="loading-bar tiny" />
            <span className="stat access">
              <i className="loading-bar" />
            </span>
          </div>
        </article>
      ))}
    </div>
  );
}

function SkeletonListHead({ pageHeader = false }: { pageHeader?: boolean }) {
  const Head = pageHeader ? 'header' : 'div';
  return (
    <Head className="loading-skeleton-panel-head page-title">
      <i className="loading-skeleton-title-icon" />
      <span className="loading-skeleton-title-copy">
        <i className="loading-bar title" />
      </span>
      <i className="sp" />
      <i className="loading-skeleton-head-action" />
    </Head>
  );
}

function SkeletonTunnelGroups() {
  return (
    <div className="loading-skeleton-tunnel-groups">
      {skeletonRange(2).map(group => (
        <div className="loading-skeleton-tunnel-group" key={group}>
          <div className="loading-skeleton-group-head">
            <i className="loading-bar name" />
            <i className="loading-bar meta" />
          </div>
          <div className="loading-skeleton-tunnel-row">
            <i className="loading-skeleton-tunnel-icon" />
            <span>
              <i className="loading-bar" />
              <i className="loading-bar faint" />
            </span>
            <i className="loading-bar aside" />
            <i className="loading-skeleton-chip" />
          </div>
        </div>
      ))}
    </div>
  );
}

function SkeletonListPage({ kind, initial }: { kind: 'nodes' | 'chains' | 'tunnels'; initial?: ConsoleInitialData }) {
  if (kind === 'chains') {
    const groups: ConsoleInitialData['chain_group_count'] = initial?.chain_group_count ?? [
      ['skeleton-group-1', 4],
      ['skeleton-group-2', 4],
      ['skeleton-group-3', 3],
      ['skeleton-group-4', 2],
    ];
    return (
      <div className="cardpage loading-skeleton-chain-page">
        <section className="panel titled loading-skeleton-list-shell chains">
          <SkeletonListHead pageHeader />
          <div className="chain-sections loading-skeleton-groups">
            {groups.map(([groupId, count]) => (
              <section className="chain-section" key={groupId}>
                <header className="chain-section-head">
                  <i className="loading-bar index" />
                  <i className="loading-bar name" />
                  <i className="loading-bar meta" />
                  <span className="count">
                    <i className="loading-bar tiny" />
                  </span>
                  <i className="loading-skeleton-head-action" />
                </header>
                <SkeletonChainCards count={count} />
              </section>
            ))}
          </div>
        </section>
      </div>
    );
  }
  if (kind === 'tunnels') {
    return (
      <section className="loading-skeleton-panel loading-skeleton-list-shell tunnels">
        <SkeletonListHead />
        <SkeletonTunnelGroups />
      </section>
    );
  }

  return (
    <section className={`loading-skeleton-panel loading-skeleton-list-shell ${kind}`}>
      <SkeletonListHead />
      <SkeletonNodeCards count={initial?.node_count} />
    </section>
  );
}

function SkeletonUserCardHead() {
  return (
    <div className="loading-skeleton-user-card-head">
      <i className="loading-skeleton-title-icon" />
      <span className="loading-skeleton-title-copy">
        <i className="loading-bar title" />
        <i className="loading-bar hint" />
      </span>
      <i className="loading-skeleton-head-stat" />
    </div>
  );
}

function SkeletonUserQuotaCard({ index }: { index: number }) {
  return (
    <div className={`loading-skeleton-user-quota q${index + 1}`}>
      <i className="loading-bar title" />
      <i className="loading-bar hint" />
      <span className="loading-skeleton-user-quota-value">
        <i className="loading-bar value" />
        <i className="loading-bar cap" />
      </span>
      <span className="loading-skeleton-user-meter">
        <i />
      </span>
      <i className="loading-bar meta" />
    </div>
  );
}

function SkeletonUserDetail() {
  return (
    <section className="loading-skeleton-user-detail">
      <div className="loading-skeleton-user-head">
        <i className="loading-skeleton-avatar large" />
        <span className="loading-skeleton-user-identity">
          <span className="loading-skeleton-user-name">
            <i className="loading-bar title" />
            <i className="loading-bar stat" />
            <i className="loading-bar stat short" />
          </span>
          <span className="loading-skeleton-user-uuid">
            <i className="loading-bar label" />
            <i className="loading-bar value" />
            <i className="loading-skeleton-user-copy" />
          </span>
        </span>
        <div className="loading-skeleton-user-actions">
          <i className="loading-skeleton-chip account" />
          <i className="loading-skeleton-chip" />
          <i className="loading-skeleton-chip" />
          <i className="loading-skeleton-chip" />
          <i className="loading-skeleton-chip more" />
        </div>
      </div>
      <div className="loading-skeleton-user-body">
        <section className="loading-skeleton-user-card quota">
          <SkeletonUserCardHead />
          <div className="loading-skeleton-user-quota-grid">
            {skeletonRange(4).map(index => (
              <SkeletonUserQuotaCard index={index} key={index} />
            ))}
          </div>
          <div className="loading-skeleton-user-card-foot">
            <i className="loading-skeleton-chip" />
            <i className="loading-bar" />
          </div>
        </section>
        <section className="loading-skeleton-user-card grants">
          <SkeletonUserCardHead />
          <div className="loading-skeleton-user-grant-grid">
            {skeletonRange(13).map(index => (
              <div className="loading-skeleton-user-grant" key={index}>
                <span>
                  <i className="loading-bar title" />
                  <i className="loading-skeleton-user-check" />
                </span>
                <i className="loading-bar hint" />
                <i className="loading-bar meta" />
              </div>
            ))}
          </div>
        </section>
        <section className="loading-skeleton-user-card probe">
          <SkeletonUserCardHead />
          <div className="loading-skeleton-user-probe-head">
            {skeletonRange(7).map(column => (
              <i className="loading-bar" key={column} />
            ))}
          </div>
          {skeletonRange(3).map(row => (
            <div className="loading-skeleton-user-probe-row" key={row}>
              <i className="loading-bar route" />
              {skeletonRange(6).map(column => (
                <i className="loading-skeleton-user-probe-cell" key={column} />
              ))}
            </div>
          ))}
        </section>
      </div>
    </section>
  );
}

function SkeletonUsers({ detailRoute = false }: { detailRoute?: boolean }) {
  return (
    <div className={`loading-skeleton-user-split${detailRoute ? ' detail-route' : ' list-route'}`}>
      <section className="loading-skeleton-panel loading-skeleton-user-roster">
        <div className="loading-skeleton-panel-head page-title">
          <i className="loading-skeleton-title-icon" />
          <span className="loading-skeleton-title-copy">
            <i className="loading-bar title" />
          </span>
          <i className="sp" />
          <i className="loading-skeleton-head-action" />
        </div>
        <div className="loading-skeleton-search">
          <i />
          <span className="loading-bar" />
        </div>
        {skeletonRange(6).map(row => (
          <div className="loading-skeleton-user-row" key={row}>
            <i className="loading-skeleton-avatar" />
            <span>
              <i className="loading-bar" />
              <i className="loading-bar faint" />
            </span>
            <i className="loading-bar amount" />
          </div>
        ))}
      </section>
      <SkeletonUserDetail />
    </div>
  );
}

function SkeletonSettings() {
  return (
    <div className="loading-skeleton-dual">
      <div>
        {[3, 4, 5].map((rows, index) => (
          <SkeletonPanel rows={rows} fields key={index} />
        ))}
      </div>
      <div>
        {[4, 5, 3].map((rows, index) => (
          <SkeletonPanel rows={rows} fields key={index} />
        ))}
      </div>
    </div>
  );
}

function SkeletonUsage() {
  return (
    <div className="loading-skeleton-usage">
      <section className="loading-skeleton-panel loading-skeleton-usage-summary">
        <div className="loading-skeleton-panel-head page-title">
          <i className="loading-skeleton-title-icon" />
          <span className="loading-skeleton-title-copy">
            <i className="loading-bar title" />
            <i className="loading-bar hint" />
          </span>
          <i className="loading-skeleton-usage-tabs" />
        </div>
        <SkeletonUsageMetrics />
      </section>
      <SkeletonFramedPanel kind="chart" />
    </div>
  );
}

function SkeletonUsageMetrics() {
  return (
    <div className="loading-skeleton-usage-metrics">
      <div className="total">
        <i className="loading-bar label" />
        <i className="loading-bar value" />
        <i className="loading-bar meta" />
      </div>
      {skeletonRange(4).map(metric => (
        <div key={metric}>
          <i className="loading-bar label" />
          <i className="loading-bar value" />
        </div>
      ))}
    </div>
  );
}

function SkeletonLinks() {
  return (
    <div className="loading-skeleton-links">
      <section className="loading-skeleton-panel loading-skeleton-link-summary">
        <div className="loading-skeleton-panel-head page-title">
          <i className="loading-skeleton-title-icon" />
          <span className="loading-skeleton-title-copy">
            <i className="loading-bar title" />
            <i className="loading-bar hint" />
          </span>
          <i className="loading-skeleton-head-stat" />
        </div>
        <div className="loading-skeleton-overview">
          {skeletonRange(4).map(metric => (
            <div key={metric}>
              <i className="loading-skeleton-title-icon" />
              <span>
                <i className="loading-bar" />
                <i className="loading-bar value" />
                <i className="loading-bar faint" />
              </span>
            </div>
          ))}
        </div>
      </section>
      <SkeletonFramedPanel kind="chart" />
      <SkeletonFramedPanel kind="table" />
    </div>
  );
}

function SkeletonDeployOverview() {
  return (
    <div className="loading-skeleton-dual loading-skeleton-deploy-overview">
      <div>
        <SkeletonPanel rows={7} />
      </div>
      <div>
        <SkeletonPanel rows={4} />
        <SkeletonPanel rows={4} />
      </div>
    </div>
  );
}

function SkeletonObservationPanel() {
  return (
    <section className="loading-skeleton-observation-panel">
      <SkeletonChart />
      <SkeletonChart />
    </section>
  );
}

function SkeletonRuntimePanel() {
  return (
    <section className="loading-skeleton-panel loading-skeleton-runtime">
      <div className="loading-skeleton-runtime-head">
        <i className="loading-skeleton-title-icon" />
        <i className="loading-bar" />
      </div>
      {[7, 9, 3].map((count, band) => (
        <div className="loading-skeleton-runtime-band" key={band}>
          <i className="loading-bar loading-skeleton-runtime-label" />
          <div className="loading-skeleton-runtime-cells">
            {skeletonRange(count).map(cell => (
              <span key={cell}>
                <i className="loading-bar label" />
                <i className="loading-bar value" />
              </span>
            ))}
          </div>
        </div>
      ))}
    </section>
  );
}

function SkeletonChainPanel({
  className = '',
  children,
  folded = false,
}: {
  className?: string;
  children?: ReactNode;
  folded?: boolean;
}) {
  return (
    <section className={`panel config-panel ${className}${folded ? ' loading-skeleton-chain-fold' : ''}`}>
      <header>
        <i className="loading-skeleton-title-icon" />
        <i className="loading-bar title" />
        <i className="loading-bar faint" />
      </header>
      {children}
    </section>
  );
}

function SkeletonChainFields({ rows }: { rows: number }) {
  return (
    <div className="loading-skeleton-chain-fields">
      {skeletonRange(rows).map(row => (
        <div className="loading-skeleton-field" key={row}>
          <i className="loading-bar label" />
          <i className="loading-skeleton-input" />
          <i className="loading-bar help" />
        </div>
      ))}
    </div>
  );
}

function SkeletonChainDetail() {
  return (
    <div className="loading-skeleton-chain-detail">
      <div className="chain-hd">
        <i className="loading-skeleton-title-icon" />
        <i className="loading-bar title" />
        <i className="loading-bar faint" />
      </div>
      <div className="loading-skeleton-chain-body">
        <div className="duo chain-config-grid">
          <div className="col">
            <div className="blk loading-skeleton-chain-probe">
              <div className="blk-hd">
                <i className="loading-bar title" />
              </div>
              <div className="blk-bd">
                <i className="loading-bar value" />
                <i className="loading-bar" />
                <i className="loading-bar faint" />
              </div>
            </div>
            <SkeletonChainPanel className="loading-skeleton-chain-ingress">
              <SkeletonChainFields rows={2} />
            </SkeletonChainPanel>
            <SkeletonChainPanel className="loading-skeleton-chain-client">
              <SkeletonChainFields rows={3} />
              <i className="loading-bar faint" />
            </SkeletonChainPanel>
            <SkeletonChainPanel className="loading-skeleton-chain-guard" folded />
          </div>
          <div className="col">
            <SkeletonChainPanel className="loading-skeleton-chain-protocols">
              <div className="protocol-picker">
                {skeletonRange(4).map(protocol => (
                  <div className="protocol-choice" key={protocol}>
                    <i className="loading-skeleton-title-icon" />
                    <span className="protocol-choice-copy">
                      <i className="loading-bar title" />
                      <i className="loading-bar faint" />
                    </span>
                  </div>
                ))}
              </div>
            </SkeletonChainPanel>
            <SkeletonChainPanel className="loading-skeleton-chain-protocol-config" folded />
          </div>
        </div>
        <SkeletonChainPanel className="rule-sheet-card loading-skeleton-chain-rules">
          <SkeletonRows count={2} />
          <div className="toolbar">
            <i className="loading-bar faint" />
            <span className="sp" />
            <i className="loading-skeleton-head-action" />
          </div>
        </SkeletonChainPanel>
      </div>
    </div>
  );
}

function SkeletonDetail() {
  return (
    <section className="loading-skeleton-detail-paper">
      <div className="loading-skeleton-detail-head">
        <i className="loading-skeleton-page-icon" />
        <span>
          <i className="loading-bar title" />
        </span>
        <div className="loading-skeleton-tabs">
          <i className="active" />
        </div>
        <i className="loading-skeleton-head-action" />
      </div>
      <div className="loading-skeleton-detail-body">
        <div className="loading-skeleton-metrics">
          {skeletonRange(6).map(metric => (
            <div className="loading-skeleton-metric" key={metric}>
              <i className="loading-bar label" />
              <i className="loading-bar value" />
              <i className="loading-skeleton-trace" />
            </div>
          ))}
        </div>
        <div className="loading-skeleton-observation-panels">
          <SkeletonObservationPanel />
          <SkeletonObservationPanel />
        </div>
        <SkeletonRuntimePanel />
      </div>
    </section>
  );
}

function SkeletonConfigDetail() {
  return (
    <section className="loading-skeleton-detail-paper loading-skeleton-config-detail">
      <div className="loading-skeleton-detail-head">
        <i className="loading-skeleton-page-icon" />
        <span>
          <i className="loading-bar title" />
          <i className="loading-bar subtitle" />
        </span>
        <i className="loading-skeleton-head-stat" />
        <div className="loading-skeleton-config-actions">
          <i className="loading-skeleton-head-action" />
          <i className="loading-skeleton-head-action" />
        </div>
      </div>
      <div className="loading-skeleton-detail-body">
        <div className="loading-skeleton-facts">
          {skeletonRange(4).map(fact => (
            <div key={fact}>
              <i className="loading-bar label" />
              <i className="loading-bar value" />
            </div>
          ))}
        </div>
        <div className="loading-skeleton-detail-columns">
          <SkeletonPanel rows={5} fields />
          <div className="loading-skeleton-config-stack">
            <SkeletonPanel rows={4} />
            <SkeletonPanel rows={3} />
          </div>
        </div>
      </div>
    </section>
  );
}

function SkeletonVpngate() {
  return (
    <section className="loading-skeleton-detail-paper loading-skeleton-vpngate">
      <div className="loading-skeleton-detail-head">
        <i className="loading-skeleton-page-icon" />
        <span>
          <i className="loading-bar title" />
          <i className="loading-bar subtitle" />
        </span>
        <div className="loading-skeleton-tabs">
          <i className="active" />
          <i />
        </div>
        <i className="loading-skeleton-head-action" />
      </div>
      <div className="loading-skeleton-vpngate-body">
        <aside className="loading-skeleton-vpngate-rail">
          <div className="loading-skeleton-vpngate-rail-head">
            <i className="loading-bar label" />
            <i className="loading-bar faint" />
          </div>
          <i className="loading-skeleton-input" />
          {skeletonRange(7).map(row => (
            <span key={row}>
              <i className="loading-skeleton-dot" />
              <i className="loading-bar" />
              <i className="loading-bar faint" />
            </span>
          ))}
        </aside>
        <div className="loading-skeleton-vpngate-work">
          <SkeletonTable rows={5} />
          <SkeletonPanel rows={3} />
          <SkeletonPanel rows={1} />
        </div>
      </div>
    </section>
  );
}

function SkeletonForm() {
  return (
    <div className="loading-skeleton-form-page">
      <SkeletonPageHead />
      <div className="loading-skeleton-stepper">
        {skeletonRange(4).map(step => (
          <span key={step}>
            <i className="loading-skeleton-step" />
            <i className="loading-bar" />
          </span>
        ))}
      </div>
      <div className="loading-skeleton-detail-columns">
        <SkeletonPanel rows={5} fields />
        <SkeletonPanel rows={4} />
      </div>
    </div>
  );
}

function SkeletonPlanSection({ rows }: { rows: number }) {
  return (
    <section className="loading-skeleton-panel loading-skeleton-plan-section">
      <div className="loading-skeleton-panel-head">
        <i className="loading-skeleton-plan-index" />
        <span className="loading-skeleton-title-copy">
          <i className="loading-bar title" />
          <i className="loading-bar hint" />
        </span>
        <i className="loading-skeleton-head-stat" />
      </div>
      <SkeletonTable rows={rows} />
    </section>
  );
}

function SkeletonPlan() {
  return (
    <div className="loading-skeleton-plan">
      <section className="loading-skeleton-plan-hero">
        <div className="loading-skeleton-plan-hero-head">
          <div className="loading-skeleton-plan-identity">
            <i className="loading-skeleton-page-icon" />
            <span>
              <i className="loading-bar eyebrow" />
              <i className="loading-bar title" />
              <i className="loading-bar subtitle" />
            </span>
          </div>
          <div className="loading-skeleton-plan-track">
            <span>
              <i className="loading-bar faint" />
              <i className="loading-bar" />
            </span>
            <i className="loading-skeleton-plan-bridge" />
            <span>
              <i className="loading-bar faint" />
              <i className="loading-bar" />
            </span>
          </div>
        </div>
        <div className="loading-skeleton-plan-metrics">
          {skeletonRange(4).map(metric => (
            <div key={metric}>
              <i className="loading-bar label" />
              <i className="loading-bar value" />
              <i className="loading-bar faint" />
            </div>
          ))}
        </div>
        <div className="loading-skeleton-plan-verdict">
          <i className="loading-skeleton-dot" />
          <span>
            <i className="loading-bar" />
            <i className="loading-bar faint" />
          </span>
        </div>
      </section>
      <SkeletonPlanSection rows={4} />
      <SkeletonPlanSection rows={5} />
    </div>
  );
}

function SkeletonDeployment() {
  return (
    <div className="loading-skeleton-deployment">
      <div className="loading-skeleton-deployment-status">
        <i className="loading-skeleton-chip" />
        <i className="loading-skeleton-chip" />
        <i className="loading-bar" />
        <span />
        <i className="loading-bar faint" />
      </div>
      <SkeletonPlanSection rows={5} />
      <SkeletonPlanSection rows={3} />
    </div>
  );
}

function SkeletonCanvas() {
  return (
    <div className="loading-skeleton-canvas">
      <div className="loading-skeleton-canvas-toolbar">
        <i className="loading-bar" />
        <i className="loading-bar" />
        <i className="loading-bar" />
        <span />
        <i className="loading-skeleton-chip" />
      </div>
      <div className="loading-skeleton-canvas-stage">
        {skeletonRange(4).map(node => (
          <div className={`loading-skeleton-canvas-node n${node + 1}`} key={node}>
            <i />
            <span>
              <i className="loading-bar" />
              <i className="loading-bar faint" />
            </span>
          </div>
        ))}
      </div>
      <div className="loading-skeleton-canvas-strip">
        {skeletonRange(6).map(item => (
          <i className="loading-bar" key={item} />
        ))}
      </div>
    </div>
  );
}

// 读取态按真实承载面建模，而不是把同一块占位内容铺到每个背景上。页面变体保留页头、
// 分栏和面板色带；rows/table/chart/code 只替换已有面板内部的内容，避免重复套卡。
// 可访问文案由 status 暴露，但不参与视觉布局。
export function Loading({
  variant = 'panel',
  sheeted = false,
  rows,
  userDetail = false,
  initial,
  showSkeleton = PREVIEW_LOADING_SKELETON,
}: {
  variant?: LoadingVariant;
  sheeted?: boolean;
  rows?: number;
  /** First-frame inventory; only list-page skeletons consume it. */
  initial?: ConsoleInitialData;
  /** User pages keep both columns on desktop, but only the routed column on narrow screens. */
  userDetail?: boolean;
  /** Test override for verifying the retained skeleton implementation. */
  showSkeleton?: boolean;
}) {
  const registry = useContext(LoadingRegistryContext);
  // A route boundary owns every loading surface below it, including smaller chart/table variants.
  // Before the first reveal they extend the blank page's lifetime; afterwards the boundary is
  // latched and suppresses them so a refetch cannot start a second loading cycle.
  const delegated = registry !== null;
  useLayoutEffect(() => (delegated ? registry.register() : undefined), [delegated, registry]);

  const pageVariant = [
    'nodes',
    'chains',
    'tunnels',
    'users',
    'usage',
    'links',
    'settings',
    'deploy',
    'detail',
    'chain-detail',
    'config-detail',
    'vpngate',
    'form',
    'plan',
    'deployment',
    'canvas',
  ].includes(variant);

  // Production does not render a loading prompt. The boundary keeps partial content out of paint,
  // and the real surfaces provide their own one-shot mount motion when the route becomes ready.
  if (delegated || !showSkeleton) return null;

  let skeleton: ReactNode;
  switch (variant) {
    case 'chart-panel':
      skeleton = <SkeletonFramedPanel kind="chart" />;
      break;
    case 'table-panel':
      skeleton = <SkeletonFramedPanel kind="table" />;
      break;
    case 'control':
      skeleton = <SkeletonControl />;
      break;
    case 'rows':
      skeleton = <SkeletonRows />;
      break;
    case 'metrics':
      skeleton = (
        <div className="loading-skeleton-overview">
          {skeletonRange(4).map(metric => (
            <div key={metric}>
              <i className="loading-skeleton-title-icon" />
              <span>
                <i className="loading-bar" />
                <i className="loading-bar value" />
                <i className="loading-bar faint" />
              </span>
            </div>
          ))}
        </div>
      );
      break;
    case 'table':
      skeleton = <SkeletonTable />;
      break;
    case 'chart':
      skeleton = <SkeletonChart />;
      break;
    case 'code':
      skeleton = <SkeletonCode />;
      break;
    case 'tree':
      skeleton = (
        <div className="loading-skeleton-tree">
          {skeletonRange(8).map(row => (
            <div style={{ paddingLeft: `${(row % 3) * 12}px` }} key={row}>
              <i className="loading-skeleton-dot" />
              <i className="loading-bar" />
            </div>
          ))}
        </div>
      );
      break;
    case 'editor':
      skeleton = <SkeletonEditor />;
      break;
    case 'nodes':
    case 'chains':
    case 'tunnels':
      skeleton = <SkeletonListPage kind={variant} initial={initial} />;
      break;
    case 'users':
      skeleton = <SkeletonUsers detailRoute={userDetail} />;
      break;
    case 'usage':
      skeleton = <SkeletonUsage />;
      break;
    case 'usage-metrics':
      skeleton = <SkeletonUsageMetrics />;
      break;
    case 'links':
      skeleton = <SkeletonLinks />;
      break;
    case 'settings':
      skeleton = <SkeletonSettings />;
      break;
    case 'deploy':
      skeleton = <SkeletonDeployOverview />;
      break;
    case 'detail':
      skeleton = <SkeletonDetail />;
      break;
    case 'chain-detail':
      skeleton = <SkeletonChainDetail />;
      break;
    case 'config-detail':
      skeleton = <SkeletonConfigDetail />;
      break;
    case 'vpngate':
      skeleton = <SkeletonVpngate />;
      break;
    case 'form':
      skeleton = <SkeletonForm />;
      break;
    case 'plan':
      skeleton = <SkeletonPlan />;
      break;
    case 'deployment':
      skeleton = <SkeletonDeployment />;
      break;
    case 'canvas':
      skeleton = <SkeletonCanvas />;
      break;
    default:
      skeleton = <SkeletonPanel rows={rows} />;
  }

  return (
    <div
      className={`loading-state loading-${variant}${pageVariant ? ' loading-page' : ''}${sheeted ? ' sheeted' : ''}`}
      role="status"
      aria-live="polite"
    >
      <span className="loading-state-label">正在读取…</span>
      <div className="loading-skeleton" aria-hidden="true">
        {skeleton}
      </div>
    </div>
  );
}

export function ErrorBox({ error }: { error: unknown }) {
  const message = error instanceof Error ? error.message : String(error);
  return <div className="callout err">{message}</div>;
}

// 危险操作的确认框。requireWord 非空时必须先输入该词才能确认——
// 回滚这类误操作后难以恢复的操作，仅二次确认不足，需要确认已阅读影响说明后才允许执行。
export function Confirm({
  title,
  body,
  confirmLabel,
  danger = true,
  requireWord,
  confirmDisabled = false,
  onConfirm,
  onCancel,
}: {
  title: string;
  body: ReactNode;
  confirmLabel: string;
  danger?: boolean;
  requireWord?: string;
  confirmDisabled?: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  const [word, setWord] = useState('');
  const ok = !requireWord || word === requireWord;
  return (
    <DialogLayer label={title} onClose={onCancel}>
      <div className="dialog-surface confirm-card">
        <div className="confirm-title">{title}</div>
        <div className="confirm-body">{body}</div>
        {requireWord && (
          <input
            className="f confirm-word"
            autoFocus
            placeholder={`输入「${requireWord}」确认`}
            aria-label={`输入「${requireWord}」确认`}
            value={word}
            onChange={e => setWord(e.target.value)}
          />
        )}
        <div className="confirm-actions">
          <DialogClose className="btn">取消</DialogClose>
          <button
            className={`btn ${danger ? 'danger' : 'primary'}`}
            type="button"
            disabled={!ok || confirmDisabled}
            onClick={onConfirm}
          >
            {confirmLabel}
          </button>
        </div>
      </div>
    </DialogLayer>
  );
}

/* 时间戳统一显示为相对时间——在拉取模型下，机器距上次拉取的时长是主要信息。绝对时刻写入 title 备查。 */
export function Ago({ at, withPastSuffix = true }: { at: string | null; withPastSuffix?: boolean }) {
  // useNow 需要在提前 return 之前调用（hook 不能有条件地跳过）。该值因此每秒自动更新，
  // 不需要依赖其他原因触发重渲染——计时器即用于该场景（ui/clock.ts）。
  const now = useNow();
  if (!at) return <span className="dim">—</span>;
  const t = Date.parse(at.endsWith('Z') || at.includes('+') ? at : `${at}Z`);
  if (Number.isNaN(t)) return <span className="ago dim">{at}</span>;
  const s = Math.max(0, Math.round((now - t) / 1000));
  // 数字与中文单位之间保留窄不换行空格，既留出轻微间隔，也不会在此处断行。
  const unitGap = '\u202f';
  const age =
    s < 60
      ? `${s}${unitGap}秒`
      : s < 3600
        ? `${Math.floor(s / 60)}${unitGap}分钟`
        : s < 86400
          ? `${Math.floor(s / 3600)}${unitGap}小时`
          : `${Math.floor(s / 86400)}${unitGap}天`;
  const text = withPastSuffix ? `${age}前` : age;
  return (
    <span className="ago" title={at}>
      {text}
    </span>
  );
}

/* 二选一开关：两格，选中的一格反白。直角外框，与按钮同高同边框，两个状态都有文字标签。

   定义在 bits 中而非某个面板内：机器详情（NAT、是否加入 overlay、出网）和链详情（投影）
   都在使用。此前它是 panes/nodes.tsx 中的私有组件，第二处使用时只能复制一份——
   复制出的实现最终会在某次修改中与原实现产生差异。 */
export function SegmentedControl<T extends string | number | boolean>({
  value,
  options,
  disabled,
  onChange,
  ariaLabel,
  className,
}: {
  value: T;
  options: readonly { value: T; label: ReactNode; disabled?: boolean }[];
  disabled?: boolean;
  onChange: (value: T) => void;
  ariaLabel?: string;
  className?: string;
}) {
  return (
    <span className={`segsw${className ? ` ${className}` : ''}`} role="group" aria-label={ariaLabel}>
      {options.map(option => (
        <button
          key={String(option.value)}
          type="button"
          aria-pressed={value === option.value}
          disabled={disabled || option.disabled}
          onClick={() => value !== option.value && onChange(option.value)}
        >
          {option.label}
        </button>
      ))}
    </span>
  );
}

export function SegSwitch({
  checked,
  disabled,
  onChange,
  off,
  on,
  ariaLabel,
}: {
  checked: boolean;
  disabled?: boolean;
  onChange: (checked: boolean) => void;
  /* 两格各自的文案。off 位于左侧（false），on 位于右侧（true） */
  off: string;
  on: string;
  ariaLabel?: string;
}) {
  return (
    <SegmentedControl
      value={checked}
      options={[
        { value: false, label: off },
        { value: true, label: on },
      ]}
      disabled={disabled}
      onChange={onChange}
      ariaLabel={ariaLabel}
    />
  );
}
