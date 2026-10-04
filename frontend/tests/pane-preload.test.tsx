import { describe, expect, it } from 'vitest';
import { initialPaneFromHash } from '../src/panes/preload';

describe('removed links page', () => {
  it('preloads the machine list for an old links bookmark', () => {
    expect(initialPaneFromHash('#/links')).toBe('nodes');
    expect(initialPaneFromHash('#/links/mtu')).toBe('nodes');
  });

  it('preserves the remaining observation and configuration pages', () => {
    for (const page of ['nodes', 'chains', 'usage', 'settings', 'topo']) {
      expect(initialPaneFromHash(`#/${page}`)).toBe(page);
    }
  });
});
