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
  channel = new Channel();
  connectionState = 'connected';
  iceGatheringState = 'complete';
  localDescription = { sdp: 'v=0\r\nm=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\n' };
  onconnectionstatechange: (() => void) | null = null;
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
  async setLocalDescription() {}
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
function connect(direct = true) {
  const options = { direct, route: vi.fn(), directReady: vi.fn(), directLost: vi.fn(), permissionDenied: vi.fn() };
  const socket = terminalTransport(new URL('wss://console.example/terminal'), credentials, options);
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
  vi.stubGlobal('WebSocket', Socket);
  vi.stubGlobal('RTCPeerConnection', Peer);
  vi.stubGlobal('isSecureContext', true);
});
afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
});
describe('terminal path selection', () => {
  it('does not gather candidates before the user opts in', () => {
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
    expect(peer.channel.sent).toEqual([bytes]);
    expect(control.sent).toHaveLength(2);
    expect(received).toHaveBeenCalledTimes(1);
    expect(options.directReady).toHaveBeenCalledOnce();
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
  it('clears denied consent and sends one legacy ticket for setup failure', async () => {
    Peer.deny = true;
    const { socket, control, options } = connect();
    await tick();
    expect(options.permissionDenied).toHaveBeenCalledOnce();
    expect(control.sent).toEqual([JSON.stringify({ ticket: 'ticket' })]);
    expect(options.directReady).not.toHaveBeenCalled();
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
