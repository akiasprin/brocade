import { expect, test, type Page } from '@playwright/test';

async function openTerminal(
  page: Page,
  width = 1440,
  theme = 'dark',
  role = 'system-admin',
  output = 'root@cvm:~# printf "你好，CVM"\r\n你好，CVM\r\nroot@cvm:~# ',
) {
  const input: string[] = [];
  let disconnected = false;
  await page.routeWebSocket('**/cvm/*/terminal/*/stream', ws => {
    ws.onMessage(message => {
      if (typeof message === 'string') {
        const command = JSON.parse(message);
        input.push(command.type ?? 'attach');
        if (command.ticket) {
          const id = 'd'.repeat(64);
          ws.send(
            JSON.stringify({
              type: 'session',
              id,
              cols: 80,
              rows: 24,
              sessions: [{ id, running: true, history_bytes: 0, history_truncated: false }],
            }),
          );
          ws.send(JSON.stringify({ type: 'ready' }));
          ws.send(JSON.stringify({ type: 'replay', active: true }));
          ws.send(Buffer.from(output));
          ws.send(JSON.stringify({ type: 'replay', active: false }));
        }
      } else input.push(message.toString('utf8'));
    });
    ws.onClose(() => {
      disconnected = true;
    });
  });
  await page.setViewportSize({ width, height: 900 });
  await page.goto(`/tests/fixtures/cvm/preview.html?scene=detail&theme=${theme}&role=${role}`);
  await page.getByRole('button', { name: '终端', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: '终端 · home-workspace' });
  await expect(dialog.getByRole('status')).toHaveText('已连接');
  await expect.poll(() => input.includes('ack')).toBe(true);
  await page.evaluate(() => document.fonts.ready);
  return { dialog, input, disconnected: () => disconnected };
}

test('closing the layer detaches and reopening resumes while ending is explicit', async ({ page }) => {
  const requests = () =>
    page.evaluate(() => (window as unknown as { __cvmMutations: Record<string, unknown>[] }).__cvmMutations);
  const { dialog, input } = await openTerminal(page);
  await expect(dialog.getByRole('tab', { name: '会话 1' })).toHaveAttribute('aria-selected', 'true');
  await dialog.locator('.cvm-terminal-tools').getByRole('button', { name: '关闭', exact: true }).click();
  await expect(dialog).toBeHidden();
  expect(input).not.toContain('close');
  await page.getByRole('button', { name: '终端', exact: true }).click();
  await expect(dialog.getByRole('status')).toHaveText('已连接');
  await expect(dialog.getByRole('tab', { name: '会话 1' })).toHaveAttribute('aria-selected', 'true');
  expect(await requests()).toEqual([{}, {}]);
  await dialog.getByRole('button', { name: '终端会话操作' }).click();
  await page.getByRole('menuitem', { name: /^结束会话/ }).click();
  const confirm = page.getByRole('dialog', { name: '结束终端会话' });
  await expect(confirm).toBeVisible();
  expect(input).not.toContain('close');
  await confirm.getByRole('button', { name: '结束会话', exact: true }).click();
  await expect.poll(() => input.includes('close')).toBe(true);
});

test('new sessions send an explicit stable creation identity', async ({ page }) => {
  const requests = () =>
    page.evaluate(() => (window as unknown as { __cvmMutations: Record<string, unknown>[] }).__cvmMutations);
  const { dialog } = await openTerminal(page);
  await dialog.getByRole('button', { name: '新建会话', exact: true }).click();
  await expect.poll(async () => (await requests()).length).toBe(2);
  expect((await requests())[1].new_session).toBe(true);
  expect((await requests())[1].terminal_id).toMatch(/^[a-f0-9]{64}$/);
});

test('paste button sends clipboard text through the active terminal', async ({ page }) => {
  await page.addInitScript(() =>
    Object.defineProperty(navigator, 'clipboard', {
      configurable: true,
      value: { readText: async () => 'printf "你好 clipboard"\n' },
    }),
  );
  const { dialog, input } = await openTerminal(page);
  await dialog.getByRole('button', { name: '粘贴', exact: true }).click();
  await expect.poll(() => input.join('').includes('你好 clipboard')).toBe(true);
  await expect(dialog.getByRole('button', { name: '粘贴', exact: true })).toContainText('已粘贴');
});

