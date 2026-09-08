import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import { runInNewContext } from 'node:vm';

const html = readFileSync(new URL('../index.html', import.meta.url), 'utf8');
const source = html.match(/<script id="appearance-bootstrap">([\s\S]*?)<\/script>/)?.[1];

test('appearance is restored in the document head before the application module', () => {
  assert.ok(source);
  assert.ok(html.indexOf('appearance-bootstrap') < html.indexOf('src="/src/main.tsx"'));
  for (const theme of ['light', 'dark']) {
    for (const palette of ['jinzi', 'dailan', 'songlv', 'oufen', 'xuanmo']) {
      const root = { dataset: {} };
      const meta = {};
      runInNewContext(source, {
        document: { documentElement: root, querySelector: () => meta },
        localStorage: { getItem: key => key.endsWith(':theme') ? theme : palette },
      });
      assert.equal(root.dataset.theme, theme);
      assert.equal(root.dataset.palette, palette);
      assert.equal(meta.content, theme);
    }
  }
});

test('unavailable local storage does not prevent startup', () => {
  const root = { dataset: {} };
  runInNewContext(source, {
    document: { documentElement: root, querySelector: () => ({}) },
    localStorage: { getItem() { throw new Error('blocked'); } },
  });
  assert.equal(root.dataset.theme, 'dark');
  assert.equal(root.dataset.palette, 'dailan');
});
