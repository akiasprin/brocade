// Synthetic API contracts for the production CVM component. No alternate UI.
import type { CvmCapabilities, CvmInstance, CvmWorkspace } from '../../../src/api';

import { allocatedNic } from './network-data';

const MiB = 1024 ** 2;
const GiB = 1024 ** 3;
export const cvmCapabilities: CvmCapabilities = {
  managed_networks: true,
  backend: 'cube-kvm-v1',
  guest_terminal: true,
  guest_terminal_persistent: true,
  primary_ip_delivery: false,
  retain_on_delete: false,
  resource_limits: {
    min_vcpus: 1,
    max_vcpus: 8,
    min_memory_bytes: 128 * MiB,
    max_memory_bytes: 4 * GiB,
    memory_step_bytes: 64 * MiB,
    min_root_disk_bytes: 3 * GiB,
    max_root_disk_bytes: 64 * GiB,
    root_disk_step_bytes: GiB,
  },
  create: true,
  stop: true,
  start: true,
  delete: true,
  stable_storage: true,
  restart: true,
  reinstall: true,
  configure: true,
  configure_network: true,
  hot_attach: true,
  hot_detach: true,
  max_interfaces: 3,
  allocatable_vcpus: 8,
  allocatable_memory_bytes: 8 * GiB,
  allocatable_disk_bytes: 80 * GiB,
  images: [
    {
      id: 'debian-13',
      name: 'Debian',
      version: '13',
      architecture: 'x86_64',
      sha256: 'a'.repeat(64),
      boot_mode: 'cube-oci',
    },
  ],
  network_sources: [
    { id: 'public', name: '公网连接', kind: 'public', description: '用于实例主网卡连接', max_attachments: 1 },
    {
      id: 'internal',
      name: '自有网络 · 香港到家庭',
      kind: 'internal',
      description: '内部业务通路',
      max_attachments: 1,
    },
    {
      id: 'internal-tokyo',
      name: '自有网络 · 东京出口',
      kind: 'internal',
      description: '独立业务出口',
      max_attachments: 1,
    },
  ],
};

