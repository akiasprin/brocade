import { expect, test, type Locator, type Page } from '@playwright/test';

const fixture = '/tests/fixtures/rule-flow-preview.html';

async function dragMachine(page: Page, handle: Locator, dx: number, dy: number, release = true) {
  const box = (await handle.boundingBox())!;
  const start = { x: box.x + 60, y: box.y + 18 };
  await page.mouse.move(start.x, start.y);
  await page.mouse.down();
  await page.mouse.move(start.x + dx, start.y + dy, { steps: 8 });
  if (release) await page.mouse.up();
}

async function wireEnds(wire: Locator) {
  return wire.evaluate(element => {
    const path = element as SVGPathElement;
    const matrix = path.getScreenCTM();
    if (!matrix) throw new Error('The visible wire must have screen coordinates');
    const point = (length: number) => {
      const at = path.getPointAtLength(length).matrixTransform(matrix);
      return { x: at.x, y: at.y };
    };
    return { start: point(0), end: point(path.getTotalLength()) };
  });
}

async function expectWireAnchored(wire: Locator, outlet: Locator, inlet: Locator) {
  await expect
    .poll(async () => {
      const ends = await wireEnds(wire);
      const out = (await outlet.boundingBox())!;
      const into = (await inlet.boundingBox())!;
      return Math.max(
        Math.abs(ends.start.x - out.x - out.width / 2),
        Math.abs(ends.start.y - out.y - out.height / 2),
        Math.abs(ends.end.x - into.x),
        Math.max(0, into.y - ends.end.y, ends.end.y - into.y - into.height),
      );
    })
    .toBeLessThan(1);
}

