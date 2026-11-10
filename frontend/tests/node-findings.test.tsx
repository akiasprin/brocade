import { cleanup, render, screen, within } from '@testing-library/react';
import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest';
import type { NodeAgentStateItem } from '../src/api';
import { cvmWorkspace } from './fixtures/cvm/data';

let runtimeFindings: typeof import('../src/panes/nodes').runtimeFindings;
let runtimeFindingAttentionCount: typeof import('../src/panes/nodes').runtimeFindingAttentionCount;
let AppliedCard: typeof import('../src/panes/nodes').AppliedCard;
let nodeLampState: typeof import('../src/panes/nodes').nodeLampState;

beforeAll(async () => {
  vi.stubGlobal(
    'matchMedia',
    vi.fn(() => ({
      matches: false,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
    })),
  );
  ({ AppliedCard, nodeLampState, runtimeFindingAttentionCount, runtimeFindings } = await import('../src/panes/nodes'));
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
          peer_node_id: 'example-uk-edge',
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
          peer_node_id: 'example-uk-edge',
          overlay_ip: '10.66.0.11',
          handshake_age_secs: 302_867,
          status: 'down',
          detail: '握手 302867 秒前，且 10.66.0.11 探不通——隧道断了',
        },
      ],
    };

    expect(
      runtimeFindings(value, undefined, new Set(['example-uk-edge'])).some(item => item.chip.startsWith('WG ')),
    ).toBe(false);
    expect(runtimeFindings(value, undefined, new Set()).some(item => item.chip === 'WG 断链 1')).toBe(true);
  });

  it('does not report healthy peers or stale health after WireGuard is disabled', () => {
    const value = node(true);
    value.wireguard_health = {
      enabled: true,
      error: null,
      peers: [
        {
          peer_node_id: 'example-uk-edge',
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

describe('handoff liveness findings', () => {
  it('keeps a runtime-reporting node online while desired polling pauses for drain', () => {
    const value = node(false);
    value.last_poll_at = '2026-10-16T14:10:28Z';
    value.runtime_reported_at = new Date().toISOString();
    value.runtime_report_fresh = true;
    value.desired_poll_fresh = false;
    value.xray_upgrade = {
      source: { kind: 'config-deployment', deployment_id: 604, claim_generation: 0 },
      release_id: -604,
      attempt: 0,
      strategy: 'handoff',
      phase: 'blocked',
      old: {
        pid: 49698,
        pid_start_ticks: 1,
        boot_id: 'boot',
        sha256: 'old',
        alive: true,
        accepting: false,
        active_connections: 3,
      },
      candidate: {
        pid: 18801,
        pid_start_ticks: 2,
        boot_id: 'boot',
        sha256: 'new',
        alive: true,
        accepting: true,
        active_connections: 32,
      },
      blocked_reason: '旧代仍有 3 条连接，Agent 重启后继续等待自然排空',
    };

    const finding = runtimeFindings(value).find(item => item.chip === 'Xray 待排空 3 条');
    expect(finding?.tone).toBe('info');
    expect(finding?.detailOnly).toBe(true);
    expect(runtimeFindingAttentionCount(runtimeFindings(value))).toBe(0);
    expect(nodeLampState(value)).toEqual({ tone: 'ok', why: '没有要处理的' });
    render(<div>{finding?.text}</div>);
    expect(screen.getByText(/上一代实例仍承载 3 条存量连接/)).not.toBeNull();
    expect(screen.queryByText(/49698|PID/)).toBeNull();
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

  it('keeps historical convergence loss out of current health attention', () => {
    const value = node(false);
    value.spool_backlog = {
      observation: 0,
      usage: 0,
      dropped: 1,
      usage_dropped: 0,
      observation_dropped: 1,
    };

    expect(runtimeFindings(value).some(item => item.chip.includes('收敛结果'))).toBe(false);
    value.runtime_report_fresh = true;
    expect(nodeLampState(value)).toEqual({ tone: 'ok', why: '没有要处理的' });
  });

  it('keeps historical load-window loss out of current health attention', () => {
    const value = node(false);
    value.spool_backlog = {
      observation: 0,
      usage: 0,
      load: 120,
      dropped: 4,
      usage_dropped: 0,
      observation_dropped: 0,
      load_dropped: 4,
    };

    const findings = runtimeFindings(value);
    expect(findings.some(item => item.chip.includes('指标窗口'))).toBe(false);
    expect(findings.some(item => item.chip.includes('未分类'))).toBe(false);
    expect(findings.some(item => item.chip.includes('条用量'))).toBe(false);
    value.runtime_report_fresh = true;
    expect(nodeLampState(value)).toEqual({ tone: 'ok', why: '没有要处理的' });
  });

  it('keeps a legacy aggregate loss out of current health attention', () => {
    const value = node(false);
    value.spool_backlog = { observation: 0, usage: 0, dropped: 3 };

    const findings = runtimeFindings(value);
    expect(findings.some(item => item.chip.includes('未分类'))).toBe(false);
    expect(findings.some(item => item.chip.includes('条用量'))).toBe(false);
    value.runtime_report_fresh = true;
    expect(nodeLampState(value)).toEqual({ tone: 'ok', why: '没有要处理的' });
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

describe('CUBE extension display', () => {
  it.each(['ready', 'stale', 'error', 'missing', 'unmanaged'] as const)(
    'uses the actual Cube inventory (%s)',
    state => {
      const value = node(false);
      const host = cvmWorkspace('detail').hosts[0];
      if (state === 'stale') host.fresh = false;
      if (state === 'error') host.error = 'Cubelet 无法连接';
      if (state === 'unmanaged') {
        host.capabilities.backend = '';
        host.error = 'CVM Rust executor configuration is unavailable';
        host.fresh = false;
      }
      render(
        <AppliedCard
          node={value}
          revisionOf={() => undefined}
          cubeHost={state === 'missing' ? undefined : host}
          cubeStatus="ready"
        />,
      );
      const chip = screen.getByText(state === 'missing' || state === 'unmanaged' ? 'CUBE' : 'CUBE[KVM]');
      expect(chip.classList.contains('nd-rt-chip')).toBe(true);
      expect(chip.getAttribute('title')).toBe(
        {
          ready: 'CUBE[KVM] 扩展 · 已启用',
          stale: 'CUBE[KVM] 扩展 · 能力报告已过期',
          error: 'CUBE[KVM] 扩展 · 执行能力异常：Cubelet 无法连接',
          missing: 'CUBE 扩展 · 未启用',
          unmanaged: 'CUBE 扩展 · 未启用',
        }[state],
      );
      expect(screen.getByText('OPENVPN')).toBeTruthy();
      if (state === 'unmanaged' || state === 'missing') {
        expect(chip.classList.contains('st-skipped')).toBe(true);
        expect(chip.querySelector('svg circle')).toBeNull();
      }
    },
  );

  it.each([
    ['cube-kvm', 'CUBE[KVM]'],
    ['cube-kvm-v1', 'CUBE[KVM]'],
    ['cube-kvm-v2', 'CUBE[KVM]'],
    ['cube-pvm', 'CUBE[PVM]'],
    ['cube-pvm-v1', 'CUBE[PVM]'],
    ['cube-qemu', 'CUBE[QEMU]'],
    ['cube-custom-v2', 'CUBE[CUSTOM-V2]'],
    ['cube', 'CUBE'],
  ])('displays the reported backend %s as %s', (backend, label) => {
    const host = cvmWorkspace('detail').hosts[0];
    host.capabilities.backend = backend;
    render(<AppliedCard node={node(false)} revisionOf={() => undefined} cubeHost={host} cubeStatus="ready" />);
    const chip = screen.getByText(label);
    expect(chip.getAttribute('title')).toBe(`${label} 扩展 · 已启用`);
  });

  it('keeps a reserved instance in scope when its Cube capabilities are unavailable', () => {
    const host = cvmWorkspace('detail').hosts[0];
    host.capabilities.backend = '';
    host.error = 'Cubelet 无法连接';
    host.fresh = false;
    render(
      <AppliedCard node={node(false)} revisionOf={() => undefined} cubeHost={host} cubeStatus="ready" cubeExpected />,
    );
    const chip = screen.getByText('CUBE');
    expect(chip.getAttribute('title')).toBe('CUBE 扩展 · 执行能力异常：Cubelet 无法连接');
    expect(chip.classList.contains('st-halted')).toBe(true);
  });

  it('does not infer a running Cube backend from a reserved instance alone', () => {
    const host = cvmWorkspace('detail').hosts[0];
    host.capabilities.backend = '';
    render(
      <AppliedCard node={node(false)} revisionOf={() => undefined} cubeHost={host} cubeStatus="ready" cubeExpected />,
    );
    const chip = screen.getByText('CUBE');
    expect(chip.getAttribute('title')).toBe('CUBE 扩展 · 尚未确认运行栈');
    expect(chip.classList.contains('st-pending')).toBe(true);
  });

  it.each(['pending', 'error'] as const)(
    'does not infer an installation state while the capability query is %s',
    status => {
      render(<AppliedCard node={node(false)} revisionOf={() => undefined} cubeStatus={status} />);
      expect(screen.getByText('CUBE').classList.contains('st-pending')).toBe(true);
    },
  );
});
