import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import {
  STUDIES,
  REFERENCE_TOKENS,
  candidateTokens,
  contrast,
  luminance,
  mix,
  toLch,
} from './fixtures/palette-candidate-colors.mjs';

const css = readFileSync(new URL('../src/styles.css', import.meta.url), 'utf8');
const declarations = selector => {
  const block = css.match(new RegExp(selector.replace(/[.*+?^${}()|[\]\\]/g, '\\$&') + ' \\{([^}]+)\\}'))?.[1];
  assert.ok(block, selector);
  return Object.fromEntries(
    [...block.matchAll(/(--[\w-]+):\s*([^;]+);/g)].map(([, name, value]) => [name, value.trim()]),
  );
};
const blueReference = theme => {
  const base = theme === 'dark' ? ":root,\n[data-theme='dark']" : ":root[data-theme='light'],\n[data-theme='light']";
  const palette =
    theme === 'dark' ? ":root[data-palette='dailan']" : ":root[data-theme='light'][data-palette='dailan']";
  const chart = theme === 'dark' ? declarations(":root[data-palette='dailan']:not([data-theme='light'])") : {};
  const raw = { ...declarations(base), ...declarations(palette), ...chart };
  const resolve = name => {
    assert.ok(raw[name], name);
    return raw[name].replace(/var\((--[\w-]+)\)/g, (_, nested) => resolve(nested));
  };
  return Object.fromEntries(REFERENCE_TOKENS.map(name => [name, resolve(name)]));
};
const lstar = hex => {
  const y = luminance(hex);
  return y > 0.008856 ? 116 * Math.cbrt(y) - 16 : 903.3 * y;
};
const pairs = [
  ['--action', '--card'],
  ['--data', '--card'],
  ['--primary-on', '--primary'],
  ['--primary-on', '--primary-hover'],
  ['--primary-on', '--primary-active'],
  ['--ink', '--card'],
];

test('dailan is a locked live reference with no replacement candidates', () => {
  assert.deepEqual(STUDIES.dailan.variants, {});
  assert.equal(STUDIES.dailan.recommended, 'current');
  for (const theme of ['dark', 'light']) {
    assert.deepEqual(candidateTokens('dailan', 'current', theme, blueReference(theme)), {});
    for (const variant of ['v1', 'v2', 'v3', 'v4', 'v5', 'fog', 'mineral', 'neutral'])
      assert.throws(() => candidateTokens('dailan', variant, theme, blueReference(theme)), /Unknown color study/);
  }
});

for (const [palette, study] of Object.entries(STUDIES).filter(([key]) => key !== 'dailan')) {
  test(`${palette} has five different candidates in the current blue design language`, () => {
    assert.equal(Object.keys(study.variants).length, 5);
    assert.ok(study.variants[study.recommended]);
    for (const theme of ['dark', 'light']) {
      const blue = blueReference(theme);
      const colors = Object.keys(study.variants).map(
        variant => candidateTokens(palette, variant, theme, blue)['--action'],
      );
      assert.equal(new Set(colors).size, 5);
      assert.deepEqual(candidateTokens(palette, 'current', theme, blue), {});
    }
  });
  for (const variant of Object.keys(study.variants))
    for (const theme of ['dark', 'light']) {
      test(`${palette}/${variant}/${theme} preserves blue typography, surfaces, states, and emphasis`, () => {
        const blue = blueReference(theme);
        const t = candidateTokens(palette, variant, theme, blue);
        for (const name of [
          '--ink',
          '--ink-2',
          '--ink-3',
          '--ink-4',
          '--ok',
          '--gold',
          '--err',
          '--line-soft',
          '--shadow',
          '--sheet-shadow',
          '--float-filter',
        ])
          assert.equal(t[name], blue[name], name);
        for (const name of [
          '--ground',
          '--sunk',
          '--surface',
          '--sheet',
          '--block',
          '--card',
          '--band',
          '--line',
          '--grid-line',
        ])
          assert.ok(Math.abs(lstar(t[name]) - lstar(blue[name])) < 0.4, `${name} stays on the blue ladder`);
        for (const [foreground, background] of pairs) {
          const actual = contrast(t[foreground], t[background]);
          const original = contrast(blue[foreground], blue[background]);
          assert.ok(Math.abs(actual - original) < 0.16, `${foreground}/${background}: ${actual} follows ${original}`);
        }
        for (const name of ['--primary', '--primary-hover', '--primary-active'])
          assert.ok(contrast(t['--primary-on'], t[name]) >= 4.5, name);
        if (theme === 'light') {
          assert.equal(t['--primary'], t['--action']);
          assert.equal(t['--primary-hover'], t['--action-hover']);
          assert.equal(t['--primary-active'], t['--action-active']);
          assert.equal(t['--primary-on'], blue['--primary-on']);
          assert.equal(t['--primary-line'], 'transparent');
        } else assert.notEqual(t['--primary'], t['--action']);
        for (const name of ['--action-wash', '--data-wash'])
          assert.equal(t[name].match(/,\s*([\d.]+)\)$/)[1], blue[name].match(/,\s*([\d.]+)\)$/)[1], `${name} opacity`);
        const line = contrast(mix(t['--card'], t['--data'], 0.72), t['--card']);
        const originalLine = contrast(mix(blue['--card'], blue['--data'], 0.72), blue['--card']);
        assert.ok(Math.abs(line - originalLine) < 0.16, 'chart emphasis follows current blue');
        const chart = Array.from({ length: 10 }, (_, i) => t[`--observe-${i + 1}`]);
        assert.equal(t['--data'], chart[0]);
        assert.equal(new Set(chart).size, 10);
        assert.ok(chart.every(value => /^#[0-9a-f]{6}$/.test(value)));
        const hues = chart.slice(0, 4).map(value => toLch(value).h);
        for (let a = 0; a < hues.length; a++)
          for (let b = a + 1; b < hues.length; b++) {
            const delta = Math.abs(hues[a] - hues[b]);
            assert.ok(Math.min(delta, 360 - delta) > 25, 'common chart categories remain distinct');
          }
      });
    }
}
