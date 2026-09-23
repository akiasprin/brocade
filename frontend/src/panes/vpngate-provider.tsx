import { RegionFlag } from '../ui/region-flag';
import { isSelectableVpngateRegion, vpngateRegionName, type VpngateOutbound } from '../vpngate-selection';

/** The list card stays independent of the VPN Gate workspace and its chart runtime. */
export function VpngateProviderSection({
  pools,
  chainCount,
  modelState = 'ready',
  onOpen,
}: {
  pools: VpngateOutbound[];
  chainCount: number;
  modelState?: 'pending' | 'ready' | 'error';
  onOpen: () => void;
}) {
  const visiblePools = pools.filter(pool => isSelectableVpngateRegion(pool.protocol.v.country_code));
  const configuredCountryCodes = [
    ...new Set(visiblePools.map(pool => pool.protocol.v.country_code.toUpperCase())),
  ].sort((left, right) => left.localeCompare(right));
  const configuredCountries = configuredCountryCodes.length;
  return (
    <section className="chain-section tunnel-group vpngate-provider-group" aria-busy={modelState === 'pending'}>
      <header className="chain-section-head">
        <span className="no" aria-hidden="true">
          01
        </span>
        <h5>VPN Gate</h5>
        <span className="tunnel-group-agg">
          {modelState === 'ready'
            ? `${configuredCountries} 个地区 · ${chainCount} 条链路`
            : modelState === 'pending'
              ? '读取中'
              : '读取失败'}
        </span>
      </header>
      <div className="tunnel-list">
        <button className="tunnel-row vpngate-provider-card" onClick={onOpen}>
          <span className="tunnel-proto managed">VG</span>
          <span className="tunnel-row-main">
            <span>
              <b>VPN Gate 出口池</b>
            </span>
            {modelState !== 'ready' ? (
              <small>{modelState === 'pending' ? '正在读取隧道模型…' : '隧道模型读取失败'}</small>
            ) : visiblePools.length > 0 ? (
              <small className="vpngate-provider-meta">
                <span>{configuredCountries} 个地区</span>
                <span
                  className="vpngate-provider-flags"
                  title={configuredCountryCodes.map(vpngateRegionName).join('、')}
                >
                  {configuredCountryCodes.slice(0, 5).map(countryCode => (
                    <RegionFlag key={countryCode} code={countryCode} />
                  ))}
                  {configuredCountries > 5 && <i>+{configuredCountries - 5}</i>}
                </span>
              </small>
            ) : (
              <small>尚未配置地区出口</small>
            )}
          </span>
          <span className="tunnel-chevron">›</span>
        </button>
      </div>
    </section>
  );
}
