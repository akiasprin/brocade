import { useEffect, useMemo, useState } from 'react';
import { useQuery } from '@tanstack/react-query';
import {
  fetchVpngateOverview,
  fetchVpngateCountryServers,
  VPNGATE_MAX_CANDIDATES,
  type VpngateCountrySummary,
} from '../api';
import { ErrorBox } from '../ui/bits';
import {
  isSelectableVpngateRegion,
  vpngateRegionName,
  vpngateServerIds,
  type VpngateOutbound,
} from '../vpngate-selection';

/** Lives inside the rule target portal. Browsing and checkbox edits never write to the model. */
export function VpngateRuleMenu({
  selected,
  onBack,
  onSelect,
}: {
  selected: VpngateOutbound | null;
  onBack: () => void;
  onSelect: (country: VpngateCountrySummary, serverIds: string[]) => void;
}) {
  const [country, setCountry] = useState<VpngateCountrySummary | null>(null);
  const [query, setQuery] = useState('');
  const [serverSearch, setServerSearch] = useState('');
  const [ids, setIds] = useState<string[]>([]);
  useEffect(() => {
    const timer = window.setTimeout(() => setServerSearch(country ? query.trim() : ''), 250);
    return () => window.clearTimeout(timer);
  }, [country, query]);
  const serverRequest = useMemo(
    () => ({
      page: 1,
      page_size: 100,
      search: serverSearch,
      filter: 'all' as const,
      sort: 'candidate' as const,
    }),
    [serverSearch],
  );
  const overview = useQuery({ queryKey: ['vpngate'], queryFn: fetchVpngateOverview, retry: false });
  const servers = useQuery({
    queryKey: ['vpngate', 'country', country?.country_code ?? '', serverRequest],
    queryFn: () => fetchVpngateCountryServers(country!.country_code, serverRequest),
    enabled: country !== null,
    retry: false,
    placeholderData: previous => previous,
  });
  const matches = (...parts: string[]) => parts.join(' ').toLowerCase().includes(query.trim().toLowerCase());
  const countries = (overview.data?.countries ?? []).filter(
    item =>
      isSelectableVpngateRegion(item.country_code) &&
      matches(item.country_code, item.country_name, vpngateRegionName(item.country_code)),
  );
  const visibleServers = (servers.data?.items ?? []).filter(server => matches(server.id, server.hostname, server.ip));
  // Preserve unavailable selected IDs instead of silently turning a manual pool into an automatic pool.
  const missing = servers.isSuccess ? ids.filter(id => !servers.data.items.some(server => server.id === id)) : [];
  return (
    <>
      <div className="external-target-menu-nav">
        <button
          type="button"
          aria-label={country ? '返回 VPN Gate 地区列表' : '返回目标列表'}
          onClick={() => {
            if (country) {
              setCountry(null);
              setQuery('');
            } else onBack();
          }}
        >
          ←
        </button>
        <span>
          <b>VPN Gate{country ? ` · ${vpngateRegionName(country.country_code)}` : ''}</b>
          <small>
            {country ? '手动节点池 · 仅在勾选节点间切换，最多 16 个' : '点击地区使用自动池；展开节点列表可手动组池'}
          </small>
        </span>
      </div>
      <input
        key={country?.country_code ?? 'countries'}
        autoFocus
        className="f external-target-search"
        aria-label={country ? '搜索 VPN Gate 节点' : '搜索 VPN Gate 地区'}
        placeholder={country ? '搜索节点名称、IP' : '搜索地区'}
        value={query}
        onChange={event => setQuery(event.target.value)}
      />
      {!country ? (
        <>
          {overview.isPending && (
            <span role="status" className="external-target-empty">
              正在读取地区列表…
            </span>
          )}
          {overview.error && (
            <>
              <ErrorBox error={overview.error} />
              <button type="button" onClick={() => void overview.refetch()}>
                重试地区列表
              </button>
            </>
          )}
          {countries.map(item => (
            <span className="external-target-option" key={item.country_code}>
              <button
                type="button"
                className={`external-target-option-select${selected?.protocol.v.country_code === item.country_code && !vpngateServerIds(selected).length ? ' on' : ''}`}
                aria-label={`${vpngateRegionName(item.country_code)} 自动节点池`}
                onClick={() => onSelect(item, [])}
              >
                <span className="external-target-kind external">{item.country_code}</span>
                <span className="external-target-copy">
                  <b>{vpngateRegionName(item.country_code)}</b>
                  <small>
                    {item.candidate_servers} 候选 · {item.measured_successful} 可用
                  </small>
                </span>
              </button>
              <button
                type="button"
                className="external-target-manage"
                aria-label={`展开 ${vpngateRegionName(item.country_code)} 节点列表`}
                onClick={() => {
                  setCountry(item);
                  setQuery('');
                  setIds(selected?.protocol.v.country_code === item.country_code ? vpngateServerIds(selected) : []);
                }}
              >
                节点 ›
              </button>
            </span>
          ))}
          {overview.isSuccess && !countries.length && <span className="external-target-empty">没有匹配的地区</span>}
        </>
      ) : (
        <>
          {servers.isPending && (
            <span role="status" className="external-target-empty">
              正在读取节点列表…
            </span>
          )}
          {servers.error && (
            <>
              <ErrorBox error={servers.error} />
              <button type="button" onClick={() => void servers.refetch()}>
                重试节点列表
              </button>
            </>
          )}
          {visibleServers.map(server => (
            <label className="vpngate-node-option" key={server.id}>
              <input
                type="checkbox"
                aria-label={`选择 ${server.hostname} (${server.ip})`}
                checked={ids.includes(server.id)}
                disabled={!ids.includes(server.id) && ids.length >= VPNGATE_MAX_CANDIDATES}
                onChange={event =>
                  setIds(event.target.checked ? [...ids, server.id] : ids.filter(id => id !== server.id))
                }
              />
              <span>
                <b>{server.hostname}</b>
                <small>
                  {server.ip} · {server.latest_connect_ms == null ? '待实测' : `建连 ${server.latest_connect_ms} ms`}
                  {server.latest_download_bps != null
                    ? ` · ${(server.latest_download_bps / 1_000_000).toFixed(1)} Mbps`
                    : ''}
                </small>
              </span>
            </label>
          ))}
          {missing.map(id => (
            <label className="vpngate-node-option" key={id}>
              <input type="checkbox" checked onChange={() => setIds(ids.filter(candidate => candidate !== id))} />
              <span>
                <b>{id}</b>
                <small>当前列表未显示 · 可取消选择</small>
              </span>
            </label>
          ))}
          {servers.isSuccess && !visibleServers.length && <span className="external-target-empty">没有匹配的节点</span>}
          <div className="vpngate-menu-apply">
            <span>
              已选 {ids.length} / {VPNGATE_MAX_CANDIDATES}
            </span>
            <button
              type="button"
              className="btn primary"
              disabled={!ids.length || !servers.isSuccess || !overview.data?.manual_pools_supported}
              onClick={() => onSelect(country, ids)}
            >
              使用手动节点池
            </button>
          </div>
          {!overview.data?.manual_pools_supported && (
            <p className="note" role="status">
              当前 Console 尚不支持手动节点池，请先升级 Console。可以预览选择，暂不能应用。
            </p>
          )}
        </>
      )}
    </>
  );
}
