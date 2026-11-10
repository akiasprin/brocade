import { expect, type Page } from '@playwright/test';

export async function openBoardInstance(page: Page, width: number) {
  if (width <= 820) {
    await page.locator('.cz-phone-row').click();
  } else {
    await page.locator('[data-pill="cvm-alice"]').click();
    await page.locator('.cz-inspector').getByRole('button', { name: '详情', exact: true }).click();
  }
  await expect(page).toHaveURL(/#\/cvm\/instance\/cvm-alice$/);
}
