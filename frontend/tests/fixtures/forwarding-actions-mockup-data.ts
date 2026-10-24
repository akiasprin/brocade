// Synthetic data for forwarding-actions-mockup. Never imported by the app.
import type {
  ForwardingEntitlement,
  ForwardingOverview,
  ForwardingProtocol,
  ForwardingQuotaJob,
  ForwardingRule,
} from '../../src/api';

export const GiB = 1024 ** 3;

export type JobStatus = ForwardingQuotaJob['status'];

export interface Job {
  id: number;
  suspended: boolean;
  status: JobStatus;
  at: string;
  error?: string | null;
}

export interface Entry {
  id: string;
  name: string;
  country: string;
  ipv4: string;
  ipv6: string | null;
  enabled: boolean;
  tcp: boolean;
  udp: boolean;
  start: number;
  end: number;
  max: number;
  allowPrivate: boolean;
  limit: number | null;
  used: number;
  /** 机器还没应用最新授权 / 应用失败；配额暂停由已用量与额度推出 */
  sync?: 'pending' | 'error';
  lastError?: string | null;
  jobs: Job[];
}

export type RuleStatus = ForwardingRule['runtime_status'];

export interface Rule {
  id: number;
  nodeId: string;
  name: string;
  protocol: ForwardingProtocol;
  port: number;
  host: string;
  targetPort: number;
  enabled: boolean;
  status: RuleStatus;
  down: number;
  up: number;
  created: string;
}

export interface Candidate {
  id: string;
  name: string;
  country: string;
}

export interface Scene {
  label: string;
  note: string;
  entries: Entry[];
  rules: Rule[];
  candidates: Candidate[];
}

const entry = (value: Partial<Entry> & Pick<Entry, 'id' | 'name' | 'country' | 'ipv4'>): Entry => ({
  ipv6: null,
  enabled: true,
  tcp: true,
  udp: true,
  start: 30_000,
  end: 59_999,
  max: 100,
  allowPrivate: true,
  limit: null,
  used: 0,
  jobs: [],
  ...value,
});

let nextRule = 1;
const rule = (nodeId: string, port: number, target: string, month: number, extra: Partial<Rule> = {}): Rule => {
  const at = target.lastIndexOf(':');
  return {
    id: nextRule++,
    nodeId,
    name: '',
    protocol: 'tcp_udp',
    port,
    host: target.slice(0, at),
    targetPort: Number(target.slice(at + 1)),
    enabled: true,
    status: 'active',
    down: month * 0.78 * GiB,
    up: month * 0.22 * GiB,
    created: '2026-10-0' + ((port % 9) + 1) + ' 08:00:00+00',
    ...extra,
  };
};

const CANDIDATES: Candidate[] = [
  { id: 'sg-1', name: '新加坡入口', country: 'SG' },
  { id: 'de-1', name: '法兰克福入口', country: 'DE' },
  { id: 'kr-1', name: '首尔入口', country: 'KR' },
];

const tokyo = () =>
  entry({
    id: 'jp-1',
    name: '东京入口',
    country: 'JP',
    ipv4: '203.0.113.24',
    ipv6: '2001:db8:24::1',
    limit: 1000 * GiB,
    used: 205 * GiB,
    jobs: [{ id: 91, suspended: false, status: 'succeeded', at: '10-01 00:00' }],
  });

const losAngeles = (enabled = true) =>
  entry({ id: 'us-1', name: '洛杉矶入口', country: 'US', ipv4: '198.51.100.7', used: 5 * GiB, enabled });

const tokyoRules = () => [
  rule('jp-1', 30000, 'example.com:443', 96),
  rule('jp-1', 30001, 'git.example.com:22', 3.2, { name: '公司 Git', protocol: 'tcp' }),
  rule('jp-1', 30002, '[2001:db8:1234:5678:abcd:ffff:1234:5678]:443', 58),
  rule('jp-1', 30003, '10.8.0.12:3389', 47.8, { name: '家里远程桌面' }),
];

