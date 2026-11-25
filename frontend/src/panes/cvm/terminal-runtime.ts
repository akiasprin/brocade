import { FitAddon } from '@xterm/addon-fit';
import { SearchAddon, type ISearchOptions } from '@xterm/addon-search';
import { Terminal } from '@xterm/xterm';
import { createCvmTerminal, type CvmTerminalSession } from '../../api';
import { copyText } from '../../ui/platform';
import { terminalTransport, type TerminalRoute, type TerminalSocket } from './terminal-transport';

export const MAX_PASTE_BYTES = 32 * 1024;
export type TerminalStatus = 'idle' | 'connecting' | 'connected' | 'disconnected' | 'failed' | 'ended';
export const terminalLabels: Record<TerminalStatus, string> = {
  idle: '后台运行',
  connecting: '正在连接',
  connected: '已连接',
  disconnected: '连接已断开',
  failed: '连接失败',
  ended: '会话已结束',
};
export type TerminalPreferences = {
  fontSize: number;
  color: 'dark' | 'light';
  cursor: 'block' | 'bar' | 'underline';
  blink: boolean;
  autoCopy: boolean;
  ctrlCopy: boolean;
  accessible: boolean;
};
export type TerminalSessionView = CvmTerminalSession & {
  ordinal: number;
  status: TerminalStatus;
  title: string;
  unread: boolean;
  error: string;
  retryAt: number | null;
  cols: number;
  rows: number;
  connectedAt: number | null;
  route: TerminalRoute | null;
  routeReason: string;
};
export type PasteTarget = { term: Terminal; socket: TerminalSocket; id: string };
type ConnectionRequest = { terminal_id?: string; new_session?: boolean };
type Runtime = {
  key: string;
  term: Terminal;
  fit: FitAddon;
  search: SearchAddon;
  element: HTMLDivElement;
  socket: TerminalSocket | null;
  relayOnly: boolean;
  controller: AbortController;
  ready: boolean;
  replaying: boolean;
  disposed: boolean;
  retries: number;
  retryTimer?: number;
  stableTimer?: number;
  deadline?: number;
  endRequested: boolean;
  subscriptions: { dispose: () => void }[];
};
type Snapshot = {
  directSupported: boolean | null;
  active: string;
  sessions: TerminalSessionView[];
  limit: number | null;
  historyLimit: number | null;
  selection: string;
  clips: string[];
  feedback: string;
  pendingPaste: { text: string; target: PasteTarget } | null;
  searchResult: { resultIndex: number; resultCount: number };
  mouseMode: boolean;
  typing: boolean;
};
const placeholder = (id: string, ordinal: number): TerminalSessionView => ({
  ordinal,
  id,
  running: false,
  history_bytes: 0,
  history_truncated: false,
  status: 'connecting',
  title: '',
  unread: false,
  error: '',
  retryAt: null,
  cols: 80,
  rows: 24,
  connectedAt: null,
  route: null,
  routeReason: '',
});
const bytes = (text: string) => new TextEncoder().encode(text);
export const cleanSelection = (text: string) => text.replace(/\r\n/g, '\n').replace(/[ \t]+$/gm, '');

/** Owns each visited session's PTY and renderer. Switching tabs never closes another session's transport. */
export class TerminalWorkspace {
  private state: Snapshot = {
    directSupported: null,
    active: '',
    sessions: [],
    limit: null,
    historyLimit: null,
    selection: '',
    clips: [],
    feedback: '',
    pendingPaste: null,
    searchResult: { resultIndex: -1, resultCount: 0 },
    mouseMode: false,
    typing: false,
  };
  private listeners = new Set<() => void>();
  private runtimes = new Map<string, Runtime>();
  private host: HTMLElement | null = null;
  private observer?: ResizeObserver;
  private typingTimer?: number;
  private feedbackTimer?: number;
  private nextOrdinal = 1;
  private directAllowed = false;
  preferences: TerminalPreferences;