for (const browserName of ['chromium', 'webkit'] as const) {
  const browserTest = test.extend({ browserName });
  for (const width of [390, 1440])
    for (const theme of ['light', 'dark']) {
      browserTest(`${browserName} flow ${width} ${theme}`, async ({ page }) => {
        const errors: string[] = [];
        page.on('pageerror', error => errors.push(error.message));
        await page.setViewportSize({ width, height: 1100 });
        await page.goto(`${fixture}?theme=${theme}`);
        await expect(page.getByRole('switch')).toHaveAttribute('aria-checked', 'true');
        await expect(page.locator('.chain-rule-section-list')).toBeHidden();
        await expect(page.getByRole('combobox', { name: '追踪域名或 IP' })).toBeVisible();
        const well = page.locator('.flow-well');
        const search = well.locator('.flow-trace-capsule');
        await expect(search.locator('svg')).toBeVisible();
        const wellBounds = (await well.boundingBox())!;
        const searchBounds = (await search.boundingBox())!;
        expect(searchBounds.x).toBeGreaterThan(wellBounds.x);
        expect(searchBounds.y).toBeGreaterThan(wellBounds.y);
        expect(Math.abs(searchBounds.x + searchBounds.width / 2 - wellBounds.x - wellBounds.width / 2)).toBeLessThan(2);
        expect(searchBounds.y - wellBounds.y).toBeLessThanOrEqual(30);
        expect(searchBounds.x + searchBounds.width).toBeLessThan(wellBounds.x + wellBounds.width);
        const cardTops = await well
          .locator('.flow-machine')
          .evaluateAll(cards => cards.map(card => card.getBoundingClientRect().top));
        expect(Math.min(...cardTops)).toBeGreaterThan(searchBounds.y + searchBounds.height);
        await expect(page.locator('.flow-machine')).toHaveCount(5);
        await expect(page.locator('.flow-wires path')).toHaveCount(10);
        await page.evaluate(() => document.fonts.ready);
        const headings = await page.locator('.flow-machine-head').evaluateAll(heads =>
          heads.map(head => {
            const port = head.querySelector('.flow-listen')!.getBoundingClientRect();
            const role = head.querySelector('.flow-machine-role')!.getBoundingClientRect();
            const name = head.querySelector('b')!.getBoundingClientRect();
            return {
              port: head.querySelector('.flow-listen')!.textContent,
              portBox: port,
              roleBox: role,
              nameBox: name,
            };
          }),
        );
        expect(headings[0].port).toBe(':443');
        for (const heading of headings.slice(1)) expect(heading.port).toBe('中转 :20001');
        for (const heading of headings) {
          expect(
            Math.abs(heading.portBox.y + heading.portBox.height / 2 - heading.roleBox.y - heading.roleBox.height / 2),
          ).toBeLessThan(1);
          expect(
            Math.abs(heading.portBox.y + heading.portBox.height / 2 - heading.nameBox.y - heading.nameBox.height / 2),
          ).toBeLessThan(1);
          expect(heading.portBox.x + heading.portBox.width).toBeLessThan(heading.roleBox.x);
        }
        expect(await page.evaluate(() => document.documentElement.scrollWidth - innerWidth)).toBeLessThanOrEqual(1);
        const toggle = page.getByRole('switch');
        const title = page.locator('.chain-rule-section-name');
        const toggleBounds = await toggle.boundingBox();
        const titleBounds = await title.boundingBox();
        expect(toggleBounds!.x).toBeGreaterThan(titleBounds!.x + titleBounds!.width);
        if (width <= 820) expect(toggleBounds!.height).toBeGreaterThanOrEqual(44);
        await expect(page.locator('.node-chain-sheet')).toHaveScreenshot(`${browserName}-flow-${width}-${theme}.png`);
        await page.locator('.flow-machine').first().locator('li button').first().click();
        const inspector = page.getByRole('dialog', { name: '链路检视' });
        await expect(inspector).toBeVisible();
        const surface = inspector.locator('.dialog-surface');
        const bounds = await surface.boundingBox();
        expect(bounds!.x).toBeGreaterThanOrEqual(0);
        expect(bounds!.x + bounds!.width).toBeLessThanOrEqual(width);
        await page.keyboard.press('Escape');
        await expect(inspector).not.toBeVisible();
        await expect(toggle).toHaveAttribute('aria-checked', 'true');
        if (browserName === 'chromium' && theme === 'light') {
          await page.getByRole('combobox', { name: '追踪域名或 IP' }).fill('video.example');
          await expect(page.getByRole('status')).toContainText('本机出网');
          await expect(page.locator('.flow-machine li.is-hit')).toHaveCount(2);
          await expect(page.locator('.node-chain-sheet')).toHaveScreenshot(`trace-${width}-light.png`);
        }
        expect(errors).toEqual([]);
      });

      browserTest(`${browserName} inspector ${width} ${theme}`, async ({ page }) => {
        const errors: string[] = [];
        page.on('pageerror', error => errors.push(error.message));
        await page.setViewportSize({ width, height: 1100 });
        await page.goto(`${fixture}?theme=${theme}`);
        await page.evaluate(() => document.fonts.ready);
        const opener = page.locator('.flow-machine').first().locator('li button').first();
        await opener.click();
        const inspector = page.getByRole('dialog', { name: '链路检视' });
        const surface = inspector.locator('.dialog-surface');
        const close = inspector.getByRole('button', { name: '关闭链路检视' });
        await expect(close).toBeFocused();
        await expect(surface).toHaveScreenshot(`${browserName}-inspector-${width}-${theme}.png`);
        await close.click();
        await expect(inspector).not.toBeVisible();
        await expect(opener).toBeFocused();

        await page.mouse.move(0, 0);
        await opener.press('Enter');
        await expect(close).toBeFocused();
        expect(await close.evaluate(button => button.matches(':focus-visible'))).toBe(true);
        if (browserName === 'chromium' && width === 1440 && theme === 'dark') {
          const closeBounds = (await close.boundingBox())!;
          await expect(page).toHaveScreenshot('inspector-close-keyboard-dark.png', {
            clip: {
              x: Math.floor(closeBounds.x) - 4,
              y: Math.floor(closeBounds.y) - 4,
              width: Math.ceil(closeBounds.width) + 8,
              height: Math.ceil(closeBounds.height) + 8,
            },
          });
        }
        await page.keyboard.press('Escape');
        await expect(inspector).not.toBeVisible();
        await expect(opener).toBeFocused();

        const machine = page.locator('.flow-machine-head').filter({ hasText: '香港 AKILE II' });
        await machine.click();
        await expect(inspector.locator('.flow-dns')).toHaveCount(3);
        await expect(inspector.getByText('GEOSITE · video', { exact: true })).toBeVisible();
        await expect(inspector.getByText('GEOSITE · abema', { exact: true })).toBeVisible();
        await expect(surface).toHaveScreenshot(`${browserName}-dns-inspector-${width}-${theme}.png`);
        if (browserName === 'chromium' && width === 1440 && theme === 'light') {
          await close.hover();
          await expect(close).toHaveScreenshot('inspector-close-hover-light.png');
        }
        await close.click();
        await expect(inspector).not.toBeVisible();
        await expect(machine).toBeFocused();
        expect(errors).toEqual([]);
      });
    }

  for (const width of [390, 1440]) {
    const menuTest = browserTest.extend({ hasTouch: width === 390 });
    menuTest(`${browserName} common domain menu ${width}`, async ({ page }) => {
      const theme = width === 390 ? 'light' : 'dark';
      await page.setViewportSize({ width, height: 1100 });
      await page.goto(`${fixture}?theme=${theme}`);
      const input = page.getByRole('combobox', { name: '追踪域名或 IP' });
      const menu = page.getByRole('listbox', { name: '常用域名' });
      const capsule = page.locator('.flow-trace-capsule');
      await expect(menu).toHaveCount(0);
      if (width === 390) await input.tap();
      else await input.click();
      await expect(menu).toBeVisible();
      await expect(menu.getByRole('option')).toHaveCount(6);
      await expect(menu.getByRole('option').locator('svg')).toHaveCount(6);
      const bounds = (await menu.boundingBox())!;
      expect(bounds.x).toBeGreaterThanOrEqual(0);
      expect(bounds.x + bounds.width).toBeLessThanOrEqual(width);
      await expect(menu).toHaveScreenshot(`${browserName}-domains-${width}-${theme}.png`);
      const muse = menu.getByRole('option', { name: 'muse.ai Muse' });
      if (width === 390) await muse.tap();
      else await muse.click();
      await expect(input).toHaveValue('muse.ai');
      await expect(menu).toHaveCount(0);
      await expect(input).toBeFocused();
      await input.click();
      await input.press('ArrowDown');
      await expect(menu.getByRole('option').first()).toHaveAttribute('aria-selected', 'true');
      await input.press('ArrowDown');
      await input.press('Enter');
      await expect(input).toHaveValue('youtube.com');
      await expect(menu).toHaveCount(0);
      await input.fill('invalid domain');
      await expect(input).toHaveAttribute('aria-invalid', 'true');
      await expect(capsule).toHaveScreenshot(`${browserName}-capsule-invalid-${width}-${theme}.png`);
      await input.press('Escape');
      await expect(input).toHaveValue('');
      await input.click();
      await input.press('Escape');
      await expect(menu).toHaveCount(0);
      const tcp = page.getByRole('radio', { name: 'TCP', exact: true });
      const udp = page.getByRole('radio', { name: 'UDP', exact: true });
      await udp.click();
      await expect(udp).toHaveAttribute('aria-checked', 'true');
      await udp.press('ArrowLeft');
      await expect(tcp).toBeFocused();
      await expect(tcp).toHaveAttribute('aria-checked', 'true');
      await input.fill('video.example');
      await page.getByRole('button', { name: '清除追踪' }).click();
      await expect(input).toHaveValue('');
      await expect(input).toBeFocused();
      await input.click();
      await page.locator('.flow-well').click({ position: { x: 3, y: 3 } });
      await expect(menu).toHaveCount(0);
    });
  }

  browserTest(`${browserName} machine dragging keeps wires anchored and rules unchanged`, async ({ page }) => {
    const errors: string[] = [];
    page.on('pageerror', error => errors.push(error.message));
    await page.setViewportSize({ width: 1440, height: 1100 });
    await page.goto(`${fixture}?theme=dark`);
    const source = page.locator('.flow-machine').first();
    const head = source.locator('.flow-machine-head');
    const target = page.locator('.flow-machine-head').filter({ hasText: '香港 AKILE II' });
    const wire = page.locator('.flow-wires path').first();
    const outlet = source.locator('[data-flow-outlet]').first();
    await expect(wire).toBeAttached();
    await page.evaluate(() => document.fonts.ready);
    const rules = await source.locator('.flow-match').allTextContents();
    const before = (await head.boundingBox())!;
    const beforeWire = await wire.getAttribute('d');
    await dragMachine(page, head, 64, 0);
    await expect.poll(async () => (await head.boundingBox())!.x).toBeCloseTo(before.x + 64, 0);
    await expect(page.getByRole('dialog', { name: '链路检视' })).toHaveCount(0);
    await expect(wire).not.toHaveAttribute('d', beforeWire!);
    await expectWireAnchored(wire, outlet, target);
    await dragMachine(page, target, 24, 12);
    await expectWireAnchored(wire, outlet, target);
    expect(await source.locator('.flow-match').allTextContents()).toEqual(rules);
    await expect(page.locator('.flow-machine')).toHaveCount(5);
    await page.mouse.move(0, 0);
    await expect(page.locator('.node-chain-sheet')).toHaveScreenshot(`${browserName}-dragged-flow-1440-dark.png`);
    await head.click();
    const inspector = page.getByRole('dialog', { name: '链路检视' });
    await expect(inspector).toBeVisible();
    await page.keyboard.press('Escape');
    await expect(inspector).not.toBeVisible();
    expect(errors).toEqual([]);
  });

  browserTest(
    `${browserName} machine dragging supports cancel, keyboard movement and compact layout`,
    async ({ page }) => {
      await page.setViewportSize({ width: 1440, height: 1100 });
      await page.goto(`${fixture}?theme=light`);
      const head = page.locator('.flow-machine-head').first();
      await expect(page.locator('.flow-wires path').first()).toBeAttached();
      await page.evaluate(() => document.fonts.ready);
      const before = (await head.boundingBox())!;
      await dragMachine(page, head, 50, 32, false);
      await page.keyboard.press('Escape');
      await page.mouse.up();
      await expect.poll(async () => (await head.boundingBox())!.x).toBeCloseTo(before.x, 0);
      await expect.poll(async () => (await head.boundingBox())!.y).toBeCloseTo(before.y, 0);
      await expect(page.getByRole('dialog', { name: '链路检视' })).toHaveCount(0);
      await head.press('ArrowRight');
      await expect.poll(async () => (await head.boundingBox())!.x).toBeCloseTo(before.x + 10, 0);
      await head.press('Shift+ArrowRight');
      await expect.poll(async () => (await head.boundingBox())!.x).toBeCloseTo(before.x + 11, 0);
      await head.press('Home');
      await expect.poll(async () => (await head.boundingBox())!.x).toBeCloseTo(before.x, 0);
      await head.press('ArrowRight');
      await page.setViewportSize({ width: 390, height: 1100 });
      await expect(head).toHaveAttribute('title', '检视机器');
      expect(await head.locator('..').evaluate(element => getComputedStyle(element).transform)).toBe('none');
      expect(await page.evaluate(() => document.documentElement.scrollWidth - innerWidth)).toBeLessThanOrEqual(1);
      await head.click();
      await expect(page.getByRole('dialog', { name: '链路检视' })).toBeVisible();
    },
  );

  browserTest(`${browserName} wire animation respects reduced motion and covered rules`, async ({ page }) => {
    await page.setViewportSize({ width: 1440, height: 1100 });
    await page.goto(`${fixture}?covered-forward`);
    const wire = page.locator('.flow-wires path').first();
    await expect(wire).toBeAttached();
    expect(await wire.evaluate(element => getComputedStyle(element).animationName)).toBe('none');
    expect(await wire.evaluate(element => getComputedStyle(element).strokeDasharray)).not.toBe('none');
    await page.emulateMedia({ reducedMotion: 'no-preference' });
    await expect.poll(() => wire.evaluate(element => getComputedStyle(element).animationName)).toBe('flow-wire-motion');
    const offset = await wire.evaluate(element => getComputedStyle(element).strokeDashoffset);
    await expect.poll(() => wire.evaluate(element => getComputedStyle(element).strokeDashoffset)).not.toBe(offset);
    const covered = page.locator('.flow-wires path.is-shadowed');
    await expect(covered).not.toHaveCount(0);
    for (const path of await covered.all())
      expect(await path.evaluate(element => getComputedStyle(element).animationName)).toBe('none');
    await page.emulateMedia({ reducedMotion: 'reduce' });
    await expect.poll(() => wire.evaluate(element => getComputedStyle(element).animationName)).toBe('none');
  });
}

