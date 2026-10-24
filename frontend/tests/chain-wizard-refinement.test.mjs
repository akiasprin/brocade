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
const screenshots = '/tmp/brocade-chain-wizard-refinement';

before(async () => {
  cacheDirectory = await mkdtemp(join(tmpdir(), 'brocade-chain-wizard-'));
  server = await createServer({
    configFile: false,
    root: fileURLToPath(new URL('..', import.meta.url)),
    plugins: [react()],
    cacheDir: join(cacheDirectory, 'node_modules/.vite'),
    server: { host: '127.0.0.1', port: 0 },
  });
  await server.listen();
  base = `http://127.0.0.1:${server.httpServer.address().port}/tests/fixtures/chain-wizard-refinement.html`;
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
  await page.goto(`${base}?${new URLSearchParams({ scene: 'relay', nav: '0', ...query })}`);
  await page
    .locator(query.view === 'claude' ? '.cw-paper' : query.view === 'current' ? '.pv-chain-fields' : '.cr-paper')
    .waitFor();
  // Configured scenarios use a manually entered name; the blank scene tests the untouched field.
  if (!['blank', 'blocked'].includes(query.scene)) {
    const name = page.getByLabel('链名称', { exact: true });
    if ((await name.inputValue()) === '') await name.fill('测试链');
  }
  return page;
}

const submit = page => page.getByRole('button', { name: '加入草稿', exact: true });

test('keeps the familiar two cards above the complete editable path', async t => {
  const page = await preview(t);
  const basic = await page.locator('.cr-basic').boundingBox();
  const protocols = await page.locator('.cr-protocols').boundingBox();
  const path = await page.locator('.cr-path').boundingBox();
  assert.equal(basic.y, protocols.y);
  assert.ok(protocols.x > basic.x + basic.width);
  assert.ok(path.y > Math.max(basic.y + basic.height, protocols.y + protocols.height));
  await expect(page.getByLabel('链名称', { exact: true })).toHaveValue('测试链');
  await expect(page.getByLabel('当前链路径')).toHaveCount(0);
  await expect(page.locator('.cr-path .chain-rule-node-head b').filter({ hasText: '东京 IIJ' })).toBeVisible();
  await expect(submit(page)).toBeEnabled();
  await page.getByLabel('在路径末尾添加机器').selectOption('us-gia');
  await expect(page.locator('.cr-path .chain-rule-node-head b').filter({ hasText: '洛杉矶 CN2 GIA' })).toBeVisible();
  await expect(page.getByLabel('链名称', { exact: true })).toHaveValue('测试链');
  await page.getByLabel('把 洛杉矶 CN2 GIA 移出本链').click();
  await expect(page.locator('.cr-path .chain-rule-node-head b').filter({ hasText: '洛杉矶 CN2 GIA' })).toHaveCount(0);
  await expect(submit(page)).toBeEnabled();
});

test('production entry group is a default and switching preserves the full option order', async t => {
  const page = await preview(t, { view: 'current' });
  const group = page.getByLabel('分组', { exact: true });
  const options = await group
    .locator('option')
    .evaluateAll(items => items.map(item => ({ value: item.value, label: item.textContent })));
  const existing = options.filter(option => !option.label?.includes('新建分组'));
  assert.ok(existing.length > 1, 'sample data includes multiple visible groups');
  await expect(group).toHaveValue(existing[0].value);
  await group.selectOption(existing.at(-1).value);
  await expect(group).toHaveValue(existing.at(-1).value);
  assert.deepEqual(
    await group
      .locator('option')
      .evaluateAll(items => items.map(item => ({ value: item.value, label: item.textContent }))),
    options,
  );
  await expect(page.getByLabel('链名称', { exact: true })).toHaveValue('测试链');
  await group.selectOption(options.find(option => option.label?.includes('新建分组')).value);
  await expect(page.getByLabel('新分组名称', { exact: true })).toBeVisible();
  await group.selectOption(existing[0].value);
  await expect(group).toHaveValue(existing[0].value);
  await expect(page.getByLabel('新分组名称', { exact: true })).toHaveCount(0);
});

test('authorization uses consistent user avatars without repeating exit facts', async t => {
  const page = await preview(t);
  await expect(page.locator('.cr-users-head > b')).toHaveText('授权用户');
  await expect(page.locator('.cr-basic').getByText('出口地区', { exact: true })).toHaveCount(0);
  await expect(page.locator('.cr-basic').getByText('谁能用', { exact: true })).toHaveCount(0);
  const users = page.locator('.cr-user');
  assert.equal(await page.locator('.cr-user .user-generated-avatar').count(), await users.count());
  const alice = page.getByRole('button', { name: 'alice', exact: true });
  const identity = await alice.locator('.user-generated-avatar').getAttribute('data-avatar-mark');
  await alice.click();
  await expect(alice).toHaveAttribute('aria-pressed', 'false');
  await expect(alice.locator('.user-generated-avatar')).toHaveAttribute('data-avatar-mark', identity);
  await alice.click();
  await expect(alice.locator('.cr-user-check')).toBeVisible();
  await expect(page.locator('.cr-path .chain-rule-node-head b').filter({ hasText: '东京 IIJ' })).toBeVisible();
  await expect(page.locator('.protocol-choice.cr-ingress-card')).toHaveCount(5);
  assert.equal(await page.locator('.cr-ingress-card > input[type=checkbox]').count(), 5);
});

test('new path rules default to geosite while preserving match menu order', async t => {
  const page = await preview(t);
  const addRule = page.getByRole('button', { name: '＋ 加一条', exact: true }).first();
  await addRule.click();
  const first = page.getByLabel('香港 BGP 三网 第 1 条匹配条件', { exact: true });
  await expect(first).toHaveValue('geosite');
  const options = await first.locator('option').evaluateAll(nodes => nodes.map(node => node.value));
  assert.deepEqual(options, [
    'any',
    'sniffing_failed',
    'geosite',
    'geoip',
    'domain_suffix',
    'domain_keyword',
    'domain_regex',
    'ip_cidr',
    'port',
    'network',
  ]);
  await first.selectOption('domain_keyword');
  await page.getByLabel('香港 BGP 三网 第 1 条匹配取值', { exact: true }).fill('google');
  await addRule.click();
  await expect(first).toHaveValue('domain_keyword');
  const second = page.getByLabel('香港 BGP 三网 第 2 条匹配条件', { exact: true });
  await expect(second).toHaveValue('geosite');
  await second.selectOption('domain_suffix');
  await page.getByLabel('香港 BGP 三网 第 2 条匹配取值', { exact: true }).fill('example.com');
  await addRule.click();
  await expect(second).toHaveValue('domain_suffix');
  await expect(page.getByLabel('香港 BGP 三网 第 3 条匹配条件', { exact: true })).toHaveValue('geosite');
});

test('search, selection and inline user creation preserve keyboard behavior', async t => {
  const page = await preview(t);
  const search = page.getByRole('searchbox', { name: '搜索可用用户' });
  await search.fill('alice');
  await expect(page.locator('.cr-user')).toHaveCount(1);
  await page.getByRole('button', { name: '清空', exact: true }).click();
  await expect(page.locator('.cr-user[aria-pressed=true]')).toHaveCount(0);
  await page.getByRole('button', { name: '全选', exact: true }).click();
  await expect(page.locator('.cr-user[aria-pressed=true]')).toHaveCount(1);
  await search.fill('');
  await expect(page.getByRole('button', { name: 'mallory 已停用', exact: true })).toBeDisabled();
  await page.getByRole('button', { name: '新增用户', exact: true }).click();
  await page.getByLabel('新用户 ID').fill('alice');
  await expect(page.getByRole('alert')).toHaveText('这个用户已存在');
  await page.getByLabel('新用户 ID').fill('new-user');
  await page.getByLabel('新用户 ID').press('Enter');
  await expect(page.getByRole('button', { name: 'new-user', exact: true })).toHaveAttribute('aria-pressed', 'true');
  await expect(page.locator('.cr-submit-state')).toHaveText('配置完整');
  await page.getByRole('button', { name: '新增用户', exact: true }).click();
  await page.getByLabel('新用户 ID').press('Escape');
  await expect(page.getByLabel('新用户 ID')).toHaveCount(0);
});

test('ports detect conflicts, reset to automatic and retain explicit values across entry changes', async t => {
  const page = await preview(t);
  const vless = page.getByLabel('VLESS 监听端口', { exact: true });
  const anytls = page.getByLabel('AnyTLS 监听端口', { exact: true });
  const port = '25444';
  await anytls.fill(port);
  await vless.fill(port);
  await expect(vless).toHaveAttribute('aria-invalid', 'true');
  await expect(submit(page)).toBeDisabled();
  await page.getByLabel('VLESS 监听端口恢复自动分配').click();
  await expect(vless).not.toHaveAttribute('aria-invalid');
  await expect(submit(page)).toBeEnabled();
  await vless.fill('25443');
  await page.getByLabel('换一台当入口').click();
  await page.getByLabel('选择入口节点').selectOption('hk-hkix');
  await expect(vless).toHaveValue('25443');
  const hy2 = page.getByRole('checkbox', { name: 'Hysteria 2', exact: true });
  await hy2.focus();
  await hy2.press('Space');
  await expect(hy2).not.toBeChecked();
  await expect(page.getByLabel('Hysteria 2 起始端口', { exact: true })).toBeDisabled();
  await hy2.check();
  await expect(hy2).toBeChecked();
  await expect(page.getByLabel('Hysteria 2 起始端口', { exact: true })).toBeVisible();
});

test('protocol details validate local custom settings without replacing the list', async t => {
  const page = await preview(t, {}, 422);
  await page.getByLabel('VLESS · TLS/REALITY 参数', { exact: true }).click();
  await page
    .getByRole('group', { name: 'REALITY 伪装目标', exact: true })
    .getByRole('button', { name: '自定义', exact: true })
    .click();
  await page.getByLabel('自定义 REALITY 目标').fill('');
  await page.getByLabel('自定义 REALITY SNI').fill('');
  await expect(submit(page)).toBeDisabled();
  await page.getByLabel('自定义 REALITY 目标').fill('example.com:443');
  await page.getByLabel('自定义 REALITY SNI').fill('example.com');
  await expect(submit(page)).toBeEnabled();
  await page.getByLabel('自定义 REALITY 目标').fill('example.com:99999');
  await expect(submit(page)).toBeDisabled();
  await page.getByLabel('自定义 REALITY 目标').fill('example.com:443');
  await page.getByLabel('自定义 REALITY SNI').fill('*.example.com');
  await expect(submit(page)).toBeDisabled();
  await page.getByLabel('自定义 REALITY SNI').fill('example.com');
  await page.getByLabel('自定义 REALITY 指纹').selectOption('firefox');
  await expect(submit(page)).toBeEnabled();
  await expect(page.getByRole('checkbox', { name: 'AnyTLS', exact: true })).toBeVisible();
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
  await page.screenshot({ path: `${screenshots}/mobile-protocol-details.png`, fullPage: true });
});

test('full descriptions stay readable and visible for every protocol, including disabled options', async t => {
  const page = await preview(t, {}, 422);
  const descriptions = [
    '支持 TCP / XHTTP，以 TLS 加密或 REALITY 伪装保护连接，适用于通用代理接入。',
    'VLESS 原生加密，适用于无证书加密接入。',
    '通过分包填充缓解嵌套 TLS 特征，以连接复用降低握手开销，适用于短连接密集场景。',
    '基于 QUIC / UDP，针对高延迟与丢包链路优化吞吐，适用于弱网场景。',
    'Telegram 原生支持的专用代理。',
  ];
  await expect(page.locator('.cr-ingress-description')).toHaveText(descriptions);
  for (const description of descriptions) await expect(page.getByText(description, { exact: true })).toBeVisible();
  await expect(page.getByLabel('VLESS Encryption 监听端口', { exact: true })).toBeDisabled();
  await page.locator('[data-protocol=encryption] .protocol-choice-copy').click();
  await expect(page.getByRole('checkbox', { name: 'VLESS · Encryption', exact: true })).toBeChecked();
  await expect(page.getByLabel('VLESS Encryption 监听端口', { exact: true })).toBeEnabled();
  const start = await page.getByLabel('Hysteria 2 起始端口', { exact: true }).boundingBox();
  const end = await page.getByLabel('Hysteria 2 结束端口', { exact: true }).boundingBox();
  assert.equal(start.y, end.y, 'a port range remains horizontal on mobile');
  assert.ok(start.height >= 44 && end.height >= 44, 'touch fields remain usable');
  const typography = await page.locator('.cr-ingress-description').evaluateAll(nodes =>
    nodes.map(node => {
      const style = getComputedStyle(node);
      return { font: parseFloat(style.fontSize), clamp: style.webkitLineClamp, overflow: style.textOverflow };
    }),
  );
  assert.ok(typography.every(style => style.font >= 12 && style.clamp === 'none' && style.overflow !== 'ellipsis'));
});

test('complete protocol settings retain custom values across toggles and entry replacement', async t => {
  const page = await preview(t);
  await page.getByLabel('VLESS · TLS/REALITY 参数', { exact: true }).click();
  await expect(page.locator('.cr-ingress-certificate')).toContainText('hk-bgp.nodes.example.net');
  await page
    .getByRole('group', { name: 'REALITY 伪装目标', exact: true })
    .getByRole('button', { name: '全局站点', exact: true })
    .click();
  await expect(page.locator('.cr-ingress-facts')).toContainText('www.cloudflare.com');
  await page
    .getByRole('group', { name: 'REALITY 伪装目标', exact: true })
    .getByRole('button', { name: '自定义', exact: true })
    .click();
  await page.getByLabel('自定义 REALITY 目标').fill('example.org:443');
  await page.getByLabel('自定义 REALITY SNI').fill('example.org');
  await page.getByLabel('自定义 REALITY 指纹').selectOption('firefox');
  const vless = page.getByRole('checkbox', { name: 'VLESS · TLS/REALITY', exact: true });
  await vless.focus();
  await vless.press('Space');
  await vless.press('Space');
  await page.getByLabel('换一台当入口').click();
  await page.getByLabel('选择入口节点').selectOption('hk-hkix');
  await expect(page.getByLabel('自定义 REALITY 目标')).toHaveValue('example.org:443');
  await expect(page.getByLabel('自定义 REALITY 指纹')).toHaveValue('firefox');
  await page.getByLabel('AnyTLS 参数', { exact: true }).click();
  await expect(page.getByLabel('VLESS · TLS/REALITY 参数', { exact: true })).toHaveAttribute('aria-expanded', 'false');
  await page
    .getByRole('group', { name: 'AnyTLS Padding', exact: true })
    .getByRole('button', { name: '自定义', exact: true })
    .click();
  await expect(submit(page)).toBeDisabled();
  await page.getByLabel('AnyTLS 自定义 Padding').fill('0=30-30');
  await expect(submit(page)).toBeEnabled();
  await page.getByRole('checkbox', { name: 'VLESS · Encryption', exact: true }).press('Space');
  await page.getByLabel('VLESS · Encryption 参数', { exact: true }).click();
  await expect(page.getByText('会话票据有效期', { exact: false })).toContainText('600s');
});

test('Hysteria exposes range counts, restores both automatic endpoints and labels bandwidth units', async t => {
  const page = await preview(t, {}, 422);
  await page.getByLabel('Hysteria 2 参数', { exact: true }).click();
  await expect(page.locator('.cr-ingress-range-value')).toContainText('100 个端口');
  await page.getByLabel('Hysteria 2 起始端口', { exact: true }).fill('32000');
  await page.getByLabel('Hysteria 2 结束端口', { exact: true }).fill('32009');
  await expect(page.locator('.cr-ingress-range-value')).toContainText('10 个端口');
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
  await page.getByRole('button', { name: '恢复自动范围', exact: true }).click();
  await expect(page.getByLabel('Hysteria 2 起始端口', { exact: true })).toHaveValue('30000');
  await expect(page.getByLabel('Hysteria 2 结束端口', { exact: true })).toHaveValue('30099');
  await expect(page.locator('.cr-ingress-range-value')).toContainText('100 个端口');
  await expect(page.getByText('留空使用 BBR；固定带宽需成对填写，如', { exact: false })).toContainText('200 mbps');
  await page.getByLabel('Hysteria 2 上行带宽').fill('200 mbps');
  await expect(submit(page)).toBeDisabled();
  await page.getByLabel('Hysteria 2 下行带宽').fill('500 mbps');
  await expect(submit(page)).toBeEnabled();
  await page.screenshot({ path: `${screenshots}/mobile-hysteria-details.png`, fullPage: true });
});

test('capability errors explain the affected protocol while its checkbox and description stay available', async t => {
  const page = await preview(t, { scene: 'blocked' });
  const mtproxy = page.locator('[data-protocol=mtproto]');
  await expect(mtproxy.getByRole('alert')).toContainText('公网 IPv4 未直接配置在网卡上');
  await expect(mtproxy.getByRole('checkbox')).toBeEnabled();
  await expect(
    mtproxy.getByText('Telegram 原生支持的专用代理。', {
      exact: true,
    }),
  ).toBeVisible();
  for (const name of ['VLESS · TLS/REALITY', 'VLESS · Encryption', 'AnyTLS', 'Hysteria 2', 'MTProxy']) {
    const toggle = page.getByRole('checkbox', { name, exact: true });
    if (await toggle.isChecked()) await toggle.press('Space');
  }
  await expect(page.getByText('至少开启一个接入协议。', { exact: true })).toBeVisible();
  await expect(submit(page)).toBeDisabled();
  await expect(page.locator('.cr-ingress-description')).toHaveCount(5);
});

test('blank start derives route facts and draft submission stays local', async t => {
  const page = await preview(t, { scene: 'blank' });
  await expect(submit(page)).toBeDisabled();
  await page.getByLabel('选择入口节点').selectOption('hk-hkix');
  await expect(page.locator('.cr-path .chain-rule-node-head b').filter({ hasText: '香港 HKIX' })).toBeVisible();
  await expect(page.getByLabel('链名称', { exact: true })).toHaveValue('');
  await expect(submit(page)).toBeDisabled();
  await page.getByLabel('链名称', { exact: true }).fill('手动命名的链');
  await expect(submit(page)).toBeEnabled();
  await page.getByRole('button', { name: '配置预览', exact: true }).click();
  await expect(page.locator('.cr-preview pre')).toContainText('香港 HKIX');
  const writes = [];
  page.on('request', request => {
    if (request.method() !== 'GET') writes.push(request.url());
  });
  await submit(page).click();
  await expect(page.getByRole('button', { name: '加入中…', exact: true })).toBeDisabled();
  await expect(page.getByRole('status')).toHaveText('已加入示例草稿');
  assert.deepEqual(writes, []);
  await page.getByRole('button', { name: '取消', exact: true }).click();
  await expect(page.getByText('已取消新建链', { exact: true })).toBeVisible();
  await page.getByRole('button', { name: '重新打开', exact: true }).click();
  await expect(page.getByLabel('选择入口节点')).toBeVisible();
});

test('all scenes fit desktop, tablet and mobile in both themes', async t => {
  for (const theme of ['light', 'dark']) {
    for (const width of [1180, 620, 390]) {
      // Release each viewport's browser context before the next seven full-page captures.
      const page = await preview(t, { theme, w: String(width) }, width + 32);
      for (const scene of ['blank', 'direct', 'relay', 'branches', 'blocked', 'node', 'crowd']) {
        await page.goto(`${base}?${new URLSearchParams({ scene, theme, w: String(width), nav: '0' })}`);
        await page.locator('.cr-paper').waitFor();
        await expect(page.locator('html')).toHaveAttribute('data-theme', theme);
        assert.equal(
          await page.evaluate(() => document.documentElement.scrollWidth > innerWidth),
          false,
          `${scene}/${theme}/${width}`,
        );
        if (scene === 'blocked') await expect(submit(page)).toBeDisabled();
        if (scene === 'node') await expect(page.getByLabel('换一台当入口')).toHaveCount(0);
        if (scene === 'crowd') await expect(page.locator('.cr-user')).toHaveCount(41);
        await page.screenshot({ path: `${screenshots}/${scene}-${theme}-${width}.png`, fullPage: true });
      }
      await page.close();
    }
  }
});

test('review toolbar can switch to production and Claude Code comparisons', async t => {
  const page = await preview(t, { nav: '1' });
  await page
    .getByRole('group', { name: '预览版本', exact: true })
    .getByRole('button', { name: '现有界面', exact: true })
    .click();
  await expect(page.locator('.pv-chain-fields')).toBeVisible();
  await page
    .getByRole('group', { name: '预览版本', exact: true })
    .getByRole('button', { name: 'Claude 原方案', exact: true })
    .click();
  await expect(page.locator('.cw-paper')).toBeVisible();
  await page
    .getByRole('group', { name: '预览版本', exact: true })
    .getByRole('button', { name: '保留原布局', exact: true })
    .click();
  await expect(page.locator('.cr-paper')).toBeVisible();
});
