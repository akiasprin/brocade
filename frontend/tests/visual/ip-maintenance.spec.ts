import { test, expect, type Locator, type Page } from '@playwright/test';

test.use({ screenshot: 'only-on-failure' });

const fixture = '/tests/fixtures/ip-maintenance-preview.html';
async function open(page: Page, query = '') {
  await page.clock.setFixedTime(new Date('2026-11-14T00:00:00Z'));
  await page.goto(`${fixture}?${query}`);
  const careTab = page.getByRole('tab', { name: '养护', exact: true });
  await expect(careTab).toBeVisible();
  if ((await careTab.getAttribute('aria-selected')) !== 'true') {
    await expect(page.getByRole('tab', { name: '配置', exact: true })).toHaveAttribute('aria-selected', 'true');
    await expect(page.locator('.care-config')).toHaveCount(0);
    await careTab.click();
  }
  await expect(page.getByRole('tab', { name: '养护', exact: true })).toHaveAttribute('aria-selected', 'true');
  const card = page.locator('.care-config');
  await expect(card).toHaveAttribute('aria-busy', 'false');
  await card.scrollIntoViewIfNeeded();
  await page.evaluate(() => document.fonts.ready);
  return card;
}

function automatic(card: Locator, enabled: boolean) {
  return card
    .getByRole('group', { name: '自动养护' })
    .getByRole('button', { name: enabled ? '开启' : '关闭', exact: true });
}

async function fits(page: Page, card: Locator) {
  await expect(card.locator('.care-config-line')).toHaveCount(2);
  for (const line of await card.locator('.care-config-line').all()) {
    const positions = await line.evaluate(element => {
      const control = element.firstElementChild!.getBoundingClientRect();
      const trail = element.querySelector('.care-config-trail')!.getBoundingClientRect();
      return {
        centerOffset: Math.abs(control.y + control.height / 2 - trail.y - trail.height / 2),
        controlRight: control.right,
        trailLeft: trail.left,
        trailRight: trail.right,
        lineRight: element.getBoundingClientRect().right,
        overflow: element.scrollWidth - element.clientWidth,
      };
    });
    expect(positions.centerOffset).toBeLessThanOrEqual(1);
    expect(positions.trailLeft).toBeGreaterThan(positions.controlRight);
    expect(positions.trailRight).toBeLessThanOrEqual(positions.lineRight + 1);
    expect(positions.overflow).toBeLessThanOrEqual(1);
  }
  expect(await page.evaluate(() => document.documentElement.scrollWidth - innerWidth)).toBeLessThanOrEqual(1);
}

