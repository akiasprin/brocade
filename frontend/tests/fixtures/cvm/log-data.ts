import type { CvmNetworkNodeLogs } from '../../../src/api';
export function networkLogs(nodeId = 'remote-ingress'): { nodes: CvmNetworkNodeLogs[] } {
  const at = Date.parse('2026-11-04T10:29:52Z') / 1000;
  return {
    nodes: [
      {
        node_id: nodeId,
        name: nodeId === 'remote-ingress' ? '香港入口机器' : '家中开发宿主',
        fresh: true,
        domains: [
          { domain: 'entry', desired_generation: 12, reported_generation: 12, error: null },
          { domain: 'egress', desired_generation: 8, reported_generation: 8, error: null },
        ],
        diagnostics: {
          collected_at: at,
          dropped_events: 9,
          resources: [
            {
              id: 'entry-tunnel-fixture',
              phase: 'failed',
              since: at - 124,
              waiting_seconds: 124,
              message: 'Phantun / WireGuard 握手等待超过 120 秒；请查看两端日志。Agent 将继续重试',
            },
          ],
          events: [
            {
              at: at - 124,
              resource_id: null,
              source: 'agent',
              level: 'info',
              message: '收到入口配置：版本 12，1 条转发、0 条投递、1 条通道',
            },
            {
              at: at - 123,
              resource_id: 'entry-tunnel-fixture',
              source: 'phantun',
              level: 'info',
              message: 'Phantun server started; listening on port 44000',
            },
            {
              at: at - 121,
              resource_id: 'entry-tunnel-fixture',
              source: 'agent',
              level: 'info',
              message: '等待 Phantun / WireGuard 握手；请检查对端进程、承载端口及防火墙',
            },
            {
              at: at - 12,
              resource_id: 'entry-tunnel-fixture',
              source: 'phantun',
              level: 'warning',
              message: '连接等待超时，正在重试；authentication=[已隐藏]',
            },
            {
              at: at - 3,
              resource_id: 'entry-tunnel-fixture',
              source: 'agent',
              level: 'error',
              message: 'Phantun / WireGuard 握手等待超过 120 秒；Agent 将继续重试',
            },
            {
              at: at - 1,
              resource_id: 'binding-other',
              source: 'xray',
              level: 'warning',
              message: '另一条出口链路暂不可达，正在重连',
            },
          ],
        },
      },
    ],
  };
}
