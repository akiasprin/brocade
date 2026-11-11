import { test, expect } from '@playwright/test';

for (const width of [1440, 390])
  for (const theme of ['dark', 'light'])
    test(`chain card surface matches machines ${width} ${theme}`, async ({ page }) => {
      const errors: string[] = [];
      page.on('pageerror', error => errors.push(error.message));
      await page.clock.setFixedTime(new Date('2026-10-27T08:00:00Z'));
      await page.setViewportSize({ width, height: 1000 });
      await page.goto(`/tests/fixtures/chain-unlock-preview.html?view=cards&scene=calm&theme=${theme}#/chains`);
      const card = page.locator('.chain-card').first();
      await expect(card).toBeVisible();
      await page.evaluate(() => document.fonts.ready);
      const fill = await card.evaluate(el => getComputedStyle(el).backgroundColor);
      await expect(page).toHaveScreenshot(`chain-fill-${width}-${theme}.png`);
      await page.getByRole('navigation', { name: '主导航' }).getByRole('button', { name: '机器', exact: true }).click();
      const machine = page.locator('.node-card').first();
      await expect(machine).toBeVisible();
      expect(await machine.evaluate(el => getComputedStyle(el).backgroundColor)).toBe(fill);
      await page.goBack();
      await expect(card).toBeVisible();
      const marker = card.locator('.extreme-point').first();
      await expect(marker).toHaveCount(1);
      expect(await marker.evaluate(el => getComputedStyle(el, '::before').backgroundColor)).toBe(fill);
      expect(await card.locator('.plot-label').evaluate(el => getComputedStyle(el).textShadow)).toContain(fill);

      if (width === 1440) {
        await card.hover();
        await expect.poll(() => card.evaluate(el => getComputedStyle(el).backgroundColor)).not.toBe(fill);
        await expect(card).toHaveScreenshot(`chain-hover-${theme}.png`);
      }
      await page.getByRole('button', { name: '多选', exact: true }).click();
      await card.locator('input[type="checkbox"]').check();
      await expect(card).toHaveClass(/picked/);
      await expect.poll(() => card.evaluate(el => getComputedStyle(el).backgroundColor)).toBe(fill);
      expect(await card.evaluate(el => getComputedStyle(el).backgroundImage)).not.toBe('none');
      await expect(card).toHaveScreenshot(`chain-picked-${width}-${theme}.png`);
      await page.getByRole('button', { name: '取消', exact: true }).click();
      if (width === 1440) {
        let bounds = await card.boundingBox();
        await page.mouse.move(bounds!.x + 80, bounds!.y + 30);
        await page.mouse.down();
        await page.mouse.move(bounds!.x + 86, bounds!.y + 30);
        await page.mouse.up();
        await expect(card).not.toHaveClass(/order-drag-active/);
        await expect(page.locator('.order-ghost')).toHaveCount(0);
        await expect.poll(() => card.evaluate(el => new DOMMatrixReadOnly(getComputedStyle(el).transform).a)).toBe(1);
        bounds = await card.boundingBox();
        await page.mouse.move(bounds!.x + 80, bounds!.y + 30);
        await page.mouse.down();
        await expect
          .poll(() => card.evaluate(el => new DOMMatrixReadOnly(getComputedStyle(el).transform).a))
          .toBe(0.985);
        // Cross the threshold in one event. Coalescing stepped pointer moves
        // otherwise changes the ghost's captured initial offset by a few px.
        await page.mouse.move(bounds!.x + 86, bounds!.y + 30);
        await expect(card).toHaveClass(/order-drag-active/);
        const ghost = page.locator('.order-ghost');
        await expect(ghost).toHaveCount(1);
        expect(Math.abs((await ghost.boundingBox())!.width - (await card.boundingBox())!.width)).toBeLessThan(0.5);
        await page.mouse.move(bounds!.x + 110, bounds!.y + 30);
        await expect
          .poll(() => card.evaluate(element => new DOMMatrixReadOnly(getComputedStyle(element).transform).a))
          .toBe(1);
        await expect(card).toHaveScreenshot(`chain-drag-${theme}.png`);
        await page.keyboard.press('Escape');
        await page.mouse.up();
        await expect(card).not.toHaveClass(/order-drag-active/);
      }
      expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
      expect(errors).toEqual([]);
    });