export const SCENES = {
  typical: (): Scene => {
    nextRule = 1;
    return {
      label: '两台机器',
      note: '东京设了 1000 GiB 配额，洛杉矶不限；共 5 条规则。',
      entries: [tokyo(), losAngeles()],
      rules: [...tokyoRules(), rule('us-1', 30000, 'media.example.net:8443', 5)],
      candidates: CANDIDATES,
    };
  },
  stopped: (): Scene => {
    nextRule = 1;
    return {
      label: '含已停止',
      note: '洛杉矶已停止转发：行上直接给「恢复」，移除放进编辑里。',
      entries: [tokyo(), losAngeles(false)],
      rules: [
        ...tokyoRules(),
        rule('us-1', 30000, 'media.example.net:8443', 5, { status: 'disabled' }),
        rule('us-1', 30001, 'ssh.example.net:22', 0.4, { status: 'disabled', protocol: 'tcp' }),
      ],
      candidates: CANDIDATES,
    };
  },
  fleet: (): Scene => {
    nextRule = 1;
    return {
      label: '五台 · 含异常',
      note: '配额暂停、同步中、收敛失败、已停止各一台；规则上限有一台已满。',
      entries: [
        tokyo(),
        entry({
          id: 'jp-2',
          name: '大阪入口',
          country: 'JP',
          ipv4: '203.0.113.88',
          limit: 500 * GiB,
          used: 512.4 * GiB,
          max: 3,
          jobs: [
            { id: 104, suspended: true, status: 'succeeded', at: '10-18 21:40' },
            { id: 77, suspended: false, status: 'succeeded', at: '10-01 00:00' },
          ],
        }),
        entry({ id: 'sg-1', name: '新加坡入口', country: 'SG', ipv4: '192.0.2.61', used: 31 * GiB, sync: 'pending' }),
        entry({
          id: 'de-1',
          name: '法兰克福入口',
          country: 'DE',
          ipv4: '192.0.2.140',
          tcp: true,
          udp: false,
          start: 40_000,
          end: 40_099,
          max: 20,
          allowPrivate: false,
          used: 2.6 * GiB,
          sync: 'error',
          lastError: 'nftables: 端口 40002 已被宿主机上的其他进程占用',
        }),
        losAngeles(false),
      ],
      rules: [
        ...tokyoRules(),
        rule('jp-2', 30000, 'nas.example.org:5001', 300, { status: 'quota-paused' }),
        rule('jp-2', 30001, 'cam.example.org:554', 150, { status: 'quota-paused', protocol: 'udp' }),
        rule('jp-2', 30002, 'game.example.org:27015', 62.4, { status: 'quota-paused' }),
        rule('sg-1', 30000, 'api.example.com:443', 31, { status: 'pending' }),
        rule('de-1', 40000, 'db.example.de:5432', 2.1, { protocol: 'tcp' }),
        rule('de-1', 40002, 'metrics.example.de:9100', 0.5, { protocol: 'tcp', status: 'error' }),
        rule('us-1', 30000, 'media.example.net:8443', 5, { status: 'disabled' }),
      ],
      candidates: [{ id: 'kr-1', name: '首尔入口', country: 'KR' }],
    };
  },
  empty: (): Scene => ({
    label: '已授权 · 无规则',
    note: '已授权东京入口，还没有规则。',
    entries: [{ ...tokyo(), used: 0 }],
    rules: [],
    candidates: CANDIDATES,
  }),
  ungranted: (): Scene => ({
    label: '尚未授权',
    note: '没有已授权机器。',
    entries: [],
    rules: [],
    candidates: CANDIDATES,
  }),
} as const;

export type SceneKey = keyof typeof SCENES;
export const SCENE_KEYS = Object.keys(SCENES) as SceneKey[];

/* ── 同一份数据转成生产接口形状，供「现状」直接挂生产组件 ── */

export function overviewOf(scene: Scene): ForwardingOverview {
  const entitlements: ForwardingEntitlement[] = scene.entries.map(item => {
    const latest = item.jobs[0];
    return {
      node_id: item.id,
      node_name: item.name,
      public_ipv4: item.ipv4,
      public_ipv6: item.ipv6,
      enabled: item.enabled,
      tcp_allowed: item.tcp,
      udp_allowed: item.udp,
      listen_port_start: item.start,
      listen_port_end: item.end,
      max_rules: item.max,
      allow_private_targets: item.allowPrivate,
      rule_count: scene.rules.filter(rule => rule.nodeId === item.id).length,
      desired_generation: 3,
      applied_generation: item.sync === 'pending' ? 2 : 3,
      reconcile_status: item.sync === 'error' ? 'error' : item.sync === 'pending' ? 'pending' : 'converged',
      last_error: item.lastError ?? null,
      cycle_limit_bytes: item.limit,
      cycle_used_bytes: item.used,
      quota_suspended: item.limit != null && item.used >= item.limit,
      quota_job_id: latest?.id ?? null,
      quota_job_status: latest?.status ?? null,
      quota_job_error: latest?.error ?? null,
    };
  });
  const rules: ForwardingRule[] = scene.rules.map(item => ({
    id: item.id,
    tenant_id: 'ops',
    user_id: 'alice',
    node_id: item.nodeId,
    name: item.name || `${item.host}:${item.targetPort}`,
    protocol: item.protocol,
    listen_port: item.port,
    target_host: item.host,
    target_port: item.targetPort,
    enabled: item.status !== 'disabled',
    runtime_status: item.status,
    uplink_bytes: item.up * 1.6,
    downlink_bytes: item.down * 1.6,
    created_at: item.created,
    updated_at: item.created,
  }));
  return { tenant_id: 'ops', user_id: 'alice', entitlements, rules };
}

export function cycleUsageOf(scene: Scene) {
  return {
    periods: [
      { tenant_id: 'ops', user_id: 'alice', starts_at: '2026-09-30T16:00:00Z', ends_at: '2026-10-31T16:00:00Z' },
    ],
    views: [],
    forwarding: scene.rules.map(item => ({
      tenant_id: 'ops',
      user_id: 'alice',
      node_id: item.nodeId,
      rule_id: item.id,
      downlink_bytes: item.down,
      uplink_bytes: item.up,
    })),
  };
}

export function jobsOf(scene: Scene, nodeId: string) {
  const found = scene.entries.find(item => item.id === nodeId);
  return {
    jobs: (found?.jobs ?? []).map(job => ({
      id: job.id,
      node_id: nodeId,
      generation: 3,
      suspended: job.suspended,
      status: job.status,
      last_error: job.error ?? null,
      created_at: `2026-${job.at}:00+08`,
      updated_at: `2026-${job.at}:00+08`,
    })),
    next_before_id: null,
  };
}