test('clipboard denial offers manual paste without disconnecting', async ({ page }) => {
  await page.addInitScript(() =>
    Object.defineProperty(navigator, 'clipboard', {
      configurable: true,
      value: {
        readText: async () => {
          throw new DOMException('denied', 'NotAllowedError');
        },
      },
    }),
  );
  const { dialog, input } = await openTerminal(page, 390, 'light');
  await dialog.getByRole('button', { name: '粘贴', exact: true }).click();
  const paste = page.getByRole('dialog', { name: '粘贴到终端' });
  await expect(paste).toBeVisible();
  await paste.getByRole('textbox', { name: '粘贴内容' }).fill('printf "手动粘贴"\n');
  await expect(paste).toHaveScreenshot('terminal-paste-390-light.png');
  await paste.getByRole('button', { name: '粘贴到终端', exact: true }).click();
  await expect.poll(() => input.join('').includes('手动粘贴')).toBe(true);
  await expect(paste).toBeHidden();
  await expect(dialog.getByRole('status')).toHaveText('已连接');
});

test('normal keyboard paste remains available and oversized paste is rejected', async ({ page }) => {
  const { dialog, input } = await openTerminal(page);
  const area = page.locator('.xterm-helper-textarea');
  await area.evaluate(element => {
    const data = new DataTransfer();
    data.setData('text/plain', 'keyboard-paste');
    element.dispatchEvent(new ClipboardEvent('paste', { clipboardData: data, bubbles: true, cancelable: true }));
  });
  await expect.poll(() => input.join('').includes('keyboard-paste')).toBe(true);
  await area.evaluate(element => {
    const data = new DataTransfer();
    data.setData('text/plain', 'TOO-LARGE'.repeat(5000));
    element.dispatchEvent(new ClipboardEvent('paste', { clipboardData: data, bubbles: true, cancelable: true }));
  });
  await expect(dialog.getByRole('alert')).toContainText('32 KiB');
  expect(input.join('')).not.toContain('TOO-LARGE');
  await expect(dialog.getByRole('status')).toHaveText('已连接');
});

test('late clipboard results cannot enter a replacement connection', async ({ page }) => {
  await page.addInitScript(() =>
    Object.defineProperty(navigator, 'clipboard', {
      configurable: true,
      value: {
        readText: () =>
          new Promise<string>(resolve => {
            (window as unknown as { resolvePaste: (text: string) => void }).resolvePaste = resolve;
          }),
      },
    }),
  );
  const { dialog, input } = await openTerminal(page);
  await dialog.getByRole('button', { name: '粘贴', exact: true }).click();
  await dialog.getByRole('button', { name: '新建会话', exact: true }).click();
  await expect(dialog.getByRole('status')).toHaveText('已连接');
  await page.evaluate(() =>
    (window as unknown as { resolvePaste: (text: string) => void }).resolvePaste('SHOULD-NOT-BE-SENT'),
  );
  await expect(dialog.getByRole('button', { name: '粘贴', exact: true })).toBeEnabled();
  expect(input.join('')).not.toContain('SHOULD-NOT-BE-SENT');
});

for (const width of [1440, 390, 320])
  for (const theme of ['dark', 'light']) {
    test(`terminal ${width} ${theme}`, async ({ page }) => {
      const { dialog, input, disconnected } = await openTerminal(page, width, theme);
      expect(await dialog.locator('.cvm-terminal-dialog').evaluate(el => el.scrollWidth > el.clientWidth)).toBe(false);
      await expect(dialog).toHaveScreenshot(`terminal-${width}-${theme}.png`, { caret: 'hide' });
      await page.locator('.xterm-helper-textarea').press('Escape');
      await expect(dialog).toBeVisible();
      await page.locator('.xterm-helper-textarea').focus();
      await page.keyboard.insertText('你好');
      await expect.poll(() => input.join('').includes('你好')).toBe(true);
      await dialog.getByRole('button', { name: 'Tab', exact: true }).click();
      await expect.poll(() => input.includes('\t')).toBe(true);
      await page.locator('.xterm-helper-textarea').press('Control+c');
      await expect.poll(() => input.includes('\u0003')).toBe(true);
      await dialog.getByRole('checkbox', { name: '辅助阅读' }).check();
      expect(input.filter(value => value === 'attach')).toHaveLength(1);
      await dialog.getByRole('button', { name: '全屏', exact: true }).click();
      await expect(dialog.getByRole('button', { name: '还原', exact: true })).toBeVisible();
      await page.setViewportSize({ width, height: 760 });
      await expect.poll(() => input.filter(value => value === 'resize').length).toBeGreaterThan(1);
      await dialog.locator('.cvm-terminal-tools').getByRole('button', { name: '关闭', exact: true }).click();
      await expect(dialog).toBeHidden();
      await expect.poll(disconnected).toBe(true);
    });
  }

