import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import { runInNewContext } from 'node:vm';

const html = readFileSync(new URL('../index.html', import.meta.url), 'utf8');
const source = html.match(/<script id="appearance-bootstrap">([\s\S]*?)<\/script>/)?.[1];

test('browser chrome and the document canvas share the header surface', () => {
  const styles = readFileSync(new URL('../src/styles.css', import.meta.url), 'utf8');
  const main = readFileSync(new URL('../src/main.tsx', import.meta.url), 'utf8');
  assert.match(html, /<meta name="theme-color"\s*\/>/);
  assert.match(styles, /html,\s*body\s*\{[^}]*background:\s*var\(--surface\);/s);
  assert.doesNotMatch(styles, /\nbody\s*\{[^}]*background:\s*var\(--ground\);/s);
  assert.match(main, /observeBrowserAppearance\(\)/);
});

test('full-screen startup and workspace layers do not become Safari fixed-edge color sources', () => {
  const styles = readFileSync(new URL('../src/styles.css', import.meta.url), 'utf8');
  for (const selector of ['#stage', '.forge']) {
    const escaped = selector.replace('.', '\\.');
    const block = styles.match(new RegExp(`\\n${escaped} \\{([^}]+)\\}`))?.[1];
    assert.ok(block, `${selector} has a layout rule`);
    assert.match(block, /position:\s*absolute;/);
    assert.match(block, /inset:\s*0;/);
    assert.doesNotMatch(block, /position:\s*(?:fixed|sticky);/);
  }
  // Desktop and topology keep the viewport shell; the touch-page override is checked below.
  assert.match(styles, /html,\s*body\s*\{[^}]*height:\s*100%;[^}]*overflow:\s*hidden;/s);
});

test('touch pages extend into the safe area using document scrolling, not a fixed transparent overlay', () => {
  const styles = readFileSync(new URL('../src/styles.css', import.meta.url), 'utf8');
  assert.match(html, /<meta name="viewport" content="[^\"]*viewport-fit=cover"/);
  assert.match(styles, /@media \(hover: none\) and \(pointer: coarse\)/);
  assert.match(styles, /html:has\(\.fg-desk:not\(\.is-topo\)\)\s*\{[^}]*overflow-y:\s*auto;/s);
  assert.match(styles, /\.fg-desk:not\(\.is-topo\)\s*\{[^}]*overflow:\s*visible;[^}]*background:\s*transparent;/s);
  assert.match(
    styles,
    /\.forge\s*\{[^}]*padding:\s*var\(--safe-top\) var\(--safe-right\) var\(--safe-bottom\) var\(--safe-left\);/s,
  );
  assert.match(styles, /--safe-top:\s*env\(safe-area-inset-top, 0px\)/);
});

test('appearance is restored in the document head before the application module', () => {
  assert.ok(source);
  assert.ok(html.indexOf('appearance-bootstrap') < html.indexOf('src="/src/main.tsx"'));
  for (const theme of ['light', 'dark']) {
    for (const palette of ['jinzi', 'dailan', 'songlv', 'oufen', 'xuanmo']) {
      const root = { dataset: {} };
      const meta = {};
      runInNewContext(source, {
        document: { documentElement: root, querySelector: () => meta },
        localStorage: { getItem: key => (key.endsWith(':theme') ? theme : palette) },
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
    localStorage: {
      getItem() {
        throw new Error('blocked');
      },
    },
  });
  assert.equal(root.dataset.theme, 'dark');
  assert.equal(root.dataset.palette, 'dailan');
});
