import { cleanup, render } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';
import type { GrantAutomationStatus } from '../src/api';
import { compactGrantAutomation, RuntimeCrumbStatus } from '../src/ui/grant-automation';

const healthy: GrantAutomationStatus = {
  pending_jobs: 0,
  retrying_jobs: 0,
  failed_jobs: 0,
  max_attempts: 0,
  latest_revision_id: null,
  oldest_pending_at: null,
  last_attempt_at: null,
  last_error: null,
};

afterEach(cleanup);

describe('permission automation status', () => {
  it('keeps the healthy idle state out of the breadcrumb', () => {
    const view = render(<RuntimeCrumbStatus state={compactGrantAutomation(healthy, false, false)} />);
    expect(view.container.textContent).toBe('');
  });

  it('keeps transient background checks out of the breadcrumb', () => {
    const view = render(<RuntimeCrumbStatus state={{ text: '检查中', tone: 'normal' }} />);
    expect(view.container.textContent).toBe('');
  });

  it('shows the revision only with an active deployment state', () => {
    const view = render(<RuntimeCrumbStatus state={{ text: '发布 #42 · 进行中', tone: 'hot' }} revision={582} />);
    expect(view.container.textContent).toBe('发布 #42 · 进行中R582');
  });

  it('shows retry progress compactly and keeps the planning error in hover detail', () => {
    const state = compactGrantAutomation(
      {
        ...healthy,
        pending_jobs: 27,
        retrying_jobs: 27,
        max_attempts: 841,
        latest_revision_id: 562,
        oldest_pending_at: '2026-08-30T12:00:00Z',
        last_attempt_at: '2026-08-30T12:05:00Z',
        last_error: '历史快照无法读取',
      },
      false,
      false,
    );
    const view = render(<RuntimeCrumbStatus state={state} />);

    expect(view.container.textContent).toBe('权限重试 27');
    expect(view.getByTitle(/最多已试 841 次/).title).toContain('历史快照无法读取');
  });

  it('uses a short waiting state before the first retry', () => {
    expect(compactGrantAutomation({ ...healthy, pending_jobs: 1 }, false, false)).toMatchObject({
      text: '权限待同步 1',
      tone: 'hot',
    });
  });
});