test('the cursor line reflows without losing text on a narrow viewport', async ({ page }) => {
  const text = 'BEGIN-abcdefghijklmnopqrstuvwxyz-abcdefghijklmnopqrstuvwxyz-END';
  const { dialog } = await openTerminal(page, 1440, 'dark', 'system-admin', text);
  const rows = dialog.locator('.xterm-rows');
  await expect(rows).toContainText(text);
  await page.setViewportSize({ width: 390, height: 900 });
  await expect.poll(async () => (await rows.textContent())?.replaceAll('\n', '')).toContain(text);
  await page.setViewportSize({ width: 1440, height: 900 });
  await expect(rows).toContainText(text);
});

test('read only cannot open a terminal', async ({ page }) => {
  await page.goto('/tests/fixtures/cvm/preview.html?scene=detail&role=readonly');
  await expect(page.getByRole('button', { name: '终端', exact: true })).toHaveCount(0);
});

test('connection failure offers an explicit retry', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 800 });
  await page.routeWebSocket('**/cvm/*/terminal/*/stream', ws => {
    ws.onMessage(() => ws.send(JSON.stringify({ type: 'error', message: 'Guest 终端不可用或实例状态已改变' })));
  });
  await page.goto('/tests/fixtures/cvm/preview.html?scene=detail&theme=light');
  await page.getByRole('button', { name: '终端', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: '终端 · home-workspace' });
  await expect(dialog.getByRole('alert')).toHaveText('Guest 终端不可用或实例状态已改变');
  await expect(dialog.getByRole('button', { name: '重新连接' })).toBeEnabled();
  await expect(dialog).toHaveScreenshot('terminal-failed-390-light.png');
});

for (const width of [1440, 390, 320])
  for (const theme of ['dark', 'light']) {
    test(`WebShell management ${width} ${theme}`, async ({ page }) => {
      await page.setViewportSize({ width, height: 900 });
      await page.goto(`/tests/fixtures/cvm/preview.html?scene=terminal-management&theme=${theme}`);
      await page.getByRole('tab', { name: '宿主', exact: true }).click();
      const dialog = page.getByRole('list', { name: '宿主' });
      const toggle = dialog.getByRole('switch').first();
      await expect(toggle).toBeChecked();
      await dialog.locator('.switch-ui').first().click();
      await expect(toggle).not.toBeChecked();
      await expect(dialog.getByText('已关闭', { exact: false }).first()).toBeVisible();
      await expect(dialog.getByRole('switch')).toHaveCount(2);
      await expect(dialog.getByText(/运行栈待升级/)).toBeVisible();
      await page.evaluate(() => document.fonts.ready);
      await expect(dialog).toHaveScreenshot(`terminal-management-${width}-${theme}.png`);
      expect(await dialog.evaluate(el => el.scrollWidth > el.clientWidth)).toBe(false);
      await toggle.focus();
      await page.keyboard.press('Space');
      await expect(toggle).toBeChecked();
      await page.getByRole('tab', { name: '组合台', exact: true }).click();
      await expect(dialog).toBeHidden();
      expect(await page.evaluate(() => (window as unknown as { __cvmMutations: unknown[] }).__cvmMutations)).toEqual([
        { enabled: false, expected_revision: 1 },
        { enabled: true, expected_revision: 2 },
      ]);
    });
  }

test('users cannot manage WebShell policy', async ({ page }) => {
  await page.goto('/tests/fixtures/cvm/preview.html?role=user');
  await expect(page.getByRole('button', { name: /创建容器/ })).toBeVisible();
  await expect(page.getByRole('button', { name: 'WebShell 管理', exact: true })).toHaveCount(0);
});

test.describe('touch terminal controls', () => {
  test.use({ hasTouch: true });
  test('mouse compatibility events do not paint hover states on a touch device', async ({ page }) => {
    const { dialog } = await openTerminal(page, 390, 'light');
    expect(await page.evaluate(() => matchMedia('(hover: none)').matches)).toBe(true);
    const button = dialog.getByRole('button', { name: '全屏', exact: true });
    await button.hover();
    expect(await button.evaluate(el => el.matches(':hover'))).toBe(true);
    await expect(button).toHaveCSS('background-color', 'rgba(0, 0, 0, 0)');
  });
});
