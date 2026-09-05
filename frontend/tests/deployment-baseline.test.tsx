import { describe, expect, it } from 'vitest';
import type { DeploymentListItem } from '../src/api';

window.matchMedia = ((query: string) => ({
  matches: false,
  media: query,
  onchange: null,
  addEventListener: () => {},
  removeEventListener: () => {},
  addListener: () => {},
  removeListener: () => {},
  dispatchEvent: () => false,
})) as unknown as typeof window.matchMedia;

const { latestSuccessfulRevision } = await import('../src/panes/deploy');

const item = (id: number, revision: number, status: string) =>
  ({ id, revision_id: revision, status }) as DeploymentListItem;

describe('发布计划基线', () => {
  it('按发布发生顺序选最近一次成功，不依赖接口数组顺序或修订号大小', () => {
    expect(
      latestSuccessfulRevision([
        item(12, 101, 'succeeded'),
        item(15, 80, 'succeeded'),
        item(18, 110, 'failed'),
        item(9, 70, 'succeeded'),
      ]),
    ).toBe(80);
  });

  it('没有成功发布时没有可撤销基线', () => {
    expect(latestSuccessfulRevision([item(2, 2, 'failed'), item(1, 1, 'canceled')])).toBeNull();
  });
});