for (const browserName of ['chromium', 'webkit'] as const) {
  const browserTest = test.extend({ browserName });
  for (const [width, theme] of [
    [1440, 'light'],
    [1000, 'dark'],
    [390, 'light'],
    [320, 'dark'],
  ] as const)
    for (const scene of ['waiting', 'requesting']) {
      browserTest(`F trail ${browserName} ${width} ${theme} ${scene}`, async ({ page }) => {
        const errors: string[] = [];
        page.on('pageerror', error => errors.push(error.message));
        await page.setViewportSize({ width, height: 1000 });
        const card = await open(page, `theme=${theme}&scene=${scene}`);
        await expect(card.locator(':scope > header')).not.toContainText(/记录|策略/);
        await expect(card.locator('.care-location-progress, .care-identity')).toHaveCount(0);
        await expect(card.locator('.row > .k')).toHaveText(['自动养护', '状态']);
        await expect(page.getByRole('tabpanel', { name: '养护', exact: true })).toContainText('IP 养护');
        await expect(card.getByRole('status')).toContainText(scene === 'waiting' ? '下一轮' : '正在养护');
        await fits(page, card);
        await expect(card).toHaveScreenshot(`f-trail-${browserName}-${width}-${theme}-${scene}.png`);
        if (width === 1440 && scene === 'waiting')
          await expect(page).toHaveScreenshot(`f-config-context-${browserName}.png`);
        expect(errors).toEqual([]);
      });
    }

  for (const width of [1440, 390, 320]) {
    browserTest(`F dialogs retain records flags and policy edits ${browserName} ${width}`, async ({ page }) => {
      await page.setViewportSize({ width, height: 1000 });
      const card = await open(page, `theme=${width === 1440 ? 'light' : 'dark'}&scene=waiting`);
      const recordsButton = card.getByRole('button', { name: '记录', exact: true });
      await recordsButton.click();
      const records = page.getByRole('dialog', { name: '养护记录' });
      await expect(records).toHaveClass(/modal/);
      await expect(records.locator('.care-record-count')).not.toHaveText('—');
      await expect(records.locator('.care-records-usage')).toContainText('1,500 次');
      await expect(records.locator('.care-round-geo .geo-flag').first()).toBeVisible();
      await expect(records.locator('.care-round-result').filter({ hasText: '冷却' })).toHaveText('冷却：限流');
      for (const content of await records.locator('.care-sheet-body, .care-records-chart, .care-rounds button').all()) {
        expect(await content.evaluate(element => element.scrollWidth - element.clientWidth)).toBeLessThanOrEqual(1);
      }
      await expect(records.locator('.care-all')).toHaveScreenshot(`f-records-${browserName}-${width}.png`);
      await records.locator('.care-rounds button').first().click();
      const round = page.getByRole('dialog', { name: '养护轮次' });
      await expect(round).toBeVisible();
      await expect(round.locator('.care-sheet-evidence .care-evidence li')).toHaveCount(3);
      await page.keyboard.press('Escape');
      await expect(records).toBeVisible();
      await page.keyboard.press('Escape');
      await expect(recordsButton).toBeFocused();

      const policyButton = card.getByRole('button', { name: '策略', exact: true });
      await policyButton.click();
      const policy = page.getByRole('dialog', { name: '养护策略' });
      await expect(policy).toHaveClass(/modal/);
      await policy.getByRole('combobox', { name: '执行频率' }).click();
      await page.getByRole('option', { name: /每 30 分钟/ }).click();
      await expect(policy.getByText('有未保存的改动')).toBeVisible();
      await expect(policy.locator('.care-policy-sheet')).toHaveScreenshot(`f-policy-${browserName}-${width}.png`);
      await policy.getByRole('button', { name: '保存', exact: true }).click();
      await expect(policy.getByText('等待节点确认', { exact: true })).toBeVisible();
      await expect(policy.getByText('已应用', { exact: true })).toBeVisible();
      await policy.getByRole('button', { name: /行为参数/ }).click();
      await expect(page.getByRole('dialog', { name: '行为参数' })).toBeVisible();
      await page.keyboard.press('Escape');
      await expect(policy).toBeVisible();
      await page.keyboard.press('Escape');
      await expect(policyButton).toBeFocused();
      await expect(card).toContainText('每 30 分钟一轮');
    });
  }

  browserTest(`F readonly configuration keeps inspection available ${browserName}`, async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 1000 });
    const card = await open(page, 'role=public&theme=dark');
    await expect(automatic(card, true)).toBeDisabled();
    await expect(card.getByRole('button', { name: '停止', exact: true })).toBeDisabled();
    await page.getByRole('tab', { name: '解锁', exact: true }).focus();
    await page.keyboard.press('ArrowRight');
    await expect(page.getByRole('tab', { name: '养护', exact: true })).toBeFocused();
    await card.getByRole('button', { name: '策略', exact: true }).click();
    const policy = page.getByRole('dialog', { name: '养护策略' });
    await expect(policy.getByRole('spinbutton', { name: '每日请求上限' })).toBeDisabled();
    await expect(policy.getByRole('combobox', { name: '执行频率' })).toBeDisabled();
    await page.keyboard.press('Escape');
    await card.getByRole('button', { name: '记录', exact: true }).click();
    await expect(page.getByRole('dialog', { name: '养护记录' })).toBeVisible();
    expect(await page.evaluate(() => (window as Window & { __maintenanceWrites: number }).__maintenanceWrites)).toBe(0);
  });

  browserTest(`F one stop waits for both dual-stack receipts ${browserName}`, async ({ page }) => {
    const card = await open(page, 'scene=dual&theme=light');
    await card.getByRole('button', { name: '停止', exact: true }).click();
    await expect(card.getByRole('status')).toHaveText('正在停止');
    await expect(card.getByRole('button', { name: '立即执行', exact: true })).toBeVisible();
    await expect(card.getByRole('status')).toContainText('下一轮');
    expect(await page.evaluate(() => (window as Window & { __maintenanceWrites: number }).__maintenanceWrites)).toBe(2);
  });

  for (const [width, theme] of [
    [1440, 'light'],
    [1000, 'dark'],
    [820, 'light'],
    [390, 'dark'],
    [320, 'light'],
  ] as const)
    for (const scene of ['dual', 'dual-idle']) {
      browserTest(`F overall dual status ${browserName} ${width} ${theme} ${scene}`, async ({ page }) => {
        await page.setViewportSize({ width, height: 1000 });
        const card = await open(page, `scene=${scene}&theme=${theme}`);
        await expect(card.getByRole('status')).toHaveCount(1);
        await expect(card.getByRole('status')).toHaveText(scene === 'dual' ? '正在养护' : '空闲');
        await expect(card.getByRole('group', { name: /IPv[46] 养护状态|查看养护出口/ })).toHaveCount(0);
        await fits(page, card);
        const styles = await page.evaluate(() => {
          const button = document.querySelector('.care-config-state > .btn')!;
          const read = (element: Element) => {
            const css = getComputedStyle(element);
            return Object.fromEntries(
              [
                'height',
                'font-family',
                'font-size',
                'font-weight',
                'line-height',
                'padding',
                'border-radius',
                'background-color',
                'border-color',
              ].map(key => [key, css.getPropertyValue(key)]),
            );
          };
          return read(button);
        });
        await page.getByRole('tab', { name: '配置', exact: true }).click();
        const calibrationStyle = await page
          .getByRole('button', { name: '校准', exact: true })
          .evaluate((element, keys) => {
            const css = getComputedStyle(element);
            return Object.fromEntries(keys.map(key => [key, css.getPropertyValue(key)]));
          }, Object.keys(styles));
        expect(styles).toEqual(calibrationStyle);
        await page.getByRole('tab', { name: '养护', exact: true }).click();
        await expect(card).toHaveAttribute('aria-busy', 'false');
        await expect(card).toHaveScreenshot(`overall-${browserName}-${width}-${theme}-${scene}.png`);
      });
    }
}

