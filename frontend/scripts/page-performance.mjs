#!/usr/bin/env node

import { chromium } from '@playwright/test';
import { performance as nodePerformance } from 'node:perf_hooks';
import { pathToFileURL } from 'node:url';

const ROUTES = Object.freeze({
  nodes: { hash: '#/nodes', label: '机器' },
  tunnels: { hash: '#/tunnels', label: '隧道' },
  chains: { hash: '#/chains', label: '线路' },
  users: { hash: '#/users', label: '用户' },
  deploy: { hash: '#/deploy', label: '发布' },
  usage: { hash: '#/usage', label: '用量' },
  settings: { hash: '#/settings', label: '设置' },
  topo: { hash: '#/topo', label: '拓扑' },
});

const DEFAULT_ROUTES = Object.keys(ROUTES);
const PUBLIC_ROUTES = DEFAULT_ROUTES.filter(name => !['deploy', 'settings'].includes(name));
const API_INITIATORS = new Set(['fetch', 'xmlhttprequest']);

const round = value => (Number.isFinite(value) ? Math.round(value * 10) / 10 : null);

export function percentile(values, fraction) {
  if (!values.length) return null;
  const ordered = [...values].sort((left, right) => left - right);
  return ordered[Math.max(0, Math.ceil(ordered.length * fraction) - 1)];
}

export function parseBaseUrl(value) {
  const url = new URL(value ?? 'http://127.0.0.1:4173');
  const loopback = ['localhost', '127.0.0.1', '[::1]'].includes(url.hostname);
  if (
    !['http:', 'https:'].includes(url.protocol) ||
    url.username ||
    url.password ||
    url.search ||
    url.hash ||
    url.pathname !== '/' ||
    (url.protocol === 'http:' && !loopback)
  ) {
    throw new Error('BROCADE_BASE_URL must be an HTTPS origin or a loopback HTTP origin without credentials or path');
  }
  return url;
}

export function parseRoutes(value) {
  const names = value
    ? value
        .split(',')
        .map(name => name.trim())
        .filter(Boolean)
    : DEFAULT_ROUTES;
  if (!names.length || new Set(names).size !== names.length || names.some(name => !Object.hasOwn(ROUTES, name))) {
    throw new Error(`BROCADE_FRONTEND_ROUTES must contain unique route names: ${DEFAULT_ROUTES.join(',')}`);
  }
  return names.map(name => ({ name, ...ROUTES[name] }));
}

export function parseRepeats(value) {
  const repeats = Number(value ?? '3');
  if (!Number.isInteger(repeats) || repeats < 2 || repeats > 10) {
    throw new Error('BROCADE_FRONTEND_REPEATS must be an integer from 2 to 10');
  }
  return repeats;
}

export function parseBudget(value) {
  if (value == null || value === '') return null;
  const budget = Number(value);
  if (!Number.isFinite(budget) || budget <= 0) {
    throw new Error('BROCADE_FRONTEND_P95_BUDGET_MS must be a positive number');
  }
  return budget;
}

function resourceSummary(samples) {
  const resources = new Map();
  for (const sample of samples) {
    for (const resource of sample.resources.filter(item => API_INITIATORS.has(item.initiator_type))) {
      const current = resources.get(resource.name) ?? { durations: [], transferBytes: [] };
      current.durations.push(resource.duration_ms);
      current.transferBytes.push(resource.transfer_bytes);
      resources.set(resource.name, current);
    }
  }
  return [...resources]
    .map(([name, values]) => ({
      name,
      p50_ms: round(percentile(values.durations, 0.5)),
      p95_ms: round(percentile(values.durations, 0.95)),
      transfer_p50_bytes: round(percentile(values.transferBytes, 0.5)),
      samples: values.durations.length,
    }))
    .sort((left, right) => (right.p95_ms ?? 0) - (left.p95_ms ?? 0))
    .slice(0, 5);
}

export function summarizeSamples(samples) {
  const values = key => samples.map(sample => sample[key]).filter(Number.isFinite);
  return {
    samples: samples.length,
    ready_p50_ms: round(percentile(values('ready_ms'), 0.5)),
    ready_p95_ms: round(percentile(values('ready_ms'), 0.95)),
    ttfb_p50_ms: round(percentile(values('ttfb_ms'), 0.5)),
    fcp_p50_ms: round(percentile(values('fcp_ms'), 0.5)),
    transfer_p50_bytes: round(percentile(values('transfer_bytes'), 0.5)),
    decoded_p50_bytes: round(percentile(values('decoded_bytes'), 0.5)),
    long_task_p95_ms: round(percentile(values('long_task_ms'), 0.95)),
    failed_samples: samples.filter(sample => sample.failed).length,
    browser_errors: samples.reduce((sum, sample) => sum + sample.browser_errors, 0),
    http_errors: samples.reduce((sum, sample) => sum + sample.http_errors, 0),
    slowest_api: resourceSummary(samples),
  };
}

