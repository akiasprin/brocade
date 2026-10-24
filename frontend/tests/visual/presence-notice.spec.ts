import { test, expect } from '@playwright/test';

const cases = [
  ...[320, 390, 820, 1440].flatMap(width =>
    ['dark', 'light'].map(theme => ({ width, theme, state: 'partial', long: false })),
  ),
  { width: 390, theme: 'dark', state: 'unavailable', long: false },
  { width: 1440, theme: 'light', state: 'unavailable', long: false },
  { width: 320, theme: 'dark', state: 'partial', long: true },
  { width: 390, theme: 'light', state: 'partial', long: true },
];

for (const { width, theme, state, long } of cases) {
  test(`${state} coverage at ${width}px in ${theme}${long ? ' with a long machine name' : ''}`, async ({ page }) => {
    const errors: string[] = [];
    page.on('pageerror', error => errors.push(error.message));
    await page.clock.setFixedTime(new Date('2026-10-23T12:00:00Z'));
    await page.setViewportSize({ width, height: 844 });
    await page.goto(
      `/tests/fixtures/user-header-preview.html?palette=jinzi&presence=${state}&theme=${theme}${long ? '&longNode' : ''}`,
    );
    const note = page.locator('.user-presence-note.warn');
    await expect(note).toBeVisible();
    await expect(note).toContainText('在线来源统计不完整：3 台入口机器中');
    await expect(note).toContainText(/上次更新 13\s*分钟前/);
    await expect(note).toContainText('尚未上报节点（尚未收到统计）');
    await page.evaluate(() => document.fonts.ready);
    await expect(note).toHaveScreenshot(`${state}-${width}-${theme}${long ? '-long' : ''}.png`);
    const layout = await note.evaluate(element => {
      const ago = element.querySelector('.ago')!;
      const prefix = ago.previousSibling!;
      const range = document.createRange();
      range.setStart(prefix, prefix.textContent!.lastIndexOf('上次更新'));
      range.setEnd(prefix, prefix.textContent!.length);
      const before = [...range.getClientRects()].filter(rect => rect.width > 0).at(-1)!;
      const time = ago.getBoundingClientRect();
      return {
        prefix: { top: before.top, right: before.right },
        time: { top: time.top, left: time.left },
        lineHeight: Number.parseFloat(getComputedStyle(element).lineHeight),
        overflow: element.scrollWidth > element.clientWidth,
      };
    });
    // The relative time follows its prefix on the same or next text line, never a separate column.
    const lineOffset = layout.time.top - layout.prefix.top;
    expect(lineOffset).toBeGreaterThanOrEqual(-1);
    expect(lineOffset).toBeLessThanOrEqual(layout.lineHeight + 1);
    if (Math.abs(lineOffset) <= 1) expect(layout.time.left).toBeGreaterThanOrEqual(layout.prefix.right - 1);
    expect(layout.overflow).toBe(false);
    expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
    expect(errors).toEqual([]);
  });
}