for (const [scene, status] of [
  ['offline', 'Agent 离线'],
  ['inactive', '机器已停用'],
  ['worker-stale', '执行器无回执'],
  ['legacy', '需要升级 Agent'],
  ['budget', '今日额度已用完'],
  ['cooldown', '异常冷却'],
  ['initial-legacy', '需要升级 Agent'],
] as const) {
  test(`F preserves availability and receipt state ${scene}`, async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 1000 });
    const card = await open(page, `scene=${scene}&theme=dark`);
    await expect(card.getByRole('status')).toContainText(status);
    await fits(page, card);
    await expect(card).toHaveScreenshot(`f-state-${scene}.png`);
  });
}

test('F first manual execution saves defaults without enabling automatic maintenance', async ({ page }) => {
  const card = await open(page, 'scene=initial&theme=light');
  await card.getByRole('button', { name: '立即执行', exact: true }).click();
  await expect(card.getByRole('button', { name: '停止', exact: true })).toBeVisible();
  await expect(automatic(card, false)).toHaveAttribute('aria-pressed', 'true');
});

test('F session reset retains policy and clears records without flashing an error', async ({ page }) => {
  const card = await open(page, 'scene=failed&theme=light&resetDelay');
  await expect(card.getByRole('status')).toHaveText('执行失败');
  await card.getByRole('button', { name: '策略', exact: true }).click();
  const policy = page.getByRole('dialog', { name: '养护策略' });
  await policy.getByRole('button', { name: '重置养护会话', exact: true }).click();
  const confirmation = page.getByRole('dialog', { name: '重置养护会话' });
  await confirmation.getByRole('button', { name: '确认重置', exact: true }).click();
  await expect(card.getByRole('status')).toHaveText('正在重置会话');
  await expect(policy.getByRole('combobox', { name: '执行频率' })).toHaveText(/每 20 分钟/);
  await page.keyboard.press('Escape');
  await expect(card.getByRole('status')).toHaveText('尚未开始');
  await expect(automatic(card, false)).toHaveAttribute('aria-pressed', 'true');
  await card.getByRole('button', { name: '记录', exact: true }).click();
  const records = page.getByRole('dialog', { name: '养护记录' });
  await expect(records.getByText('还没有执行记录')).toBeVisible();
  await expect(records.locator('.care-record-count')).toHaveText('0');
  await expect(page.locator('.care-config .callout.err, .care-sheet .callout.err')).toHaveCount(0);
});

test('F stop remains independent of a failed model snapshot', async ({ page }) => {
  const card = await open(page, 'scene=model-error&theme=light');
  await card.getByRole('button', { name: '停止', exact: true }).click();
  await expect(card).toContainText('本轮已中断');
});

test('retained maintenance window state opens the standalone maintenance tab', async ({ page }) => {
  const card = await open(page, 'legacyTab=care&theme=light');
  await expect(card.getByRole('heading', { name: 'IP 养护' })).toBeVisible();
});
