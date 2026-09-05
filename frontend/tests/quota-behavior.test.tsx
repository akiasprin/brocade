import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { SnapshotApp } from '../src/api';
import { QuotaRow } from '../src/panes/users';

const GiB = 1024 ** 3;
const app: SnapshotApp = {
  id: 'global',
  label: '全球线路',
  chains: [],
  steps: [],
  ingresses: [],
  fronts: [],
  grants: [],
};

afterEach(cleanup);

describe('用户额度交互', () => {
  it('用量读取失败时显示未知，不把未知算成 0 或剩余额度', () => {
    render(
      <QuotaRow app={app} used={null} limit={10 * GiB} over={false} editable busy={false} onSave={vi.fn()} />,
    );

    expect(screen.getByText('—')).toBeTruthy();
    expect(screen.getByText('本月用量未知')).toBeTruthy();
    expect(screen.queryByText(/剩余/)).toBeNull();
  });

  it('保存失败时不退出编辑，也不丢掉刚输入的额度', async () => {
    const onSave = vi.fn().mockRejectedValue(new Error('额度保存失败'));
    render(<QuotaRow app={app} used={1 * GiB} limit={10 * GiB} over={false} editable busy={false} onSave={onSave} />);

    fireEvent.click(screen.getByRole('button', { name: '改额度' }));
    const input = screen.getByRole('textbox') as HTMLInputElement;
    fireEvent.change(input, { target: { value: '12' } });
    fireEvent.click(screen.getByRole('button', { name: '保存' }));

    await waitFor(() => expect(onSave).toHaveBeenCalledWith(12 * GiB));
    expect(screen.getByRole('textbox')).toBe(input);
    expect(input.value).toBe('12');
  });
});
