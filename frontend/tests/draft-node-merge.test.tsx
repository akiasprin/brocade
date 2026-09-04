import { afterEach, describe, expect, it } from 'vitest';
import { draft } from '../src/draft';

afterEach(() => {
  draft.clear();
});

describe('node draft merge', () => {
  it('keeps independent WireGuard fields in one pending node update', () => {
    draft.init('draft-node-merge-test');
    draft.push({
      op: 'update_node',
      node_id: 'akko-lon',
      node: { wg_listen_port: 51_821 },
    });
    draft.push({
      op: 'update_node',
      node_id: 'akko-lon',
      node: { wg_transport: { t: 'fake_tcp', v: { port: 39_743 } } },
    });

    expect(draft.ops()).toEqual([
      {
        op: 'update_node',
        node_id: 'akko-lon',
        node: {
          wg_listen_port: 51_821,
          wg_transport: { t: 'fake_tcp', v: { port: 39_743 } },
        },
      },
    ]);
  });

  it('uses the latest value without dropping unrelated node fields', () => {
    draft.init('draft-node-latest-test');
    draft.push({ op: 'update_node', node_id: 'akko-lon', node: { mtu: 1_420, wg_listen_port: 51_820 } });
    draft.push({ op: 'update_node', node_id: 'akko-lon', node: { wg_listen_port: 51_821 } });

    expect(draft.ops()[0]).toMatchObject({
      node: { mtu: 1_420, wg_listen_port: 51_821 },
    });
  });

  it('keeps different disabled links separate and canonicalizes each pair', () => {
    draft.init('draft-wireguard-link-pairs-test');
    draft.push({ op: 'set_wireguard_link_disabled', a: 'tw', b: 'lax', disabled: true });
    draft.push({ op: 'set_wireguard_link_disabled', a: 'tw', b: 'hkg', disabled: true });
    draft.push({ op: 'set_wireguard_link_disabled', a: 'lax', b: 'tw', disabled: false });

    expect(draft.ops()).toEqual([
      { op: 'set_wireguard_link_disabled', a: 'lax', b: 'tw', disabled: false },
      { op: 'set_wireguard_link_disabled', a: 'hkg', b: 'tw', disabled: true },
    ]);
  });
});
