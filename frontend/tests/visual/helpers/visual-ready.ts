import type { Locator, Page } from '@playwright/test';

/**
 * The workspace scrolls inside `.fg-desk`, not the document, so the part of an element below the desk's
 * visible height is never painted into an element screenshot (it comes out blank). Grow the viewport to the
 * element's bottom edge first so tall panels are captured whole.
 */
export async function fitViewportTo(page: Page, target: Locator) {
  const size = page.viewportSize();
  if (!size) throw new Error('fitViewportTo needs a fixed viewport');
  const bottom = await target.evaluate(el => {
    const desk = el.closest('.fg-desk');
    return el.getBoundingClientRect().bottom + (desk?.scrollTop ?? 0);
  });
  const height = Math.ceil(bottom) + 24;
  if (height > size.height) await page.setViewportSize({ width: size.width, height });
}

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