  constructor(
    readonly instance: string,
    preferences: TerminalPreferences,
    private readonly directPreferenceKey?: string,
  ) {
    this.preferences = preferences;
    try {
      this.directAllowed = !!directPreferenceKey && localStorage.getItem(directPreferenceKey) === 'allowed';
    } catch {
      // Storage restrictions leave the first direct attempt manual.
    }
  }
  snapshot = () => this.state;
  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };
  private publish(change: Partial<Snapshot>) {
    this.state = { ...this.state, ...change };
    this.listeners.forEach(listener => listener());
  }
  private patch(key: string, change: Partial<TerminalSessionView>) {
    this.publish({ sessions: this.state.sessions.map(s => (s.id === key ? { ...s, ...change } : s)) });
  }
  private retainSessions(sessions: TerminalSessionView[], protectedKey: string) {
    const evicted = new Set(
      sessions
        .filter(s => !s.running && s.status !== 'connecting' && s.id !== this.state.active && s.id !== protectedKey)
        .sort((a, b) => b.ordinal - a.ordinal)
        .slice(6)
        .map(s => s.id),
    );
    for (const key of evicted) {
      const old = this.runtimes.get(key);
      if (old) this.dispose(old);
      this.runtimes.delete(key);
    }
    return sessions.filter(s => !evicted.has(s.id)).sort((a, b) => a.ordinal - b.ordinal);
  }
  private active() {
    return this.runtimes.get(this.state.active);
  }
  start(host: HTMLElement) {
    this.host = host;
    this.observer = new ResizeObserver(() => this.fit());
    this.observer.observe(host);
    this.connect({});
  }
  stop() {
    this.observer?.disconnect();
    for (const runtime of this.runtimes.values()) this.dispose(runtime);
    this.runtimes.clear();
    this.host = null;
    window.clearTimeout(this.typingTimer);
    window.clearTimeout(this.feedbackTimer);
  }
  private dispose(runtime: Runtime, keepTerminal = false) {
    runtime.disposed = true;
    runtime.controller.abort();
    window.clearTimeout(runtime.retryTimer);
    window.clearTimeout(runtime.stableTimer);
    window.clearTimeout(runtime.deadline);
    if (runtime.socket) {
      runtime.socket.onclose = null;
      runtime.socket.onmessage = null;
      runtime.socket.onerror = null;
      runtime.socket.close();
    }
    runtime.subscriptions.forEach(s => s.dispose());
    if (!keepTerminal) {
      runtime.term.dispose();
      runtime.element.remove();
    }
  }
  fit() {
    const runtime = this.active();
    if (!runtime || runtime.replaying || !this.host?.clientHeight) return;
    runtime.fit.fit();
    this.patch(runtime.key, { cols: runtime.term.cols, rows: runtime.term.rows });
    if (runtime.ready)
      this.send(
        runtime,
        JSON.stringify({
          type: 'resize',
          cols: Math.min(500, runtime.term.cols),
          rows: Math.min(300, runtime.term.rows),
        }),
      );
  }
  focus() {
    this.active()?.term.focus();
  }
  select(id: string) {
    if (id === this.state.active) return;
    this.publish({ active: id, selection: '', pendingPaste: null, mouseMode: false });
    this.patch(id, { unread: false });
    if (!this.runtimes.has(id)) this.connect({ terminal_id: id });
    for (const runtime of this.runtimes.values()) runtime.element.hidden = runtime.key !== id;
    this.fit();
    this.focus();
  }
  newSession() {
    if (this.state.limit !== null && this.state.sessions.filter(s => s.running).length >= this.state.limit) {
      this.feedback('宿主会话名额已用完，请先结束一个会话。');
      return;
    }
    const terminal_id = Array.from(crypto.getRandomValues(new Uint8Array(32)), b =>
      b.toString(16).padStart(2, '0'),
    ).join('');
    this.connect({ terminal_id, new_session: true });
  }
  reconnect() {
    const old = this.active();
    if (old) {
      this.dispose(old, true);
      this.runtimes.delete(old.key);
    }
    const id = this.state.active;
    this.connect(id && id !== 'initial' ? { terminal_id: id } : {}, true, 0, old);
  }
  tryDirect() {
    this.directAllowed = true;
    const runtime = this.active();
    if (runtime) runtime.relayOnly = false;
    this.reconnect();
  }
  useConsole() {
    const runtime = this.active();
    if (runtime) runtime.relayOnly = true;
    this.reconnect();
  }
  end(id: string) {
    let runtime = this.runtimes.get(id);
    if (!runtime) {
      this.connect({ terminal_id: id }, false);
      runtime = this.runtimes.get(id);
    }
    if (!runtime) return;
    runtime.endRequested = true;
    if (runtime.ready && !runtime.replaying) this.send(runtime, JSON.stringify({ type: 'close' }));
  }
  private send(runtime: Runtime, payload: string | Uint8Array) {
    const ws = runtime.socket;
    if (runtime.disposed || ws?.readyState !== WebSocket.OPEN) return false;
    if (ws.bufferedAmount > 64 * 1024) {
      this.patch(runtime.key, { error: '输入发送过慢，连接已关闭，请重新连接。', status: 'failed' });
      runtime.ready = false;
      ws.close();
      return false;
    }
    ws.send(payload);
    return true;
  }
  sendKey(text: string) {
    const runtime = this.active();
    if (runtime?.ready && !runtime.replaying) this.input(runtime, text);
    this.focus();
  }
  private input(runtime: Runtime, text: string) {
    const encoded = bytes(text);
    for (let offset = 0; offset < encoded.length; offset += 16 * 1024) {
      if (!this.send(runtime, encoded.slice(offset, offset + 16 * 1024))) break;
    }
    this.publish({ typing: true });
    window.clearTimeout(this.typingTimer);
    this.typingTimer = window.setTimeout(() => this.publish({ typing: false }), 1600);
  }
  target(): PasteTarget | null {
    const runtime = this.active();
    return runtime?.ready && !runtime.replaying && runtime.socket?.readyState === WebSocket.OPEN
      ? { term: runtime.term, socket: runtime.socket, id: runtime.key }
      : null;
  }
  validTarget(target: PasteTarget) {
    const runtime = this.active();
    return (
      runtime?.key === target.id &&
      runtime.term === target.term &&
      runtime.socket === target.socket &&
      runtime.ready &&
      !runtime.replaying &&
      target.socket.readyState === WebSocket.OPEN
    );
  }
  paste(text: string, target = this.target(), confirmed = false) {
    if (!target || !this.validTarget(target)) {
      this.feedback('终端连接已变化，请重新打开粘贴。');
      return false;
    }
    if (bytes(text).length > MAX_PASTE_BYTES) {
      this.feedback('本次最多粘贴 32 KiB，请分段粘贴。');
      return false;
    }
    if (!text) {
      this.feedback('粘贴内容为空。');
      return false;
    }
    if (!confirmed && /[\r\n]/.test(text) && !target.term.modes.bracketedPasteMode) {
      this.publish({ pendingPaste: { text, target } });
      return false;
    }
    target.term.paste(text);
    target.term.focus();
    this.publish({ pendingPaste: null });
    this.feedback('已粘贴');
    return true;
  }
  cancelPaste() {
    this.publish({ pendingPaste: null });
  }
  async copy(text = this.state.selection) {
    if (!text) return;
    const cleaned = cleanSelection(text);
    const copied = await copyText(cleaned);
    if (!this.host) return;
    const recordFits = bytes(cleaned).length <= 256 * 1024;
    if (copied && recordFits)
      this.publish({
        clips: [cleaned, ...this.state.clips.filter(s => s !== cleaned)].slice(0, 10),
      });
    this.feedback(
      copied ? (recordFits ? '已复制' : '已复制；内容超过 256 KiB，未加入复制记录。') : '复制失败，请使用系统复制',
    );
  }
  copyOnSelect() {
    if (this.preferences.autoCopy && this.state.selection) void this.copy();
  }
  selectAll() {
    this.active()?.term.selectAll();
  }
  clear() {
    this.active()?.term.clear();
    this.focus();
  }
  text() {
    const buffer = this.active()?.term.buffer.active;
    if (!buffer) return '';
    const lines: string[] = [];
    for (let i = 0; i < buffer.length; i++) {
      const line = buffer.getLine(i);
      if (!line) continue;
      const text = line.translateToString(true);
      if (line.isWrapped && lines.length) lines[lines.length - 1] += text;
      else lines.push(text);
    }
    return lines.join('\n');
  }
  find(query: string, options: ISearchOptions, previous = false) {
    const search = this.active()?.search;
    if (!search) return;
    if (!query) {
      search.clearDecorations();
      this.publish({ searchResult: { resultIndex: -1, resultCount: 0 } });
      return;
    }
    const settings = {
      ...options,
      decorations: {
        matchBackground: '#6c572a',
        activeMatchBackground: '#97691c',
        matchOverviewRuler: '#d8b877',
        activeMatchColorOverviewRuler: '#d8b877',
      },
    };
    if (previous) search.findPrevious(query, settings);
    else search.findNext(query, settings);
  }
  feedback(feedback: string) {
    this.publish({ feedback });
    window.clearTimeout(this.feedbackTimer);
    this.feedbackTimer = window.setTimeout(() => this.publish({ feedback: '' }), 3500);
  }
  setPreferences(preferences: TerminalPreferences) {
    this.preferences = preferences;
    for (const runtime of this.runtimes.values()) this.applyPreferences(runtime.term);
    this.fit();
  }
  private applyPreferences(term: Terminal) {
    const p = this.preferences;
    term.options.fontSize = p.fontSize;
    term.options.cursorStyle = p.cursor;
    term.options.cursorBlink = p.blink && !matchMedia('(prefers-reduced-motion: reduce)').matches;
    term.options.screenReaderMode = p.accessible;
    term.options.theme =
      p.color === 'dark'
        ? { background: '#0d0e10', foreground: '#d5d8dd', cursor: '#e8eaee', selectionBackground: '#40567c' }
        : { background: '#fbfbfc', foreground: '#2a2e35', cursor: '#2a2e35', selectionBackground: '#c4d4ed' };
  }
  private connect(request: ConnectionRequest, activate = true, retry = 0, previous?: Runtime) {
    if (!this.host) return;
    const key = request.terminal_id ?? 'initial';
    this.publish({ sessions: this.retainSessions(this.state.sessions, key) });
    const element = previous?.element ?? document.createElement('div');
    element.className = 'cvm-terminal-renderer';
    element.hidden = !activate;
    const term =
      previous?.term ??
      new Terminal({
        fontFamily: getComputedStyle(this.host).getPropertyValue('--mono'),
        scrollback: 3000,
        reflowCursorLine: true,
        allowProposedApi: true,
      });
    this.applyPreferences(term);
    const fit = previous?.fit ?? new FitAddon();
    const search = previous?.search ?? new SearchAddon({ highlightLimit: 1000 });
    if (!previous) {
      term.loadAddon(fit);
      term.loadAddon(search);
      this.host.append(element);
      term.open(element);
    }
    const runtime: Runtime = {
      key,
      term,
      fit,
      search,
      element,
      socket: null,
      relayOnly: previous?.relayOnly ?? false,
      controller: new AbortController(),
      ready: false,
      replaying: true,
      disposed: false,
      retries: retry,
      endRequested: previous?.endRequested ?? false,
      subscriptions: [],
    };
    this.runtimes.set(key, runtime);
    if (!this.state.sessions.some(s => s.id === key))
      this.publish({ sessions: [...this.state.sessions, placeholder(key, this.nextOrdinal++)] });
    this.patch(key, { status: 'connecting', error: '', retryAt: null, route: null, routeReason: '' });
    if (activate) {
      this.publish({ active: key, selection: '', pendingPaste: null });
      for (const r of this.runtimes.values()) r.element.hidden = r !== runtime;
    }
    // Native clipboard events go through the same byte limit and session identity checks as menu paste.
    const paste = (event: ClipboardEvent) => {
      event.preventDefault();
      event.stopImmediatePropagation();
      this.paste(event.clipboardData?.getData('text/plain') ?? '');
    };
    element.addEventListener('paste', paste, true);
    runtime.subscriptions.push(
      { dispose: () => element.removeEventListener('paste', paste, true) },
      term.onData(data => {
        if (runtime.ready && !runtime.replaying) this.input(runtime, data);
      }),
      term.onBinary(data => {
        if (runtime.ready && !runtime.replaying)
          this.send(
            runtime,
            Uint8Array.from(data, c => c.charCodeAt(0)),
          );
      }),
      term.onTitleChange(title => this.patch(runtime.key, { title: title.slice(0, 256) })),
      term.onSelectionChange(() => {
        if (runtime.key === this.state.active) this.publish({ selection: cleanSelection(term.getSelection()) });
      }),
      term.onWriteParsed(() => {
        if (runtime.key === this.state.active && this.state.mouseMode !== (term.modes.mouseTrackingMode !== 'none'))
          this.publish({ mouseMode: term.modes.mouseTrackingMode !== 'none' });
      }),
      search.onDidChangeResults(result => {
        if (runtime.key === this.state.active) this.publish({ searchResult: result });
      }),
    );
    this.fit();
    runtime.deadline = window.setTimeout(
      () => {
        if (runtime.disposed || runtime.ready) return;
        this.patch(runtime.key, { status: 'failed', error: '终端连接超时，请稍后重试。' });
        runtime.controller.abort();
        runtime.socket?.close();
      },
      this.directAllowed && !runtime.relayOnly ? 35_000 : 20_000,
    );
    void createCvmTerminal(this.instance, runtime.controller.signal, request)
      .then(credentials => {
        if (runtime.disposed) return;
        this.publish({ limit: credentials.max_sessions_per_node, historyLimit: credentials.history_bytes });
        this.publish({
          directSupported:
            !!credentials.webrtc?.supported && window.isSecureContext && typeof RTCPeerConnection !== 'undefined',
        });
        const url = new URL(
          `/cvm/${encodeURIComponent(this.instance)}/terminal/${credentials.session_id}/stream`,
          location.origin,
        );
        url.protocol = location.protocol === 'https:' ? 'wss:' : 'ws:';
        const ws = terminalTransport(url, credentials, {
          direct: this.directAllowed && !runtime.relayOnly,
          route: (route, reason) => this.patch(runtime.key, { route, routeReason: reason ?? '' }),
          directReady: () => {
            if (!this.directPreferenceKey) return;
            try {
              localStorage.setItem(this.directPreferenceKey, 'allowed');
            } catch {
              // A working connection does not depend on persistent preferences.
            }
          },
          directLost: () => {
            runtime.relayOnly = true;
          },
          permissionDenied: () => {
            this.directAllowed = false;
            try {
              if (this.directPreferenceKey) localStorage.removeItem(this.directPreferenceKey);
            } catch {
              // The in-memory permission is already cleared.
            }
          },
        });
        runtime.socket = ws;
        ws.binaryType = 'arraybuffer';
        ws.onopen = () => this.send(runtime, JSON.stringify({ ticket: credentials.ticket }));
        ws.onmessage = event => {
          if (runtime.disposed) return;
          if (event.data instanceof ArrayBuffer) {
            const data = new Uint8Array(event.data);
            if (!runtime.ready || data.byteLength > 16 * 1024) {
              ws.close();
              return;
            }
            term.write(data, () => {
              this.send(runtime, JSON.stringify({ type: 'ack', bytes: data.byteLength }));
              if (!runtime.replaying && runtime.key !== this.state.active) this.patch(runtime.key, { unread: true });
            });
            return;
          }
          try {
            const message = JSON.parse(String(event.data));
            if (message.type === 'session' && typeof message.id === 'string' && Array.isArray(message.sessions)) {
              const oldKey = runtime.key;
              runtime.key = message.id;
              this.runtimes.delete(oldKey);
              this.runtimes.set(runtime.key, runtime);
              const current =
                this.state.sessions.find(s => s.id === oldKey) ?? placeholder(runtime.key, this.nextOrdinal++);
              const incoming: CvmTerminalSession[] = message.sessions;
              const merged = this.state.sessions.filter(s => s.id !== oldKey && !incoming.some(i => i.id === s.id));
              for (const session of incoming) {
                const known =
                  this.state.sessions.find(s => s.id === session.id) ??
                  (session.id === runtime.key ? current : placeholder(session.id, this.nextOrdinal++));
                merged.push({
                  ...known,
                  ...session,
                  status:
                    session.id === runtime.key
                      ? 'connecting'
                      : session.running
                        ? this.runtimes.has(session.id)
                          ? known.status
                          : 'idle'
                        : 'ended',
                });
              }
              this.publish({
                sessions: this.retainSessions(merged, runtime.key),
                active: this.state.active === oldKey ? runtime.key : this.state.active,
              });
              if (message.cols && message.rows) term.resize(message.cols, message.rows);
            } else if (message.type === 'ready') {
              runtime.ready = true;
              window.clearTimeout(runtime.deadline);
            } else if (message.type === 'replay') {
              runtime.replaying = !!message.active;
              if (message.active) term.reset();
              if (!message.active)
                term.write('', () => {
                  if (runtime.disposed || !runtime.ready) return;
                  window.clearTimeout(runtime.stableTimer);
                  runtime.stableTimer = window.setTimeout(() => {
                    runtime.retries = 0;
                  }, 30_000);
                  this.patch(runtime.key, { status: 'connected', connectedAt: Date.now(), retryAt: null, error: '' });
                  if (runtime.key === this.state.active) {
                    this.fit();
                    this.focus();
                  }
                  if (runtime.endRequested) this.send(runtime, JSON.stringify({ type: 'close' }));
                });
            } else if (message.type === 'exit') {
              runtime.ready = false;
              this.patch(runtime.key, { status: 'ended', running: false, retryAt: null });
              term.write('', () => {
                if (!runtime.disposed) {
                  term.writeln(`\r\n进程已退出（${message.code ?? 0}）`);
                  ws.close();
                }
              });
            } else if (message.type === 'error') {
              runtime.ready = false;
              this.patch(runtime.key, {
                status: 'failed',
                error: typeof message.message === 'string' ? message.message : '终端连接失败',
              });
              ws.close();
            } else {
              this.patch(runtime.key, { status: 'failed', error: '终端协议不兼容，请升级后重试。' });
              ws.close();
            }
          } catch {
            this.patch(runtime.key, { status: 'failed', error: '终端消息格式无效。' });
            ws.close();
          }
        };
        ws.onerror = () => {
          if (!runtime.disposed) this.patch(runtime.key, { error: '无法连接实例终端，请确认机器在线。' });
        };
        ws.onclose = () => {
          runtime.ready = false;
          window.clearTimeout(runtime.stableTimer);
          window.clearTimeout(runtime.deadline);
          if (runtime.disposed) return;
          const session = this.state.sessions.find(s => s.id === runtime.key);
          if (session?.status === 'ended' || session?.status === 'failed') return;
          // Never retry a server rejection; transport losses get five bounded attempts and never replay input.
          const retryAt = runtime.retries < 5 ? Date.now() + Math.min(30, 2 ** runtime.retries) * 1000 : null;
          this.patch(runtime.key, { status: 'disconnected', retryAt });
          if (retryAt !== null)
            runtime.retryTimer = window.setTimeout(() => {
              if (runtime.disposed) return;
              const active = runtime.key === this.state.active;
              this.dispose(runtime, true);
              this.runtimes.delete(runtime.key);
              this.connect({ terminal_id: runtime.key }, active, runtime.retries + 1, runtime);
            }, retryAt - Date.now());
        };
      })
      .catch(cause => {
        if (runtime.disposed || runtime.controller.signal.aborted) return;
        window.clearTimeout(runtime.deadline);
        this.patch(runtime.key, { status: 'failed', error: cause instanceof Error ? cause.message : '终端连接失败' });
      });
  }
}
