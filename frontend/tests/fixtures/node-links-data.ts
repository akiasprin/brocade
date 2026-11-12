import type { GrantProbeJob, GrantProbePlan, GrantProbePlanItem } from '../../src/api';

// Synthetic API data for the production node browser, with mixed regions, families and protocols.
export function nodeLinksFixture(tenant: string, user: string, scene: string) {
  const links: { uri: string; protocol: string; family: string; item?: GrantProbePlanItem }[] = [];
  for (const [region, label, count] of [
    ['HK', '香港示例', 6],
    ['JP', '东京示例', 4],
    ['US', '美国示例', 3],
    ['', '未标地区', 1],
  ] as const) {
    for (let index = 1; index <= count; index++) {
      const flag = Array.from(region, letter => String.fromCodePoint(127462 + letter.charCodeAt(0) - 65)).join('');
      const base = `${flag}${label} ${String(index).padStart(2, '0')}`;
      const families = region === 'JP' && index === 1 ? ['ipv4', 'ipv6'] : ['ipv4'];
      for (const family of families) {
        for (const protocol of ['vless', 'hysteria2'] as const) {
          const name = `${base}${protocol === 'hysteria2' ? ' | QUIC' : ''}${family === 'ipv6' ? ' | v6' : ''}`;
          const host =
            family === 'ipv6' ? '[2001:db8:1234:5678:abcd:ef01:2345:6789]' : `192.0.2.${index + links.length}`;
          const uri =
            protocol === 'vless'
              ? `vless://fixture@${host}:48000?encryption=none&security=reality&type=tcp#${encodeURIComponent(name)}`
              : `hysteria2://fixture@${host}:30000-30099?sni=node.example.test#${encodeURIComponent(name)}`;
          links.push({
            uri,
            protocol,
            family,
            item: {
              id: `${region || 'other'}-${index}-${family}-${protocol}`,
              name,
              app_id: 'example',
              app_name: '示例应用',
              chain_id: `${region}-${index}`,
              ingress_id: `${region}-${index}`,
              family: family as 'ipv4' | 'ipv6',
              protocol,
            },
          });
        }
      }
      if (region === 'HK' && index === 1)
        links.push({
          uri: `# @brocade-name=${encodeURIComponent(`${base} | MTProxy`)}\ntg://proxy?server=192.0.2.250&port=15443&secret=dd00112233445566778899aabbccddeeff`,
          protocol: 'mtproto',
          family: 'ipv4',
        });
    }
  }
  const plan: GrantProbePlan = {
    serving_revision: 1,
    serving_generation: 1,
    items: links.flatMap(link => link.item ?? []),
  };
  const makeJob = (ids: string[], running = false): GrantProbeJob => ({
    id: 'node-links-probe',
    tenant_id: tenant,
    user_id: user,
    serving_revision: 1,
    serving_generation: 1,
    status: running ? 'running' : 'completed',
    message: running ? '拨测中' : '拨测完成',
    created_at_unix_secs: 1,
    finished_at_unix_secs: running ? null : 2,
    items: plan.items
      .filter(item => ids.length === 0 || ids.includes(item.id))
      .map((item, index) => ({
        ...item,
        status: running
          ? index === 0
            ? 'running'
            : 'waiting'
          : item.id === 'HK-1-ipv4-vless' || item.id === 'JP-1-ipv6-hysteria2'
            ? 'failed'
            : 'passed',
        detail: running
          ? null
          : item.id === 'HK-1-ipv4-vless'
            ? '握手超时，请检查入口连接'
            : item.id === 'JP-1-ipv6-hysteria2'
              ? 'IPv6 连接被拒绝'
              : null,
        ttfb_ms: null,
      })),
  });
  let job = scene === 'idle' ? null : makeJob([], scene === 'running');
  return {
    job,
    content: (params: URLSearchParams) =>
      links
        .filter(
          link =>
            (!params.has('protocol') || link.protocol === params.get('protocol')) &&
            (!params.has('family') || link.family === (params.get('family') === 'v4' ? 'ipv4' : 'ipv6')),
        )
        .map(link => link.uri)
        .join('\n'),
    transport: (url: URL, init?: RequestInit) => {
      if (url.pathname === '/grant-probes/capability')
        return Response.json({ available: true, version: 'fixture', concurrency: 30, reason: null });
      if (url.pathname === `/users/${tenant}/${user}/grant-probes`) {
        if (init?.method === 'POST') {
          const { item_ids } = JSON.parse(String(init.body)) as { item_ids: string[] };
          job = makeJob(item_ids);
          return Response.json({ job, reused: false });
        }
        return Response.json(plan);
      }
      if (url.pathname === '/grant-probes/node-links-probe') {
        if (init?.method === 'DELETE' && job)
          job = {
            ...job,
            status: 'canceled',
            message: '已取消拨测',
            items: job.items.map(item => ({ ...item, status: 'canceled' })),
          };
        return Response.json(job);
      }
      return null;
    },
  };
}
