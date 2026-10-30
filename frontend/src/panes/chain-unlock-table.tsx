import { useEffect } from 'react';
import { useInfiniteQuery, useQueryClient } from '@tanstack/react-query';
import { fetchChainUnlockProbes, type SnapshotApp, type SnapshotChain } from '../api';
import { ErrorBox } from '../ui/bits';
import { useNow } from '../ui/clock';
import { RegionFlag } from '../ui/region-flag';
import { ServiceMark } from './node-unlock';
import { CATEGORIES, CATEGORY, DISPLAY_SERVICE_NAMES, SERVICES } from './unlock-report';
import { Glyph } from './unlock-status';
import { summarizeChainUnlock } from './chain-unlock-summary';
import './chain-unlock-table.css';

const categories = CATEGORIES.map(category => ({
  ...category,
  services: SERVICES.filter(service => CATEGORY[service] === category.key),
}));
const services = categories.flatMap(category => category.services);
const shortNames = { youtube_premium: 'Premium', prime_video: 'Prime' };
export interface ChainUnlockGroup {
  app: SnapshotApp;
  chains: { chain: SnapshotChain; disabled: boolean; tone: string; title: string }[];
}

function Ago({ at }: { at: number }) {
  const seconds = Math.max(0, Math.floor(useNow() / 1000 - at));
  const label =
    seconds < 60
      ? '刚刚'
      : seconds < 3600
        ? `${Math.floor(seconds / 60)} 分钟前`
        : seconds < 86400
          ? `${Math.floor(seconds / 3600)} 小时前`
          : `${Math.floor(seconds / 86400)} 天前`;
  return (
    <time
      dateTime={new Date(at * 1000).toISOString()}
      title={new Date(at * 1000).toLocaleString('zh-CN', { hour12: false })}
    >
      {label}
    </time>
  );
}

