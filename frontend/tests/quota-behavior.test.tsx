import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { SnapshotApp } from '../src/api';
import { monthlyUsageText, QuotaRow } from '../src/panes/users';

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
  it('区分读取中、失败、无样本和实际用量', () => {
    expect(monthlyUsageText('pending', 0, 0)).toBe('读取中…');
    expect(monthlyUsageText('failed', 0, 0)).toBe('暂不可用');
    expect(monthlyUsageText('ready', 0, 0)).toBe('—');
    expect(monthlyUsageText('ready', 1, 2 * GiB)).toBe('2.00 GiB');
  });

  it('月度汇总仍在读取时只占位用量，不阻塞额度内容', () => {
    const { container } = render(
      <QuotaRow
        app={app}
        used={null}
        limit={10 * GiB}
        over={false}
        usageState="pending"
        editable
        busy={false}
        onSave={vi.fn()}
      />,
    );

    expect(screen.getByText('读取中…')).toBeTruthy();
    expect(screen.getByText('/ 10 GiB')).toBeTruthy();
    expect(container.querySelector('.qta-left')).toBeNull();
    expect(container.querySelector('.qta-t')).toBeNull();
  });

  it('月度汇总失败时明确标为不可用', () => {
    render(
      <QuotaRow
        app={app}
        used={null}
        limit={10 * GiB}
        over={false}
        usageState="failed"
        editable
        busy={false}
        onSave={vi.fn()}
      />,
    );

    expect(screen.getByText('—')).toBeTruthy();
    expect(screen.getByText('用量暂不可用')).toBeTruthy();
  });

  it('用量读取失败时显示未知，不把未知算成 0 或剩余额度', () => {
    const { container } = render(
      <QuotaRow app={app} used={null} limit={10 * GiB} over={false} editable busy={false} onSave={vi.fn()} />,
    );

    expect(screen.getByText('—')).toBeTruthy();
    expect(screen.getByText('用量未知')).toBeTruthy();
    expect(screen.queryByText(/剩余/)).toBeNull();
    expect(container.querySelector('.qta-r')?.classList.contains('unknown')).toBe(true);
    expect(container.querySelector('.qta-t')).toBeNull();
  });

  it('按额度比例分档：常态、注意（≥95%）、用尽', () => {
    const tone = (used: number, limit: number) => {
      const { container, unmount } = render(
        <QuotaRow app={app} used={used} limit={limit} over={used >= limit} editable busy={false} onSave={vi.fn()} />,
      );
      const row = container.querySelector('.qta-r')!;
      const result = {
        tone: ['ok', 'warn', 'over'].find(name => row.classList.contains(name)),
        pct: row.querySelector('.qta-pct')?.textContent,
        left: row.querySelector('.qta-left')?.textContent,
      };
      unmount();
      return result;
    };

    expect(tone(41.6 * GiB, 100 * GiB)).toEqual({ tone: 'ok', pct: '42%', left: '剩余58.40 GiB' });
    expect(tone(28.9 * GiB, 30 * GiB)).toEqual({ tone: 'warn', pct: '96%', left: '剩余1.10 GiB' });
    expect(tone(103.4 * GiB, 100 * GiB)).toEqual({ tone: 'over', pct: '103%', left: '超出3.40 GiB' });
  });

  it('未设额度时写明不限，不画额度条', () => {
    const { container } = render(
      <QuotaRow app={app} used={12.5 * GiB} limit={null} over={false} editable busy={false} onSave={vi.fn()} />,
    );

    expect(container.querySelector('.qta-r')?.classList.contains('unlimited')).toBe(true);
    expect(container.querySelector('.qta-figs')?.textContent).toBe('12.50GiB');
    expect(screen.getByText('不限额度')).toBeTruthy();
    expect(container.querySelector('.qta-t')).toBeNull();
    expect(screen.getByRole('button', { name: '设额度' })).toBeTruthy();
  });

  it('名称下方列出已授权的链；被系统停用时只写数量，名单进悬停提示', () => {
    const { container, rerender } = render(
      <QuotaRow
        app={app}
        used={1 * GiB}
        limit={10 * GiB}
        over={false}
        access={{ countries: ['JP', 'HK'], chains: ['东京 IIJ', '香港 HKT'], suspended: [] }}
        editable
        busy={false}
        onSave={vi.fn()}
      />,
    );

    expect(container.querySelectorAll('.qta-app .geo-flag')).toHaveLength(2);
    expect(container.querySelector('.qta-sub')?.textContent).toBe('东京 IIJ、香港 HKT');

    rerender(
      <QuotaRow
        app={app}
        used={11 * GiB}
        limit={10 * GiB}
        over
        access={{ countries: ['JP'], chains: [], suspended: ['东京 IIJ', '东京 IIJ'] }}
        editable
        busy={false}
        onSave={vi.fn()}
      />,
    );

    const stop = container.querySelector('.qta-sub.stop');
    expect(stop?.textContent).toBe('系统已停用 2 个接入点');
    expect(stop?.getAttribute('title')).toBe('东京 IIJ 已被系统停用；补足额度或月初重置后自动恢复');
  });

  it('非法额度提示原因并禁止保存，留空表示不限', async () => {
    const onSave = vi.fn().mockResolvedValue(undefined);
    render(<QuotaRow app={app} used={1 * GiB} limit={10 * GiB} over={false} editable busy={false} onSave={onSave} />);

    fireEvent.click(screen.getByRole('button', { name: '改额度' }));
    const input = screen.getByRole('textbox', { name: '全球线路 的月度额度（GiB）' }) as HTMLInputElement;
    expect(input.value).toBe('10');
    expect(screen.getByText('留空表示不限')).toBeTruthy();

    fireEvent.change(input, { target: { value: '0' } });
    expect(screen.getByText('请输入大于 0 的数字')).toBeTruthy();
    expect(input.getAttribute('aria-invalid')).toBe('true');
    expect((screen.getByRole('button', { name: '保存' }) as HTMLButtonElement).disabled).toBe(true);

    fireEvent.change(input, { target: { value: '' } });
    fireEvent.click(screen.getByRole('button', { name: '保存' }));
    await waitFor(() => expect(onSave).toHaveBeenCalledWith(null));
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
