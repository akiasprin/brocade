// Synthetic entry board: published entry addresses, their occupancy and visible entries.
import type { CvmEntry, CvmEntryBoard, CvmEntryPublicationView, CvmWorkspace } from '../../../src/api';

const masked = '0.0.0.0';
export function entryBoard(workspace: CvmWorkspace, scene: string): CvmEntryBoard {
  const ids = workspace.instances.map(item => item.desired.id);
  const first = ids[0];
  const second = ids[1] ?? first;
  const entries: CvmEntry[] = first
    ? [
        {
          id: 'entry-ssh',
          revision: 1,
          publication_id: 'entrypub-hk',
          provider_node: 'remote-ingress',
          instance_id: first,
          address: '203.0.113.20',
          visible_address: '203.0.113.20',
          mode: { kind: 'l4', protocol: 'tcp', listen_port: 30022, target_port: 22 },
          remote: true,
          state: 'active',
          error: null,
        },
        {
          id: 'entry-dns',
          revision: 2,
          publication_id: 'entrypub-hk',
          provider_node: 'remote-ingress',
          instance_id: second,
          address: '203.0.113.20',
          visible_address: '203.0.113.20',
          mode: { kind: 'l4', protocol: 'udp', listen_port: 30053, target_port: 53 },
          remote: true,
          state: scene === 'board' ? 'pending' : 'active',
          error: null,
        },
        {
          id: 'entry-web',
          revision: 1,
          publication_id: 'entrypub-local',
          provider_node: 'hk-hkix',
          instance_id: first,
          address: '10.8.0.30',
          visible_address: '198.51.100.30',
          mode: { kind: 'l3' },
          remote: false,
          state: 'active',
          error: null,
        },
      ]
    : [];
  const publications: CvmEntryPublicationView[] = [
    {
      id: 'entrypub-hk',
      revision: 1,
      admitting: true,
      retired: false,
      provider_name: 'GreenVPS · 香港网络',
      active_entries: 2,
      spec: {
        name: '香港共享入口',
        provider_node: 'remote-ingress',
        addresses: [{ address: '203.0.113.20', kind: 'shared', port_start: 30000, port_end: 30999 }],
        transports: [],
        grants: [{ tenant_id: 'platform', user_id: 'alice' }],
        max_entries: 64,
      },
      usage: [
        {
          address: '203.0.113.20',
          whole_taken: false,
          whole_entry_id: null,
          ports: [
            { protocol: 'tcp', port_start: 30022, port_end: 30022, entry_id: 'entry-ssh' },
            { protocol: 'tcp', port_start: 30080, port_end: 30081, entry_id: null },
            { protocol: 'udp', port_start: 30053, port_end: 30053, entry_id: 'entry-dns' },
          ],
        },
      ],
    },
    {
      id: 'entrypub-local',
      revision: 1,
      admitting: true,
      retired: false,
      provider_name: '香港 HKIX · 计算',
      active_entries: 1,
      spec: {
        name: '香港独立地址',
        provider_node: 'hk-hkix',
        addresses: [
          { address: '10.8.0.30', public_address: '198.51.100.30', kind: 'dedicated' },
          { address: '10.8.0.31', public_address: '198.51.100.31', kind: 'dedicated' },
        ],
        transports: [],
        grants: [{ tenant_id: 'platform', user_id: 'alice' }],
        max_entries: 8,
      },
      usage: [
        { address: '198.51.100.30', whole_taken: true, whole_entry_id: 'entry-web', ports: [] },
        { address: '198.51.100.31', whole_taken: false, whole_entry_id: null, ports: [] },
      ],
    },
  ];
  if (workspace.redacted)
    for (const item of [...entries, ...publications.flatMap(p => p.usage)] as {
      address: string;
      visible_address?: string;
    }[]) {
      item.address = masked;
      if ('visible_address' in item) item.visible_address = masked;
    }
  return { publications, entries, can_manage: !workspace.redacted };
}
