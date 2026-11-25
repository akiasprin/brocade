import { expect, test, type Page, type WebSocketRoute } from '@playwright/test';
import { waitForVisualAssets } from '../helpers/visual-ready';
import { HTTP_ORIGIN, serveHttpFixtures, expectInsecureHttp } from '../helpers/http-origin';

const pageErrors = new Map<Page, string[]>();
test.beforeEach(async ({ context }) => {
  pageErrors.clear();
  const capture = (page: Page) => {
    const errors: string[] = [];
    pageErrors.set(page, errors);
    page.on('pageerror', error => errors.push(error.message));
  };
  context.pages().forEach(capture);
  context.on('page', capture);
});
test.afterEach(async () => {
  for (const errors of pageErrors.values()) expect(errors).toEqual([]);
});

const FIRST = 'd'.repeat(64),
  SECOND = 'e'.repeat(64);
const output =
  Array.from({ length: 120 }, (_, i) => `service-${i}  running   payments ready`).join('\r\n') + '\r\nroot@cvm:~# ';
async function installTransport(page: Page, { text = output, bracketed = false, failure = '', denyNew = false } = {}) {
  const input: string[] = [];
  const urls: string[] = [];
  const sockets = new Map<string, WebSocketRoute>();
  const sessions = [FIRST, SECOND].map(id => ({ id, running: true, history_bytes: 2048, history_truncated: false }));
  let attaches = 0,
    detaches = 0;
  await page.routeWebSocket('**/cvm/*/terminal/*/stream', ws => {
    urls.push(ws.url());
    let id = FIRST;
    ws.onMessage(async message => {
      if (typeof message !== 'string') {
        input.push(message.toString('utf8'));
        return;
      }
      const command = JSON.parse(message);
      input.push(command.type ?? 'attach');
      if (command.ticket) {
        attaches++;
        if (failure) {
          ws.send(JSON.stringify({ type: 'error', message: failure }));
          return;
        }
        const request = await page.evaluate(() =>
          (
            window as unknown as { __cvmMutations: { terminal_id?: string; new_session?: boolean }[] }
          ).__cvmMutations.at(-1),
        );
        if (denyNew && request?.new_session) {
          ws.send(JSON.stringify({ type: 'error', message: '宿主会话名额已用完' }));
          return;
        }
        id = request?.terminal_id ?? FIRST;
        if (request?.new_session && !sessions.some(s => s.id === id))
          sessions.push({ id, running: true, history_bytes: 0, history_truncated: false });
        sockets.set(id, ws);
        ws.send(JSON.stringify({ type: 'session', id, sessions, cols: 80, rows: 24 }));
        ws.send(JSON.stringify({ type: 'ready' }));
        ws.send(JSON.stringify({ type: 'replay', active: true }));
        ws.send(Buffer.from((bracketed ? '\x1b[?2004h' : '') + '\x1b]2;/srv/app\x07' + text));
        ws.send(JSON.stringify({ type: 'replay', active: false }));
      }
      if (command.type === 'close') {
        const session = sessions.find(s => s.id === id);
        if (session) session.running = false;
        ws.send(JSON.stringify({ type: 'exit', code: 0 }));
      }
    });
    ws.onClose(() => {
      detaches++;
    });
  });
  return { input, urls, sockets, sessions, attaches: () => attaches, detaches: () => detaches };
}
async function openTerminal(
  page: Page,
  width = 1440,
  theme = 'dark',
  options: Parameters<typeof installTransport>[1] = {},
  origin = '',
  rtc = false,
) {
  const errors: string[] = [];
  page.on('pageerror', e => errors.push(e.message));
  await page.clock.setFixedTime(new Date('2026-11-23T12:00:00Z'));
  const transport = await installTransport(page, options);
  await page.setViewportSize({ width, height: width < 820 ? 844 : 900 });
  await page.goto(
    `${origin}/tests/fixtures/cvm/preview.html?scene=detail&theme=${theme}&webshell=cvm-alice${rtc ? '&rtc=1' : ''}`,
  );
  const terminal = page.getByRole('main', { name: '终端 · home-workspace', exact: true });
  await expect(terminal).toBeVisible();
  if (!options.failure) await expect(terminal.getByRole('status', { exact: false }).first()).toHaveText('已连接');
  await waitForVisualAssets(page);
  return { ...transport, terminal, errors };
}
async function menu(page: Page, item: string) {
  await page.getByRole('button', { name: '更多', exact: true }).click();
  await page.getByRole('menuitem', { name: item, exact: true }).click();
}
async function nativePaste(page: Page, text: string) {
  await page.locator('.cvm-terminal-renderer:not([hidden]) .xterm-helper-textarea').evaluate((element, text) => {
    const data = new DataTransfer();
    data.setData('text/plain', text);
    element.dispatchEvent(new ClipboardEvent('paste', { clipboardData: data, bubbles: true, cancelable: true }));
  }, text);
}
async function context(page: Page, item: string) {
  await page.locator('.cvm-terminal-screen').click({ button: 'right', position: { x: 100, y: 100 } });
  await page.getByRole('menuitem', { name: item, exact: true }).click();
}

