import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  ORCHESTRATE_MEMBERS,
  isOrchestrateMember,
  lastOrchestrateView,
  rememberOrchestrateView,
} from '../src/forge/orchestrate';

const KEY = 'brocade-console:orchestrate-view';

describe('编排入口记住的子页', () => {
  afterEach(() => localStorage.clear());

  it('线路优先：组内顺序与首次进入都是线路', () => {
    expect(ORCHESTRATE_MEMBERS).toEqual(['chains', 'cvm']);
    expect(lastOrchestrateView()).toBe('chains');
  });

  it('回到上次停留的子页', () => {
    rememberOrchestrateView('cvm');
    expect(lastOrchestrateView()).toBe('cvm');
    rememberOrchestrateView('chains');
    expect(lastOrchestrateView()).toBe('chains');
  });

  it('存储里的未知值不会把入口带到组外页面', () => {
    localStorage.setItem(KEY, 'settings');
    expect(lastOrchestrateView()).toBe('chains');
  });

  it('存储不可用时仍可进入线路，记录失败不抛错', () => {
    vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => {
      throw new DOMException('blocked', 'SecurityError');
    });
    vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => {
      throw new DOMException('blocked', 'SecurityError');
    });
    expect(lastOrchestrateView()).toBe('chains');
    expect(() => rememberOrchestrateView('cvm')).not.toThrow();
  });

  it('只有线路与容器属于这一组', () => {
    expect(isOrchestrateMember('chains')).toBe(true);
    expect(isOrchestrateMember('cvm')).toBe(true);
    expect(isOrchestrateMember('nodes')).toBe(false);
    expect(isOrchestrateMember('usage')).toBe(false);
  });
});
