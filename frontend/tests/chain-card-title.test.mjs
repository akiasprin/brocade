import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const chains = readFileSync(new URL('../src/panes/chains.tsx', import.meta.url), 'utf8');
const styles = readFileSync(new URL('../src/styles.css', import.meta.url), 'utf8');

test('线路卡片把地区旗垂直居中且不显示拖拽按钮', () => {
  assert.match(
    chains,
    /<span className="chain-card-flag">\s*<RegionFlag code=\{r\.chain\.subscription_country\} \/>\s*<\/span>/,
  );
  assert.doesNotMatch(chains, /className="order-grip chain-order-grip"/);
  assert.match(styles, /\.chain-card-flag \{[^}]*align-items: center;[^}]*height: 18px;/s);
});