test('direct candidates require a user action and a failed attempt preserves the Shell', async ({ page }) => {
  await page.addInitScript(() => {
    let attempts = 0;
    Object.defineProperty(window, '__rtcAttempts', { get: () => attempts });
    window.RTCPeerConnection = class extends EventTarget {
      constructor() {
        super();
        attempts++;
      }
      createDataChannel() {
        return { close() {} };
      }
      async createOffer() {
        throw new DOMException('fixture blocked', 'NotAllowedError');
      }
      close() {}
    } as unknown as typeof RTCPeerConnection;
  });
  const { attaches, input } = await openTerminal(page, 1440, 'dark', {}, '', true);
  expect(await page.evaluate(() => Reflect.get(window, '__rtcAttempts'))).toBe(0);
  await menu(page, '尝试直连');
  await expect.poll(attaches).toBe(2);
  await expect(page.getByRole('status').first()).toHaveText('已连接');
  expect(await page.evaluate(() => Reflect.get(window, '__rtcAttempts'))).toBe(1);
  await page.getByRole('button', { name: '更多', exact: true }).click();
  await expect(page.locator('.cvm-terminal-info')).toContainText('Console 转发');
  await expect(page.locator('.cvm-terminal-info')).toContainText('直连暂不可用');
  expect(input.filter(value => value === 'attach')).toHaveLength(2);
  const mutations = await page.evaluate(() => Reflect.get(window, '__cvmMutations'));
  expect(mutations.at(-1)).toEqual({ terminal_id: FIRST });
});

for (const width of [1440, 390, 320])
  for (const theme of ['dark', 'light']) {
    test(`B terminal ${width} ${theme}`, async ({ page }) => {
      const { terminal, input, errors } = await openTerminal(page, width, theme);
      const screen = await terminal.locator('.cvm-terminal-screen').boundingBox();
      expect(screen?.x).toBe(0);
      expect(screen?.width).toBe(width);
      expect(screen?.y).toBe(width > 820 ? 0 : 38);
      if (width <= 820) expect((await terminal.locator('.cvm-terminal-capsule').boundingBox())!.height).toBe(38);
      expect(
        await page.evaluate(
          () => document.documentElement.scrollWidth <= innerWidth && document.scrollingElement?.scrollTop === 0,
        ),
      ).toBe(true);
      await expect(page).toHaveScreenshot(`terminal-${width}-${theme}.png`, { caret: 'hide' });
      const area = page.locator('.cvm-terminal-renderer:not([hidden]) .xterm-helper-textarea');
      await area.focus();
      await area.press('Escape');
      await area.press('Tab');
      await area.press('Control+c');
      await page.keyboard.insertText('你好');
      await expect.poll(() => input.join('')).toContain('\x1b');
      await expect.poll(() => input.join('')).toContain('\t');
      await expect.poll(() => input.join('')).toContain('\x03');
      await expect.poll(() => input.join('')).toContain('你好');
      const before = input.filter(i => i === 'resize').length;
      await page.setViewportSize({ width, height: 700 });
      await expect.poll(() => input.filter(i => i === 'resize').length).toBeGreaterThan(before);
      expect(errors).toEqual([]);
    });
  }