export function ChainUnlockTable({
  groups,
  onOpen,
}: {
  groups: ChainUnlockGroup[];
  onOpen: (app: string, chain: string) => void;
}) {
  const client = useQueryClient();
  const query = useInfiniteQuery({
    queryKey: ['chain-unlock-probes'],
    initialPageParam: undefined as string | undefined,
    queryFn: ({ pageParam, signal }) => fetchChainUnlockProbes(pageParam, signal),
    getNextPageParam: page => page.next_cursor ?? undefined,
    refetchInterval: current =>
      current.state.data?.pages.some(page =>
        page.chains.some(item =>
          Object.values(item.probes).some(probe => probe?.state === 'queued' || probe?.state === 'running'),
        ),
      )
        ? 2_000
        : 30_000,
  });
  const { hasNextPage, isFetching, isError, fetchNextPage } = query;
  const pageCount = query.data?.pages.length ?? 0;
  useEffect(() => {
    if (hasNextPage && !isFetching && !isError) void fetchNextPage();
  }, [hasNextPage, isFetching, isError, fetchNextPage, pageCount]);
  const items = new Map(
    query.data?.pages.flatMap(page => page.chains.map(item => [`${item.app_id}/${item.chain_id}`, item] as const)),
  );
  return (
    <>
      {query.error && (
        <div className="chain-unlock-error">
          <ErrorBox error={query.error} />
          <button
            className="btn"
            onClick={() => void (query.isFetchNextPageError ? query.fetchNextPage() : query.refetch())}
          >
            重试
          </button>
        </div>
      )}
      <div
        className="chain-unlock-scroll"
        role="region"
        aria-label="线路解锁对照表，可横向滚动"
        tabIndex={0}
        aria-busy={query.isPending || query.isFetchingNextPage}
      >
        <table className="chain-unlock-table" aria-label="线路服务解锁">
          <thead>
            <tr className="cut-cats">
              <th className="cut-corner" />
              {categories.map(category => (
                <th key={category.key} colSpan={category.services.length} scope="colgroup">
                  {category.name}
                </th>
              ))}
              <th className="cut-when" />
            </tr>
            <tr className="cut-heads">
              <th className="cut-corner" scope="col">
                链
              </th>
              {services.map(service => (
                <th
                  key={service}
                  scope="col"
                  aria-label={DISPLAY_SERVICE_NAMES[service]}
                  title={DISPLAY_SERVICE_NAMES[service]}
                  data-first={categories.some(category => category.services[0] === service) || undefined}
                >
                  <ServiceMark service={service} />
                  <span>
                    {service in shortNames
                      ? shortNames[service as keyof typeof shortNames]
                      : DISPLAY_SERVICE_NAMES[service]}
                  </span>
                </th>
              ))}
              <th className="cut-when" scope="col">
                检测
              </th>
            </tr>
          </thead>
          {groups.map((group, index) => (
            <tbody key={group.app.id}>
              <tr className="cut-group">
                <th colSpan={services.length + 2} scope="rowgroup">
                  <span className="cut-group-in">
                    <span className="no">{String(index + 1).padStart(2, '0')}</span>
                    <b>{group.app.label || group.app.id}</b>
                    <span className="count">{group.chains.length} 条链</span>
                  </span>
                </th>
              </tr>
              {group.chains.length === 0 && (
                <tr>
                  <td colSpan={services.length + 2} className="cut-blank">
                    还没有链
                  </td>
                </tr>
              )}
              {group.chains.map(row => {
                const { chain } = row;
                const item = items.get(`${group.app.id}/${chain.id}`);
                const summary = item ? summarizeChainUnlock(item) : null;
                const testable = group.app.ingresses.some(ingress => ingress.chain === chain.id);
                const state =
                  !item && query.isError
                    ? '读取失败'
                    : !item && query.isSuccess && !query.hasNextPage
                      ? '无报告'
                      : summary?.state;
                const blank = !testable
                  ? '缺接入面'
                  : !item
                    ? query.isError
                      ? '读取失败'
                      : query.isSuccess && !query.hasNextPage
                        ? '尚未发布或尚未检测'
                        : '读取中…'
                    : !summary?.hasResults
                      ? (state ?? '未取得结果')
                      : null;
                const open = () => {
                  if (item) {
                    client.setQueryData(['chain-unlock-capabilities', item.app_id, item.chain_id], item.capabilities);
                    for (const selection of ['v4', 'v6', 'auto'] as const) {
                      client.setQueryData(
                        ['chain-unlock-probe', item.app_id, item.chain_id, selection],
                        item.probes[selection],
                      );
                    }
                  }
                  onOpen(group.app.id, chain.id);
                };
                const detail = summary?.detail;
                return (
                  <tr key={chain.id} className="cut-row" data-off={row.disabled || undefined} onClick={open}>
                    <th scope="row" className="cut-chain">
                      <button
                        type="button"
                        className="cut-chain-in"
                        data-route-focus={`chain:${group.app.id}/${chain.id}`}
                        title={row.title}
                        onClick={event => {
                          event.stopPropagation();
                          open();
                        }}
                      >
                        <i className="chain-card-status" data-tone={row.tone} aria-label={row.title} />
                        <RegionFlag code={chain.subscription_country} />
                        <span className="cut-name">{chain.name || chain.id}</span>
                      </button>
                    </th>
                    {blank !== null ? (
                      <td className="cut-blank" colSpan={services.length}>
                        {blank}
                      </td>
                    ) : (
                      services.map(service => {
                        const cell = summary!.cells.find(cell => cell.service === service)!;
                        const title = cell.title;
                        return (
                          <td
                            key={service}
                            aria-label={title}
                            title={title}
                            data-service={service}
                            data-tone={cell.verdict.tone}
                            data-dim={cell.dim || (query.isError && summary?.tone !== 'obsolete') || undefined}
                            data-first={categories.some(category => category.services[0] === service) || undefined}
                          >
                            <span className="cut-cell">
                              <Glyph tone={cell.verdict.tone} />
                              <span className="cut-regions">
                                <span>
                                  {cell.regionFamily === 'v6' && !cell.onlyFamily && 'v6 '}
                                  {cell.region ?? '—'}
                                </span>
                                {cell.ipv6Region && <small className="cut-v6">v6 {cell.ipv6Region}</small>}
                              </span>
                              {cell.change && <i className="cut-dot" aria-label={cell.change} />}
                            </span>
                          </td>
                        );
                      })
                    )}
                    <td className="cut-when" data-tone={summary?.tone} title={detail} aria-label={detail || undefined}>
                      {summary?.automatic && <small className="cut-auto">不分族</small>}
                      {query.isError && summary?.hasResults
                        ? '刷新失败'
                        : !testable
                          ? '—'
                          : (state ?? (summary?.at != null ? <Ago at={summary.at} /> : '—'))}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          ))}
        </table>
      </div>
      <p className="chain-unlock-legend">
        <Glyph tone="ok" />
        可用 <Glyph tone="part" />
        一族受限或异常 <Glyph tone="no" />
        不可用 <Glyph tone="unknown" />
        未取得结果 <i className="cut-dot" />
        较上次变化
        <span>
          地区以 IPv4 为主；不同时另列 v6 地区。IPv6 无解析地址或无可用出站时采用 IPv4 结果。Reddit 显示 CDN 节点代码。
        </span>
      </p>
    </>
  );
}