test('seven chains keep independent modes, queries, draft inputs and scoped rule reveal', async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 1100 });
  await page.goto(`${fixture}?node`);
  const sections = page.locator('.chain-rule-section');
  await expect(sections).toHaveCount(7);
  const first = sections.nth(0);
  const second = sections.nth(1);
  for (const section of await sections.all()) {
    await expect(section.getByRole('switch')).toHaveAttribute('aria-checked', 'true');
    await expect(section.getByRole('combobox', { name: '追踪域名或 IP' })).toBeVisible();
  }
  await first.getByRole('switch').click();
  const input = first.getByRole('textbox', { name: '匹配内容（规则 1）', exact: true });
  await input.fill('unsaved.example');
  await first.getByRole('switch').click();
  await first.getByRole('combobox', { name: '追踪域名或 IP' }).fill('unsaved.example');
  await second.getByRole('combobox', { name: '追踪域名或 IP' }).fill('tw.example');
  await expect(first.getByRole('status')).toContainText('本机出网');
  await expect(second.getByRole('status')).toContainText('本机出网');
  await expect(sections.nth(2).getByRole('switch')).toHaveAttribute('aria-checked', 'true');
  await expect(first.locator('.flow-self')).toHaveText('本机');
  await first.getByRole('switch').click();
  await expect(input).toHaveValue('unsaved.example');
  await first.getByRole('switch').press('Space');
  await expect(first.getByRole('combobox', { name: '追踪域名或 IP' })).toHaveValue('unsaved.example');
  await first.locator('.flow-machine').first().locator('li button').nth(1).click();
  await page.getByRole('button', { name: '在规则表中查看', exact: true }).click();
  await expect(first.getByRole('switch')).toHaveAttribute('aria-checked', 'false');
  await expect(first.locator('.chain-rule-row-flash')).toHaveCount(1);
  await expect(first.locator('.chain-rule-row-flash')).toBeFocused();
  await expect(second.getByRole('switch')).toHaveAttribute('aria-checked', 'true');
  await expect(second.getByRole('combobox', { name: '追踪域名或 IP' })).toHaveValue('tw.example');
  await expect(input).toBeVisible();
  await expect(page.locator('.chain-rule-node-head .b-chain')).toHaveCount(0);
  await expect(page.getByRole('button', { name: '保存到草稿', exact: true })).toHaveCount(1);
});