test('B capsule fades on input and restores on hover; terminal and page scroll independently', async ({ page }) => {
  const { terminal } = await openTerminal(page);
  const capsule = terminal.locator('.cvm-terminal-capsule');
  await page.mouse.move(300, 400);
  await page.locator('.xterm-helper-textarea').press('x');
  await expect(capsule).toHaveCSS('opacity', '0.5');
  await capsule.hover();
  await expect(capsule).toHaveCSS('opacity', '1');
  const rows = page.locator('.xterm-rows');
  await page.mouse.move(200, 400);
  await page.mouse.wheel(0, -5000);
  await expect(rows).not.toContainText('service-119');
  await page.mouse.wheel(0, 5000);
  await expect(rows).toContainText('service-119');
  expect(await page.evaluate(() => document.scrollingElement?.scrollTop)).toBe(0);
});

test('switch, new, capacity reason, rename, background output and anchored ending', async ({ page }) => {
  const { sockets, input, attaches, terminal } = await openTerminal(page);
  await page.getByRole('tab', { name: '会话 2', exact: true }).click();
  await expect.poll(attaches).toBe(2);
  await expect(terminal.getByRole('status').first()).toHaveText('已连接');
  await page.getByRole('tab', { name: '会话 1', exact: true }).click();
  expect(attaches()).toBe(2);
  sockets.get(SECOND)!.send(Buffer.from('\r\nbackground-update\r\n'));
  await expect(page.locator('.cvm-terminal-dot.unread')).toHaveCount(1);
  await page.getByRole('tab', { name: '会话 2', exact: false }).click();
  await expect(page.locator('.cvm-terminal-dot.unread')).toHaveCount(0);
  await page.getByRole('button', { name: '新建会话', exact: true }).click();
  await expect(page.getByRole('tab', { name: '会话 3', exact: true })).toHaveAttribute('aria-selected', 'true');
  await expect(terminal.getByRole('status').first()).toHaveText('已连接');
  const plus = page.getByRole('button', { name: '新建会话', exact: true });
  await expect(plus).toHaveAttribute('aria-disabled', 'true');
  await plus.click({ force: true });
  await expect(page.getByText('宿主会话名额已用完，请先结束一个会话。')).toBeVisible();
  const tab = page.getByRole('tab', { name: '会话 1', exact: true });
  await tab.dblclick();
  await page.getByRole('textbox', { name: '会话名称' }).fill('deploy');
  await page.keyboard.press('Enter');
  await expect(page.getByRole('tab', { name: 'deploy', exact: true })).toBeVisible();
  await page.getByRole('button', { name: '结束会话 3', exact: true }).click();
  const confirm = page.getByRole('dialog', { name: '结束终端会话' });
  const cb = await confirm.locator('.dialog-surface').boundingBox();
  const tb = await page.getByRole('tab', { name: '会话 3', exact: true }).boundingBox();
  expect(cb!.y).toBeGreaterThan(tb!.y + tb!.height);
  expect(input).not.toContain('close');
  await confirm.getByRole('button', { name: '结束会话', exact: true }).click();
  await expect(page.locator('.cvm-terminal-dot.ended')).toHaveCount(1);
  await expect(terminal.getByRole('status').first()).toHaveText('已连接');
  await page.getByRole('tab', { name: '会话 3', exact: true }).click();
  await expect(terminal.getByRole('status').first()).toHaveText('会话已结束');
  await expect(page.getByRole('button', { name: '导出记录' })).toBeVisible();
  expect(input).toContain('close');
});

test('rejected creations do not consume confirmed slots or retain unbounded renderers', async ({ page }) => {
  const { attaches } = await openTerminal(page, 1440, 'dark', { denyNew: true });
  for (let i = 0; i < 12; i++) {
    await page.getByRole('button', { name: '新建会话', exact: true }).click();
    await expect.poll(attaches).toBe(i + 2);
    await expect(page.getByRole('alert')).toContainText('宿主会话名额已用完');
  }
  await expect(page.getByRole('tab')).toHaveCount(10);
  await page.getByRole('button', { name: '更多', exact: true }).click();
  await expect(page.locator('.cvm-terminal-info')).toContainText('本实例 2');
});

test('tabs support keyboard navigation and persistent browser names', async ({ page }) => {
  await openTerminal(page);
  await page.getByRole('tab', { name: '会话 1', exact: true }).focus();
  await page.keyboard.press('ArrowRight');
  await expect(page.getByRole('tab', { name: '会话 2', exact: true })).toBeFocused();
  await page.keyboard.press('Enter');
  await expect(page.getByRole('tab', { name: '会话 2', exact: true })).toHaveAttribute('aria-selected', 'true');
  await menu(page, '重命名会话');
  await page.getByRole('textbox', { name: '会话名称' }).fill('logs');
  await page.keyboard.press('Enter');
  await page.reload();
  await expect(page.getByRole('tab', { name: 'logs', exact: true })).toBeVisible();
});

