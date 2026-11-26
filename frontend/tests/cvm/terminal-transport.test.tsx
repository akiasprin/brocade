import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { terminalTransport } from '../../src/panes/cvm/terminal-transport';

class Socket {
  static OPEN = 1;
  static CLOSED = 3;
  static instances: Socket[] = [];
  static grants = true;
  readyState = 0;
  bufferedAmount = 0;
  binaryType = 'arraybuffer';
  sent: unknown[] = [];
  onopen: (() => void) | null = null;
  onmessage: ((event: MessageEvent) => void) | null = null;
  onerror: (() => void) | null = null;
  onclose: (() => void) | null = null;
  constructor() {
    Socket.instances.push(this);
  }
  send(data: unknown) {
    this.sent.push(data);
    if (typeof data === 'string') {
      const value = JSON.parse(data);
      if (Socket.grants && value.type === 'control_probe')
        queueMicrotask(() => this.receive(JSON.stringify({ type: 'control_grant', nonce: value.nonce })));
    }
  }
  open() {
    this.readyState = 1;
    this.onopen?.();
  }
  receive(data: unknown) {
    this.onmessage?.(new MessageEvent('message', { data }));
  }
  close() {
    this.readyState = 3;
    this.onclose?.();
  }
}
class Channel {
  readyState = 'open';
  bufferedAmount = 0;
  sent: unknown[] = [];
  binaryType = 'arraybuffer';
  onmessage: ((event: MessageEvent) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  send(value: unknown) {
    this.sent.push(value);
  }
  receive(value: unknown) {
    this.onmessage?.(new MessageEvent('message', { data: value }));
  }
  close() {
    this.readyState = 'closed';
    this.onclose?.();
  }
}
class Peer extends EventTarget {
  static instances: Peer[] = [];
  static deny = false;
  static gathering = false;
  static early = false;
  channel = new Channel();
  connectionState = 'connected';
  iceGatheringState = Peer.gathering ? 'gathering' : 'complete';
  localDescription = { sdp: 'v=0\r\na=ice-ufrag:abcd\r\nm=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\n' };
  onconnectionstatechange: (() => void) | null = null;
  onicecandidate: ((event: { candidate: RTCIceCandidate | null }) => void) | null = null;
  remote = vi.fn();
  closed = false;
  constructor(readonly config: RTCConfiguration) {
    super();
    Peer.instances.push(this);
  }
  createDataChannel = vi.fn(() => this.channel);
  async createOffer() {
    if (Peer.deny) throw new DOMException('blocked', 'NotAllowedError');
    return this.localDescription;
  }
  async setLocalDescription() {
    if (Peer.early) {
      this.candidate('candidate:1 1 udp 2113929471 192.0.2.1 30000 typ host');
      this.candidate(null);
    }
  }
  candidate(value: string | null, usernameFragment = 'abcd') {
    this.onicecandidate?.({
      candidate: value === null ? null : ({ candidate: value, usernameFragment } as RTCIceCandidate),
    });
  }
  async setRemoteDescription(value: unknown) {
    this.remote(value);
  }
  close() {
    this.closed = true;
  }
}
const credentials = {
  session_id: 'a'.repeat(64),
  ticket: 'ticket',
  expires_in_seconds: 30,
  max_sessions_per_node: 3,
  history_bytes: 1024,
  webrtc: { supported: true, stun_servers: ['stun:stun.cloudflare.com:3478'] },
};
const tick = async () => {
  for (let i = 0; i < 8; i++) await Promise.resolve();
};
function connect(direct = true, trickle = false) {
  const options = { direct, route: vi.fn(), directLost: vi.fn(), permissionDenied: vi.fn() };
  const socket = terminalTransport(
    new URL('wss://console.example/terminal'),
    { ...credentials, webrtc: { ...credentials.webrtc, trickle } },
    options,
  );
  socket.onopen = () => socket.send(JSON.stringify({ ticket: 'ticket' }));
  const control = Socket.instances.at(-1)!;
  control.open();
  return { socket, control, options };
}
beforeEach(() => {
  Socket.instances = [];
  Socket.grants = true;
  Peer.instances = [];
  Peer.deny = false;
  Peer.gathering = false;
  Peer.early = false;
  vi.stubGlobal('WebSocket', Socket);
  vi.stubGlobal('RTCPeerConnection', Peer);
  vi.stubGlobal('isSecureContext', true);
});
afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
});
describe('terminal path selection', () => {
  it('sends the authenticated offer before candidates gathered during setLocalDescription', async () => {
    Peer.early = true;
    const { socket, control } = connect(true, true);
    await tick();
    const sent = control.sent.map(value => JSON.parse(String(value)));
    expect(sent[0]).toMatchObject({ ticket: 'ticket', trickle: true });
    expect(sent[0].rtc_offer).not.toContain('a=candidate:');
    expect(sent[1].type).toBe('control_probe');
    expect(sent[2]).toMatchObject({ type: 'ice_candidate', username_fragment: 'abcd' });
    expect(sent[3]).toEqual({ type: 'ice_complete', username_fragment: 'abcd' });
    socket.close();
  });
  it('forwards a public candidate arriving after 2.2 seconds without replacing the attachment', async () => {
    vi.useFakeTimers();
    Peer.gathering = true;
    const { socket, control } = connect(true, true);
    await tick();
    expect(JSON.parse(String(control.sent[0]))).toHaveProperty('rtc_offer');
    await vi.advanceTimersByTimeAsync(3100);
    const peer = Peer.instances[0];
    peer.candidate('candidate:2 1 udp 1686052607 203.0.113.9 41000 typ srflx raddr 192.168.1.2 rport 30000');
    expect(
      control.sent.map(value => JSON.parse(String(value))).filter(value => value.type === 'ice_candidate'),
    ).toEqual([
      expect.objectContaining({ candidate: expect.stringContaining('203.0.113.9'), username_fragment: 'abcd' }),
    ]);
    control.receive(JSON.stringify({ type: 'rtc_answer', sdp: 'answer' }));
    await tick();
    peer.channel.receive(JSON.stringify({ type: 'transport', kind: 'webrtc' }));
    socket.send(new Uint8Array([7]));
    expect(peer.channel.sent).toEqual([new Uint8Array([7])]);
    expect(Socket.instances).toHaveLength(1);
    socket.close();
  });
  it('deduplicates candidates, ignores stale generations and bounds the candidate exchange', async () => {
    const { socket, control } = connect(true, true);
    await tick();
    const peer = Peer.instances[0];
    const value = 'candidate:1 1 udp 2113929471 192.0.2.1 30000 typ host';
    peer.candidate(value, 'old1');
    peer.candidate(value);
    peer.candidate(value);
    for (let i = 1; i < 40; i++) peer.candidate(`candidate:${i + 1} 1 udp 2113929471 192.0.2.1 ${30000 + i} typ host`);
    const sent = control.sent.map(value => JSON.parse(String(value)));
    expect(sent.filter(value => value.type === 'ice_candidate')).toHaveLength(32);
    expect(sent.filter(value => value.type === 'ice_complete')).toHaveLength(1);
    expect(peer.onicecandidate).toBeNull();
    socket.close();
  });
  it('falls back explicitly instead of sending an incomplete legacy offer', async () => {
    vi.useFakeTimers();
    Peer.gathering = true;
    const { socket, control, options } = connect();
    await tick();
    await vi.advanceTimersByTimeAsync(2200);
    expect(control.sent).toEqual([JSON.stringify({ ticket: 'ticket' })]);
    expect(options.route).toHaveBeenCalledWith('console', 'ice_gather_timeout');
    socket.close();
  });
  it('stops trickling after relay selection without mixing candidates into terminal input', async () => {
    const { socket, control } = connect(true, true);
    await tick();
    control.receive(JSON.stringify({ type: 'transport', kind: 'console' }));
    const before = control.sent.length;
    Peer.instances[0].candidate('candidate:1 1 udp 2113929471 192.0.2.1 30000 typ host');
    expect(control.sent).toHaveLength(before);
    socket.send(new Uint8Array([3]));
    expect(control.sent.at(-1)).toEqual(new Uint8Array([3]));
    socket.close();
  });
  it('uses WebSocket without gathering candidates when explicitly selected', () => {
    const { socket, control, options } = connect(false);
    expect(Peer.instances).toHaveLength(0);
    expect(control.sent).toEqual([JSON.stringify({ ticket: 'ticket' })]);
    expect(options.route).toHaveBeenCalledWith('console', undefined);
    socket.close();
  });
  it('uses RTC only after the authenticated answer and Agent selection, keeping input off Console', async () => {
    const { socket, control, options } = connect();
    await tick();
    const peer = Peer.instances[0];
    expect(peer.config.iceServers).toEqual([{ urls: 'stun:stun.cloudflare.com:3478' }]);
    expect(peer.createDataChannel).toHaveBeenCalledWith('brocade-terminal-v1', { ordered: true });
    expect(JSON.parse(String(control.sent[0]))).toHaveProperty('rtc_offer');
    control.receive(JSON.stringify({ type: 'rtc_answer', sdp: 'answer' }));
    await tick();
    expect(peer.remote).toHaveBeenCalledWith({ type: 'answer', sdp: 'answer' });
    peer.channel.receive(JSON.stringify({ type: 'transport', kind: 'webrtc' }));
    const received = vi.fn();
    socket.onmessage = received;
    const bytes = new Uint8Array([1, 2, 3]);
    socket.send(bytes);
    peer.channel.receive(bytes.buffer);
    const probe = JSON.stringify({ type: 'ping', nonce: 42 });
    socket.send(probe);
    peer.channel.receive(JSON.stringify({ type: 'pong', nonce: 42 }));
    expect(peer.channel.sent).toEqual([bytes, probe]);
    expect(control.sent).toHaveLength(2);
    expect(received).toHaveBeenCalledTimes(2);
    socket.close();
    expect(options.directLost).not.toHaveBeenCalled();
  });
  it('falls back during negotiation without creating another attachment or replaying input', async () => {
    const { socket, control, options } = connect();
    await tick();
    control.receive(JSON.stringify({ type: 'transport', kind: 'console', reason: 'direct_unavailable' }));
    const data = new Uint8Array([4, 5]);
    socket.send(data);
    expect(Peer.instances[0].closed).toBe(true);
    expect(control.sent).toHaveLength(3);
    expect(control.sent[2]).toBe(data);
    expect(options.route).toHaveBeenCalledWith('console', 'direct_unavailable');
    expect(Socket.instances).toHaveLength(1);
    socket.close();
  });
  it('detaches on a selected RTC failure and asks the next attempt to use Console', async () => {
    const { socket, control, options } = connect();
    await tick();
    const channel = Peer.instances[0].channel;
    channel.receive(JSON.stringify({ type: 'transport', kind: 'webrtc' }));
    socket.send(new Uint8Array([9]));
    channel.close();
    socket.send(new Uint8Array([8]));
    expect(options.directLost).toHaveBeenCalledOnce();
    expect(control.readyState).toBe(3);
    expect(control.sent).toHaveLength(2);
    expect(channel.sent).toHaveLength(1);
  });
  it('closes the direct channel when its Console authorization connection is lost', async () => {
    const { control, options } = connect();
    await tick();
    const channel = Peer.instances[0].channel;
    channel.receive(JSON.stringify({ type: 'transport', kind: 'webrtc' }));
    control.close();
    expect(options.directLost).toHaveBeenCalledOnce();
    expect(channel.readyState).toBe('closed');
  });
  it('handles a denied browser permission and sends one legacy ticket for setup failure', async () => {
    Peer.deny = true;
    const { socket, control, options } = connect();
    await tick();
    expect(options.permissionDenied).toHaveBeenCalledOnce();
    expect(control.sent).toEqual([JSON.stringify({ ticket: 'ticket' })]);
    socket.close();
  });
  it('surfaces authorization errors on the selected direct path without retrying them', async () => {
    const { socket, control } = connect();
    await tick();
    Peer.instances[0].channel.receive(JSON.stringify({ type: 'transport', kind: 'webrtc' }));
    const received = vi.fn();
    socket.onmessage = received;
    control.receive(JSON.stringify({ type: 'error', message: 'access revoked' }));
    expect(received).toHaveBeenCalledOnce();
    expect(control.sent).toHaveLength(2);
    socket.close();
  });
  it('stops input within five seconds even while the control socket looks open', async () => {
    vi.useFakeTimers();
    const { socket, control, options } = connect();
    await tick();
    const channel = Peer.instances[0].channel;
    channel.receive(JSON.stringify({ type: 'transport', kind: 'webrtc' }));
    Socket.grants = false;
    await vi.advanceTimersByTimeAsync(4999);
    socket.send(new Uint8Array([1]));
    expect(channel.sent).toHaveLength(1);
    await vi.advanceTimersByTimeAsync(1);
    socket.send(new Uint8Array([2]));
    expect(channel.sent).toHaveLength(1);
    expect(control.readyState).toBe(3);
    expect(options.directLost).toHaveBeenCalledOnce();
  });
});
