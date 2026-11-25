import type { CvmTerminalCredentials } from '../../api';

export type TerminalRoute = 'webrtc' | 'console';
export type TerminalSocket = Pick<
  WebSocket,
  'readyState' | 'bufferedAmount' | 'binaryType' | 'send' | 'close' | 'onopen' | 'onmessage' | 'onerror' | 'onclose'
>;
type Options = {
  direct: boolean;
  route: (kind: TerminalRoute, reason?: string) => void;
  directReady: () => void;
  directLost: () => void;
  permissionDenied: () => void;
};
const MAX_SDP = 32 * 1024;
const MAX_DATA = 16 * 1024;
const MAX_BUFFER = 64 * 1024;

/** The Console socket remains the authorization channel for the lifetime of RTC.
 * A selected channel never changes underneath an input/paste target. Recovery
 * creates a fresh attachment through the workspace's existing reconnect flow.
 */
export function terminalTransport(url: URL, credentials: CvmTerminalCredentials, options: Options): TerminalSocket {
  if (
    !options.direct ||
    !credentials.webrtc?.supported ||
    !window.isSecureContext ||
    typeof RTCPeerConnection === 'undefined'
  ) {
    options.route('console', options.direct ? 'direct_unavailable' : undefined);
    return new WebSocket(url);
  }
  return new ShortcutSocket(url, credentials, options);
}

class ShortcutSocket implements TerminalSocket {
  onopen: WebSocket['onopen'] = null;
  onmessage: WebSocket['onmessage'] = null;
  onerror: WebSocket['onerror'] = null;
  onclose: WebSocket['onclose'] = null;
  binaryType: BinaryType = 'arraybuffer';
  private socket: WebSocket;
  private peer: RTCPeerConnection | null = null;
  private channel: RTCDataChannel | null = null;
  private selected: TerminalRoute | null = null;
  private stopped = false;
  private closing = false;
  private attached = false;
  private gatherCancel?: () => void;
  private leaseTimer?: number;
  private leaseExpires = 0;
  private leaseConfirmed = false;
  private leaseSequence = 0;
  private probes = new Map<number, number>();

