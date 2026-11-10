import { expect, type Page } from '@playwright/test';

export interface LayerCase {
  id: string;
  url: string;
  open: (page: Page) => Promise<void>;
  mode: 'modal' | 'drawer' | 'popover';
}

const userPage = '/tests/fixtures/user-header-preview.html?presence=sources';
const tunnelPage = '/tests/fixtures/tunnel-layout-preview.html';

export const layerCases: LayerCase[] = [
  {
    id: 'dynamic-confirmation',
    url: '/tests/fixtures/layer-coverage-preview.html?dynamic',
    mode: 'modal',
    open: async page => {
      await page.getByRole('button', { name: '打开确认', exact: true }).click();
      await page.locator('.confirm-card').boundingBox();
      await page.evaluate(() => window.dispatchEvent(new Event('fixture:grow-layer')));
      await expect(page.locator('.confirm-body li')).toHaveCount(80);
    },
  },
  {
    id: 'dynamic-choice',
    url: '/tests/fixtures/layer-coverage-preview.html?dynamic',
    mode: 'drawer',
    open: async page => {
      await page.getByRole('combobox', { name: '动态选项', exact: true }).click();
      await page.locator('.choice-menu').boundingBox();
      await page.evaluate(() => window.dispatchEvent(new Event('fixture:grow-layer')));
      await expect(page.locator('.choice-menu [role="option"]')).toHaveCount(80);
    },
  },
  {
    id: 'long-confirmation',
    url: '/tests/fixtures/layer-coverage-preview.html',
    mode: 'modal',
    open: async page => {
      await page.getByRole('button', { name: '打开确认', exact: true }).click();
    },
  },
  {
    id: 'source-rollout',
    url: '/tests/fixtures/layer-coverage-preview.html',
    mode: 'popover',
    open: async page => {
      await page.locator('button.source-ban-rollout').click();
    },
  },
  {
    id: 'user-password',
    url: userPage,
    mode: 'modal',
    open: async page => {
      await page.locator('.user-dhead-tools').getByRole('button', { name: '更多操作', exact: true }).click();
      await page.getByRole('menuitem', { name: /^修改密码/ }).click();
    },
  },
  {
    id: 'user-cycle-reset',
    url: userPage,
    mode: 'modal',
    open: async page => {
      await page
        .locator('.user-usage-card:not(.user-forwarding-usage)')
        .getByRole('button', { name: '重置', exact: true })
        .click();
    },
  },
  {
    id: 'user-service-policy',
    url: userPage,
    mode: 'popover',
    open: async page => {
      await page
        .getByRole('button', { name: /^重置日/ })
        .first()
        .click();
    },
  },
  {
    id: 'source-ban',
    url: userPage,
    mode: 'popover',
    open: async page => {
      await page.getByRole('button', { name: '封禁 198.51.100.77', exact: true }).click();
    },
  },
  {
    id: 'dns-import',
    url: '/tests/fixtures/dns-groups-preview.html?scene=editing',
    mode: 'modal',
    open: async page => {
      await page.getByRole('button', { name: '从节点导入', exact: true }).click();
    },
  },
  {
    id: 'dns-source-choice',
    url: '/tests/fixtures/dns-groups-preview.html?scene=editing',
    mode: 'drawer',
    open: async page => {
      await page.getByRole('button', { name: '从节点导入', exact: true }).click();
      await page.getByRole('dialog', { name: '从节点导入 DNS 策略' }).getByLabel('来源节点').click();
    },
  },
  {
    id: 'tunnel-kind',
    url: tunnelPage,
    mode: 'modal',
    open: async page => {
      await page.getByRole('button', { name: '＋ 新建隧道', exact: true }).click();
    },
  },
  {
    id: 'tunnel-editor',
    url: tunnelPage,
    mode: 'drawer',
    open: async page => {
      await page.getByRole('button', { name: '＋ 新建隧道', exact: true }).click();
      await page.getByRole('button', { name: /导入或自定义配置/ }).click();
      await expect(page.getByRole('dialog', { name: '创建隧道', exact: true })).toBeVisible();
    },
  },
  {
    id: 'mux-editor',
    url: '/tests/fixtures/mux-editor-preview.html',
    mode: 'drawer',
    open: async page => {
      await page.getByRole('button', { name: '配置参数', exact: true }).click();
    },
  },
  {
    id: 'chain-group-move',
    url: '/tests/fixtures/chain-unlock-preview.html?view=cards&scene=calm#/chains/chain/apac/hk-direct',
    mode: 'modal',
    open: async page => {
      await page.getByRole('button', { name: '移动到分组', exact: true }).click();
    },
  },
  {
    id: 'chain-unlock',
    url: '/tests/fixtures/node-unlock-preview.html?scope=chain&scene=typical',
    mode: 'modal',
    open: async page => {
      await page.getByRole('button', { name: '解锁', exact: true }).click();
      await expect(page.getByRole('article')).toHaveCount(13);
    },
  },
  {
    id: 'youtube-details',
    url: '/tests/fixtures/node-unlock-preview.html?scope=chain&youtube-details=1',
    mode: 'popover',
    open: async page => {
      await page.getByRole('button', { name: '解锁', exact: true }).click();
      await page.getByRole('button', { name: 'YouTube 分项', exact: true }).click();
    },
  },
  {
    id: 'node-inflow',
    url: '/tests/fixtures/inflow-details-preview.html',
    mode: 'modal',
    open: async page => {
      await expect(page.getByRole('list', { name: '来源', exact: true }).locator(':scope > li')).toHaveCount(13);
    },
  },
  {
    id: 'node-region',
    url: '/tests/fixtures/navigation-preview.html#/nodes',
    mode: 'modal',
    open: async page => {
      await page.locator('.node-card').first().click();
      await page.getByRole('button', { name: /^更换 .* 的地区/ }).click();
    },
  },
  {
    id: 'node-usage',
    url: '/tests/fixtures/node-traffic-acceptance.html?scene=solo&run=0',
    mode: 'modal',
    open: async page => {
      await page.clock.setFixedTime(new Date('2026-10-18T13:40:00Z'));
      await page.getByRole('button', { name: '香港测试机器', exact: true }).click();
      await expect(page.getByRole('dialog', { name: '香港测试机器 · 用量' }).locator('.node-usage-body')).toBeVisible();
      await expect(page.locator('.ndtp-bars .observe-plot-canvas canvas')).toBeVisible();
    },
  },
  {
    id: 'cvm-quota',
    url: '/tests/fixtures/cvm/preview.html?scene=list',
    mode: 'modal',
    open: async page => {
      await page.getByRole('tab', { name: '额度', exact: true }).click();
      await page.getByRole('button', { name: '编辑额度', exact: true }).click();
    },
  },
  {
    id: 'cvm-network',
    url: '/tests/fixtures/cvm/preview.html?scene=detail',
    mode: 'modal',
    open: async page => {
      await page.getByRole('tab', { name: '网络', exact: true }).click();
      await page.getByRole('button', { name: '路由表', exact: true }).click();
    },
  },
  {
    id: 'cvm-terminal',
    url: '/tests/fixtures/cvm/preview.html?scene=detail',
    mode: 'modal',
    open: async page => {
      await page.routeWebSocket('**/cvm/*/terminal/*/stream', socket => {
        socket.onMessage(message => {
          if (typeof message !== 'string' || !JSON.parse(message).ticket) return;
          socket.send(JSON.stringify({ type: 'ready' }));
          socket.send(Buffer.from('root@fixture:~# printf "你好"\r\n你好\r\n'));
        });
      });
      await page.getByRole('button', { name: '终端', exact: true }).click();
      // This audit covers the connection-state surface; terminal IO has its own transport suite.
      await expect(
        page.getByRole('dialog', { name: '终端 · home-workspace' }).locator('.cvm-terminal-screen'),
      ).toBeVisible();
    },
  },
];