test('multiline guard applies to keyboard and menu paste, while bracketed paste is immediate', async ({ page }) => {
  const { input } = await openTerminal(page);
  await nativePaste(page, 'echo one\necho two\n');
  const guard = page.getByRole('dialog', { name: '多行粘贴确认' });
  await expect(guard).toBeVisible();
  expect(input.join('')).not.toContain('echo one');
  await guard.getByRole('button', { name: '取消', exact: true }).click();
  expect(input.join('')).not.toContain('echo one');
  await nativePaste(page, 'echo one\necho two\n');
  await guard.getByRole('button', { name: '确认粘贴' }).click();
  await expect.poll(() => input.join('')).toContain('echo one\recho two\r');
});

test('bracketed paste stays atomic and enforces the UTF-8 byte limit', async ({ page }) => {
  const { input } = await openTerminal(page, 1440, 'dark', { bracketed: true });
  await nativePaste(page, 'echo 一\necho 二');
  await expect.poll(() => input.join('')).toContain('\x1b[200~echo 一\recho 二\x1b[201~');
  await expect(page.getByRole('dialog', { name: '多行粘贴确认' })).toHaveCount(0);
  await nativePaste(page, '大'.repeat(12000));
  await expect(page.getByText('本次最多粘贴 32 KiB，请分段粘贴。')).toBeVisible();
  expect(input.join('')).not.toContain('大');
});

