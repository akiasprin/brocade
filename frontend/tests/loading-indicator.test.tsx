import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';
import { FieldLoading, LOADING_TEXT, PanelLoading } from '../src/ui/loading';

afterEach(cleanup);

describe('semantic loading indicators', () => {
  it('keeps panel progress independent of the global card styling', () => {
    render(<PanelLoading />);
    const indicator = screen.getByRole('status', { name: LOADING_TEXT });
    expect(indicator.classList.contains('panel')).toBe(false);
    expect(indicator.classList.contains('loading-mark-panel')).toBe(true);
    expect(indicator.getAttribute('aria-live')).toBe('polite');
  });

  it('keeps field progress compact with a namespaced modifier', () => {
    render(<FieldLoading label="正在加载规则…" />);
    const indicator = screen.getByRole('status', { name: '正在加载规则…' });
    expect(indicator.classList.contains('field')).toBe(false);
    expect(indicator.classList.contains('loading-mark-field')).toBe(true);
    expect(indicator.textContent).toBe('正在加载规则…');
  });

  it('lets the owning surface announce progress without a nested live region', () => {
    render(<PanelLoading announce={false} />);
    expect(screen.queryByRole('status')).toBeNull();
    expect(screen.getByText(LOADING_TEXT).closest('[aria-live]')).toBeNull();
  });
});