test('readonly users can inspect and trace without enabling any rule writes', async ({ page }) => {
  await page.goto(`${fixture}?readonly`);
  await expect(page.getByRole('switch')).toHaveAttribute('aria-checked', 'true');
  await page.getByRole('combobox', { name: '追踪域名或 IP' }).fill('unknown.example');
  await expect(page.getByRole('status')).toContainText('本机出网');
  await expect(page.locator('.flow-machine li.is-uncertain')).toHaveCount(0);
  await page.getByRole('combobox', { name: '追踪域名或 IP' }).fill('openai.com');
  await expect(page.getByRole('status')).toContainText('东京出口');
  await expect(page.locator('.flow-machine li.is-hit')).toHaveCount(2);
  await page.getByRole('combobox', { name: '追踪域名或 IP' }).fill('unavailable.example');
  await expect(page.getByRole('status')).toContainText('统一 geosite 规则集暂时无法加载');
  await expect(page.locator('.flow-machine li.is-uncertain')).toHaveCount(1);
  await page.getByRole('combobox', { name: '追踪域名或 IP' }).fill('192.0.2.1');
  await expect(page.getByRole('status')).toContainText('需节点嗅探结果');
  await page.getByRole('switch').click();
  await expect(page.getByRole('textbox', { name: '匹配内容（规则 1）' })).toBeDisabled();
});

