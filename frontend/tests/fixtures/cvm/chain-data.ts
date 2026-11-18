import type { CvmChainCatalog, CvmWorkspace } from '../../../src/api';
export function chainCatalog(workspace: CvmWorkspace, id: string | null): CvmChainCatalog {
  const instance = workspace.instances.find(i => i.desired.id === id);
  const owner = instance ? { tenant_id: instance.tenant_id, user_id: instance.user_id, name: instance.user_id } : null;
  return {
    owner,
    supported: !!instance && instance.desired.spec.execution?.kind !== 'baremetal',
    reason: instance ? null : '请选择容器',
    hidden_unauthorized: 2,
    chains: [
      {
        id: 'chain-us',
        app_id: 'app-main',
        app_name: '主项目',
        name: '洛杉矶 · 三跳线路',
        country: 'US',
        machines: ['香港', '东京', '洛杉矶'],
        authorized: owner?.user_id === 'alice',
        used_by: [workspace.instances[0]?.desired.id ?? ''],
        ipv6: true,
        reachable: true,
        rtt_ms: 143,
      },
      {
        id: 'chain-jp',
        app_id: 'app-main',
        app_name: '主项目',
        name: '东京 IIJ',
        country: 'JP',
        machines: ['香港', '东京'],
        authorized: owner?.user_id === 'alice',
        used_by: [workspace.instances[0]?.desired.id ?? ''],
        ipv6: false,
        reachable: false,
        rtt_ms: null,
      },
    ],
    traffic: owner
      ? [
          {
            kind: 'access',
            app_id: 'app-main',
            policy: { quota_bytes: '10737418240', exhausted_action: { kind: 'suspend' } },
            used_bytes: '2276332666',
            exhausted: false,
            service_available: true,
            unsupported_nodes: [],
          },
        ]
      : [],
  };
}