test('insecure HTTP uses ws and falls back to the floating command editor', async ({ page }) => {
  await serveHttpFixtures(page);
  const { input, urls, terminal } = await openTerminal(page, 390, 'light', {}, HTTP_ORIGIN);
  await expectInsecureHttp(page);
  const before = await terminal.locator('.cvm-terminal-screen').boundingBox();
  await page.getByRole('button', { name: '粘贴', exact: true }).click();
  const editor = page.getByRole('region', { name: '命令输入框', exact: true });
  await expect(editor.getByRole('textbox', { name: '命令内容' })).toBeFocused();
  await editor.getByRole('textbox').fill('printf HTTP');
  await editor.getByRole('button', { name: /^发送/ }).click();
  await expect.poll(() => input.join('')).toContain('printf HTTP');
  expect(await terminal.locator('.cvm-terminal-screen').boundingBox()).toEqual(before);
  expect(urls[0]).toMatch(/^ws:\/\/brocade-http\.test\//);
});

test('clipboard denial, late clipboard and editor identity do not misdirect input', async ({ page }) => {
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
  const { input, terminal } = await openTerminal(page);
  await menu(page, '粘贴');
  await page.getByRole('tab', { name: '会话 2', exact: true }).click();
  await expect(terminal.getByRole('status').first()).toHaveText('已连接');
  await page.evaluate(() =>
    (window as unknown as { resolvePaste: (text: string) => void }).resolvePaste('SHOULD-NOT-SEND'),
  );
  expect(input.join('')).not.toContain('SHOULD-NOT-SEND');
  await menu(page, '命令输入框');
  await page.getByRole('textbox', { name: '命令内容' }).fill('STALE-EDITOR');
  await page.getByRole('tab', { name: '会话 1', exact: true }).click();
  await page.getByRole('button', { name: /^发送/ }).click();
  await expect(page.getByText('终端连接已变化，请重新打开粘贴。')).toBeVisible();
  expect(input.join('')).not.toContain('STALE-EDITOR');
});

test('selection preserves newlines, copies automatically and keeps bounded in-memory records', async ({ page }) => {
  const writes: string[] = [];
  await page.exposeFunction('captureCopy', (text: string) => writes.push(text));
  await page.addInitScript(() =>
    Object.defineProperty(navigator, 'clipboard', {
      configurable: true,
      value: {
        writeText: (text: string) =>
          (window as unknown as { captureCopy: (text: string) => Promise<void> }).captureCopy(text),
      },
    }),
  );
  await openTerminal(page, 1440, 'dark', { text: 'alpha   \r\nbeta    \r\ngamma\r\nroot# ' });
  const screen = page.locator('.xterm-screen');
  const box = (await screen.boundingBox())!;
  await page.mouse.move(box.x + 1, box.y + 5);
  await page.mouse.down();
  await page.mouse.move(box.x + 80, box.y + 33, { steps: 10 });
  await page.mouse.up();
  await expect(page.getByRole('toolbar', { name: '选区操作' })).toBeVisible();
  await expect.poll(() => writes.length).toBeGreaterThan(0);
  expect(writes[0]).toContain('\n');
  expect(writes[0]).not.toMatch(/ +\n/);
  await page.locator('.xterm-helper-textarea').press('Control+c');
  await expect.poll(() => writes.length).toBeGreaterThan(1);
  await menu(page, '复制记录');
  await expect(page.locator('.cvm-terminal-clips code')).toHaveCount(1);
  expect(await page.evaluate(() => Object.values(localStorage).some(v => String(v).includes('alpha')))).toBe(false);
});

test('context actions, floating editor, find options, settings and fullscreen', async ({ page }) => {
  const { input, terminal } = await openTerminal(page);
  await context(page, '全选');
  await expect(page.getByRole('toolbar', { name: '选区操作' })).toBeVisible();
  await context(page, '命令输入框');
  await expect(page.getByRole('textbox', { name: '命令内容' })).not.toHaveValue('');
  await page.getByRole('textbox', { name: '命令内容' }).fill('echo editor');
  await page.keyboard.press('Control+Enter');
  await expect.poll(() => input.join('')).toContain('echo editor');
  await page.getByRole('button', { name: '关闭输入框' }).click();
  await page.locator('.xterm-helper-textarea').press('Control+Shift+f');
  const find = page.getByRole('region', { name: '查找终端' });
  await expect(find.getByRole('textbox')).toBeFocused();
  await find.getByRole('textbox').fill('payments');
  await expect(find).toContainText('/ 120');
  await find.getByRole('button', { name: '下一个' }).click();
  await find.getByRole('button', { name: '上一个' }).click();
  await find.getByRole('checkbox', { name: '区分大小写' }).check();
  await find.getByRole('textbox').fill('PAYMENTS');
  await expect(find).toContainText('0 / 0');
  await find.getByRole('checkbox', { name: '全词', exact: true }).check();
  await find.getByRole('checkbox', { name: '正则', exact: true }).check();
  await find.getByRole('textbox').fill('[');
  await expect(find).toContainText('正则表达式无效');
  await find.getByRole('textbox').press('Escape');
  await expect(find).toBeHidden();
  await menu(page, '终端设置');
  await page.getByRole('combobox', { name: '配色', exact: true }).selectOption('light');
  await expect(terminal).toHaveAttribute('data-theme', 'light');
  await page.getByRole('combobox', { name: '光标', exact: true }).selectOption('bar');
  await page.getByRole('checkbox', { name: '辅助阅读' }).check();
  await page.keyboard.press('Escape');
  await menu(page, '全屏');
  await expect.poll(() => page.evaluate(() => !!document.fullscreenElement)).toBe(true);
  await menu(page, '退出全屏');
  await expect.poll(() => page.evaluate(() => !!document.fullscreenElement)).toBe(false);
});

test('find selections do not automatically copy, and selection search uses the chosen text', async ({ page }) => {
  const copies: string[] = [];
  await page.exposeFunction('captureCopy', (text: string) => copies.push(text));
  await page.addInitScript(() =>
    Object.defineProperty(navigator, 'clipboard', {
      configurable: true,
      value: {
        writeText: (text: string) =>
          (window as unknown as { captureCopy: (text: string) => Promise<void> }).captureCopy(text),
      },
    }),
  );
  await openTerminal(page);
  await menu(page, '查找');
  await page.getByRole('textbox', { name: '查找内容' }).fill('payments');
  await expect(page.getByRole('region', { name: '查找终端' })).toContainText('/ 120');
  expect(copies).toEqual([]);
  await page.getByRole('button', { name: '关闭查找' }).click();
  await context(page, '全选');
  await page.getByRole('button', { name: '查找选区', exact: true }).click();
  await expect(page.getByRole('textbox', { name: '查找内容' })).not.toHaveValue('');
});

test('transport loss reconnects the same Shell and never replays input', async ({ page }) => {
  const { sockets, input, attaches, terminal } = await openTerminal(page);
  await page.locator('.xterm-helper-textarea').press('z');
  await expect.poll(() => input.join('')).toContain('z');
  sockets.get(FIRST)!.close({ code: 1001, reason: 'network fixture' });
  await expect(page.locator('.cvm-terminal-banner')).toContainText('重连');
  await expect.poll(attaches).toBe(2);
  await expect(terminal.getByRole('status').first()).toHaveText('已连接');
  expect(input.filter(s => s === 'z')).toHaveLength(1);
});

test('server rejection stays explicit and closed output can still be exported', async ({ page }) => {
  const { sockets, attaches, terminal } = await openTerminal(page);
  sockets.get(FIRST)!.send(JSON.stringify({ type: 'error', message: 'Guest 终端不可用或实例状态已改变' }));
  await expect(terminal.getByRole('alert')).toContainText('Guest 终端不可用');
  const download = page.waitForEvent('download');
  await menu(page, '导出历史');
  expect((await download).suggestedFilename()).toContain('terminal.txt');
  expect(attaches()).toBe(1);
});

test.describe('phone keys and selection', () => {
  test.use({ hasTouch: true, isMobile: true });
  test('Ctrl/Alt latch, arrows, selection mode and compact menu', async ({ page }) => {
    const { input } = await openTerminal(page, 390);
    await page.getByRole('button', { name: 'Ctrl', exact: true }).tap();
    await page.keyboard.press('c');
    await expect.poll(() => input.join('')).toContain('\x03');
    await expect(page.getByRole('button', { name: 'Ctrl', exact: true })).toHaveAttribute('aria-pressed', 'false');
    await page.getByRole('button', { name: 'Alt', exact: true }).tap();
    await page.keyboard.press('x');
    await expect.poll(() => input.join('')).toContain('\x1bx');
    await page.getByRole('button', { name: '方向键↑', exact: true }).tap();
    await expect.poll(() => input.join('')).toContain('\x1b[A');
    await page.getByRole('button', { name: '选择', exact: true }).tap();
    await page.getByRole('button', { name: '全选', exact: true }).tap();
    await expect(page.getByRole('button', { name: '复制', exact: true })).toBeEnabled();
    await page.getByRole('button', { name: '完成', exact: true }).tap();
    await page.getByRole('button', { name: '更多', exact: true }).tap();
    await expect(page.getByRole('menuitem', { name: '全屏', exact: true })).toHaveCount(0);
    await expect(page.locator('.cvm-terminal-menu kbd')).toHaveCount(0);
    await expect(page.getByText('IP 未上报', { exact: false })).toHaveCount(0);
  });
});

for (const width of [1440, 390])
  for (const theme of ['dark', 'light']) {
    for (const scene of ['menu', 'compose', 'search', 'settings', 'clips', 'context', 'ended']) {
      test(`B state ${scene} ${width} ${theme}`, async ({ page }) => {
        const { sockets, errors } = await openTerminal(page, width, theme);
        if (scene === 'menu') await page.getByRole('button', { name: '更多', exact: true }).click();
        if (scene === 'compose') {
          await menu(page, '命令输入框');
          await page.getByRole('textbox', { name: '命令内容' }).fill('printf "你好，Brocade"');
        }
        if (scene === 'search') {
          await menu(page, '查找');
          await page.getByRole('textbox', { name: '查找内容' }).fill('payments');
        }
        if (scene === 'settings') await menu(page, '终端设置');
        if (scene === 'clips') await menu(page, '复制记录');
        if (scene === 'context')
          await page.locator('.cvm-terminal-screen').click({ button: 'right', position: { x: 100, y: 100 } });
        if (scene === 'ended') {
          sockets.get(FIRST)!.send(JSON.stringify({ type: 'exit', code: 0 }));
          await expect(page.locator('.cvm-terminal-banner')).toContainText('会话已结束');
        }
        expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
        await expect(page).toHaveScreenshot(`terminal-${scene}-${width}-${theme}.png`, { caret: 'hide' });
        expect(errors).toEqual([]);
      });
    }
  }
for (const scene of ['paste', 'rename', 'select', 'mouse', 'lost', 'failed', 'full'])
  test(`B special state ${scene}`, async ({ page }) => {
    const { sockets } = await openTerminal(
      page,
      1440,
      'dark',
      scene === 'failed' ? { failure: 'Guest 终端不可用或实例状态已改变' } : {},
    );
    if (scene === 'paste') await nativePaste(page, 'echo one\necho two\n');
    if (scene === 'rename') {
      await page.getByRole('tab', { name: '会话 1', exact: true }).dblclick();
      await page.getByRole('textbox', { name: '会话名称' }).fill('deploy');
    }
    if (scene === 'select') await context(page, '全选');
    if (scene === 'mouse') {
      sockets.get(FIRST)!.send(Buffer.from('\x1b[?1000h'));
      await expect(page.getByText('程序正在使用鼠标，按住 Shift 拖动选择')).toBeVisible();
    }
    if (scene === 'lost') {
      sockets.get(FIRST)!.close({ code: 1001 });
      await expect(page.locator('.cvm-terminal-banner')).toContainText('重连');
    }
    if (scene === 'failed') await expect(page.getByRole('alert')).toContainText('Guest 终端不可用');
    if (scene === 'full') {
      await page.getByRole('button', { name: '新建会话', exact: true }).click();
      await expect(page.getByRole('button', { name: '新建会话', exact: true })).toHaveAttribute(
        'aria-disabled',
        'true',
      );
    }
    await expect(page).toHaveScreenshot(`terminal-${scene}-1440-dark.png`, { caret: 'hide' });
  });

test('phone keyboard viewport keeps fixed controls visible and the editor floating', async ({ page }) => {
  await openTerminal(page, 390, 'light');
  await menu(page, '命令输入框');
  await page.getByRole('textbox', { name: '命令内容' }).fill('echo keyboard');
  await page.evaluate(() => {
    Object.defineProperty(visualViewport, 'height', { configurable: true, value: 480 });
    visualViewport!.dispatchEvent(new Event('resize'));
  });
  await expect.poll(() => page.locator('.cvm-webshell').evaluate(e => e.getBoundingClientRect().height)).toBe(480);
  expect((await page.locator('.cvm-terminal-keybar').boundingBox())!.y).toBe(438);
  expect((await page.locator('.cvm-terminal-compose').boundingBox())!.y).toBeGreaterThan(38);
  await expect(page).toHaveScreenshot('terminal-keyboard-390-light.png', { caret: 'hide' });
});

test('denied clipboard opens the command editor and a clean window closes without ending its Shell', async ({
  page,
}) => {
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
  const { input } = await openTerminal(page);
  await menu(page, '粘贴');
  await expect(page.getByRole('textbox', { name: '命令内容' })).toBeFocused();
  await page.getByRole('textbox', { name: '命令内容' }).fill('echo denied');
  await page.getByRole('button', { name: /^发送/ }).click();
  await expect.poll(() => input.join('')).toContain('echo denied');
  await page.getByRole('button', { name: '关闭输入框' }).click();
  await page.evaluate(() => {
    window.close = () => {};
  });
  await menu(page, '关闭窗口，保留会话');
  await expect(page.getByRole('button', { name: '终端', exact: true })).toBeVisible();
  expect(input).not.toContain('close');
});

test('one independent terminal window survives navigating from the list to another instance and can reopen', async ({
  page,
  context: browser,
}) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto('/tests/fixtures/cvm/preview.html?scene=fleet&theme=dark');
  const list = page.getByRole('table', { name: '按宿主分组的容器' });
  const row = list.getByRole('row').filter({ has: page.getByRole('button', { name: 'home-workspace', exact: true }) });
  const launcher = row.getByRole('button', { name: '终端', exact: true });
  const popupEvent = page.waitForEvent('popup');
  await launcher.click();
  const popup = await popupEvent;
  // The popup uses the authenticated production route; the instance list remains in the parent.
  const terminal = popup.getByRole('main', { name: '终端 · home-workspace', exact: true });
  await expect(terminal).toBeVisible();
  await expect(list).toBeVisible();
  await launcher.click();
  expect(browser.pages()).toHaveLength(2);
  await list.getByRole('button', { name: 'build-runner', exact: true }).click();
  await expect(page.getByRole('heading', { name: 'build-runner', exact: true })).toBeVisible();
  await expect(terminal).toBeVisible();
  await popup.close();
  await page.getByRole('button', { name: '上一个容器', exact: true }).click();
  await expect(page.getByRole('heading', { name: 'home-workspace', exact: true })).toBeVisible();
  const next = page.waitForEvent('popup');
  await page.getByRole('button', { name: '终端', exact: true }).click();
  const reopened = await next;
  await expect(reopened.getByRole('main', { name: '终端 · home-workspace', exact: true })).toBeVisible();
  await reopened.close();
});

