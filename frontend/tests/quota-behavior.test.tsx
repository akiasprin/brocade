import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';
import type { SnapshotApp } from '../src/api';
import { combinedQuotaPercent, cycleUsageText, QuotaRow } from '../src/panes/users';

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
  it('名册环形按总已用除以总额度，不取单条线路的最大比例', () => {
    expect(
      combinedQuotaPercent([
        { used: 9 * GiB, limit: 10 * GiB },
        { used: 10 * GiB, limit: 90 * GiB },
      ]),
    ).toBeCloseTo(19);
    expect(combinedQuotaPercent([{ used: 4 * GiB, limit: null }])).toBeNull();
  });

  it('区分读取中、失败、无样本和实际用量', () => {
    expect(cycleUsageText('pending', 0, 0)).toBe('读取中…');
    expect(cycleUsageText('failed', 0, 0)).toBe('暂不可用');
    expect(cycleUsageText('ready', 0, 0)).toBe('—');
    expect(cycleUsageText('ready', 1, 2 * GiB)).toBe('2.00 GiB');
  });

  it('月度汇总仍在读取时只占位用量，不阻塞额度内容', () => {
    const { container } = render(<QuotaRow app={app} used={null} limit={10 * GiB} over={false} usageState="pending" />);

    expect(screen.getByText('读取中…')).toBeTruthy();
    expect(screen.getByText('/ 10 GiB')).toBeTruthy();
    expect(container.querySelector('.qta-left')).toBeNull();
    expect(container.querySelector('.qta-t')).toBeNull();
  });

  it('月度汇总失败时明确标为不可用', () => {
    render(<QuotaRow app={app} used={null} limit={10 * GiB} over={false} usageState="failed" />);

    expect(screen.getByText('—')).toBeTruthy();
    expect(screen.getByText('用量暂不可用')).toBeTruthy();
  });

  it('用量读取失败时显示未知，不把未知算成 0 或剩余额度', () => {
    const { container } = render(<QuotaRow app={app} used={null} limit={10 * GiB} over={false} />);

    expect(screen.getByText('—')).toBeTruthy();
    expect(screen.getByText('用量未知')).toBeTruthy();
    expect(screen.queryByText(/剩余/)).toBeNull();
    expect(container.querySelector('.qta-r')?.classList.contains('unknown')).toBe(true);
    expect(container.querySelector('.qta-t')).toBeNull();
  });

  it('按额度比例分档：常态、注意（≥80%）、用尽', () => {
    const tone = (used: number, limit: number) => {
      const { container, unmount } = render(<QuotaRow app={app} used={used} limit={limit} over={used >= limit} />);
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
    expect(tone(79.99 * GiB, 100 * GiB)).toEqual({ tone: 'ok', pct: '80%', left: '剩余20.01 GiB' });
    expect(tone(80 * GiB, 100 * GiB)).toEqual({ tone: 'warn', pct: '80%', left: '剩余20.00 GiB' });
    expect(tone(80.01 * GiB, 100 * GiB)).toEqual({ tone: 'warn', pct: '80%', left: '剩余19.99 GiB' });
    expect(tone(28.9 * GiB, 30 * GiB)).toEqual({ tone: 'warn', pct: '96%', left: '剩余1.10 GiB' });
    expect(tone(103.4 * GiB, 100 * GiB)).toEqual({ tone: 'over', pct: '103%', left: '超出3.40 GiB' });
  });

  it('未设额度时写明不限，不画额度条', () => {
    const { container } = render(<QuotaRow app={app} used={12.5 * GiB} limit={null} over={false} />);

    expect(container.querySelector('.qta-r')?.classList.contains('unlimited')).toBe(true);
    expect(container.querySelector('.qta-figs')?.textContent).toBe('12.50GiB');
    expect(screen.getByText('不限额度')).toBeTruthy();
    expect(container.querySelector('.qta-t')).toBeNull();
  });

  it('名称下方列出已授权的链；被系统停用时只写数量，名单进悬停提示', () => {
    const { container, rerender } = render(
      <QuotaRow
        app={app}
        used={1 * GiB}
        limit={10 * GiB}
        over={false}
        access={{ countries: ['JP', 'HK'], chains: ['东京 IIJ', '香港 HKT'], suspended: [] }}
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
      />,
    );

    const stop = container.querySelector('.qta-sub.stop');
    expect(stop?.textContent).toBe('系统已停用 2 个接入点');
    expect(stop?.getAttribute('title')).toBe('东京 IIJ 已被系统停用；补足额度或账期重置后自动恢复');
  });

  it('各 app 独立显示额度与超额处理', () => {
    const policy = (appId: string, exhausted: boolean) => ({
      kind: 'access' as const,
      app_id: appId,
      policy: { quota_bytes: String(10 * GiB), exhausted_action: { kind: 'throttle' as const, rate_bps: 5_000_000 } },
      used_bytes: String((exhausted ? 12 : 1) * GiB),
      exhausted,
      service_available: true,
      unsupported_nodes: [],
      pending_nodes: [],
    });
    const { container } = render(
      <>
        <QuotaRow app={app} used={12 * GiB} limit={10 * GiB} over policy={policy(app.id, true)} />
        <QuotaRow
          app={{ ...app, id: 'other', label: '其他线路' }}
          used={GiB}
          limit={10 * GiB}
          over={false}
          policy={policy('other', false)}
        />
      </>,
    );
    const rows = container.querySelectorAll('.qta-r');
    expect(screen.getByText('限速中 · 5 Mbps')).toBeTruthy();
    expect(screen.getByText('用满后限速 5 Mbps')).toBeTruthy();
    expect(rows[0].classList.contains('warn')).toBe(true);
    expect(rows[0].classList.contains('over')).toBe(false);
    expect(rows[0].querySelector('.qta-left')?.textContent).toBe('超出2.00 GiB');
    expect(rows[1].querySelector('.qta-left')?.textContent).toBe('剩余9.00 GiB');
    const run = rows[0].querySelector<HTMLElement>('.qta-meter-run')!;
    expect(parseFloat(run.style.getPropertyValue('--at'))).toBeCloseTo((100 * 10) / 12);
    expect(rows[0].querySelector('.qta-t > i')?.getAttribute('style')).toContain('width: 100%');
    expect(rows[0].querySelector('.qta-overflow')).toBeTruthy();
    expect(rows[1].querySelector('.qta-overflow')).toBeNull();
    expect(screen.queryByRole('button')).toBeNull();
    expect(container.textContent).not.toContain('共享总额度');
  });
});
