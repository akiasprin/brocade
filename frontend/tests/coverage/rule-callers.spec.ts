import { test, expect } from '@playwright/test';
for (const mode of ['forward', 'listener'])
  for (const width of [390, 1440]) {
    test(`${mode} Mux applies, stages and survives document reload at ${width}`, async ({ page }) => {
      await page.setViewportSize({ width, height: 844 });
      await page.goto(
        `/tests/fixtures/rule-coverage-preview.html?mode=${mode}&theme=${width === 390 ? 'dark' : 'light'}`,
      );
      const configure = page.getByRole('button', { name: '配置参数', exact: true });
      await configure.click();
      const drawer = page.locator('.mux-config-drawer'),
        field = drawer.getByRole('spinbutton', { name: '复用流数量', exact: true });
      await field.fill('7');
      await expect(drawer.getByRole('button', { name: '应用', exact: true })).toBeInViewport({ ratio: 1 });
      await expect(drawer).toHaveScreenshot(`${mode}-${width}-edited.png`);
      await drawer.getByRole('button', { name: '应用', exact: true }).click();
      await expect(drawer).toHaveCount(0);
      await page.getByRole('button', { name: '保存到草稿', exact: true }).click();
      await expect
        .poll(() =>
          page.evaluate(() =>
            Reflect.get(window, 'coverageDraftOps')().some(
              (op: any) =>
                op.op === 'put_step' &&
                op.node_id === 'source' &&
                op.step.rules.some((r: any) => r.a.pool?.v?.concurrency === 7),
            ),
          ),
        )
        .toBe(true);
      await page.reload();
      await page.getByRole('button', { name: '配置参数', exact: true }).click();
      await expect(page.getByRole('spinbutton', { name: '复用流数量', exact: true })).toHaveValue('7');
      await expect(page.locator('.mux-config-drawer')).toHaveScreenshot(`${mode}-${width}-reopened.png`);
    });
    test(`${mode} readonly Mux at ${width}`, async ({ page }) => {
      await page.setViewportSize({ width, height: 844 });
      await page.goto(`/tests/fixtures/rule-coverage-preview.html?mode=${mode}&role=readonly`);
      const view = page.getByRole('button', { name: '查看参数', exact: true });
      if (mode === 'listener') {
        await expect(page.locator('.listener-reference-panel')).toHaveScreenshot(
          `listener-${width}-readonly-entry.png`,
        );
        test.fail(true, 'UI-012: 引用监听的查看按钮继承 fieldset disabled，无法进入只读参数层');
        await expect(view).toBeEnabled();
      }
      await view.click();
      const drawer = page.locator('.mux-config-drawer');
      await expect(drawer.getByRole('button', { name: '应用', exact: true })).toHaveCount(0);
      for (const input of await drawer.locator('input').all()) await expect(input).toBeDisabled();
      await expect(drawer).toHaveScreenshot(`${mode}-${width}-readonly.png`);
      await drawer.getByRole('button', { name: '取消', exact: true }).click();
      await expect(drawer).toHaveCount(0);
    });
  }
for (const mode of ['forward', 'listener'])
  test(`${mode} long unbroken title keeps header close reachable`, async ({ page }) => {
    await page.setViewportSize({ width: 320, height: 740 });
    await page.goto(`/tests/fixtures/rule-coverage-preview.html?mode=${mode}&longName`);
    await page.getByRole('button', { name: '配置参数', exact: true }).click();
    const drawer = page.locator('.mux-config-drawer');
    await expect(page.getByRole('dialog')).toHaveScreenshot(`${mode}-long-title.png`);
    test.fail(true, 'UI-011: Mux 标题的 flex 最小内容宽度把关闭按钮推出视口；本轮仅记录');
    await expect(drawer.locator('header button')).toBeInViewport({ ratio: 1 });
    expect(await drawer.evaluate(e => e.scrollWidth <= e.clientWidth)).toBe(true);
  });