function monitorPage(page, baseOrigin) {
  const counts = { browserErrors: 0, httpErrors: 0 };
  page.on('pageerror', () => counts.browserErrors++);
  page.on('console', message => {
    if (message.type() === 'error') counts.browserErrors++;
  });
  page.on('requestfailed', request => {
    if (new URL(request.url()).origin === baseOrigin) counts.httpErrors++;
  });
  page.on('response', response => {
    if (new URL(response.url()).origin === baseOrigin && response.status() >= 400) counts.httpErrors++;
  });
  return counts;
}

async function configureContext(context, baseOrigin, token) {
  if (token) {
    await context.route('**/*', async route => {
      const request = route.request();
      if (new URL(request.url()).origin !== baseOrigin) return route.continue();
      await route.continue({
        headers: { ...request.headers(), authorization: `Bearer ${token}` },
      });
    });
  }
  await context.addInitScript(() => {
    globalThis.__brocadeLongTasks = [];
    try {
      new PerformanceObserver(entries => {
        globalThis.__brocadeLongTasks.push(
          ...entries.getEntries().map(entry => ({ startTime: entry.startTime, duration: entry.duration })),
        );
      }).observe({ type: 'longtask', buffered: true });
    } catch {
      // Long-task timing is optional. Route readiness and resource timings remain available.
    }
  });
}

async function waitForRoute(page, route, timeoutMs) {
  const state = await page.waitForFunction(
    ({ hash, label }) => {
      if (document.querySelector('.login-main')) return 'login';
      const surface = [...document.querySelectorAll('[role="main"]')].find(
        element => element.getAttribute('aria-label') === `${label}内容`,
      );
      if (location.hash === hash && surface && !surface.querySelector('.loading-boundary-guard')) return 'ready';
      return false;
    },
    route,
    { timeout: timeoutMs },
  );
  if ((await state.jsonValue()) === 'login') {
    throw new Error('authentication required; provide BROCADE_ADMIN_TOKEN or BROCADE_PERF_STORAGE_STATE');
  }
  await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
}

async function browserMetrics(page, since, readyMs, counts) {
  const metrics = await page.evaluate(startTime => {
    const navigation = performance.getEntriesByType('navigation')[0];
    const paints = performance.getEntriesByType('paint');
    const fcp = paints.find(entry => entry.name === 'first-contentful-paint');
    const resources = performance
      .getEntriesByType('resource')
      .filter(entry => entry.startTime >= startTime)
      .map(entry => {
        const url = new URL(entry.name);
        const queryKeys = [...new Set(url.searchParams.keys())].sort();
        const sameOrigin = url.origin === location.origin;
        return {
          name: `${sameOrigin ? '' : '[cross-origin]'}${url.pathname}${queryKeys.length ? `?${queryKeys.join('&')}` : ''}`,
          initiator_type: entry.initiatorType,
          duration_ms: entry.duration,
          transfer_bytes: entry.transferSize,
          decoded_bytes: entry.decodedBodySize,
        };
      });
    const longTasks = (globalThis.__brocadeLongTasks ?? []).filter(entry => entry.startTime >= startTime);
    return {
      ttfb_ms: startTime === 0 && navigation ? navigation.responseStart - navigation.requestStart : null,
      fcp_ms: startTime === 0 && fcp ? fcp.startTime : null,
      transfer_bytes: resources.reduce((sum, resource) => sum + resource.transfer_bytes, 0),
      decoded_bytes: resources.reduce((sum, resource) => sum + resource.decoded_bytes, 0),
      long_task_ms: longTasks.reduce((sum, task) => sum + task.duration, 0),
      resources,
    };
  }, since);
  return {
    ...metrics,
    ready_ms: readyMs,
    browser_errors: counts.browserErrors,
    http_errors: counts.httpErrors,
  };
}

async function newContext(browser, options, baseOrigin, token) {
  const context = await browser.newContext({
    viewport: { width: 1440, height: 1000 },
    reducedMotion: 'reduce',
    serviceWorkers: 'block',
    ...options,
  });
  await configureContext(context, baseOrigin, token);
  return context;
}

async function measureDirect(browser, config, route) {
  const context = await newContext(browser, config.contextOptions, config.base.origin, config.token);
  try {
    const page = await context.newPage();
    const counts = monitorPage(page, config.base.origin);
    const started = nodePerformance.now();
    const response = await page.goto(new URL(route.hash, config.base).href, {
      waitUntil: 'domcontentloaded',
      timeout: config.timeoutMs,
    });
    if (!response || response.status() >= 400) throw new Error(`${route.name}: document request failed`);
    await waitForRoute(page, route, config.timeoutMs);
    return await browserMetrics(page, 0, round(nodePerformance.now() - started), counts);
  } finally {
    await context.close();
  }
}