test('popup blocking offers a usable new-tab link and read only cannot open a terminal', async ({ page }) => {
  await page.goto('/tests/fixtures/cvm/preview.html?scene=detail');
  await page.evaluate(() => {
    window.open = () => null;
  });
  await page.getByRole('button', { name: '终端', exact: true }).click();
  await expect(page.getByRole('link', { name: '弹窗被拦截，改在新标签页打开' })).toHaveAttribute(
    'href',
    /webshell=cvm-alice/,
  );
  await page.goto('/tests/fixtures/cvm/preview.html?scene=detail&role=readonly');
  await expect(page.getByRole('button', { name: '终端', exact: true })).toHaveCount(0);
  await page.goto('/tests/fixtures/cvm/preview.html?scene=detail&role=readonly&webshell=cvm-alice');
  await expect(page.getByRole('alert')).toContainText('无权连接终端');
});
for (const width of [1440, 390, 320])
  for (const theme of ['dark', 'light']) {
    test(`WebShell management ${width} ${theme}`, async ({ page }) => {
      // Host reports in this scene are dated Nov 4; keep their relative labels stable.
      await page.clock.setFixedTime(new Date('2026-11-18T12:00:00Z'));
      await page.setViewportSize({ width, height: 900 });
      await page.goto(`/tests/fixtures/cvm/preview.html?scene=terminal-management&theme=${theme}`);
      await page.getByRole('tab', { name: '宿主', exact: true }).click();
      await expect(page).toHaveURL(/#\/cvm\/hosts$/);
      const hosts = page.getByRole('table', { name: '宿主', exact: true });
      const primary = hosts
        .getByRole('row')
        .filter({ has: page.getByRole('button', { name: '香港 HKIX', exact: true }) });
      const toggle = primary.getByRole('switch', { name: '香港 HKIX WebShell', exact: true });
      await expect(toggle).toBeChecked();
      await primary.locator('.switch-ui').click();
      await expect(toggle).not.toBeChecked();
      await expect(primary.getByText('已关闭', { exact: true })).toBeVisible();
      await expect(hosts.getByRole('switch')).toHaveCount(2);
      await expect(hosts.getByText('运行栈待升级', { exact: true })).toBeVisible();
      await waitForVisualAssets(page);
      await expect(hosts).toHaveScreenshot(`terminal-management-${width}-${theme}.png`);
      expect(await hosts.evaluate(el => el.scrollWidth > el.clientWidth)).toBe(false);
      await toggle.focus();
      await page.keyboard.press('Space');
      await expect(toggle).toBeChecked();
      await page.getByRole('tab', { name: '容器', exact: true }).click();
      await expect(page).toHaveURL(/#\/cvm$/);
      await expect(hosts).toHaveCount(0);
      await expect(page.getByRole('table', { name: '按宿主分组的容器', exact: true })).toBeVisible();
      expect(await page.evaluate(() => (window as unknown as { __cvmMutations: unknown[] }).__cvmMutations)).toEqual([
        { enabled: false, expected_revision: 1 },
        { enabled: true, expected_revision: 2 },
      ]);
    });
  }

test('users can launch a terminal from their list but cannot manage WebShell policy', async ({ page }) => {
  await page.goto('/tests/fixtures/cvm/preview.html?role=user');
  const list = page.getByRole('table', { name: '按宿主分组的容器' });
  await list.getByRole('button', { name: 'home-workspace 更多操作', exact: true }).click();
  const actions = page.getByRole('menu', { name: 'home-workspace 更多操作', exact: true });
  await expect(actions.getByRole('menuitem', { name: /^终端/ })).toBeEnabled();
  await expect(page.getByRole('tab', { name: '宿主', exact: true })).toHaveCount(0);
  await expect(page.getByRole('switch', { name: /WebShell/ })).toHaveCount(0);
  await expect(page.getByRole('button', { name: '创建容器', exact: true })).toHaveCount(0);
});
