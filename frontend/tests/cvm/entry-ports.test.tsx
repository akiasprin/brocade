import { expect, it } from 'vitest';
import { entryModeFromPorts, entryPortText, entrySshPort } from '../../src/panes/cvm/entry-ports';

it('uses one interval shape for single ports and maps SSH within an offset interval', () => {
  expect(entryModeFromPorts('tcp', '30022', '22')).toEqual({
    kind: 'l4_range',
    protocol: 'tcp',
    listen_start: 30022,
    listen_end: 30022,
    target_start: 22,
    target_end: 22,
  });
  const mode = entryModeFromPorts('tcp_udp', '30000-30009', '20-29');
  expect(entrySshPort(mode)).toBe(30002);
  expect(entryPortText(mode)).toBe('30000-30009');
  expect(entryPortText(mode, true)).toBe('20-29');
  expect(entrySshPort(entryModeFromPorts('udp', '30000-30009', '20-29'))).toBeNull();
});
it.each([
  ['30000-30001', '80'],
  ['0-2', '1-3'],
  ['65534-65536', '10-12'],
  ['100-99', '100-99'],
  ['1-4097', '1-4097'],
  ['100-', '100-'],
])('rejects ambiguous or invalid port mappings %s → %s', (listen, target) => {
  expect(() => entryModeFromPorts('tcp', listen, target)).toThrow();
});

import {
  initialEntryTraffic,
  entryTrafficFromDraft,
  parsePortRanges,
  effectiveEntryRanges,
  entryUsageConflict,
} from '../../src/panes/cvm/entry-policy';

it('normalizes lists and ranges and keeps reserved host SSH distinct from guest SSH', () => {
  expect(parsePortRanges('443, 22  8002-8004，8000-8002')).toEqual([
    { start: 22, end: 22 },
    { start: 443, end: 443 },
    { start: 8000, end: 8004 },
  ]);
  const offer = {
    kind: 'dedicated',
    address: '203.0.113.8',
    reserved_ports: { tcp: [{ start: 22, end: 22 }], udp: [] },
  } as const;
  const mutable = structuredClone({ ...offer, reserved_ports: { tcp: [{ start: 22, end: 22 }], udp: [] } });
  const draft = initialEntryTraffic();
  draft.tcp.mappings = '30022 → 22';
  const mode = entryTrafficFromDraft(draft, mutable);
  expect(entrySshPort(mode, mutable.reserved_ports)).toBe(30022);
  expect(entrySshPort(entryTrafficFromDraft(initialEntryTraffic(), mutable), mutable.reserved_ports)).toBeNull();
  expect(effectiveEntryRanges(mode, 'tcp', mutable)).toEqual([
    { start: 1, end: 21 },
    { start: 23, end: 65535 },
  ]);
  expect(effectiveEntryRanges(mode, 'udp', mutable)).toEqual([{ start: 1, end: 65535 }]);
});
it('bounds shared all to the offered interval and checks effective occupancy', () => {
  const offer = {
    kind: 'shared',
    address: '203.0.113.8',
    port_start: 30000,
    port_end: 30099,
    reserved_ports: { tcp: [{ start: 30050, end: 30050 }], udp: [] },
  } as const;
  const value = { ...offer, reserved_ports: { tcp: [{ start: 30050, end: 30050 }], udp: [] } };
  const draft = initialEntryTraffic(true);
  draft.tcp.all = true;
  const mode = entryTrafficFromDraft(draft, value);
  expect(entrySshPort(mode, value.reserved_ports, value)).toBeNull();
  expect(effectiveEntryRanges(mode, 'tcp', value)).toEqual([
    { start: 30000, end: 30049 },
    { start: 30051, end: 30099 },
  ]);
  expect(
    entryUsageConflict(mode, value, {
      whole_taken: false,
      ports: [{ protocol: 'tcp', port_start: 30050, port_end: 30050 }],
    }),
  ).toBe(false);
  draft.tcp.mappings = '30000-30099 → 40000-40098';
  expect(() => entryTrafficFromDraft(draft, value)).toThrow(/等长/);
  draft.tcp.mappings = '30000-30099 → 40000-40099';
  expect(() => entryTrafficFromDraft(draft, value)).not.toThrow();
  draft.icmp = true;
  expect(() => entryTrafficFromDraft(draft, value)).toThrow(/共享/);
});
