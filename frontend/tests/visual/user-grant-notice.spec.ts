import { test, expect } from '@playwright/test';

for (const width of [320, 390, 820, 1440])
  for (const theme of ['dark', 'light'])
    for (const state of ['stale', 'error']) {
      test(`${state} grant confirmation at ${width}px in ${theme}`, async ({ page }) => {
        const errors: string[] = [];
        page.on('pageerror', error => errors.push(error.message));
        await page.clock.setFixedTime(new Date('2026-10-23T08:00:00Z'));
        await page.setViewportSize({ width, height: 1000 });
        await page.goto(`/tests/fixtures/user-header-preview.html?grants=${state}&theme=${theme}`);
        await expect(page.locator('.grant-card').first()).toBeVisible();
        await page.locator('.grant-card').first().click();
        await page.getByRole('button', { name: '保存', exact: true }).click();
        const notice = page.getByRole('status').filter({ hasText: '授权状态尚未确认' });
        await expect(notice).toBeVisible();
        const editor = page.locator('.user-grant-editor');
        await expect(editor.getByRole('status')).toContainText('授权状态尚未确认');
        await expect(page.locator('.user-cardpage > [role="status"]')).toHaveCount(0);
        if (state === 'error') await expect(notice).toContainText('暂时无法读取最新授权');
        await expect(page.locator('.grant-card').first()).toBeDisabled();
        await page.evaluate(() => document.fonts.ready);
        const layout = await notice.evaluate(element => {
          const text = element.querySelector('span')!.getBoundingClientRect();
          const footer = element.closest('footer')!;
          const actions = footer.querySelector('.grant-editor-buttons')!.getBoundingClientRect();
          const notice = element.getBoundingClientRect();
          const footerStyle = getComputedStyle(footer);
          const cards = element.closest('.user-grant-editor')!.querySelector('.grant-cards')!.getBoundingClientRect();
          return {
            textAlign: getComputedStyle(element).textAlign,
            leftInset: text.left - footer.getBoundingClientRect().left - Number.parseFloat(footerStyle.paddingLeft),
            beforeActions: notice.right <= actions.left,
            aligned: Math.abs((notice.top + notice.bottom) / 2 - (actions.top + actions.bottom) / 2) < 1,
            followsCards: notice.top >= cards.bottom,
            overflows: element.scrollWidth > element.clientWidth,
          };
        });
        expect(layout).toEqual({
          textAlign: 'left',
          leftInset: 0,
          beforeActions: true,
          aligned: true,
          followsCards: true,
          overflows: false,
        });
        await expect(notice).toHaveScreenshot(`${state}-${width}-${theme}-notice.png`);
        await expect(editor).toHaveScreenshot(`${state}-${width}-${theme}.png`);
        expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
        expect(errors).toEqual([]);
      });
    }