  constructor(
    url: URL,
    private credentials: CvmTerminalCredentials,
    private options: Options,
  ) {
    this.socket = new WebSocket(url);
    this.socket.binaryType = 'arraybuffer';
    this.socket.onopen = event => this.onopen?.call(this.socket, event);
    this.socket.onerror = event => this.onerror?.call(this.socket, event);
    this.socket.onclose = event => {
      if (this.stopped) return;
      if (!this.closing && this.selected === 'webrtc') this.options.directLost();
      this.stopped = true;
      window.clearInterval(this.leaseTimer);
      this.cleanup();
      this.onclose?.call(this.socket, event);
    };
    this.socket.onmessage = event => this.receive(event, 'console');
  }
  get readyState() {
    return this.stopped ? WebSocket.CLOSED : this.socket.readyState;
  }
  get bufferedAmount() {
    return this.selected === 'webrtc' ? (this.channel?.bufferedAmount ?? MAX_BUFFER + 1) : this.socket.bufferedAmount;
  }
  send(data: Parameters<WebSocket['send']>[0]) {
    if (this.stopped || this.socket.readyState !== WebSocket.OPEN) return;
    if (!this.attached) {
      this.attached = true;
      // Only the first ticket frame is asynchronous. Keyboard input is never queued.
      void this.attach(data);
      return;
    }
    if (this.selected === 'webrtc' && this.channel?.readyState === 'open') {
      if (!this.leaseConfirmed || performance.now() >= this.leaseExpires) {
        this.lost();
        return;
      }
      if (typeof data === 'string') this.channel.send(data);
      else if (data instanceof ArrayBuffer) this.channel.send(data);
      else if (ArrayBuffer.isView(data))
        this.channel.send(Uint8Array.from(new Uint8Array(data.buffer, data.byteOffset, data.byteLength)));
      else this.close();
    } else if (this.selected === 'console') this.socket.send(data);
  }
  close(code?: number, reason?: string) {
    this.closing = true;
    window.clearInterval(this.leaseTimer);
    this.cleanup();
    this.socket.close(code, reason);
  }
  private cleanup() {
    this.gatherCancel?.();
    this.gatherCancel = undefined;
    if (this.channel) {
      this.channel.onclose = null;
      this.channel.onerror = null;
      this.channel.onmessage = null;
      this.channel.close();
      this.channel = null;
    }
    if (this.peer) {
      this.peer.onconnectionstatechange = null;
      this.peer.close();
      this.peer = null;
    }
  }
  private lost() {
    if (this.selected !== 'webrtc' || this.stopped) return;
    this.options.directLost();
    this.close();
  }
  private startLease() {
    this.leaseExpires = performance.now() + 5000;
    const probe = () => {
      const now = performance.now();
      if (this.stopped || this.closing || this.socket.readyState !== WebSocket.OPEN) return;
      if (now >= this.leaseExpires) {
        if (this.selected === 'webrtc') this.options.directLost();
        this.close();
        return;
      }
      for (const [nonce, sent] of this.probes) if (now - sent >= 5000) this.probes.delete(nonce);
      this.probes.set(++this.leaseSequence, now);
      this.socket.send(JSON.stringify({ type: 'control_probe', nonce: this.leaseSequence }));
    };
    probe();
    this.leaseTimer = window.setInterval(probe, 1000);
  }
  private async attach(data: Parameters<WebSocket['send']>[0]) {
    try {
      if (typeof data !== 'string' || JSON.parse(data).ticket !== this.credentials.ticket) throw new Error('ticket');
      this.peer = new RTCPeerConnection({
        iceServers: (this.credentials.webrtc?.stun_servers ?? []).slice(0, 2).map(urls => ({ urls })),
        iceTransportPolicy: 'all',
      });
      const peer = this.peer;
      const channel = peer.createDataChannel('brocade-terminal-v1', { ordered: true });
      this.channel = channel;
      channel.binaryType = 'arraybuffer';
      channel.onmessage = event => this.receive(event, 'webrtc');
      channel.onclose = () => this.lost();
      channel.onerror = () => this.lost();
      peer.onconnectionstatechange = () => {
        if (peer.connectionState === 'failed' || peer.connectionState === 'closed') this.lost();
      };
      await peer.setLocalDescription(await peer.createOffer());
      await new Promise<void>(resolve => {
        const done = () => {
          window.clearTimeout(timer);
          peer.removeEventListener('icegatheringstatechange', changed);
          this.gatherCancel = undefined;
          resolve();
        };
        const changed = () => {
          if (peer.iceGatheringState === 'complete') done();
        };
        const timer = window.setTimeout(done, 2200);
        this.gatherCancel = done;
        peer.addEventListener('icegatheringstatechange', changed);
        changed();
      });
      if (this.stopped || this.socket.readyState !== WebSocket.OPEN || peer !== this.peer) return;
      const sdp = peer.localDescription?.sdp;
      if (!sdp || sdp.length > MAX_SDP || sdp.split('\n').filter(line => line.startsWith('a=candidate:')).length > 32)
        throw new Error('sdp');
      this.socket.send(JSON.stringify({ ticket: this.credentials.ticket, rtc_offer: sdp }));
      this.startLease();
    } catch (cause) {
      if (cause instanceof DOMException && cause.name === 'NotAllowedError') this.options.permissionDenied();
      this.cleanup();
      if (!this.stopped && this.socket.readyState === WebSocket.OPEN) {
        this.selected = 'console';
        this.options.route('console', 'browser_unavailable');
        this.socket.send(JSON.stringify({ ticket: this.credentials.ticket }));
      }
    }
  }
  private receive(event: MessageEvent, source: TerminalRoute) {
    if (this.stopped) return;
    if (typeof event.data === 'string') {
      let message;
      try {
        message = JSON.parse(event.data);
      } catch {
        this.close();
        return;
      }
      if (source === 'console' && message.type === 'control_grant') {
        const sent = this.probes.get(message.nonce);
        this.probes.delete(message.nonce);
        if (sent !== undefined && performance.now() < sent + 5000) {
          this.leaseExpires = Math.max(this.leaseExpires, sent + 5000);
          this.leaseConfirmed = true;
        }
        return;
      }
      if (source === 'console' && message.type === 'rtc_answer' && !this.selected) {
        if (typeof message.sdp !== 'string' || message.sdp.length > MAX_SDP || !this.peer) {
          this.close();
          return;
        }
        void this.peer.setRemoteDescription({ type: 'answer', sdp: message.sdp }).catch(cause => {
          if (cause instanceof DOMException && cause.name === 'NotAllowedError') this.options.permissionDenied();
          this.cleanup();
          // The Agent selects relay after its bounded negotiation deadline.
        });
        return;
      }
      if (message.type === 'transport' && !this.selected && message.kind === source) {
        this.selected = source;
        this.options.route(source, message.reason);
        if (source === 'webrtc') this.options.directReady();
        else this.cleanup();
        return;
      }
      // Authorization/backend failures still arrive through Console while RTC
      // is selected. They remain hard failures, never a reason to bypass checks.
      if (source === 'console' && message.type === 'error') {
        this.onmessage?.call(this.socket, event);
        return;
      }
    } else if (!(event.data instanceof ArrayBuffer) || event.data.byteLength > MAX_DATA) {
      this.close();
      return;
    }
    if (source !== this.selected) {
      this.close();
      return;
    }
    this.onmessage?.call(this.socket, event);
  }
}