async function measureTransition(browser, config, route) {
  const context = await newContext(browser, config.contextOptions, config.base.origin, config.token);
  try {
    const page = await context.newPage();
    const counts = monitorPage(page, config.base.origin);
    const source = route.name === 'nodes' ? ROUTES.usage : ROUTES.nodes;
    const response = await page.goto(new URL(source.hash, config.base).href, {
      waitUntil: 'domcontentloaded',
      timeout: config.timeoutMs,
    });
    if (!response || response.status() >= 400) throw new Error(`${route.name}: document request failed`);
    await waitForRoute(page, source, config.timeoutMs);
    counts.browserErrors = 0;
    counts.httpErrors = 0;
    const since = await page.evaluate(() => {
      performance.clearResourceTimings();
      globalThis.__brocadeLongTasks = [];
      return performance.now();
    });
    const started = nodePerformance.now();
    await page.evaluate(hash => {
      location.hash = hash;
    }, route.hash);
    await waitForRoute(page, route, config.timeoutMs);
    return await browserMetrics(page, since, round(nodePerformance.now() - started), counts);
  } finally {
    await context.close();
  }
}

export function formatBytes(bytes) {
  if (bytes == null) return '—';
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 102.4) / 10} KiB`;
  return `${Math.round(bytes / 1024 / 102.4) / 10} MiB`;
}

function printHuman(report) {
  process.stdout.write(`Brocade 前端页面基线 · ${report.captured_at}\n`);
  process.stdout.write(
    `会话 ${report.session_mode}；每个场景 ${report.repeats} 次；完整打开使用独立浏览器上下文，站内切换从另一页面进入。\n\n`,
  );
  const rows = report.routes.map(route => ({
    页面: route.label,
    '完整打开 P50': `${route.direct.ready_p50_ms} ms`,
    '完整打开 P95': `${route.direct.ready_p95_ms} ms`,
    '站内切换 P50': `${route.transition.ready_p50_ms} ms`,
    '站内切换 P95': `${route.transition.ready_p95_ms} ms`,
    '传输 P50': formatBytes(route.direct.transfer_p50_bytes),
    错误:
      route.direct.failed_samples +
      route.transition.failed_samples +
      route.direct.browser_errors +
      route.direct.http_errors +
      route.transition.browser_errors +
      route.transition.http_errors,
  }));
  console.table(rows);
  for (const route of report.routes) {
    const slow = route.direct.slowest_api[0];
    if (slow) process.stdout.write(`${route.label}最慢接口：${slow.name} · P95 ${slow.p95_ms} ms\n`);
  }
  if (report.budget_ms != null) {
    const failed = report.routes.filter(route => route.over_budget).map(route => route.label);
    process.stdout.write(
      `\nP95 预算 ${report.budget_ms} ms：${failed.length ? `超限 ${failed.join('、')}` : '全部通过'}\n`,
    );
  }
}

async function measuredSample(measure, route, mode, sampleNumber, timeoutMs) {
  try {
    return await measure();
  } catch (error) {
    if (error instanceof Error && error.message.startsWith('authentication required')) throw error;
    const reason =
      error instanceof Error && /timeout/i.test(error.message) ? `timed out after ${timeoutMs} ms` : 'failed';
    process.stderr.write(`${route.name} ${mode} sample ${sampleNumber} ${reason}\n`);
    return {
      ready_ms: timeoutMs,
      ttfb_ms: null,
      fcp_ms: null,
      transfer_bytes: 0,
      decoded_bytes: 0,
      long_task_ms: 0,
      browser_errors: 0,
      http_errors: 0,
      resources: [],
      failed: true,
    };
  }
}

export async function main(env = process.env, argv = process.argv.slice(2)) {
  if (argv.some(argument => !['--json', '--help'].includes(argument))) {
    throw new Error('supported arguments: --json, --help');
  }
  if (argv.includes('--help')) {
    process.stdout.write(`Usage: npm run perf:pages -- [--json]\n\n`);
    process.stdout.write(
      `BROCADE_BASE_URL                  HTTPS or loopback origin (default http://127.0.0.1:4173)\n`,
    );
    process.stdout.write(`BROCADE_ADMIN_TOKEN                optional bearer token, sent only to the target origin\n`);
    process.stdout.write(`BROCADE_PERF_STORAGE_STATE         optional Playwright storage-state JSON path\n`);
    process.stdout.write(
      `BROCADE_FRONTEND_ROUTES            comma-separated route names (${DEFAULT_ROUTES.join(',')}); public default excludes deploy/settings\n`,
    );
    process.stdout.write(`BROCADE_FRONTEND_REPEATS           2..10 (default 3)\n`);
    process.stdout.write(`BROCADE_FRONTEND_TIMEOUT_MS        per navigation (default 30000)\n`);
    process.stdout.write(`BROCADE_FRONTEND_P95_BUDGET_MS     optional failure threshold\n`);
    return { help: true };
  }

  const base = parseBaseUrl(env.BROCADE_BASE_URL);
  const token = env.BROCADE_ADMIN_TOKEN?.trim();
  const hasStoredSession = Boolean(env.BROCADE_PERF_STORAGE_STATE);
  const routes = parseRoutes(
    env.BROCADE_FRONTEND_ROUTES ?? (!token && !hasStoredSession ? PUBLIC_ROUTES.join(',') : undefined),
  );
  const repeats = parseRepeats(env.BROCADE_FRONTEND_REPEATS);
  const budgetMs = parseBudget(env.BROCADE_FRONTEND_P95_BUDGET_MS);
  const timeoutMs = Number(env.BROCADE_FRONTEND_TIMEOUT_MS ?? '30000');
  if (!Number.isInteger(timeoutMs) || timeoutMs < 1_000 || timeoutMs > 120_000) {
    throw new Error('BROCADE_FRONTEND_TIMEOUT_MS must be an integer from 1000 to 120000');
  }
  const contextOptions = env.BROCADE_PERF_STORAGE_STATE ? { storageState: env.BROCADE_PERF_STORAGE_STATE } : {};
  const config = { base, token, contextOptions, timeoutMs };
  const sessionMode = token ? 'bearer-token' : hasStoredSession ? 'storage-state' : 'automatic-public';
  const browser = await chromium.launch({ headless: true });
  const measured = [];
  try {
    // Public consoles still create a normal HttpOnly session. Establish it once and retain the
    // storage state in memory, otherwise every isolated sample would create another public session.
    if (!token && !env.BROCADE_PERF_STORAGE_STATE) {
      const context = await newContext(browser, {}, base.origin, undefined);
      try {
        const page = await context.newPage();
        const response = await page.goto(new URL(ROUTES.nodes.hash, base).href, {
          waitUntil: 'domcontentloaded',
          timeout: timeoutMs,
        });
        if (!response || response.status() >= 400) throw new Error('nodes: document request failed');
        await waitForRoute(page, { name: 'nodes', ...ROUTES.nodes }, timeoutMs);
        config.contextOptions = { storageState: await context.storageState() };
      } finally {
        await context.close();
      }
    }
    for (const route of routes) {
      const direct = [];
      const transition = [];
      for (let index = 0; index < repeats; index++) {
        direct.push(
          await measuredSample(() => measureDirect(browser, config, route), route, 'direct', index + 1, timeoutMs),
        );
        transition.push(
          await measuredSample(
            () => measureTransition(browser, config, route),
            route,
            'transition',
            index + 1,
            timeoutMs,
          ),
        );
      }
      measured.push({ route, direct, transition });
    }
  } finally {
    await browser.close();
  }

  const report = {
    schema: 1,
    captured_at: new Date().toISOString(),
    origin: base.origin,
    session_mode: sessionMode,
    repeats,
    budget_ms: budgetMs,
    note: 'Complete-open samples use isolated browser contexts. They are not proof of a cold server or operating-system cache. Query values and credentials are omitted.',
    routes: measured
      .map(({ route, direct, transition }) => {
        const directSummary = summarizeSamples(direct);
        const transitionSummary = summarizeSamples(transition);
        return {
          name: route.name,
          label: route.label,
          direct: directSummary,
          transition: transitionSummary,
          over_budget:
            budgetMs != null && (directSummary.ready_p95_ms > budgetMs || transitionSummary.ready_p95_ms > budgetMs),
        };
      })
      .sort((left, right) => right.direct.ready_p95_ms - left.direct.ready_p95_ms),
  };
  if (argv.includes('--json')) process.stdout.write(`${JSON.stringify(report, null, 2)}\n`);
  else printHuman(report);
  if (report.routes.some(route => route.direct.failed_samples || route.transition.failed_samples)) process.exitCode = 1;
  else if (report.routes.some(route => route.over_budget)) process.exitCode = 2;
  return report;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().catch(error => {
    const safe =
      /^(BROCADE_|authentication required|supported arguments|\w+: document request failed|\w+ (direct|transition) sample)/.test(
        error.message,
      );
    process.stderr.write(
      `${safe ? error.message : 'frontend baseline failed; inspect connectivity without logging credentials'}\n`,
    );
    process.exitCode = 1;
  });
}
