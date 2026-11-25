import { expect, type Page } from '@playwright/test';

export async function openListInstance(page: Page) {
  const list = page.getByRole('table', { name: '按宿主分组的容器', exact: true });
  await list.getByRole('button', { name: 'home-workspace', exact: true }).click();
  await expect(page).toHaveURL(/#\/cvm\/instance\/cvm-alice$/);
}
