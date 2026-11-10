import { expect, test, type Page } from '@playwright/test';

// 首次上线清单：右下角常驻、不加遮罩。展开时列出五步与当前一步的操作；到了当前一步的生产页面收成
// 胶囊，并抬到页面贴底操作条之上，不遮挡主按钮。
async function open(page: Page, phase: string, theme: string, width: number, hash = '') {
  await page.clock.setFixedTime(new Date('2026-11-02T10:00:00Z'));
  await page.setViewportSize({ width, height: width < 600 ? 844 : 900 });
  await page.goto(`/tests/fixtures/onboarding-preview.html?phase=${phase}&theme=${theme}${hash}`);
  await expect(page.locator('.onb-dock')).toBeVisible();
  await page.evaluate(() => document.fonts.ready);
}

const checklist = (page: Page) => page.getByRole('region', { name: '首次上线' });

async function expectFooterGap(page: Page) {
  await expect
    .poll(() =>
      page.evaluate(() => {
        const pill = document.querySelector('.onb-dock')?.getBoundingClientRect();
        const footer = document.querySelector('.fg-view .pv-foot')?.getBoundingClientRect();
        return pill && footer ? Math.round(footer.top - pill.bottom) : -1;
      }),
    )
    .toBe(8);
}

for (const width of [1440, 390])
  for (const theme of ['light', 'dark']) {
    test(`checklist beside the machine page ${width} ${theme}`, async ({ page }) => {
      await open(page, 'chain', theme, width);
      await expect(page.getByRole('dialog')).toHaveCount(0);
      await expect(checklist(page).getByRole('list', { name: '首次上线步骤' }).locator('> li')).toHaveCount(5);
      await expect(checklist(page).getByRole('button', { name: '创建线路' })).toBeVisible();
      // 不遮挡页面：机器页的主按钮仍可点击。
      await expect(page.getByRole('button', { name: '＋ 纳管机器' })).toBeVisible();
      await expect(page.locator('.onb-dock')).toHaveScreenshot(`onboarding-chain-${width}-${theme}.png`);
    });
  }

const PHASES: [phase: string, current: string][] = [
  ['user', '创建第一个用户'],
  ['publish', '发布配置'],
  ['activate', '配置上线中'],
  ['failed', '发布没有完成'],
  ['verify', '端到端拨测'],
  ['complete', '第一条线路已经可用'],
];

for (const [phase, current] of PHASES)
  test(`checklist ${phase} step`, async ({ page }) => {
    await open(page, phase, 'light', 1440);
    await expect(checklist(page).getByText(current, { exact: true }).first()).toBeVisible();
    if (phase === 'verify') await expect(checklist(page).getByRole('button', { name: '拨测' })).toBeEnabled();
    await expect(page.locator('.onb-dock')).toHaveScreenshot(`onboarding-${phase}-1440-light.png`);
  });

for (const width of [1440, 390])
  test(`pill rises above the chain wizard footer ${width}`, async ({ page }) => {
    await open(page, 'chain', 'light', width, '#/nodes/chain/hk-01');
    await expect(page.getByRole('heading', { name: '新建链', exact: true })).toBeVisible();
    const dock = page.locator('.onb-dock');
    await expect(dock).toHaveAttribute('data-state', 'pill');
    await expect(checklist(page)).toHaveCount(0);
    await expect(dock).toContainText('在本页选好协议后「加入草稿」');
    const submit = page.getByRole('button', { name: '加入草稿', exact: true });
    await expect(submit).toBeVisible();
    // 抬升后胶囊底边与操作条顶边固定相隔 8px；等它收敛到这个位置再截图。
    await expectFooterGap(page);
    const viewport = page.viewportSize()!;
    await expect(page).toHaveScreenshot(`onboarding-wizard-pill-${width}.png`, {
      clip: { x: 0, y: viewport.height - 200, width: viewport.width, height: 200 },
    });
  });

for (const width of [1440, 390])
  test(`pill settles after the paper entrance animation ${width}`, async ({ page }) => {
    await page.emulateMedia({ reducedMotion: 'no-preference' });
    await open(page, 'chain', 'light', width, '#/nodes/chain/hk-01');
    const paper = page.locator('.fg-view .fg-sheet').filter({ has: page.locator('.pv-foot') });
    await expect(paper).toBeVisible();
    await paper.evaluate(element => Promise.all(element.getAnimations().map(animation => animation.finished)));
    await expectFooterGap(page);
  });

test.describe('touch viewport', () => {
  test.use({ hasTouch: true, isMobile: true });

  test('pill clears the footer with a bottom safe area', async ({ page }) => {
    await page.addInitScript(() => {
      document.addEventListener('DOMContentLoaded', () => {
        document.documentElement.style.setProperty('--safe-bottom', '34px');
      });
    });
    await open(page, 'chain', 'light', 390, '#/nodes/chain/hk-01');
    await expectFooterGap(page);
    await expect(page).toHaveScreenshot('onboarding-wizard-pill-safe-area-390.png', {
      clip: { x: 0, y: 644, width: 390, height: 200 },
    });
  });
});
