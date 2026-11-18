import type { Page } from '@playwright/test';

/** CSS background images can still show the inline PNG preview after fonts and layout are ready. */
export async function waitForVisualAssets(page: Page) {
  await page.evaluate(async () => {
    await document.fonts.ready;
    const flag = document.querySelector('.geo-flag');
    if (!flag) return;
    const source = getComputedStyle(flag).backgroundImage.match(/url\("([^"]+)"\)/)?.[1];
    if (!source) throw new Error('Flag sprite has no image source');
    const image = new Image();
    image.src = source;
    await image.decode();
  });
}
