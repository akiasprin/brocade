import { afterEach, describe, expect, it } from 'vitest';
import { draft } from '../src/draft';

const base = {
  id: 'ingress-1',
  chain_id: 'chain-1',
  node_id: 'node-1',
  bind: '0.0.0.0',
  port: 443,
  reality: {
    fallback_mode: 'global-site' as const,
    fallback_limits: { mode: 'off' as const },
    fallback_guard: true,
    dest: '',
    server_names: [],
    flow: '',
  },
  projection: {
    v4: { host: '198.51.100.10', port: 443, download: null },
  },
  wires: {
    vless: {
      kind: 'vless-reality-xhttp' as const,
      xhttp: { path: '/xhttp', host: null, xmux: null, tuning: null, mode: 'auto' as const },
    },
  },
};

afterEach(() => {
  draft.clear();
});

describe('ingress draft merge', () => {
  it('keeps independent projection and XHTTP changes in one pending ingress', () => {
    const projectionChange = {
      ...base,
      projection: { v4: { host: 'edge.example.net', port: 8443, download: null } },
    };
    const xhttpChange = {
      ...base,
      wires: {
        ...base.wires,
        vless: {
          ...base.wires.vless,
          xhttp: { ...base.wires.vless.xhttp, host: 'upload.example.net' },
        },
      },
    };

    draft.init('draft-ingress-merge-test');
    draft.pushIngress('app-1', base, projectionChange);
    draft.pushIngress('app-1', base, xhttpChange);

    expect(draft.ops()).toHaveLength(1);
    expect(draft.ops()[0]).toMatchObject({
      op: 'upsert_ingress',
      ingress: {
        projection: { v4: { host: 'edge.example.net', port: 8443 } },
        wires: { vless: { xhttp: { host: 'upload.example.net' } } },
      },
    });
  });

  it('uses the later value when both edits target the same field', () => {
    const first = { ...base, bind: '127.0.0.1' };
    const second = { ...base, bind: '127.0.0.2' };

    draft.init('draft-ingress-merge-same-field-test');
    draft.pushIngress('app-1', base, first);
    draft.pushIngress('app-1', base, second);

    expect(draft.ops()[0]).toMatchObject({ ingress: { bind: '127.0.0.2' } });
  });

  it('does not resurrect a projection removed before an XHTTP-only edit', () => {
    const removed = { ...base, projection: { v4: null } };
    const xhttpChange = {
      ...base,
      projection: {
        v4: {
          ...base.projection.v4,
          download: { host: 'download.example.net', port: 8443, origin_port: null, http_host: null, mux: null },
        },
      },
    };

    draft.init('draft-ingress-merge-delete-test');
    draft.pushIngress('app-1', base, removed);
    draft.pushIngress('app-1', base, xhttpChange);

    expect(draft.ops()[0]).toMatchObject({ ingress: { projection: { v4: null } } });
  });
});