test('shared geosite highlights the matching hop and clears a stale result when the domain changes', async ({
  page,
}) => {
  await page.setViewportSize({ width: 1440, height: 1100 });
  await page.goto(`${fixture}?theme=light`);
  const input = page.getByRole('combobox', { name: '追踪域名或 IP' });
  await input.fill('openai.com');
  await expect(page.getByRole('status')).toContainText('东京出口');
  await expect(page.getByRole('status')).toContainText('本机出网');
  await expect(page.locator('.flow-machine li.is-hit')).toHaveCount(2);
  await expect(page.locator('.node-chain-sheet')).toHaveScreenshot('geosite-hit-1440-light.png');
  await input.fill('unknown.example');
  await expect(page.getByRole('status')).toContainText('香港 FXT II');
  await expect(page.getByRole('status')).toContainText('本机出网');
  await expect(page.locator('.flow-machine li.is-hit')).toHaveCount(1);
});

test('simple chains render one straight hop and generated fallback reveal expands the target table', async ({
  page,
}) => {
  await page.setViewportSize({ width: 1440, height: 1100 });
  await page.goto(`${fixture}?node`);
  const sections = page.locator('.chain-rule-section');
  await expect(sections.nth(3).locator('.flow-wires path')).toHaveCount(1);
  await expect(sections.nth(3).locator('.flow-hop')).toHaveCount(1);
  const generated = sections
    .first()
    .locator('.flow-machine')
    .filter({ has: page.locator('.flow-machine-head', { hasText: '香港 AKILE II' }) })
    .locator('li button')
    .first();
  await generated.click();
  await page.getByRole('button', { name: '在规则表中查看', exact: true }).click();
  const target = sections.first().locator('[data-rule-node="hk-akile"]');
  await expect(target.locator('> .chain-rule-node-head')).toHaveAttribute('aria-expanded', 'true');
  await expect(target.locator('.chain-rule-row-flash')).toContainText('自动补齐');
  await expect(sections.nth(3).getByRole('switch')).toHaveAttribute('aria-checked', 'true');
});

test('machine chain headings and separators match the adopted design', async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 1100 });
  await page.goto(`${fixture}?node&theme=dark`);
  const sections = page.locator('.chain-rule-section');
  await page.evaluate(() => document.fonts.ready);
  await expect(sections.first()).toHaveScreenshot('machine-flow-first-chain-dark.png');
  await expect(sections.nth(3)).toHaveScreenshot('machine-flow-simple-chain-dark.png');
  const separator = await page
    .locator('.node-chain-use')
    .nth(1)
    .evaluate(element => getComputedStyle(element, '::before').height);
  expect(separator).toBe('1px');
});
