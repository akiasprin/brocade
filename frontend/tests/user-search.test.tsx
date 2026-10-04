import { describe, expect, it } from 'vitest';
import type { UserListItem, UserPresence } from '../src/api';
import {
  accountTypeChip,
  rosterFilterMatches,
  userMatchesSearch,
  userPresenceText,
  type UserFacts,
} from '../src/panes/users';

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
    expect(userMatchesSearch(user, 'PLATFORM.ACME')).toBe(false);
    expect(userMatchesSearch(user, '5d8f0b2e')).toBe(true);
  });

  it('keeps every user for an empty query and rejects unrelated text', () => {
    expect(userMatchesSearch(user, '   ')).toBe(true);
    expect(userMatchesSearch(user, 'tokyo')).toBe(false);
  });

  it('marks only test accounts in the roster', () => {
    expect(accountTypeChip('test')).toBe('测试');
    expect(accountTypeChip('formal')).toBeNull();
    expect(accountTypeChip(undefined)).toBeNull();
  });
});

describe('roster filter', () => {
  const facts = (overrides: Partial<UserFacts> = {}): UserFacts => ({
    status: 'active',
    grants: 2,
    exhausted: [],
    suspended: 0,
    ...overrides,
  });

  it('counts exhausted and system-suspended users as needing attention, but not disabled or ungranted ones', () => {
    expect(rosterFilterMatches('attention', user, facts({ exhausted: ['东京'] }))).toBe(true);
    expect(rosterFilterMatches('attention', user, facts({ grants: 0, suspended: 2 }))).toBe(true);
    expect(rosterFilterMatches('attention', user, facts({ grants: 0 }))).toBe(false);
    const disabled = { ...user, status: 'disabled' };
    expect(rosterFilterMatches('attention', disabled, facts({ status: 'disabled', exhausted: ['东京'] }))).toBe(false);
    expect(rosterFilterMatches('disabled', disabled, facts({ status: 'disabled' }))).toBe(true);
  });

  it('selects test accounts and keeps everyone under all', () => {
    expect(rosterFilterMatches('test', { ...user, account_type: 'test' }, facts())).toBe(true);
    expect(rosterFilterMatches('test', user, facts())).toBe(false);
    expect(rosterFilterMatches('all', { ...user, status: 'disabled' }, facts({ status: 'disabled' }))).toBe(true);
  });
});

describe('online source wording', () => {
  const presence = (state: UserPresence['state'], count: number): UserPresence => ({
    tenant_id: 'platform.acme',
    user_id: 'alice',
    state,
    expected_nodes: 2,
    reporting_nodes: state === 'complete' ? 2 : state === 'partial' ? 1 : 0,
    sources: Array.from({ length: count }, (_, index) => ({
      ip: `1.1.1.${index + 1}`,
      first_observed_at: '',
      last_observed_at: '',
      xray_last_seen_at: '',
      node_ids: [],
      ingress_ids: [],
    })),
  });

  it('distinguishes complete zero, partial lower bounds and unavailable reports', () => {
    expect(userPresenceText(presence('complete', 0))).toBe('暂无在线连接');
    expect(userPresenceText(presence('complete', 2))).toBe('在线来源 2');
    expect(userPresenceText(presence('partial', 2))).toBe('至少 2 个在线来源');
    expect(userPresenceText(presence('partial', 0))).toBe('在线来源 —');
    expect(userPresenceText(presence('unavailable', 0))).toBe('在线来源 —');
    expect(userPresenceText(undefined)).toBe('在线来源 —');
  });
});
