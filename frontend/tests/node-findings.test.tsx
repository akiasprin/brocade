import { cleanup, render, screen, within } from '@testing-library/react';
import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest';
import type { NodeAgentStateItem } from '../src/api';

let runtimeFindings: typeof import('../src/panes/nodes').runtimeFindings;
let AppliedCard: typeof import('../src/panes/nodes').AppliedCard;

beforeAll(async () => {
  vi.stubGlobal(
    'matchMedia',
    vi.fn(() => ({
      matches: false,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
    })),
  );
  ({ AppliedCard, runtimeFindings } = await import('../src/panes/nodes'));
});

afterEach(cleanup);

function node(overlay: boolean, appliedState: string = 'present'): NodeAgentStateItem {
  return {
    overlay,
    retired_at: null,
    runtime_versions: {
      agent: 'agent-build',
      xray: null,
      phantun: null,
      wg_tools: 'wireguard-tools v1',
      wg_backend: 'userspace',
    },
    spool_backlog: null,
    last_local_reconcile: null,
    applied: { wireguard: { state: appliedState } },
  } as unknown as NodeAgentStateItem;
}

const hasUserspaceWarning = (value: NodeAgentStateItem, enabled?: boolean) =>
  runtimeFindings(value, enabled).some(finding => finding.chip === '用户态');

describe('WireGuard runtime finding', () => {
  it('appears only when WireGuard is enabled and not applied as disabled', () => {
    expect(hasUserspaceWarning(node(true))).toBe(true);
    expect(hasUserspaceWarning(node(false))).toBe(false);
    expect(hasUserspaceWarning(node(true, 'disabled'))).toBe(false);
  });

  it('uses the draft-aware effective switch supplied by the detail page', () => {
    expect(hasUserspaceWarning(node(true), false)).toBe(false);
  });

  it('surfaces unreachable peers as a warning without marking the node red', () => {
    const value = node(true);
    value.wireguard_health = {
      enabled: true,
      error: null,
      peers: [
        {
          peer_node_id: 'akko-lon',
          overlay_ip: '10.66.0.11',
          handshake_age_secs: 302_867,
          status: 'down',
          detail: '握手 302867 秒前，且 10.66.0.11 探不通——隧道断了',
        },
      ],
    };

    const finding = runtimeFindings(value).find(item => item.chip === 'WG 断链 1');
    expect(finding?.tone).toBe('warn');
  });

  it('hides an unreachable peer only while that peer is operationally isolated', () => {
    const value = node(true);
    value.wireguard_health = {
      enabled: true,
      error: null,
      peers: [
        {
          peer_node_id: 'akko-lon',
          overlay_ip: '10.66.0.11',
          handshake_age_secs: 302_867,
          status: 'down',
          detail: '握手 302867 秒前，且 10.66.0.11 探不通——隧道断了',
        },
      ],
    };

    expect(runtimeFindings(value, undefined, new Set(['akko-lon'])).some(item => item.chip.startsWith('WG '))).toBe(
      false,
    );
    expect(runtimeFindings(value, undefined, new Set()).some(item => item.chip === 'WG 断链 1')).toBe(true);
  });

  it('does not report healthy peers or stale health after WireGuard is disabled', () => {
    const value = node(true);
    value.wireguard_health = {
      enabled: true,
      error: null,
      peers: [
        {
          peer_node_id: 'akko-lon',
          overlay_ip: '10.66.0.11',
          handshake_age_secs: 3,
          status: 'up',
          detail: null,
        },
      ],
    };
    expect(runtimeFindings(value).some(item => item.chip.startsWith('WG '))).toBe(false);

    value.wireguard_health.peers[0].status = 'down';
    expect(runtimeFindings(value, false).some(item => item.chip.startsWith('WG '))).toBe(false);
  });
});

describe('usage runtime findings', () => {
  it('does not warn merely because the frozen generation skipped stale labels', () => {
    const value = node(false);
    value.usage_last_result = {
      accepted_readings: 2,
      inserted_samples: 1,
      skipped_counters: 3,
      rejected_counters: 0,
      gap_samples: 0,
    };

    expect(runtimeFindings(value).some(finding => finding.chip.includes('未识别'))).toBe(false);
    expect(runtimeFindings(value).some(finding => finding.chip.includes('未归属流量'))).toBe(false);
  });

  it('warns when an unknown counter grew since the previous in-memory observation', () => {
    const value = node(false);
    value.usage_last_result = {
      accepted_readings: 2,
      inserted_samples: 1,
      skipped_counters: 3,
      growing_unknown_counters: 1,
      rejected_counters: 0,
      gap_samples: 0,
    };

    const finding = runtimeFindings(value).find(item => item.chip === '未归属流量 1 项');
    expect(finding?.tone).toBe('warn');
  });
});

describe('durable spool-loss findings', () => {
  it('reserves the traffic-loss warning for classified usage reports', () => {
    const value = node(false);
    value.spool_backlog = {
      observation: 0,
      usage: 0,
      dropped: 2,
      usage_dropped: 2,
      observation_dropped: 0,
    };

    const finding = runtimeFindings(value).find(item => item.chip === '丢了 2 条用量');
    expect(finding?.tone).toBe('bad');
    const { container } = render(<>{finding?.text}</>);
    expect(container.textContent).toContain('流量明细');
  });

  it('reports rejected convergence evidence without claiming traffic was lost', () => {
    const value = node(false);
    value.spool_backlog = {
      observation: 0,
      usage: 0,
      dropped: 1,
      usage_dropped: 0,
      observation_dropped: 1,
    };

    const finding = runtimeFindings(value).find(item => item.chip === '丢了 1 条收敛结果');
    expect(finding?.tone).toBe('warn');
    const { container } = render(<>{finding?.text}</>);
    expect(container.textContent).toContain('不代表流量丢失');
  });

  it('keeps an old aggregate loss unclassified instead of inventing lost usage', () => {
    const value = node(false);
    value.spool_backlog = { observation: 0, usage: 0, dropped: 3 };

    const findings = runtimeFindings(value);
    expect(findings.find(item => item.chip === '丢了 3 条未分类上报')?.tone).toBe('warn');
    expect(findings.some(item => item.chip.includes('条用量'))).toBe(false);
  });
});

describe('OpenVPN runtime status', () => {
  it.each([
    ['OpenVPN 2.6.12 x86_64-pc-linux-gnu', 'OPENVPN 已应用'],
    [null, 'OPENVPN 已关闭'],
  ])('renders %s as an extension chip in the CONFIG band', (openvpn, title) => {
    const value = node(false);
    value.runtime_versions = { ...value.runtime_versions!, openvpn };
    render(<AppliedCard node={value} revisionOf={() => undefined} />);

    const configBand = screen.getByText('CONFIG').closest('section');
    expect(configBand).not.toBeNull();
    expect(within(configBand!).getByText('扩展应用')).toBeTruthy();
    expect(within(configBand!).getByText('来自').parentElement?.classList.contains('w2')).toBe(false);
    expect(within(configBand!).getByText('产物').parentElement?.classList.contains('w3')).toBe(true);
    const chip = within(configBand!).getByText('OPENVPN');
    expect(chip.classList.contains('nd-rt-chip')).toBe(true);
    expect(chip.getAttribute('title')).toBe(title);
  });
});
