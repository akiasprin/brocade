import { FitAddon } from '@xterm/addon-fit';
import { Terminal } from '@xterm/xterm';
import '@xterm/xterm/css/xterm.css';
import { useEffect, useId, useRef, useState } from 'react';
import { createCvmTerminal, type CvmTerminalSession } from '../../api';
import { Confirm } from '../../ui/bits';
import { ActionMenu } from '../../ui/choice-menu';
import { DialogClose, DialogLayer } from '../../ui/dialog';
import { Icon } from '../../ui/icons';
import { readClipboardText } from '../../ui/platform';

type Status = 'connecting' | 'connected' | 'closed' | 'failed';
const labels: Record<Status, string> = {
  connecting: '正在连接',
  connected: '已连接',
  closed: '连接已断开',
  failed: '连接失败',
};
const MAX_PASTE_BYTES = 32 * 1024;
type PasteTarget = { terminal: Terminal; socket: WebSocket };

export default function CvmTerminal({ id, name, onClose }: { id: string; name: string; onClose: () => void }) {
  const host = useRef<HTMLDivElement>(null);
  const terminal = useRef<Terminal | null>(null);
  const socket = useRef<WebSocket | null>(null);
  const accessible = useRef(false);
  const [attempt, setAttempt] = useState(0);
  const [status, setStatus] = useState<Status>('connecting');
  const [error, setError] = useState('');
  const [expanded, setExpanded] = useState(false);
  const [sessions, setSessions] = useState<CvmTerminalSession[]>([]);
  const [sessionId, setSessionId] = useState('');
  const [confirmEnd, setConfirmEnd] = useState(false);
  const [limits, setLimits] = useState<{ sessions: number; history: number } | null>(null);
  const connection = useRef<{ terminal_id?: string; new_session?: boolean }>({});
  const screenId = useId();
  const canInput = useRef(false);
  const pasteGeneration = useRef(Symbol());
  const pasteTarget = useRef<PasteTarget | null>(null);
  const pasteFeedbackTimer = useRef<number | undefined>(undefined);
  const [pasteOpen, setPasteOpen] = useState(false);
  const [pasteText, setPasteText] = useState('');
  const [pasteError, setPasteError] = useState('');
  const [pasteBusy, setPasteBusy] = useState(false);
  const [pasteFeedback, setPasteFeedback] = useState('');

  const reconnect = (request = connection.current) => {
    connection.current = request;
    pasteGeneration.current = Symbol();
    pasteTarget.current = null;
    setPasteBusy(false);
    setPasteOpen(false);
    setPasteText('');
    setPasteError('');
    setPasteFeedback('');
    window.clearTimeout(pasteFeedbackTimer.current);
    setAttempt(value => value + 1);
  };

  useEffect(() => {
    const element = host.current;
    if (!element) return;
    let disposed = false;
    canInput.current = false;
    const controller = new AbortController();
    const css = getComputedStyle(element);
    const term = new Terminal({
      cursorBlink: !window.matchMedia('(prefers-reduced-motion: reduce)').matches,
      fontFamily: css.getPropertyValue('--mono'),
      fontSize: 13,
      scrollback: 3000,
      reflowCursorLine: true,
      screenReaderMode: accessible.current,
      allowProposedApi: false,
      theme: { background: css.backgroundColor, foreground: css.color, cursor: css.color },
    });
    terminal.current = term;
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(element);
    let ws: WebSocket | null = null;
    let ready = false;
    let replaying = false;
    let persistent = false;
    const send = (payload: string | Uint8Array) => {
      if (!disposed && ws?.readyState === WebSocket.OPEN) {
        if (ws.bufferedAmount > 64 * 1024) {
          setError('输入发送过慢，连接已关闭，请重新连接。');
          ws.close();
          return;
        }
        ws.send(payload);
      }
    };
    const size = () => {
      if (replaying) return;
      fit.fit();
      if (ready && !replaying)
        send(
          JSON.stringify({
            type: 'resize',
            cols: Math.min(500, Math.max(2, term.cols)),
            rows: Math.min(300, Math.max(1, term.rows)),
          }),
        );
    };
    const observer = new ResizeObserver(size);
    observer.observe(element);
    size();
    // Escape and Tab belong to terminal applications. Ctrl+Shift+Escape retains dialog escape.
    const keys = (event: KeyboardEvent) => {
      if (event.key === 'Tab' || (event.key === 'Escape' && !(event.ctrlKey && event.shiftKey)))
        event.stopPropagation();
    };
    element.addEventListener('keydown', keys);
    const paste = (event: ClipboardEvent) => {
      const text = event.clipboardData?.getData('text/plain') ?? '';
      if (text.length > MAX_PASTE_BYTES || new TextEncoder().encode(text).byteLength > MAX_PASTE_BYTES) {
        event.preventDefault();
        event.stopImmediatePropagation();
        setError('本次粘贴内容超过 32 KiB，请分段粘贴。');
      }
    };
    element.addEventListener('paste', paste, true);
    const encoder = new TextEncoder();
    const input = term.onData(data => {
      if (!ready || replaying) return;
      const bytes = encoder.encode(data);
      for (let offset = 0; offset < bytes.length; offset += 16 * 1024) send(bytes.slice(offset, offset + 16 * 1024));
    });
    const binary = term.onBinary(data => {
      if (ready && !replaying) send(Uint8Array.from(data, char => char.charCodeAt(0)));
    });
    const deadline = window.setTimeout(() => {
      if (disposed || ready) return;
      setStatus('failed');
      setError('终端连接超时，请稍后重试。');
      controller.abort();
      ws?.close();
    }, 20_000);
    setStatus('connecting');
    setError('');
    void createCvmTerminal(id, controller.signal, connection.current)
      .then(credentials => {
        if (disposed) return;
        setLimits({ sessions: credentials.max_sessions_per_node, history: credentials.history_bytes });
        const url = new URL(
          `/cvm/${encodeURIComponent(id)}/terminal/${credentials.session_id}/stream`,
          location.origin,
        );
        url.protocol = location.protocol === 'https:' ? 'wss:' : 'ws:';
        ws = new WebSocket(url);
        socket.current = ws;
        ws.binaryType = 'arraybuffer';
        ws.onopen = () => send(JSON.stringify({ ticket: credentials.ticket }));
        ws.onmessage = event => {
          if (disposed) return;
          if (event.data instanceof ArrayBuffer) {
            const data = new Uint8Array(event.data);
            if (!ready || data.byteLength > 16 * 1024) {
              ws?.close();
              return;
            }
            term.write(data, () => send(JSON.stringify({ type: 'ack', bytes: data.byteLength })));
            return;
          }
          try {
            const message = JSON.parse(String(event.data)) as {
              type: string;
              code?: number;
              message?: string;
              active?: boolean;
              id?: string;
              sessions?: CvmTerminalSession[];
              cols?: number;
              rows?: number;
            };
            if (message.type === 'session' && message.id && message.sessions) {
              persistent = true;
              replaying = true;
              canInput.current = false;
              connection.current = { terminal_id: message.id };
              setSessionId(message.id);
              setSessions(message.sessions);
              if (message.cols && message.rows) term.resize(message.cols, message.rows);
            } else if (message.type === 'replay') {
              if (message.active) {
                replaying = true;
                canInput.current = false;
                setStatus('connecting');
              } else
                term.write('', () => {
                  if (disposed || !ready) return;
                  replaying = false;
                  canInput.current = true;
                  setStatus('connected');
                  size();
                  term.focus();
                });
            } else if (message.type === 'ready') {
              ready = true;
              window.clearTimeout(deadline);
              if (!persistent) {
                canInput.current = true;
                setStatus('connected');
                size();
                term.focus();
              }
            } else if (message.type === 'exit') {
              ready = false;
              canInput.current = false;
              setSessions(previous =>
                previous.map(session =>
                  session.id === connection.current.terminal_id ? { ...session, running: false } : session,
                ),
              );
              term.write('', () => {
                if (disposed) return;
                setStatus('closed');
                term.writeln(`\r\n进程已退出（${message.code ?? 0}）`);
                ws?.close();
              });
            } else if (message.type === 'error') {
              ready = false;
              canInput.current = false;
              setStatus('failed');
              setError(message.message ?? '终端连接失败');
              ws?.close();
            } else ws?.close();
          } catch {
            ws?.close();
          }
        };
        ws.onclose = () => {
          ready = false;
          canInput.current = false;
          window.clearTimeout(deadline);
          if (!disposed) setStatus(previous => (previous === 'failed' ? 'failed' : 'closed'));
        };
        ws.onerror = () => {
          if (!disposed) {
            setStatus('failed');
            setError('无法连接实例终端，请确认机器在线。');
          }
        };
      })
      .catch(cause => {
        if (!disposed && !controller.signal.aborted) {
          window.clearTimeout(deadline);
          setStatus('failed');
          setError(cause instanceof Error ? cause.message : '终端连接失败');
        }
      });
    return () => {
      disposed = true;
      canInput.current = false;
      pasteGeneration.current = Symbol();
      window.clearTimeout(pasteFeedbackTimer.current);
      controller.abort();
      window.clearTimeout(deadline);
      observer.disconnect();
      input.dispose();
      binary.dispose();
      element.removeEventListener('keydown', keys);
      element.removeEventListener('paste', paste, true);
      if (ws) {
        ws.onmessage = null;
        ws.onerror = null;
        ws.onclose = null;
        ws.close();
      }
      socket.current = null;
      terminal.current = null;
      term.dispose();
    };
  }, [id, attempt]);

  const sendKey = (value: string) => {
    const ws = socket.current;
    if (canInput.current && ws?.readyState === WebSocket.OPEN && ws.bufferedAmount < 64 * 1024) {
      ws.send(new TextEncoder().encode(value));
      terminal.current?.focus();
    }
  };

  const pasteInto = (text: string, target: PasteTarget | null) => {
    if (text.length > MAX_PASTE_BYTES || new TextEncoder().encode(text).byteLength > MAX_PASTE_BYTES) {
      setPasteError('本次最多粘贴 32 KiB，请分段粘贴。');
      return false;
    }
    if (
      !target ||
      !canInput.current ||
      terminal.current !== target.terminal ||
      socket.current !== target.socket ||
      target.socket.readyState !== WebSocket.OPEN
    ) {
      setPasteError('终端连接已变化，请重新打开粘贴。');
      return false;
    }
    if (!text) {
      setPasteError('粘贴内容为空。');
      return false;
    }
    target.terminal.paste(text);
    target.terminal.focus();
    setPasteFeedback('已粘贴');
    window.clearTimeout(pasteFeedbackTimer.current);
    pasteFeedbackTimer.current = window.setTimeout(() => setPasteFeedback(''), 1800);
    return true;
  };

  const pasteClipboard = async () => {
    const term = terminal.current;
    const ws = socket.current;
    if (!term || !ws || !canInput.current) return;
    const generation = Symbol();
    pasteGeneration.current = generation;
    const target = { terminal: term, socket: ws };
    pasteTarget.current = target;
    setPasteBusy(true);
    setPasteError('');
    let timer: number | undefined;
    try {
      const text = await Promise.race([
        readClipboardText(),
        new Promise<never>((_, reject) => {
          timer = window.setTimeout(() => reject(new Error('clipboard timeout')), 5000);
        }),
      ]);
      if (pasteGeneration.current !== generation) return;
      if (!pasteInto(text, target)) setPasteOpen(true);
    } catch {
      if (pasteGeneration.current === generation) {
        setPasteText('');
        setPasteOpen(true);
      }
    } finally {
      window.clearTimeout(timer);
      if (pasteGeneration.current === generation) setPasteBusy(false);
    }
  };

  return (
    <DialogLayer
      label={`终端 · ${name}`}
      className={`cvm-terminal-layer${expanded ? ' expanded' : ''}`}
      onClose={onClose}
    >
      <section className="dialog-surface cvm-terminal-dialog">
        <header>
          <div className="cvm-terminal-heading">
            <span className="cvm-terminal-mark" aria-hidden="true">
              <Icon of="nodes" size={18} />
            </span>
            <div className="cvm-terminal-title">
              <h2 title={name}>{name}</h2>
              <div className="cvm-terminal-meta">
                <span className="cvm-terminal-account">root</span>
                <span role="status" className={`cvm-terminal-status ${status}`}>
                  <i aria-hidden="true" />
                  {labels[status]}
                </span>
              </div>
            </div>
          </div>
          <div className="cvm-terminal-tools">
            {(status === 'closed' || status === 'failed') && (
              <button
                className="btn small cvm-terminal-reconnect"
                aria-label="重新连接"
                title="重新连接"
                onClick={() => reconnect()}
              >
                <Icon of="refresh" size={13} />
                <span>重新连接</span>
              </button>
            )}
            <ActionMenu
              label="终端会话操作"
              items={[
                {
                  label: '结束会话',
                  note: '结束当前 Shell；仅关闭窗口可保留会话',
                  danger: true,
                  disabled: status !== 'connected',
                  onSelect: () => setConfirmEnd(true),
                },
              ]}
            />
            <button
              className="btn cvm-terminal-icon"
              aria-label={expanded ? '还原' : '全屏'}
              title={expanded ? '还原窗口' : '全屏'}
              onClick={() => setExpanded(value => !value)}
            >
              <Icon of={expanded ? 'restore' : 'maximize'} size={15} />
            </button>
            <DialogClose className="btn cvm-terminal-icon" aria-label="关闭" title="关闭窗口，保留会话">
              <Icon of="close" size={15} />
            </DialogClose>
          </div>
        </header>
        <div className="cvm-terminal-sessionbar">
          <div
            className="cvm-terminal-tabs"
            role="tablist"
            aria-label="终端会话"
            onKeyDown={event => {
              if (!['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) return;
              const tabs = [
                ...event.currentTarget.querySelectorAll<HTMLButtonElement>('button[role="tab"]:not(:disabled)'),
              ];
              if (!tabs.length) return;
              event.preventDefault();
              const current = tabs.indexOf(document.activeElement as HTMLButtonElement);
              const next =
                event.key === 'Home'
                  ? 0
                  : event.key === 'End'
                    ? tabs.length - 1
                    : event.key === 'ArrowRight'
                      ? (current + 1) % tabs.length
                      : (current - 1 + tabs.length) % tabs.length;
              tabs[next].focus();
            }}
          >
            {sessions.length === 0 && <span className="cvm-terminal-tab-placeholder">持久会话</span>}
            {sessions.map((session, index) => (
              <button
                key={session.id}
                id={`${screenId}-${session.id}`}
                role="tab"
                aria-selected={session.id === sessionId}
                aria-controls={screenId}
                tabIndex={session.id === sessionId ? 0 : -1}
                disabled={status === 'connecting'}
                className={`cvm-terminal-tab${session.id === sessionId ? ' selected' : ''}`}
                onClick={() => reconnect({ terminal_id: session.id })}
              >
                <i className={session.running ? 'running' : ''} aria-hidden="true" />
                {`会话 ${index + 1}${session.running ? '' : ' · 已结束'}`}
              </button>
            ))}
          </div>
          <button
            className="btn small cvm-terminal-new"
            aria-label="新建会话"
            title="新建终端会话"
            disabled={status === 'connecting'}
            onClick={() =>
              reconnect({
                terminal_id: Array.from(crypto.getRandomValues(new Uint8Array(32)), byte =>
                  byte.toString(16).padStart(2, '0'),
                ).join(''),
                new_session: true,
              })
            }
          >
            <Icon of="plus" size={13} />
            <span>新建</span>
          </button>
        </div>
        <div
          ref={host}
          id={screenId}
          role="tabpanel"
          className="cvm-terminal-screen"
          aria-label="实例交互终端"
          aria-labelledby={sessionId ? `${screenId}-${sessionId}` : undefined}
        />
        <div className="cvm-terminal-inputbar">
          <div className="cvm-terminal-keys" aria-label="终端快捷键">
            <button
              className="btn small cvm-terminal-key"
              disabled={status !== 'connected'}
              onClick={() => sendKey('\x03')}
            >
              Ctrl+C
            </button>
            <button
              className="btn small cvm-terminal-key"
              disabled={status !== 'connected'}
              onClick={() => sendKey('\t')}
            >
              Tab
            </button>
            <button
              className="btn small cvm-terminal-key"
              disabled={status !== 'connected'}
              onClick={() => sendKey('\x1b')}
            >
              Esc
            </button>
          </div>
          <div className="cvm-terminal-input-actions">
            <label className="cvm-terminal-accessible" title="为屏幕阅读器优化终端输出">
              <input
                type="checkbox"
                onChange={event => {
                  accessible.current = event.target.checked;
                  if (terminal.current) terminal.current.options.screenReaderMode = event.target.checked;
                }}
              />
              辅助阅读
            </label>
            <button
              className="btn small cvm-terminal-paste"
              aria-label="粘贴"
              title="粘贴剪贴板内容到当前终端"
              disabled={status !== 'connected' || pasteBusy}
              onClick={() => void pasteClipboard()}
            >
              <Icon of={pasteFeedback ? 'check' : 'paste'} size={14} />
              <span aria-live="polite">{pasteBusy ? '读取中' : pasteFeedback || '粘贴'}</span>
            </button>
          </div>
        </div>
        {error && (
          <p className="cvm-terminal-error" role="alert">
            {error}
          </p>
        )}
        <footer>
          <span className="cvm-terminal-persistence">
            <Icon of="check" size={12} />
            关闭窗口后继续运行
          </span>
          {limits && (
            <span className="cvm-terminal-capacity">
              每节点 {limits.sessions} 个会话 · 历史 {limits.history / 1024 ** 2} MiB / 会话
            </span>
          )}
          {sessions.find(session => session.id === sessionId)?.history_truncated && <span>较早历史已截断</span>}
        </footer>
        {confirmEnd && (
          <Confirm
            title="结束终端会话"
            body="将结束当前 Shell 和本次终端连接。关闭窗口可以保留会话继续运行。"
            confirmLabel="结束会话"
            onCancel={() => setConfirmEnd(false)}
            onConfirm={() => {
              const ws = socket.current;
              if (ws?.readyState === WebSocket.OPEN) ws.send(JSON.stringify({ type: 'close' }));
              setConfirmEnd(false);
            }}
          />
        )}
        {pasteOpen && (
          <DialogLayer
            label="粘贴到终端"
            onClose={() => {
              setPasteOpen(false);
              setPasteText('');
              setPasteError('');
              pasteTarget.current = null;
            }}
          >
            <section className="dialog-surface cvm-terminal-paste-dialog">
              <header>
                <div>
                  <h2>粘贴到终端</h2>
                  <p>在下方粘贴内容，再发送到当前会话。</p>
                </div>
                <DialogClose className="btn cvm-terminal-icon" aria-label="关闭粘贴">
                  <Icon of="close" size={15} />
                </DialogClose>
              </header>
              <textarea
                className="f mono"
                aria-label="粘贴内容"
                placeholder="在这里粘贴文字或命令…"
                autoFocus
                rows={6}
                maxLength={MAX_PASTE_BYTES}
                value={pasteText}
                onChange={event => {
                  setPasteText(event.target.value);
                  setPasteError('');
                }}
              />
              {pasteError && (
                <p className="cvm-terminal-paste-error" role="alert">
                  {pasteError}
                </p>
              )}
              <footer>
                <span>单次最多 32 KiB</span>
                <div>
                  <DialogClose className="btn">取消</DialogClose>
                  <button
                    className="btn primary"
                    disabled={!pasteText || status !== 'connected'}
                    onClick={() => {
                      if (pasteInto(pasteText, pasteTarget.current)) {
                        setPasteOpen(false);
                        setPasteText('');
                        setPasteError('');
                        pasteTarget.current = null;
                      }
                    }}
                  >
                    粘贴到终端
                  </button>
                </div>
              </footer>
            </section>
          </DialogLayer>
        )}
      </section>
    </DialogLayer>
  );
}
