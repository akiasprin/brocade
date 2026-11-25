import type { CvmArtifactHistory, CvmArtifactKind, CvmInstance } from '../../../src/api';
export function artifactHistory(): CvmArtifactHistory {
  const objects = {
    'cube/sandbox.json': {
      backend: 'plain',
      containers: [{ resources: { cpu: '2000m', mem: '1024Mi' } }],
      labels: { 'brocade.cvm.instance': 'cvm-alice' },
    },
    'cube/egress.json': { allow_internet_access: true, allow_out: [], deny_out: ['0.0.0.0/0'], rules: [] },
    'cube/attachments.json': {
      contract: 'cubevs-attachments-v2',
      instance_id: 'cvm-alice',
      interfaces: [{ id: 'nic-business', source_id: 'singapore', purpose: 'business' }],
      routing: {
        dns: [],
        priorities: [{ interface_id: 'nic-business', ipv4: 100 }],
        routes: [{ destination: '0.0.0.0/0', interface_id: 'nic-business' }],
      },
    },
    'guest/init.json': {
      contract: 'brocade-guest-init-v2',
      user: 'cube',
      credential_revision: 1,
      regenerate_host_keys_once: true,
    },
  };
  const current = {
    compiler: 'cube-artifacts-v1',
    input_sha256: 'a'.repeat(64),
    spec_sha256: 'b'.repeat(64),
    files: Object.entries(objects).map(([kind, value], index) => ({
      kind: kind as CvmArtifactKind,
      content: JSON.stringify(value, null, 2) + '\n',
      sha256: String(index + 1).repeat(64),
    })),
  };
  const previous = structuredClone(current);
  previous.files[2].sha256 = '9'.repeat(64);
  previous.files[2].content =
    JSON.stringify({ ...objects['cube/attachments.json'], routing: { dns: [], priorities: [], routes: [] } }, null, 2) +
    '\n';
  return { generation: 2, current, previous };
}
export function artifactReceipt(instance: CvmInstance, mismatch = false) {
  const data = artifactHistory().current!;
  instance.report!.generation = instance.desired.generation;
  instance.report!.operation_id = instance.desired.operation_id;
  instance.report!.status = mismatch ? 'failed' : 'applied';
  instance.report!.artifacts = {
    compiler: data.compiler,
    input_sha256: data.input_sha256,
    mismatches: mismatch ? ['cube/attachments.json'] : [],
    files: data.files.map(f => ({ kind: f.kind, compiled_sha256: f.sha256, effective_sha256: null, bindings: {} })),
  };
}
