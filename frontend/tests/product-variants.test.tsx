import { afterEach, describe, expect, it, vi } from 'vitest';
import { fetchConsoleBootstrap } from '../src/api';
import { CVM_ENABLED } from '../src/product';
import { routeForViewer } from '../src/forge/access';
import { initialPaneFromHash } from '../src/panes/preload';

afterEach(() => vi.unstubAllGlobals());

describe('product variant boundary', () => {
  it('requires the server capability to match the compiled UI', async () => {
    for (const features of [undefined, {}, { cvm: !CVM_ENABLED }, { cvm: 'true' }]) {
      vi.stubGlobal(
        'fetch',
        vi.fn(async () => Response.json({ features })),
      );
      await expect(fetchConsoleBootstrap()).rejects.toThrow('产品变体不匹配');
    }
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => Response.json({ features: { cvm: CVM_ENABLED } })),
    );
    await expect(fetchConsoleBootstrap()).resolves.toMatchObject({ features: { cvm: CVM_ENABLED } });
  });

  it('resolves restored CVM routes before mounting a page', () => {
    expect(initialPaneFromHash('#/cvm/instance/old-id')).toBe(CVM_ENABLED ? 'cvm' : 'nodes');
    expect(
      routeForViewer(
        { nav: 'cvm', drill: { p: 'instance', id: 'old-id' } },
        {
          operator_id: 'root',
          role: 'system-admin',
          tenant_scope: null,
          token_prefix: '',
          masked_assets: false,
        },
      ),
    ).toEqual(CVM_ENABLED ? { nav: 'cvm', drill: { p: 'instance', id: 'old-id' } } : { nav: 'nodes' });
  });
});