export function themedUrl(url: string, theme: string) {
  const parsed = new URL(url, 'http://fixture.test');
  parsed.searchParams.set('theme', theme);
  return parsed.pathname + parsed.search + parsed.hash;
}

export async function layerMetrics(page: Page) {
  return page
    .locator('.dialog-layer')
    .last()
    .evaluate(layer => {
      const surface = layer.querySelector<HTMLElement>(':scope > .dialog-surface')!;
      const box = (element: Element) => {
        const rect = element.getBoundingClientRect();
        return { x: rect.x, y: rect.y, width: rect.width, height: rect.height, right: rect.right, bottom: rect.bottom };
      };
      const font = (element: Element) => {
        const style = getComputedStyle(element);
        return {
          family: style.fontFamily,
          size: style.fontSize,
          weight: style.fontWeight,
          lineHeight: style.lineHeight,
        };
      };
      const controls = [...surface.querySelectorAll<HTMLElement>('.btn, .segsw button, .choice-menu button')]
        .filter(element => element.getBoundingClientRect().width > 0)
        .map(element => ({
          text: (element.getAttribute('aria-label') ?? element.textContent ?? '').trim().slice(0, 60),
          className: element.className,
          rect: box(element),
          font: font(element),
        }));
      const scrim = getComputedStyle(layer.querySelector('.dialog-scrim')!);
      return {
        name: layer.getAttribute('aria-label'),
        mode: layer.className,
        surfaceClassName: surface.className,
        layer: box(layer),
        surface: box(surface),
        viewport: { width: innerWidth, height: innerHeight },
        layerFont: font(layer),
        surfaceFont: font(surface),
        controls,
        horizontalOverflow: surface.scrollWidth > surface.clientWidth + 1,
        pageOverflow: document.documentElement.scrollWidth > innerWidth + 1,
        filterSupport: {
          standard: CSS.supports('backdrop-filter', 'blur(4px)'),
          webkit: CSS.supports('-webkit-backdrop-filter', 'blur(4px)'),
          scrim: scrim.backdropFilter,
          webkitScrim: scrim.getPropertyValue('-webkit-backdrop-filter'),
        },
      };
    });
}
