import { describe, expect, it } from 'vitest';
import type { UserListItem } from '../src/api';
import { accountTypeBadge, userMatchesSearch } from '../src/panes/users';

const user: UserListItem = {
  tenant_id: 'platform.acme',
  id: 'Alice-London',
  uuid: '7c9e2b41-3f6a-4e2d-9a1c-5d8f0b2e6a93',
  status: 'active',
  created_at: '2026-08-31T00:00:00Z',
  created_revision: 582,
};

describe('user search', () => {
  it('matches username, tenant and UUID without case or surrounding-space sensitivity', () => {
    expect(userMatchesSearch(user, ' alice ')).toBe(true);
    expect(userMatchesSearch(user, 'PLATFORM.ACME')).toBe(true);
    expect(userMatchesSearch(user, '5d8f0b2e')).toBe(true);
  });

  it('keeps every user for an empty query and rejects unrelated text', () => {
    expect(userMatchesSearch(user, '   ')).toBe(true);
    expect(userMatchesSearch(user, 'tokyo')).toBe(false);
  });

  it('labels both formal and test users in the roster', () => {
    expect(accountTypeBadge('formal')).toBe('正式');
    expect(accountTypeBadge('test')).toBe('测试');
    expect(accountTypeBadge(undefined)).toBe('正式');
  });
});