export function cvmInstance(id = 'cvm-alice'): CvmInstance {
  return {
    tenant_id: 'platform',
    user_id: 'alice',
    node_id: 'hk-hkix',
    status: 'applied',
    reserved: true,
    created_at: '2026-11-04T09:00:00Z',
    updated_at: '2026-11-04T10:30:00Z',
    desired: {
      id,
      operation_id: 'op-first',
      generation: 1,
      spec_sha256: 'd'.repeat(64),
      power: 'running',
      delete_data: false,
      restart_epoch: 0,
      reinstall_epoch: 0,
      spec: {
        routing: { revision: 1, priorities: [{ interface_id: 'nic-business', ipv4: 10, ipv6: 10 }], routes: [{ destination: '0.0.0.0/0', interface_id: 'nic-business' }, { destination: '::/0', interface_id: 'nic-business' }], dns: [] },
        name: 'home-workspace',
        image: cvmCapabilities.images[0],
        resources: { vcpus: 1, memory_bytes: 512 * MiB, root_disk_bytes: 3_000_000_000 },
        ssh_public_key: 'ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEiBcpQEzqzjADp8VXvOigHnqLh5L5xP76cwoCD8YMK5 fixture',
        interfaces: [
          { id: 'nic-public', source_id: 'public', purpose: 'contact', default_ipv4: false, default_ipv6: false },
          { id: 'nic-business', source_id: 'network-hk', allocated: allocatedNic(), purpose: 'business', default_ipv4: true, default_ipv6: false },
        ],
      },
    },
    report: {
      status: 'applied',
      power: 'running',
      runtime_id: 'runtime-fixture',
      volume_id: 'volume-fixture',
      access_address: '192.168.0.21',
      interfaces: [
        { id: 'nic-public', device: 'enp0s1', mac: '02:00:00:00:00:01', addresses: ['192.0.2.21/32'], attached: true },
        { id: 'nic-business', device: 'enp0s2', mac: '02:00:00:00:00:02', addresses: ['10.60.0.2/30'], attached: true },
      ],
      network_absent: false,
      detached_interfaces: [],
      volume_retained: false,
      volume_deleted: false,
      error: null,
    },
  };
}
export function cvmWorkspace(scene = 'list', role = 'system-admin'): CvmWorkspace {
  const instance = cvmInstance();
  if (scene === 'long') {
    instance.desired.spec.name = '这是一个用于验证长名称边界的 CVM 实例';
    instance.user_id = 'long-account-name-that-must-not-overflow-the-instance-card';
  }
  if (scene === 'stopped' || scene === 'network-unconfigured') {
    instance.desired.power = 'stopped';
    instance.report!.power = 'stopped';
    instance.report!.runtime_id = null;
    instance.report!.volume_retained = true;
    instance.report!.network_absent = true;
    instance.report!.interfaces = [];
    instance.report!.access_address = null;
  }
  if (scene === 'pending') {
    instance.status = 'pending';
    instance.report = null;
  }
  if (scene === 'unknown') {
    instance.status = 'unknown';
    instance.report!.status = 'unknown';
    instance.report!.error = '运行时联络中断，原系统盘与预约已保留。';
  }
  const caps = structuredClone(cvmCapabilities);
  if (scene === 'network-unconfigured' || scene === 'shared-l4') {
    instance.desired.spec.interfaces = instance.desired.spec.interfaces.filter(nic => nic.purpose === 'contact');
    caps.network_sources = caps.network_sources.filter(source => source.kind === 'public');
    caps.max_interfaces = 1;
    caps.hot_attach = caps.hot_detach = false;
    instance.report!.interfaces = instance.report!.interfaces.filter(nic => nic.id === 'nic-public');
  }
  if (scene === 'experimental') {
    caps.stable_storage = false;
    caps.stop = false;
    caps.start = false;
    caps.hot_attach = false;
    caps.hot_detach = false;
    caps.max_interfaces = 1;
  }
  const redacted = role === 'readonly' || role === 'public';
  if (redacted) {
    instance.desired.spec.ssh_public_key = '';
    instance.report!.runtime_id = null;
    instance.report!.volume_id = null;
    instance.report!.access_address = null;
    instance.report!.interfaces.forEach(nic => {
      nic.addresses = [];
      nic.mac = '';
    });
  }
  return {
    can_manage_network: role === 'system-admin',
    hosts:
      scene === 'empty'
        ? []
        : [
            {
              node_id: 'hk-hkix',
              name: scene === 'long' ? '这是一个很长的宿主名称 · 华东家庭网络' : '香港 HKIX',
              fresh: scene !== 'stale',
              observed_at: '2026-11-04T10:30:00Z',
              capabilities: caps,
              terminal_enabled: true,
              host_revision: 0,
              can_manage_host: role === 'system-admin',
              terminal_revision: 1,
              can_manage_terminal: role === 'system-admin',
              error: null,
            },
          ],
    instances: ['empty', 'new', 'experimental'].includes(scene) ? [] : [instance],
    owners: [{ tenant_id: 'platform', user_id: 'alice', name: 'alice' }],
    quotas: [
      {
        tenant_id: 'platform',
        user_id: 'alice',
        enabled: true,
        allowed_nodes: ['hk-hkix'],
        max_instances: 4,
        max_vcpus: 4,
        max_memory_bytes: 2 * GiB,
        max_disk_bytes: 20 * GiB,
        max_interfaces: 12,
      },
    ],
    usage: [{ tenant_id: 'platform', user_id: 'alice', instances: ['empty', 'new', 'experimental'].includes(scene) ? 0 : 1, vcpus: ['empty', 'new', 'experimental'].includes(scene) ? 0 : instance.desired.spec.resources.vcpus, memory_bytes: ['empty', 'new', 'experimental'].includes(scene) ? 0 : instance.desired.spec.resources.memory_bytes, disk_bytes: ['empty', 'new', 'experimental'].includes(scene) ? 0 : instance.desired.spec.resources.root_disk_bytes, interfaces: ['empty', 'new', 'experimental'].includes(scene) ? 0 : instance.desired.spec.interfaces.length }],
    can_manage_quotas: role === 'system-admin',
    can_write: !redacted,
    redacted,
  };
}

