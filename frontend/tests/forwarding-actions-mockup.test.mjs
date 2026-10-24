import assert from 'node:assert/strict';
import { mkdir, mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { after, before, test } from 'node:test';
import { chromium, expect } from '@playwright/test';
import react from '@vitejs/plugin-react';
import { createServer } from 'vite';

let server;
let browser;
let base;
let cacheDirectory;
const screenshots = '/tmp/brocade-forwarding-actions-mockup';

before(async () => {
  cacheDirectory = await mkdtemp(join(tmpdir(), 'brocade-forwarding-actions-'));
  server = await createServer({
    configFile: false,
    root: fileURLToPath(new URL('..', import.meta.url)),
    plugins: [react()],
    cacheDir: join(cacheDirectory, 'node_modules/.vite'),
    server: { host: '127.0.0.1', port: 0 },
  });
  await server.listen();
  base = `http://127.0.0.1:${server.httpServer.address().port}/tests/fixtures/forwarding-actions-mockup.html`;
  browser = await chromium.launch();
  await mkdir(screenshots, { recursive: true });
});

after(async () => {
  await browser?.close();
  await server?.close();
  if (cacheDirectory) await rm(cacheDirectory, { recursive: true, force: true });
});

async function preview(t, query = {}, width = 1280) {
  const page = await browser.newPage({ viewport: { width, height: 1000 }, reducedMotion: 'reduce' });
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  page.on('requestfailed', request => {
    if (['document', 'script', 'stylesheet'].includes(request.resourceType())) {
      errors.push(`${request.failure()?.errorText}: ${request.url()}`);
    }
  });
  page.on('console', message => {
    if (message.type() === 'error') errors.push(message.text());
  });
  t.after(async () => {
    const failures = [...errors];
    await page.close();
    assert.deepEqual(failures, [], 'no browser or React errors');
  });
  await page.goto(`${base}?${new URLSearchParams({ scene: 'fleet', nav: '0', ...query })}`);
  await page.locator(query.v === 'current' ? '.user-forwarding-card' : '.fwr-panel').waitFor();
  return page;
}

const rows = page => page.locator('.fwr-item');
const scope = page => page.getByRole('combobox', { name: '筛选转发机器' });
const states = page => page.getByRole('group', { name: '筛选规则状态' });
const group = (page, machine) => page.getByRole('region', { name: `${machine}的规则`, exact: true });

async function recordPings(page) {
  await page.evaluate(() => {
    const original = window.fetch;
    window.mockPingRequests = [];
    window.fetch = (input, init) => {
      if (String(input).endsWith('/tcp-ping') && init?.method === 'POST')
        window.mockPingRequests.push({ path: String(input), body: JSON.parse(String(init.body)) });
      return original(input, init);
    };
  });
}

const ping = (page, machine = '东京入口', port = 30000) =>
  group(page, machine).getByRole('region', { name: `入口 ${port} TCP Ping`, exact: true });

test('TCP Ping is manual, prevents repeat clicks, and shows connection, DNS and IP separately', async t => {
  const page = await preview(t, { open: 'probe' });
  await recordPings(page);
  const probe = ping(page);
  await expect(probe.getByRole('status')).toHaveText('未测试');
  assert.deepEqual(await page.evaluate(() => window.mockPingRequests), []);
  await probe.getByRole('button', { name: '测试延迟', exact: true }).click();
  await expect(probe.getByRole('button', { name: '连接中…', exact: true })).toBeDisabled();
  await expect(probe.getByRole('status')).toContainText('TCP 连通');
  await expect(probe.getByRole('status')).toContainText('12 ms');
  await expect(probe).toContainText('DNS 3 ms');
  await expect(probe).toContainText('192.0.2.80');
  await expect(probe).toContainText('不代表 UDP 连通性');
  await page.waitForTimeout(1500);
  assert.deepEqual(await page.evaluate(() => window.mockPingRequests), [
    { path: '/users/ops/alice/forwarding/1/tcp-ping', body: {} },
  ]);
});

for (const [outcome, label] of [
  ['connect-failed', 'TCP 建连失败，请检查远程 TCP 监听和网络连通性'],
  ['dns-failed', 'DNS 解析失败'],
  ['timeout', '超时或远程地址非 TCP 监听'],
  ['agent-upgrade-required', '入口 Agent 需升级后才能执行 TCP Ping'],
  ['expired', '机器未及时返回，请重试'],
]) {
  test(`TCP Ping presents ${outcome} without a fabricated latency`, async t => {
    const page = await preview(t, { open: 'probe', ping: outcome }, 390);
    const probe = ping(page);
    await probe.getByRole('button', { name: '测试延迟', exact: true }).click();
    await expect(probe.getByRole('status')).toHaveText(label);
    await expect(probe.getByRole('status')).not.toContainText('ms');
    await expect(probe.getByRole('button', { name: '测试延迟', exact: true })).toBeEnabled();
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
  });
}

test('TCP Ping submission failure permits manual retry with a different review outcome', async t => {
  const page = await preview(t, { open: 'probe', ping: 'unavailable', nav: '1' });
  await recordPings(page);
  const probe = ping(page);
  await probe.getByRole('button', { name: '测试延迟', exact: true }).click();
  await expect(probe).toContainText('模拟请求失败，请重试');
  await expect(probe.getByRole('button', { name: '测试延迟', exact: true })).toBeEnabled();
  await page.getByRole('combobox', { name: '模拟 TCP Ping 结果' }).selectOption('connected');
  await probe.getByRole('button', { name: '测试延迟', exact: true }).click();
  await expect(probe.getByRole('status')).toContainText('TCP 连通');
  assert.equal(await page.evaluate(() => window.mockPingRequests.length), 2);
});

test('TCP Ping is disabled for quota-paused, stopped and disabled rules', async t => {
  const page = await preview(t);
  await recordPings(page);
  for (const [machine, target] of [
    ['大阪入口', 'nas.example.org:5001'],
    ['洛杉矶入口', 'media.example.net:8443'],
  ]) {
    await group(page, machine)
      .getByRole('button', { name: `入口 30000 转发到 ${target}`, exact: true })
      .click();
    await expect(ping(page, machine).getByRole('button', { name: '测试延迟', exact: true })).toBeDisabled();
  }
  const tokyo = group(page, '东京入口');
  await tokyo.getByRole('button', { name: '东京入口 :30000 的更多操作', exact: true }).click();
  await page.getByRole('menuitem', { name: /^停用/ }).click();
  await tokyo.getByRole('button', { name: '入口 30000 转发到 example.com:443', exact: true }).click();
  await expect(ping(page).getByRole('button', { name: '测试延迟', exact: true })).toBeDisabled();
  assert.deepEqual(await page.evaluate(() => window.mockPingRequests), []);
});

test('a saved rule can TCP Ping while its machine is still applying configuration', async t => {
  const page = await preview(t);
  await group(page, '新加坡入口')
    .getByRole('button', { name: '入口 30000 转发到 api.example.com:443', exact: true })
    .click();
  const probe = ping(page, '新加坡入口');
  await expect(probe.getByRole('button', { name: '测试延迟', exact: true })).toBeEnabled();
  await probe.getByRole('button', { name: '测试延迟', exact: true }).click();
  await expect(probe.getByRole('status')).toContainText('TCP 连通');
});

test('TCP Ping survives folding and machine filtering without retesting', async t => {
  const page = await preview(t, { open: 'probe' });
  await recordPings(page);
  const probe = ping(page);
  await probe.getByRole('button', { name: '测试延迟', exact: true }).click();
  await expect(probe.getByRole('status')).toContainText('TCP 连通');
  const disclosure = group(page, '东京入口').getByRole('button', {
    name: '入口 30000 转发到 example.com:443',
    exact: true,
  });
  await disclosure.click();
  await disclosure.click();
  await expect(probe.getByRole('status')).toContainText('TCP 连通');
  await scope(page).selectOption('sg-1');
  await scope(page).selectOption('jp-1');
  await expect(probe.getByRole('status')).toContainText('TCP 连通');
  assert.equal(await page.evaluate(() => window.mockPingRequests.length), 1);
});

test('editing a rule invalidates an in-flight TCP Ping and discards its delayed result', async t => {
  const page = await preview(t, { open: 'probe' });
  await recordPings(page);
  const probe = ping(page);
  await probe.getByRole('button', { name: '测试延迟', exact: true }).click();
  await page.getByRole('button', { name: '编辑 东京入口 :30000', exact: true }).click();
  const form = page.getByRole('form', { name: '编辑入口 30000 的规则', exact: true });
  await form.getByLabel('目标地址', { exact: true }).fill('changed.example.com:8443');
  await form.getByRole('button', { name: '保存', exact: true }).click();
  await expect(probe.getByRole('status')).toHaveText('规则或授权已变化，请重新测试');
  await page.waitForTimeout(1500);
  await expect(probe.getByRole('status')).not.toContainText('TCP 连通');
  assert.equal(await page.evaluate(() => window.mockPingRequests.length), 1);
  await probe.getByRole('button', { name: '测试延迟', exact: true }).click();
  await expect(probe.getByRole('status')).toContainText('TCP 连通');
});

for (const width of [1180, 620, 390]) {
  test(`TCP Ping details fit ${width}px in light and dark themes`, async t => {
    for (const theme of ['light', 'dark']) {
      const page = await preview(t, { open: 'probe', w: String(width), theme }, width);
      const probe = ping(page);
      await probe.getByRole('button', { name: '测试延迟', exact: true }).click();
      await expect(probe.getByRole('status')).toContainText('TCP 连通');
      const colors = await probe.locator('.fwd-st.ok').evaluate(element => {
        const sample = document.createElement('span');
        sample.style.color = 'var(--ok)';
        element.append(sample);
        const colors = { actual: getComputedStyle(element).color, expected: getComputedStyle(sample).color };
        sample.remove();
        return colors;
      });
      assert.equal(colors.actual, colors.expected, 'TCP success keeps the shared success color');
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
      if (width <= 620)
        assert.ok((await probe.getByRole('button', { name: '测试延迟', exact: true }).boundingBox()).height >= 44);
      await group(page, '东京入口').screenshot({ path: `${screenshots}/ping-${width}-${theme}.png` });
    }
  });
}

test('machine scope belongs to the rule panel and leaves the usage panel intact', async t => {
  const page = await preview(t);
  const usage = page.locator('.user-forwarding-usage');
  const before = await usage.innerText();
  await scope(page).selectOption('us-1');
  await expect(rows(page)).toHaveCount(1);
  await usage.getByText('东京入口', { exact: true }).click();
  await expect(scope(page)).toHaveValue('us-1');
  assert.equal(await usage.innerText(), before);
  assert.equal(await usage.locator('.fwd-quota-pick').count(), 0, 'quota names no longer filter rules');
  assert.equal(await usage.locator('[aria-pressed]').count(), 2, 'only accounting buttons remain');
  await scope(page).selectOption('jp-1');
  await expect(rows(page)).toHaveCount(4);
  await page.getByRole('button', { name: '编辑 法兰克福入口 的转发授权', exact: true }).click();
  await expect(scope(page)).toHaveValue('jp-1');
  await expect(rows(page)).toHaveCount(4);
  // Changing the independent scope also preserves an open authorization form.
  await scope(page).selectOption('sg-1');
  await expect(page.getByRole('form', { name: '编辑 法兰克福入口 的转发授权' })).toBeVisible();
  await expect(rows(page)).toHaveCount(1);
});

test('all machines are grouped so repeated ports retain their machine context', async t => {
  const page = await preview(t);
  await expect(page.locator('.fwr-group')).toHaveCount(5);
  await expect(group(page, '东京入口').getByRole('listitem')).toHaveCount(4);
  await expect(group(page, '大阪入口').getByRole('listitem')).toHaveCount(3);
  for (const machine of ['东京入口', '大阪入口', '洛杉矶入口']) {
    await expect(group(page, machine).locator('.fwd-port').first()).toHaveText(':30000');
  }
});

test('status counts follow machine scope and empty reset keeps the selected machine', async t => {
  const page = await preview(t);
  await expect(states(page).getByRole('button', { name: /^已生效\s*5$/ })).toBeVisible();
  await states(page)
    .getByRole('button', { name: /^未生效\s*5$/ })
    .click();
  await expect(rows(page)).toHaveCount(5);
  await scope(page).selectOption('jp-1');
  await expect(rows(page)).toHaveCount(0);
  await expect(page.getByText('没有匹配的规则', { exact: true })).toBeVisible();
  await page.getByRole('button', { name: '清除搜索与状态筛选' }).click();
  await expect(scope(page)).toHaveValue('jp-1');
  await expect(rows(page)).toHaveCount(4);
  await scope(page).selectOption('us-1');
  await states(page)
    .getByRole('button', { name: /^已停用\s*1$/ })
    .click();
  await expect(rows(page)).toHaveCount(1);
});

test('search covers ports, names, targets, IPv6 and machine names', async t => {
  const page = await preview(t);
  for (const [query, count] of [
    ['30000', 4],
    ['公司 Git', 1],
    ['git.example.com', 1],
    ['1234:5678', 1],
    ['东京入口', 4],
    ['missing', 0],
  ]) {
    await page.getByRole('searchbox', { name: '搜索规则' }).fill(query);
    await expect(rows(page)).toHaveCount(count);
  }
});

test('sorting stays within each machine group', async t => {
  const page = await preview(t);
  await scope(page).selectOption('jp-1');
  const ports = group(page, '东京入口').locator('.fwd-port');
  await expect(ports).toHaveText([':30000', ':30001', ':30002', ':30003']);
  await page.getByRole('combobox', { name: '规则排序' }).selectOption('month');
  await expect(ports).toHaveText([':30000', ':30002', ':30003', ':30001']);
  await page.getByRole('combobox', { name: '规则排序' }).selectOption('created');
  await expect(ports).toHaveText([':30003', ':30002', ':30001', ':30000']);
});

for (const variant of ['inline', 'popover', 'drawer']) {
  test(`${variant}: edit, save and cancel retain scope, readings and focus`, async t => {
    const page = await preview(t, { v: variant });
    await scope(page).selectOption('jp-1');
    const opener = page.getByRole('button', { name: '编辑 东京入口 :30000', exact: true });
    await opener.click();
    const form = page.getByRole('form', { name: '编辑入口 30000 的规则', exact: true });
    await expect(form).toBeVisible();
    await expect(group(page, '东京入口').locator('.fwd-cycle').first()).toContainText('96.00 GiB');
    await form.getByLabel('名称', { exact: true }).fill('新目标名称');
    await form.getByRole('button', { name: '保存', exact: true }).click();
    await expect(form).toHaveCount(0);
    await expect(opener).toBeFocused();
    await expect(group(page, '东京入口')).toContainText('新目标名称');
    await expect(scope(page)).toHaveValue('jp-1');
    await opener.click();
    await expect(form).toBeVisible();
    await form.getByLabel('名称', { exact: true }).fill('不保存');
    await page.keyboard.press('Escape');
    await expect(form).toHaveCount(0);
    await expect(opener).toBeFocused();
    await expect(group(page, '东京入口')).not.toContainText('不保存');
  });

  test(`${variant}: new rules use the selected machine and show validation`, async t => {
    const page = await preview(t, { v: variant });
    await scope(page).selectOption('jp-1');
    await page.getByRole('button', { name: '新建规则', exact: true }).click();
    const form = page.getByRole('form', { name: '新建转发规则', exact: true });
    await expect(form).toBeVisible();
    await expect(form.getByRole('combobox')).toHaveCount(0);
    await form.getByLabel('目标地址', { exact: true }).fill('new.example.com:443');
    await form.getByLabel('入口端口', { exact: true }).fill('30000');
    await expect(form.getByRole('button', { name: '创建规则', exact: true })).toBeDisabled();
    await expect(form).toContainText('占用');
    await form.getByLabel('入口端口', { exact: true }).fill('30004');
    await form.getByRole('button', { name: '创建规则', exact: true }).click();
    await expect(form).toHaveCount(0);
    await expect(rows(page)).toHaveCount(5);
    await expect(group(page, '东京入口')).toContainText('new.example.com:443');
    await expect(scope(page)).toHaveValue('jp-1');
  });

  test(`${variant}: stopping and resuming a machine preserves the rule scope`, async t => {
    const page = await preview(t, { v: variant, scene: 'typical' });
    await scope(page).selectOption('jp-1');
    await page.getByRole('button', { name: '编辑 东京入口 的转发授权', exact: true }).click();
    const form = page.getByRole('form', { name: '编辑 东京入口 的转发授权', exact: true });
    await form.getByRole('button', { name: '停止转发', exact: true }).click();
    await expect(form.getByRole('alert')).toContainText('4 条规则停止监听');
    await expect(rows(page)).toHaveCount(4);
    await form.getByRole('button', { name: '停止转发', exact: true }).click();
    await expect(form).toHaveCount(0);
    await expect(group(page, '东京入口').locator('.fwd-now')).toHaveText(['已停用', '已停用', '已停用', '已停用']);
    await expect(scope(page)).toHaveValue('jp-1');
    await page.getByRole('button', { name: '恢复', exact: true }).click();
    await expect(group(page, '东京入口').locator('.fwd-now').first()).toHaveText('同步中');
    await expect(scope(page)).toHaveValue('jp-1');
  });
}

test('removing the selected machine clears only the invalid machine scope', async t => {
  const page = await preview(t);
  await scope(page).selectOption('us-1');
  await page.getByRole('button', { name: '编辑 洛杉矶入口 的转发授权', exact: true }).click();
  const form = page.getByRole('form', { name: '编辑 洛杉矶入口 的转发授权', exact: true });
  await form.getByRole('button', { name: '移除入口', exact: true }).click();
  await expect(form.getByRole('alert')).toContainText('连同 1 条规则一起移除');
  await form.getByRole('button', { name: '移除', exact: true }).click();
  await expect(form).toHaveCount(0);
  await expect(scope(page)).toHaveValue('');
  await expect(rows(page)).toHaveCount(10);
  await expect(scope(page).getByRole('option', { name: /洛杉矶/ })).toHaveCount(0);
});

test('authorizing another machine adds a filter option without changing the selected scope', async t => {
  const page = await preview(t, { scene: 'typical' });
  await scope(page).selectOption('jp-1');
  await page.getByRole('button', { name: '授权机器', exact: true }).click();
  const form = page.getByRole('form', { name: '授权转发入口', exact: true });
  await form.getByRole('radio', { name: '新加坡入口' }).click();
  await form.getByRole('button', { name: '授权', exact: true }).click();
  await expect(form).toHaveCount(0);
  await expect(scope(page)).toHaveValue('jp-1');
  await expect(scope(page).getByRole('option', { name: '新加坡入口 · 0 条', exact: true })).toHaveCount(1);
  await scope(page).selectOption('sg-1');
  await expect(page.getByText('这台机器还没有规则', { exact: true })).toBeVisible();
  await expect(page.getByRole('button', { name: '新建规则', exact: true })).toBeEnabled();
});

test('current comparison retains production components and TCP Ping on the same data', async t => {
  const page = await preview(t, { v: 'current', scene: 'typical', nav: '1' });
  await expect(page.locator('.user-forwarding-usage .qta-r')).toHaveCount(2);
  await expect(page.locator('.user-forwarding-card .fwd-item')).toHaveCount(5);
  await page.locator('.user-forwarding-card .fwr-rmain').first().click();
  const probe = page.locator('.user-forwarding-card .fwd-tcp-ping').first();
  await probe.getByRole('button', { name: '测试延迟', exact: true }).click();
  await expect(probe.getByRole('status')).toContainText('TCP 连通');
  await page
    .getByRole('group', { name: '方案', exact: true })
    .getByRole('button', { name: 'A 行内展开', exact: true })
    .click();
  await expect(page.locator('.fwr-panel')).toBeVisible();
  await expect(rows(page)).toHaveCount(5);
});

test('a scoped stopped or full machine cannot silently create on another machine', async t => {
  const page = await preview(t);
  const create = page.getByRole('button', { name: '新建规则', exact: true });
  await scope(page).selectOption('us-1');
  await expect(create).toBeDisabled();
  await expect(create).toHaveAttribute('title', '这台机器已停止转发');
  await expect(page.locator('.fwr-constraint')).toHaveText('这台机器已停止转发');
  await scope(page).selectOption('jp-2');
  await expect(create).toBeDisabled();
  await expect(create).toHaveAttribute('title', '规则数已达上限');
  await expect(page.locator('.fwr-constraint')).toHaveText('规则数已达上限');
  await scope(page).selectOption('sg-1');
  await expect(create).toBeEnabled();
});

test('details expose full IPv4 and IPv6 endpoints and copy actions', async t => {
  const page = await preview(t);
  const first = group(page, '东京入口').getByRole('listitem').first();
  const disclosure = first.getByRole('button', { name: '入口 30000 转发到 example.com:443', exact: true });
  await disclosure.click();
  await expect(disclosure).toHaveAttribute('aria-expanded', 'true');
  await expect(first.getByRole('button', { name: '复制入口 203.0.113.24:30000', exact: true })).toBeVisible();
  await expect(first.getByRole('button', { name: '复制入口 [2001:db8:24::1]:30000', exact: true })).toBeVisible();
  await expect(first.locator('.fwr-detail')).toContainText('74.88 GiB');
  await disclosure.click();
  await expect(first.locator('.fwr-detail')).toHaveCount(0);
});

test('rule menu can disable, re-enable and delete with confirmation', async t => {
  const page = await preview(t);
  await scope(page).selectOption('jp-1');
  const first = group(page, '东京入口').getByRole('listitem').first();
  const menu = first.getByRole('button', { name: '东京入口 :30000 的更多操作', exact: true });
  await menu.click();
  await page.getByRole('menuitem', { name: /^停用/ }).click();
  await expect(first.locator('.fwd-now')).toHaveText('已停用');
  await menu.click();
  await page.getByRole('menuitem', { name: /^启用/ }).click();
  await expect(first.locator('.fwd-now')).toHaveText('已生效');
  await menu.click();
  await page.getByRole('menuitem', { name: /^删除规则/ }).click();
  await expect(rows(page)).toHaveCount(4);
  const dialog = page.getByRole('dialog', { name: '删除转发规则', exact: true });
  await dialog.getByRole('button', { name: '删除规则', exact: true }).click();
  await expect(rows(page)).toHaveCount(3);
  await expect(scope(page)).toHaveValue('jp-1');
});

for (const variant of ['inline', 'popover']) {
  test(`${variant}: changing machine, search, status or sort leaves no hidden or orphaned editor`, async t => {
    const page = await preview(t, { v: variant });
    for (const change of [
      () => scope(page).selectOption('sg-1'),
      () => page.getByRole('searchbox', { name: '搜索规则' }).fill('git'),
      () =>
        states(page)
          .getByRole('button', { name: /^未生效/ })
          .click(),
      () => page.getByRole('combobox', { name: '规则排序' }).selectOption('month'),
    ]) {
      await scope(page).selectOption('jp-1');
      await page.getByRole('searchbox', { name: '搜索规则' }).fill('');
      await states(page).getByRole('button', { name: /^全部/ }).click();
      await page.getByRole('combobox', { name: '规则排序' }).selectOption('port');
      await page.getByRole('button', { name: '编辑 东京入口 :30000', exact: true }).click();
      await expect(page.getByRole('form', { name: '编辑入口 30000 的规则' })).toBeVisible();
      await change();
      await expect(page.locator('.fwe-form.rule')).toHaveCount(0);
      await expect(page.locator('.fwe-pop')).toHaveCount(0);
      await page.getByRole('searchbox', { name: '搜索规则' }).fill('');
      await expect(page.locator('.fwe-form.rule')).toHaveCount(0);
    }
  });
}

for (const [scene, emptyLabel, disabled] of [
  ['empty', '还没有转发规则', false],
  ['ungranted', '尚未授权转发机器', true],
]) {
  test(`${scene}: distinguishes empty rules from missing authorization`, async t => {
    const page = await preview(t, { scene });
    await expect(page.getByText(emptyLabel, { exact: true })).toBeVisible();
    assert.equal(await page.getByRole('button', { name: '新建规则', exact: true }).isDisabled(), disabled);
  });
}

test('light and dark layouts fit desktop, tablet and phone with usable controls', async t => {
  for (const width of [1180, 620, 390]) {
    for (const theme of ['light', 'dark']) {
      const page = await preview(t, { w: String(width), theme, v: 'popover' }, width);
      await scope(page).selectOption('jp-1');
      await page.locator('.fwr-panel').scrollIntoViewIfNeeded();
      const overflow = await page.evaluate(() => document.documentElement.scrollWidth > innerWidth);
      assert.equal(overflow, false, `${width}/${theme}: no document overflow`);
      const overlaps = await page.locator('.fwr-item .fwd-row').evaluateAll(elements =>
        elements.some(row => {
          const target = row.querySelector('.fwd-target').getBoundingClientRect();
          const actions = row.querySelector('.fwr-acts').getBoundingClientRect();
          return (
            target.left < actions.right &&
            target.right > actions.left &&
            target.top < actions.bottom &&
            target.bottom > actions.top
          );
        }),
      );
      assert.equal(overlaps, false, `${width}/${theme}: actions never cover the target`);
      if (width <= 620) {
        for (const control of await page.locator('.fwr-edit, .fwr-acts .fwd-ib').all()) {
          const box = await control.boundingBox();
          assert.ok(box.width >= 44 && box.height >= 44, 'touch actions have 44px targets');
        }
      }
      await page.locator('.fwr-panel').screenshot({ path: `${screenshots}/${width}-${theme}.png` });
      await page.getByRole('button', { name: '编辑 东京入口 :30000', exact: true }).click();
      await expect(page.getByRole('form', { name: '编辑入口 30000 的规则' })).toBeVisible();
      await expect(page.locator(width < 560 ? '.fwe-rule-sheet' : '.fwe-pop')).toBeVisible();
      const form = page.getByRole('form', { name: '编辑入口 30000 的规则', exact: true });
      const cancelBox = await form.getByRole('button', { name: '取消', exact: true }).boundingBox();
      const saveBox = await form.getByRole('button', { name: '保存', exact: true }).boundingBox();
      assert.equal(cancelBox.y, saveBox.y, 'cancel and save share one row');
      assert.ok(saveBox.x > cancelBox.x, 'save remains to the right of cancel');
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
      await page.keyboard.press('Escape');
      await expect(page.locator('.fwe-form.rule')).toHaveCount(0);
    }
  }
});
