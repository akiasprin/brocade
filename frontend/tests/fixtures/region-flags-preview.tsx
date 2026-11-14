import { createRoot } from 'react-dom/client';
import { theme } from '../../src/forge/theme';
import { RegionFlag } from '../../src/ui/region-flag';
import '../../src/styles.css';

const desired = new URLSearchParams(location.search).get('theme') === 'dark' ? 'dark' : 'light';
if (theme.snapshot() !== desired) theme.toggle();

// Include both ends of a full sprite row, the last partial row, and the reported edge artifacts.
const regions = ['AD', 'AZ', 'CN', 'TW', 'SG', 'HK', 'MO', 'JP', 'MY', 'ZW'];

createRoot(document.getElementById('root')!).render(
  <>
    <main className="flag-preview" style={{ margin: 24, padding: 20, width: 480, background: 'var(--card)' }}>
      <h2>地区旗渲染</h2>
      <table style={{ width: '100%', borderCollapse: 'collapse' }}>
        <thead>
          <tr>
            <th>地区</th>
            <th>15 × 10</th>
            <th>16 × 11</th>
            <th>方形标识</th>
          </tr>
        </thead>
        <tbody>
          {regions.map(code => (
            <tr key={code} data-region={code} style={{ height: 48 }}>
              <th>{code}</th>
              <td data-size="15x10" style={{ textAlign: 'center' }}>
                <RegionFlag code={code} />
              </td>
              <td data-size="16x11" style={{ textAlign: 'center' }}>
                <span className="rut-source" style={{ display: 'inline-flex' }}>
                  <RegionFlag code={code} />
                </span>
              </td>
              <td data-size="square" style={{ textAlign: 'center' }}>
                <span className="nd-idplate" style={{ display: 'inline-block' }}>
                  <span className="nd-idplate-clip">
                    <RegionFlag code={code} square />
                  </span>
                </span>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </main>
    {/* Integer coordinates keep color probes independent of inline text baseline rounding. */}
    <aside style={{ position: 'absolute', left: 560, top: 24 }}>
      {['CN', 'TW', 'SG'].map(code => (
        <div
          key={code}
          className="rut-source"
          data-probe={code}
          style={{ display: 'flex', height: 32, alignItems: 'flex-start' }}
        >
          <RegionFlag code={code} />
        </div>
      ))}
    </aside>
  </>,
);