export function cvmMonitoring(scene = 'detail', now = Date.now()): import('../../../src/api').CvmMonitoring {
  const end = now - (scene === 'stale' ? 180_000 : 5_000);
  const samples = Array.from({ length: 55 }, (_, i) => ({
    observed_at_unix_millis: end - (54 - i) * 60_000,
    window_start_unix_millis: end - (55 - i) * 60_000,
    uptime_millis: 7_200_000 + i * 60_000,
    cpu_usage_basis_points: i === 22 ? null : Math.round(1600 + Math.sin(i / 3) * 900),
    cpu_peak_basis_points: i === 22 ? null : Math.round(2200 + Math.sin(i / 3) * 900),
    memory_available_min_bytes: 200 * MiB,
    has_gap: i === 22,
    diagnostics: {
      cpus: [
        { cpu: -1, user: 10, system: 5, softirq: 1, iowait: 2, steal: 0 },
        { cpu: 0, user: 12, system: 5, softirq: 1, iowait: 2, steal: 0 },
      ],
      load: [0.2, 0.3, 0.25] as [number, number, number],
      running_processes: 2,
      total_processes: 71,
      memory: { AnonPages: 100 * MiB, Cached: 80 * MiB, Buffers: 5 * MiB, SReclaimable: 9 * MiB },
      swap_used_bytes: 0,
      oom_kills: 0,
      major_faults: 2,
      pressure: [
        { resource: 'cpu', some_basis_points: 10, full_basis_points: null },
        { resource: 'memory', some_basis_points: 0, full_basis_points: 0 },
        { resource: 'io', some_basis_points: 0, full_basis_points: 0 },
      ],
      inode_total: 100000,
      inode_free: 90000,
      disk_io: null,
      tcp: { CurrEstab: 4, RetransSegs: 0, AttemptFails: 0, EstabResets: 0, UdpInErrors: 0 },
      unsupported: ['root_block_io'],
    },
    memory_total_bytes: 475 * MiB,
    memory_used_bytes: Math.round((230 + Math.sin(i / 9) * 25) * MiB),
    root_total_bytes: 2.8 * GiB,
    root_used_bytes: 0.9 * GiB,
    interfaces: [
      {
        id: 'nic-public',
        device: 'enp0s1',
        rx_pps: i === 22 ? null : 10,
        tx_pps: i === 22 ? null : 12,
        drops: i === 22 ? null : 0,
        errors: i === 22 ? null : 0,
        carrier: true,
        rx_bps: i === 22 ? null : 12000 + i * 100,
        tx_bps: i === 22 ? null : 24000,
      },
      {
        id: 'nic-business',
        device: 'enp0s2',
        rx_pps: i === 22 ? null : 540,
        tx_pps: i === 22 ? null : 340,
        drops: i === 22 ? null : 0,
        errors: i === 22 ? null : 0,
        carrier: true,
        rx_bps: i === 22 ? null : Math.round(7000000 + Math.sin(i / 5) * 4500000),
        tx_bps: i === 22 ? null : Math.round(3000000 + Math.cos(i / 7) * 1500000),
      },
    ],
  }));
  // Explicit observation gap, not an interpolated curve over an absent report.
  samples[23].window_start_unix_millis = samples[23].observed_at_unix_millis;
  return {
    state: scene === 'stopped' ? 'stopped' : scene === 'stale' ? 'stale' : scene === 'unknown' ? 'unavailable' : 'live',
    error: scene === 'unknown' ? '实例监控采集失败，等待下一次采样' : null,
    samples_truncated: false,
    latest: samples.at(-1) ?? null,
    health: {
      id: 'cvm-alice',
      generation: 1,
      observed_at_unix_millis: end,
      runtime_id: 'cube-fixture',
      runtime_state: scene === 'stopped' ? 'stopped' : 'running',
      guest_state: scene === 'unknown' ? 'unreachable' : 'reachable',
      ssh_state: 'reachable',
      services: [],
      error: null,
    },
    issues: [],
    events: [{ at_unix_millis: end - 1800000, kind: 'reboot', detail: '检测到 Guest 启动身份变化' }],
    range_start_unix_millis: now - 3600000,
    range_end_unix_millis: now,
    samples,
  };
}


export function cvmReadings(workspace: CvmWorkspace, scene = 'list'): import('../../../src/api').CvmReadings {
  const data = cvmMonitoring(scene);
  const cardSample = (sample: import('../../../src/api').CvmMonitoringSample) => ({
    observed_at_unix_millis: sample.observed_at_unix_millis, window_start_unix_millis: sample.window_start_unix_millis, has_gap: sample.has_gap,
    cpu_usage_basis_points: sample.cpu_usage_basis_points, memory_total_bytes: sample.memory_total_bytes, memory_used_bytes: sample.memory_used_bytes, root_total_bytes: sample.root_total_bytes, root_used_bytes: sample.root_used_bytes,
    rx_bps: sample.interfaces.every(n => n.rx_bps !== null) ? sample.interfaces.reduce((sum, n) => sum + n.rx_bps!, 0) : null,
    tx_bps: sample.interfaces.every(n => n.tx_bps !== null) ? sample.interfaces.reduce((sum, n) => sum + n.tx_bps!, 0) : null,
  });
  return { observed_at_unix_millis: Date.now(), instances: workspace.instances.map(item => ({ id: item.desired.id, state: item.report?.power === 'stopped' ? 'stopped' : data.state, latest: data.latest ? cardSample(data.latest) : null, samples: data.samples.map(cardSample), month_used_bytes: 22 * 1024 ** 3, month_limit_bytes: 100 * 1024 ** 3, month_coverage_gap: false, month_observed_at_unix_millis: Date.now() - 5000 })) };
}
