import type { CvmAllocatedNic, CvmInstance, CvmNicCatalog, CvmNicState, CvmNicPublicationSpec } from '../../../src/api';

const spec: CvmNicPublicationSpec = {
  name: '香港 · 公网出口',
  provider_node: 'remote-ingress',
  resource_domain: 'pool-hk',
  ipv4_pool: '10.60.0.0/24',
  ipv6_pool: 'fd60::/64',
  public_ipv4_pool: null,
  public_ipv6_pool: null,
  shared_ipv4: '203.0.113.20',
  shared_ipv6: '2001:db8::20',
  address_mode: 'shared_nat',
  destinations: ['0.0.0.0/0', '::/0'],
  route_presets: ['0.0.0.0/0', '::/0'],
  dns_suggestions: ['1.1.1.1', '2606:4700:4700::1111'],
  grants: [{ tenant_id: 'platform', user_id: 'alice' }],
  transports: [
    {
      kind: 'wireguard',
      listen_side: 'provider',
      endpoint: '203.0.113.20',
      port_start: 42000,
      port_end: 42999,
      mtu: 1400,
    },
    { kind: 'xray', listen_side: 'provider', endpoint: '203.0.113.20', port_start: 43000, port_end: 43999, mtu: 1400 },
  ],
  limits: { max_attachments: 64, upload_bps: null, download_bps: null, monthly_bytes: 100 * 1024 ** 3 },
};
export function allocatedNic(id = 'nic-business'): CvmAllocatedNic {
  return {
    slot: 1,
    publication_id: 'network-hk',
    publication_revision: 1,
    provider_node: spec.provider_node,
    resource_domain: spec.resource_domain,
    allocation_id: `allocation-${id}`,
    mac: '02:00:00:00:00:02',
    ipv4: '10.60.0.2',
    ipv6: 'fd60::2',
    public_ipv4: spec.shared_ipv4,
    public_ipv6: spec.shared_ipv6,
    address_mode: spec.address_mode,
    destinations: [...spec.destinations],
    transport: 'wireguard',
    transport_generation: 1,
    mtu: 1400,
    blocked: false,
    quota_exceeded: false,
    limits: { ...spec.limits },
  };
}
export function nicCatalog(manage = true, instance?: CvmInstance): CvmNicCatalog {
  return {
    attachments:
      manage && instance
        ? instance.desired.spec.interfaces
            .filter(nic => nic.allocated)
            .map(nic => ({
              allocation_id: nic.allocated!.allocation_id,
              interface_id: nic.id,
              instance_id: instance.desired.id,
              instance_name: instance.desired.spec.name,
              publication_id: nic.allocated!.publication_id,
              publication_revision: nic.allocated!.publication_revision,
              generation: instance.desired.generation,
              attached: true,
              route_references: 3,
              cleanup_pending: false,
            }))
        : [],
    can_manage: manage,
    publications: [{ id: 'network-hk', revision: 2, admitting: true, retired: false, spec: structuredClone(spec) }],
    providers: manage
      ? [
          {
            node_id: spec.provider_node,
            name: 'GreenVPS · 香港网络',
            fresh: true,
            capabilities: {
              contract: 67,
              transports: ['wireguard', 'xray'],
              provide: true,
              consume: false,
              max_bindings: 64,
              addresses: ['203.0.113.20', '2001:db8::20'],
              host_ports: [{ protocol: 'tcp', port: 22 }],
              error: null,
              entries: true,
              route_service: true,
            },
          },
        ]
      : [],
  };
}
export function nicState(instance: CvmInstance, _redacted = false): CvmNicState {
  const bindings: CvmNicState['bindings'] = [];
  const interface_options: CvmNicState['interface_options'] = {};
  const usage: CvmNicState['usage'] = {};
  for (const nic of instance.desired.spec.interfaces.filter(nic => nic.allocated)) {
    const id = `binding-${nic.id}`;
    const base = {
      id,
      generation: 1,
      phase: 'active' as const,
      configured: true,
      reachable: true,
      rx_bytes: 12345,
      tx_bytes: 54321,
      error: null,
    };
    bindings.push({
      consumer_fresh: true,
      provider_fresh: true,
      id,
      interface_id: nic.id,
      transport: nic.allocated!.transport,
      generation: 1,
      phase: 'active',
      consumer: { ...base, side: 'consumer' },
      provider: { ...base, side: 'provider' },
      error: null,
    });
    interface_options[nic.id] = { transports: ['wireguard', 'xray'], error: null };
    usage[nic.id] = {
      period: '2026-11-01',
      uploaded_bytes: 1024 ** 3,
      downloaded_bytes: 2 * 1024 ** 3,
      coverage_gap: false,
      observed_at: 1793944800000,
    };
  }
  return {
    bindings,
    usage,
    interface_options,
    mount_options: { 'network-hk': { transports: ['wireguard', 'xray'], error: null } },
  };
}
