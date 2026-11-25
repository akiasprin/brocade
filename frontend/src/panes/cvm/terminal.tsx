import '@xterm/xterm/css/xterm.css';
import { useEffect, useRef, useState, useSyncExternalStore, type ReactNode } from 'react';
import { useSession } from '../../session';
import { DialogClose, DialogLayer } from '../../ui/dialog';
import { menuArrowKey } from '../../ui/choice-menu';
import { Icon, type IconName } from '../../ui/icons';
import { readClipboardText } from '../../ui/platform';
import { useNarrow } from '../../ui/viewport';
import {
  MAX_PASTE_BYTES,
  TerminalWorkspace,
  terminalLabels,
  type PasteTarget,
  type TerminalPreferences,
} from './terminal-runtime';
import './terminal.css';

const PREFS_KEY = 'brocade:webshell:preferences';
function preferences(): TerminalPreferences {
  const defaults: TerminalPreferences = {
    fontSize: 13,
    color: document.documentElement.dataset.theme === 'dark' ? 'dark' : 'light',
    cursor: 'block',
    blink: true,
    autoCopy: true,
    ctrlCopy: true,
    accessible: false,
  };
  try {
    const p = JSON.parse(localStorage.getItem(PREFS_KEY) ?? '{}');
    return {
      fontSize: Number.isInteger(p.fontSize) ? Math.min(24, Math.max(10, p.fontSize)) : defaults.fontSize,
      color: p.color === 'dark' || p.color === 'light' ? p.color : defaults.color,
      cursor: ['block', 'bar', 'underline'].includes(p.cursor) ? p.cursor : defaults.cursor,
      blink: typeof p.blink === 'boolean' ? p.blink : defaults.blink,
      autoCopy: typeof p.autoCopy === 'boolean' ? p.autoCopy : defaults.autoCopy,
      ctrlCopy: typeof p.ctrlCopy === 'boolean' ? p.ctrlCopy : defaults.ctrlCopy,
      accessible: typeof p.accessible === 'boolean' ? p.accessible : defaults.accessible,
    };
  } catch {
    return defaults;
  }
}
function readNames(key: string): Record<string, string> {
  try {
    const value = JSON.parse(localStorage.getItem(key) ?? '{}');
    const names: Record<string, string> = {};
    for (const [id, name] of Object.entries(value)) {
      if (/^[a-f0-9]{64}$/.test(id) && typeof name === 'string' && name.length <= 40) names[id] = name;
    }
    return names;
  } catch {
    return {};
  }
}
const byteLabel = (n: number) =>
  n < 1024 ? `${n} B` : n < 1024 ** 2 ? `${(n / 1024).toFixed(1)} KiB` : `${(n / 1024 ** 2).toFixed(1)} MiB`;
function Tool({
  label,
  icon,
  onClick,
  disabled = false,
}: {
  label: string;
  icon: IconName;
  onClick: () => void;
  disabled?: boolean;
}) {
  return (
    <button className="cvm-terminal-icon" aria-label={label} title={label} disabled={disabled} onClick={onClick}>
      <Icon of={icon} size={15} />
    </button>
  );
}
function Search({
  workspace,
  onClose,
  initialQuery = '',
}: {
  workspace: TerminalWorkspace;
  onClose: () => void;
  initialQuery?: string;
}) {
  const state = useSyncExternalStore(workspace.subscribe, workspace.snapshot);
  const [query, setQuery] = useState(initialQuery);
  useEffect(() => {
    if (initialQuery) workspace.find(initialQuery, { incremental: true });
  }, [workspace, initialQuery]);
  const [options, setOptions] = useState({ caseSensitive: false, wholeWord: false, regex: false });
  const [error, setError] = useState('');
  const search = (q = query, o = options, previous = false, incremental = false) => {
    try {
      if (o.regex) new RegExp(q);
      setError('');
      workspace.find(q, { ...o, incremental }, previous);
    } catch {
      setError('正则表达式无效');
    }
  };
  return (
    <section
      className="cvm-terminal-search cvm-terminal-float"
      aria-label="查找终端"
      onKeyDown={event => {
        if (event.key === 'Escape') {
          event.stopPropagation();
          workspace.find('', {});
          onClose();
          workspace.focus();
        }
        if (event.key === 'Enter') {
          event.preventDefault();
          search(query, options, event.shiftKey);
        }
      }}
    >
      <div>
        <input
          className="f"
          aria-label="查找内容"
          autoFocus
          value={query}
          onChange={event => {
            setQuery(event.target.value);
            search(event.target.value, options, false, true);
          }}
        />
        <span aria-live="polite">
          {error || `${state.searchResult.resultIndex + 1} / ${state.searchResult.resultCount}`}
        </span>
        <Tool label="上一个" icon="arrowUp" onClick={() => search(query, options, true)} />
        <Tool label="下一个" icon="chevronDown" onClick={() => search()} />
        <Tool
          label="关闭查找"
          icon="close"
          onClick={() => {
            workspace.find('', {});
            onClose();
            workspace.focus();
          }}
        />
      </div>
      <div>
        {(
          [
            ['caseSensitive', '区分大小写'],
            ['wholeWord', '全词'],
            ['regex', '正则'],
          ] as const
        ).map(([key, label]) => (
          <label key={key}>
            <input
              type="checkbox"
              checked={options[key]}
              onChange={event => {
                const next = { ...options, [key]: event.target.checked };
                setOptions(next);
                search(query, next, false, true);
              }}
            />
            {label}
          </label>
        ))}
      </div>
    </section>
  );
}

export default function CvmTerminal({
  id,
  name,
  hostName = '',
  warning,
  onClose,
}: {
  id: string;
  name: string;
  hostName?: string;
  warning?: string;
  onClose: () => void;
}) {
  const { who } = useSession();
  const namesKey = `brocade:webshell:names:${who.operator_id}:${id}`;
  const [prefs, setPrefs] = useState(preferences);
  const [workspace] = useState(
    () => new TerminalWorkspace(id, prefs, `brocade:webshell:direct:${who.operator_id}:${id}`),
  );
  const state = useSyncExternalStore(workspace.subscribe, workspace.snapshot);
  const current = state.sessions.find(s => s.id === state.active);
  const connected = current?.status === 'connected';
  const phone = useNarrow();
  const [names, setNames] = useState(() => readNames(namesKey));
  const [rename, setRename] = useState('');
  const [renameText, setRenameText] = useState('');
  const [panel, setPanel] = useState<'menu' | 'settings' | 'clips' | 'context' | null>(null);
  const [endId, setEndId] = useState('');
  const [compose, setCompose] = useState(false);
  const [draft, setDraft] = useState('');
  const [composeTarget, setComposeTarget] = useState<PasteTarget | null>(null);
  const [manualPaste, setManualPaste] = useState(false);
  const [pasteBusy, setPasteBusy] = useState(false);
  const clipboardGeneration = useRef(0);
  const [find, setFind] = useState(false);
  const [findSeed, setFindSeed] = useState('');
  const [now, setNow] = useState(() => Date.now());
  const [mobileText, setMobileText] = useState<string | null>(null);
  const [mobileSelection, setMobileSelection] = useState('');
  const [modifiers, setModifiers] = useState({ ctrl: false, alt: false });
  const [contextAt, setContextAt] = useState({ x: 0, y: 0 });
  const host = useRef<HTMLDivElement>(null);
  const shell = useRef<HTMLElement>(null);
  const closeApproved = useRef(false);
  const moreAnchor = useRef<HTMLButtonElement>(null);
  const endAnchor = useRef<HTMLElement>(null);
  const contextAnchor = useRef<HTMLSpanElement>(null);
  const composeArea = useRef<HTMLTextAreaElement>(null);
  const [selectionAt, setSelectionAt] = useState(() => ({ x: 12, y: window.innerHeight - 100 }));
  const [fullscreen, setFullscreen] = useState(false);
  const labelOf = (session: string) =>
    names[session] || `会话 ${state.sessions.find(s => s.id === session)?.ordinal ?? 1}`;
  const openCompose = (manual = false, text?: string) => {
    setComposeTarget(workspace.target());
    setManualPaste(manual);
    setCompose(true);
    setPanel(null);
    if (text !== undefined) setDraft(text);
    requestAnimationFrame(() => composeArea.current?.focus({ preventScroll: true }));
  };
  const pasteClipboard = async () => {
    const target = workspace.target();
    if (!target || pasteBusy) return;
    const generation = ++clipboardGeneration.current;
    setPasteBusy(true);
    setPanel(null);
    let timer: number | undefined;
    try {
      const text = await Promise.race([
        readClipboardText(),
        new Promise<never>((_, reject) => {
          timer = window.setTimeout(() => reject(new Error('clipboard timeout')), 5000);
        }),
      ]);
      if (generation === clipboardGeneration.current && workspace.validTarget(target)) workspace.paste(text, target);
    } catch {
      if (generation === clipboardGeneration.current && workspace.validTarget(target)) openCompose(true);
    } finally {
      window.clearTimeout(timer);
      if (generation === clipboardGeneration.current) setPasteBusy(false);
    }
  };
  const exportText = () => {
    const url = URL.createObjectURL(new Blob([workspace.text()], { type: 'text/plain;charset=utf-8' }));
    const a = document.createElement('a');
    a.href = url;
    a.download = `${name.replace(/[^\p{L}\p{N}_.-]/gu, '_')}-terminal.txt`;
    a.click();
    window.setTimeout(() => URL.revokeObjectURL(url), 1000);
    setPanel(null);
  };
  const changePrefs = (change: Partial<TerminalPreferences>) => {
    const next = { ...prefs, ...change };
    setPrefs(next);
    workspace.setPreferences(next);
    try {
      localStorage.setItem(PREFS_KEY, JSON.stringify(next));
    } catch {
      workspace.feedback('浏览器无法保存设置，本窗口仍然生效。');
    }
  };
  const close = () => {
    if (!draft || window.confirm('命令输入框里有未发送的内容，仍要关闭窗口吗？')) {
      closeApproved.current = true;
      onClose();
    }
  };
  const toggleFullscreen = () => {
    const action = document.fullscreenElement
      ? document.exitFullscreen()
      : document.documentElement.requestFullscreen?.();
    void action?.catch(() => workspace.feedback('浏览器未允许全屏，请使用浏览器的全屏功能。'));
    setPanel(null);
  };
  useEffect(() => {
    if (host.current) workspace.start(host.current);
    return () => {
      workspace.stop();
    };
  }, [workspace]);
  useEffect(() => {
    const viewport = window.visualViewport;
    const element = shell.current;
    if (!phone || !viewport || !element) return;
    const resize = () => {
      element.style.setProperty('--webshell-height', `${viewport.height}px`);
      element.style.setProperty('--webshell-top', `${viewport.offsetTop}px`);
    };
    resize();
    viewport.addEventListener('resize', resize);
    viewport.addEventListener('scroll', resize);
    return () => {
      viewport.removeEventListener('resize', resize);
      viewport.removeEventListener('scroll', resize);
      element.style.removeProperty('--webshell-height');
      element.style.removeProperty('--webshell-top');
    };
  }, [phone]);
  useEffect(() => {
    document.title = `${terminalLabels[current?.status ?? 'connecting']} · ${name} · WebShell`;
  }, [current?.status, name]);
  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    const fullscreenChanged = () => setFullscreen(!!document.fullscreenElement);
    document.addEventListener('fullscreenchange', fullscreenChanged);
    return () => {
      window.clearInterval(timer);
      document.removeEventListener('fullscreenchange', fullscreenChanged);
    };
  }, []);
  useEffect(() => {
    if (!draft) return;
    const unload = (event: BeforeUnloadEvent) => {
      if (closeApproved.current) return;
      event.preventDefault();
      event.returnValue = '';
    };
    window.addEventListener('beforeunload', unload);
    return () => window.removeEventListener('beforeunload', unload);
  }, [draft]);
  useEffect(() => {
    let frame = 0;
    let selecting = false;
    const down = (event: MouseEvent) => {
      selecting = event.button === 0 && event.target instanceof Node && !!host.current?.contains(event.target);
    };
    const up = (event: MouseEvent) => {
      if (!selecting) return;
      selecting = false;
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => {
        if (!workspace.snapshot().selection) return;
        setSelectionAt({
          x: Math.max(8, Math.min(event.clientX, window.innerWidth - 280)),
          y: Math.max(52, Math.min(event.clientY + 16, window.innerHeight - 80)),
        });
        workspace.copyOnSelect();
      });
    };
    window.addEventListener('mousedown', down);
    window.addEventListener('mouseup', up);
    return () => {
      window.removeEventListener('mousedown', down);
      window.removeEventListener('mouseup', up);
      cancelAnimationFrame(frame);
    };
  }, [workspace]);
  useEffect(() => {
    if (mobileText === null) return;
    const selection = () => setMobileSelection(window.getSelection()?.toString() ?? '');
    document.addEventListener('selectionchange', selection);
    return () => document.removeEventListener('selectionchange', selection);
  }, [mobileText]);
  useEffect(() => {
    const keys = (event: KeyboardEvent) => {
      if (panel || endId || state.pendingPaste) return;
      const target = event.target;
      const inTerminal = target instanceof Node && host.current?.contains(target);
      const key = event.key.toLowerCase();
      if (event.ctrlKey && event.shiftKey && event.key === 'Escape') {
        event.preventDefault();
        close();
        return;
      }
      if (event.ctrlKey && event.shiftKey && key === 'f') {
        event.preventDefault();
        event.stopImmediatePropagation();
        setFind(true);
        return;
      }
      if (!inTerminal) return;
      if (
        ((event.ctrlKey && event.shiftKey) ||
          event.metaKey ||
          (event.ctrlKey && prefs.ctrlCopy && !!state.selection)) &&
        key === 'c'
      ) {
        event.preventDefault();
        event.stopImmediatePropagation();
        void workspace.copy();
        return;
      }
      if (((event.ctrlKey && event.shiftKey) || event.metaKey) && key === 'v') {
        event.preventDefault();
        event.stopImmediatePropagation();
        void pasteClipboard();
        return;
      }
      if ((modifiers.ctrl || modifiers.alt) && key.length === 1) {
        event.preventDefault();
        event.stopImmediatePropagation();
        workspace.sendKey(
          `${modifiers.alt ? '\x1b' : ''}${modifiers.ctrl ? String.fromCharCode(key.toUpperCase().charCodeAt(0) & 31) : event.key}`,
        );
        setModifiers({ ctrl: false, alt: false });
      }
    };
    document.addEventListener('keydown', keys, true);
    return () => document.removeEventListener('keydown', keys, true);
  });
  const switchTo = (session: string) => {
    clipboardGeneration.current++;
    setPasteBusy(false);
    setFind(false);
    setRename('');
    setMobileText(null);
    setPanel(null);
    workspace.select(session);
  };
  const requestEnd = (session: string, anchor: HTMLElement) => {
    endAnchor.current = anchor;
    setPanel(null);
    setEndId(session);
  };
  const newSession = () => {
    clipboardGeneration.current++;
    setPasteBusy(false);
    workspace.newSession();
  };
  const tabKeys = (event: React.KeyboardEvent) => {
    if (!['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) return;
    event.preventDefault();
    const tabs = [...event.currentTarget.querySelectorAll<HTMLButtonElement>('[role="tab"]')];
    const index = tabs.indexOf(document.activeElement as HTMLButtonElement);
    const next =
      event.key === 'Home'
        ? 0
        : event.key === 'End'
          ? tabs.length - 1
          : (index + (event.key === 'ArrowRight' ? 1 : tabs.length - 1)) % tabs.length;
    tabs[next]?.focus();
    tabs[next]?.scrollIntoView({ block: 'nearest', inline: 'nearest' });
  };
  const panelTitle =
    panel === 'settings'
      ? '终端设置'
      : panel === 'clips'
        ? '复制记录'
        : panel === 'context'
          ? '终端右键菜单'
          : '终端菜单';
  const remaining = current?.connectedAt ? Math.max(0, 7200 - Math.floor((now - current.connectedAt) / 1000)) : null;
  const pending = state.pendingPaste;
  const full = state.limit !== null && state.sessions.filter(s => s.running).length >= state.limit;
  return (
    <main
      ref={shell}
      className={`cvm-webshell${state.typing ? ' typing' : ''}`}
      data-theme={prefs.color}
      aria-label={`终端 · ${name}`}
    >
      <div
        ref={host}
        className="cvm-terminal-screen"
        role="tabpanel"
        id="terminal-screen"
        aria-label="实例交互终端"
        aria-labelledby={current ? `terminal-tab-${current.id}` : undefined}
        onContextMenu={event => {
          if (event.shiftKey) return;
          event.preventDefault();
          setContextAt({ x: event.clientX, y: event.clientY });
          setPanel('context');
        }}
      />
      <nav className={`cvm-terminal-capsule${panel || rename || endId ? ' hold' : ''}`} aria-label="终端会话操作">
        <div className="cvm-terminal-tabs" role="tablist" aria-label="终端会话" onKeyDown={tabKeys}>
          {state.sessions.map(session => (
            <div className={`cvm-terminal-tab-wrap${session.id === state.active ? ' selected' : ''}`} key={session.id}>
              {rename === session.id ? (
                <input
                  className="f cvm-terminal-rename"
                  aria-label="会话名称"
                  autoFocus
                  maxLength={40}
                  value={renameText}
                  onChange={event => setRenameText(event.target.value)}
                  onKeyDown={event => {
                    event.stopPropagation();
                    if (event.key === 'Escape') setRename('');
                    if (event.key === 'Enter') {
                      const next = { ...names, [session.id]: renameText.trim() || labelOf(session.id) };
                      setNames(next);
                      setRename('');
                      try {
                        localStorage.setItem(namesKey, JSON.stringify(next));
                      } catch {
                        workspace.feedback('浏览器无法保存名称，本窗口仍然生效。');
                      }
                    }
                  }}
                />
              ) : (
                <button
                  role="tab"
                  id={`terminal-tab-${session.id}`}
                  aria-controls="terminal-screen"
                  aria-selected={session.id === state.active}
                  tabIndex={session.id === state.active ? 0 : -1}
                  title={`${session.title || '路径未上报'} · 历史 ${byteLabel(session.history_bytes)}（连接时快照）${session.history_truncated ? ' · 较早历史已截断' : ''} · ${terminalLabels[session.status]}${session.unread ? ' · 有新输出' : ''}`}
                  onClick={() => switchTo(session.id)}
                  onDoubleClick={() => {
                    setRename(session.id);
                    setRenameText(labelOf(session.id));
                  }}
                >
                  <i
                    className={`cvm-terminal-dot ${session.status}${session.unread ? ' unread' : ''}`}
                    aria-hidden="true"
                  />
                  <span>{labelOf(session.id)}</span>
                  <span className="sr-only">{session.unread ? '，有新输出' : ''}</span>
                </button>
              )}
              {session.running && (
                <button
                  className="cvm-terminal-tab-close"
                  aria-label={`结束${labelOf(session.id)}`}
                  title="结束会话"
                  onClick={event => requestEnd(session.id, event.currentTarget.parentElement!)}
                >
                  <Icon of="close" size={11} />
                </button>
              )}
            </div>
          ))}
        </div>
        <button
          className="cvm-terminal-icon cvm-terminal-new"
          aria-label="新建会话"
          aria-disabled={full || current?.status === 'connecting'}
          title={full ? '名额已用完，点击查看原因' : '新建会话'}
          onClick={() => {
            if (current?.status !== 'connecting') newSession();
          }}
        >
          <Icon of="plus" size={15} />
        </button>
        <button
          ref={moreAnchor}
          className="cvm-terminal-icon"
          aria-label="更多"
          aria-haspopup="menu"
          aria-expanded={panel !== null}
          title="粘贴、输入框、查找与终端设置"
          onClick={() => setPanel(panel ? null : 'menu')}
        >
          <Icon of="more" size={16} />
        </button>
      </nav>
      <span className="sr-only" role="status">
        {terminalLabels[current?.status ?? 'connecting']}
      </span>
      {current && current.status !== 'connected' && (
        <div className={`cvm-terminal-banner ${current.status}`} role={current.error ? 'alert' : undefined}>
          <span>
            {current.error || terminalLabels[current.status]}
            {current.retryAt ? `，${Math.max(0, Math.ceil((current.retryAt - now) / 1000))} 秒后重连` : ''}
          </span>
          {(current.status === 'failed' || current.status === 'disconnected') && (
            <button className="btn small" onClick={() => workspace.reconnect()}>
              重新连接
            </button>
          )}
          {current.status === 'ended' && (
            <>
              <button className="btn small" onClick={newSession}>
                新建会话
              </button>
              <button className="btn small" onClick={exportText}>
                导出记录
              </button>
            </>
          )}
        </div>
      )}
      {warning && connected && (
        <div className="cvm-terminal-banner" role="alert">
          {warning}
        </div>
      )}
      {state.mouseMode && <p className="cvm-terminal-mouse-hint">程序正在使用鼠标，按住 Shift 拖动选择</p>}
      {state.feedback && (
        <div className="cvm-terminal-toast" role="status">
          {state.feedback}
        </div>
      )}
      {!!state.selection && !find && !panel && mobileText === null && (
        <div
          className="cvm-terminal-selection cvm-terminal-float"
          role="toolbar"
          aria-label="选区操作"
          style={{ left: selectionAt.x, top: selectionAt.y }}
          onPointerDown={event => event.preventDefault()}
        >
          <small>
            {state.selection.split('\n').length} 行 · {state.selection.length} 字符
          </small>
          <Tool label="复制选区" icon="copy" onClick={() => void workspace.copy()} />
          <Tool
            label="插入到光标"
            icon="paste"
            onClick={() => workspace.paste(state.selection)}
            disabled={!connected}
          />
          <Tool
            label="查找选区"
            icon="search"
            onClick={() => {
              setFindSeed(state.selection);
              setFind(true);
            }}
          />
        </div>
      )}
      {find && (
        <Search
          workspace={workspace}
          initialQuery={findSeed}
          onClose={() => {
            setFind(false);
            setFindSeed('');
          }}
        />
      )}
      {compose && (
        <section
          className="cvm-terminal-compose cvm-terminal-float"
          aria-label="命令输入框"
          onKeyDown={event => {
            if (event.ctrlKey && event.key === 'Enter') {
              event.preventDefault();
              if (workspace.paste(draft, composeTarget)) setDraft('');
            }
            if (event.key === 'Escape') {
              setCompose(false);
              workspace.focus();
            }
          }}
        >
          <header>
            <strong>命令输入框</strong>
            <Tool
              label="关闭输入框"
              icon="close"
              onClick={() => {
                setCompose(false);
                workspace.focus();
              }}
            />
          </header>
          {manualPaste && <p>无法直接读取剪贴板，请在此按 Ctrl+V（Mac：⌘V）。</p>}
          <textarea
            ref={composeArea}
            className="f mono"
            aria-label="命令内容"
            placeholder="输入或粘贴命令…"
            value={draft}
            onChange={event => setDraft(event.target.value)}
            rows={4}
          />
          <footer>
            <small className={new TextEncoder().encode(draft).length > MAX_PASTE_BYTES ? 'danger' : ''}>
              {byteLabel(new TextEncoder().encode(draft).length)} / 32 KiB
            </small>
            <button
              className="btn primary small"
              disabled={!draft || !connected || new TextEncoder().encode(draft).length > MAX_PASTE_BYTES}
              onClick={() => {
                if (workspace.paste(draft, composeTarget)) setDraft('');
              }}
            >
              发送<span className="cvm-terminal-desktop"> Ctrl+Enter</span>
            </button>
          </footer>
        </section>
      )}
      {phone && (
        <div className="cvm-terminal-keybar">
          <div className="cvm-terminal-keys">
            {[
              ['Esc', '\x1b'],
              ['Tab', '\t'],
            ].map(([label, key]) => (
              <button key={label} disabled={!connected} onClick={() => workspace.sendKey(key)}>
                {label}
              </button>
            ))}
            {(['ctrl', 'alt'] as const).map(key => (
              <button
                key={key}
                aria-pressed={modifiers[key]}
                disabled={!connected}
                onClick={() => {
                  setModifiers({ ...modifiers, [key]: !modifiers[key] });
                  workspace.focus();
                }}
              >
                {key === 'ctrl' ? 'Ctrl' : 'Alt'}
              </button>
            ))}
            {[
              ['←', '\x1b[D'],
              ['↓', '\x1b[B'],
              ['↑', '\x1b[A'],
              ['→', '\x1b[C'],
            ].map(([label, key]) => (
              <button
                key={label}
                aria-label={`方向键${label}`}
                disabled={!connected}
                onClick={() => {
                  workspace.sendKey(`${modifiers.alt ? '\x1b' : ''}${key}`);
                  setModifiers({ ctrl: false, alt: false });
                }}
              >
                {label}
              </button>
            ))}
          </div>
          <div className="cvm-terminal-fixed-keys">
            <button
              onClick={() => {
                setMobileText(workspace.text());
                setMobileSelection('');
              }}
            >
              选择
            </button>
            <button disabled={!connected || pasteBusy} onClick={() => void pasteClipboard()}>
              粘贴
            </button>
            <button disabled={!connected} onClick={() => openCompose()}>
              输入框
            </button>
          </div>
        </div>
      )}
      {mobileText !== null && (
        <section className="cvm-terminal-mobile-selection" aria-label="终端文字选择">
          <header>
            <span>拖动系统手柄选择文字</span>
            <button
              className="btn small"
              onClick={() => {
                setMobileText(null);
                workspace.focus();
              }}
            >
              完成
            </button>
          </header>
          <pre tabIndex={0}>{mobileText}</pre>
          <footer>
            <button
              className="btn small"
              onClick={() => {
                const pre = document.querySelector('.cvm-terminal-mobile-selection pre');
                if (!pre) return;
                const range = document.createRange();
                range.selectNodeContents(pre);
                window.getSelection()?.removeAllRanges();
                window.getSelection()?.addRange(range);
                setMobileSelection(mobileText);
              }}
            >
              全选
            </button>
            <button
              className="btn small"
              disabled={!mobileSelection}
              onClick={() => void workspace.copy(mobileSelection)}
            >
              复制
            </button>
          </footer>
        </section>
      )}
      <span
        ref={contextAnchor}
        className="cvm-terminal-context-anchor"
        style={{ left: contextAt.x, top: contextAt.y }}
      />
      {panel && (
        <DialogLayer
          label={panelTitle}
          mode="popover"
          anchor={panel === 'context' ? contextAnchor : moreAnchor}
          align="end"
          className="cvm-webshell-popover"
          onClose={() => setPanel(null)}
        >
          <section className="dialog-surface cvm-terminal-panel" data-theme={prefs.color}>
            <header>
              {panel === 'settings' || panel === 'clips' ? (
                <Tool label="返回菜单" icon="chevronLeft" onClick={() => setPanel('menu')} />
              ) : null}
              <strong>{panelTitle}</strong>
              <DialogClose className="cvm-terminal-icon" aria-label="关闭菜单">
                <Icon of="close" size={14} />
              </DialogClose>
            </header>
            {panel === 'menu' && (
              <div className="cvm-terminal-info">
                <b>{name}</b>
                <dl>
                  <dt>会话</dt>
                  <dd className="mono" title={current?.id}>
                    {current?.id === 'initial' ? '待连接' : current?.id.slice(0, 12)}
                  </dd>
                  <dt>登录</dt>
                  <dd>root{!phone ? ' · IP 未上报' : ''}</dd>
                  <dt>宿主</dt>
                  <dd>{hostName || '未上报'}</dd>
                  <dt>连接路径</dt>
                  <dd>
                    {current?.route === 'webrtc'
                      ? 'WebRTC 直连'
                      : current?.route === 'console'
                        ? 'Console 转发'
                        : '正在选择'}
                    {current?.routeReason && <small>直连暂不可用，已使用 Console 转发</small>}
                  </dd>
                  <dt>名额</dt>
                  <dd>
                    本实例 {state.active === 'initial' ? '未知' : state.sessions.filter(s => s.running).length} ·
                    宿主上限 {state.limit ?? '未知'}
                    <small>其他实例占用未上报</small>
                  </dd>
                  {!phone && (
                    <>
                      <dt>窗口</dt>
                      <dd>
                        {current?.cols} × {current?.rows}
                      </dd>
                    </>
                  )}
                  <dt>历史</dt>
                  <dd>
                    {byteLabel(current?.history_bytes ?? 0)} /{' '}
                    {state.historyLimit === null ? '未知' : byteLabel(state.historyLimit)}
                    {current?.history_truncated ? ' · 较早历史已截断' : ''}
                  </dd>
                  <dt>连接剩余</dt>
                  <dd>{remaining === null ? '未连接' : `${Math.floor(remaining / 60)} 分 ${remaining % 60} 秒`}</dd>
                </dl>
              </div>
            )}
            {(panel === 'menu' || panel === 'context') && (
              <div role="menu" aria-label={panelTitle} onKeyDown={menuArrowKey} className="cvm-terminal-menu">
                {panel === 'menu' && (
                  <>
                    <TerminalMenuItem
                      phone={phone}
                      onClose={() => setPanel(null)}
                      label={
                        state.directSupported === false
                          ? '直连未启用'
                          : current?.route === 'webrtc'
                            ? '重新检测直连'
                            : '尝试直连'
                      }
                      shortcut=""
                      disabled={!state.directSupported || current?.status === 'connecting'}
                      onSelect={() => workspace.tryDirect()}
                    />
                    {current?.route === 'webrtc' && (
                      <TerminalMenuItem
                        phone={phone}
                        onClose={() => setPanel(null)}
                        label="使用 Console 转发"
                        shortcut=""
                        onSelect={() => workspace.useConsole()}
                      />
                    )}
                  </>
                )}
                {panel === 'context' && (
                  <>
                    {
                      <TerminalMenuItem
                        phone={phone}
                        onClose={() => setPanel(null)}
                        label={'复制选区'}
                        shortcut={'Ctrl+Shift+C'}
                        onSelect={() => void workspace.copy()}
                        disabled={!state.selection}
                      />
                    }
                    {
                      <TerminalMenuItem
                        phone={phone}
                        onClose={() => setPanel(null)}
                        label={'插入选区'}
                        shortcut={''}
                        onSelect={() => workspace.paste(state.selection)}
                        disabled={!connected || !state.selection}
                      />
                    }
                    {
                      <TerminalMenuItem
                        phone={phone}
                        onClose={() => setPanel(null)}
                        label={'全选'}
                        shortcut={''}
                        onSelect={() => workspace.selectAll()}
                      />
                    }
                  </>
                )}
                {
                  <TerminalMenuItem
                    phone={phone}
                    onClose={() => setPanel(null)}
                    label={'粘贴'}
                    shortcut={'Ctrl+Shift+V'}
                    onSelect={() => void pasteClipboard()}
                    disabled={!connected || pasteBusy}
                  />
                }
                {
                  <TerminalMenuItem
                    phone={phone}
                    onClose={() => setPanel(null)}
                    label={'命令输入框'}
                    shortcut={''}
                    onSelect={() => openCompose(false, panel === 'context' ? state.selection : undefined)}
                    disabled={!connected}
                  />
                }
                {
                  <TerminalMenuItem
                    phone={phone}
                    onClose={() => setPanel(null)}
                    label={'查找'}
                    shortcut={'Ctrl+Shift+F'}
                    onSelect={() => setFind(true)}
                  />
                }
                {
                  <TerminalMenuItem
                    phone={phone}
                    onClose={() => setPanel(null)}
                    label={'复制记录'}
                    shortcut={''}
                    onSelect={() => setPanel('clips')}
                  />
                }
                {!phone && (
                  <TerminalMenuItem
                    phone={phone}
                    onClose={() => setPanel(null)}
                    label={fullscreen ? '退出全屏' : '全屏'}
                    shortcut={''}
                    onSelect={toggleFullscreen}
                  />
                )}
                {
                  <TerminalMenuItem
                    phone={phone}
                    onClose={() => setPanel(null)}
                    label={'终端设置'}
                    shortcut={''}
                    onSelect={() => setPanel('settings')}
                  />
                }
                {
                  <TerminalMenuItem
                    phone={phone}
                    onClose={() => setPanel(null)}
                    label={'导出历史'}
                    shortcut={''}
                    onSelect={exportText}
                  />
                }
                {panel === 'context' && (
                  <TerminalMenuItem
                    phone={phone}
                    onClose={() => setPanel(null)}
                    label={'清空本地画面'}
                    shortcut={''}
                    onSelect={() => workspace.clear()}
                  />
                )}
                {
                  <TerminalMenuItem
                    phone={phone}
                    onClose={() => setPanel(null)}
                    label={'重命名会话'}
                    shortcut={''}
                    onSelect={() => {
                      setRename(state.active);
                      setRenameText(labelOf(state.active));
                      requestAnimationFrame(() =>
                        document.querySelector<HTMLInputElement>('.cvm-terminal-rename')?.focus(),
                      );
                    }}
                  />
                }
                {
                  <TerminalMenuItem
                    phone={phone}
                    onClose={() => setPanel(null)}
                    label={'结束会话'}
                    shortcut={''}
                    onSelect={() => {
                      if (moreAnchor.current) requestEnd(state.active, moreAnchor.current);
                    }}
                    disabled={!current?.running}
                    danger={true}
                  />
                }
                {
                  <TerminalMenuItem
                    phone={phone}
                    onClose={() => setPanel(null)}
                    label={'关闭窗口，保留会话'}
                    shortcut={'Ctrl+Shift+Esc'}
                    onSelect={close}
                  />
                }
              </div>
            )}
            {panel === 'settings' && (
              <div className="cvm-terminal-settings">
                <Setting label="字号">
                  <input
                    type="range"
                    aria-label="字号"
                    min={10}
                    max={24}
                    value={prefs.fontSize}
                    onChange={event => changePrefs({ fontSize: Number(event.target.value) })}
                  />
                  <output>{prefs.fontSize}</output>
                </Setting>
                <Setting label="配色">
                  <select
                    className="f"
                    aria-label="配色"
                    value={prefs.color}
                    onChange={event => changePrefs({ color: event.target.value as TerminalPreferences['color'] })}
                  >
                    <option value="dark">深色</option>
                    <option value="light">浅色</option>
                  </select>
                </Setting>
                <Setting label="光标">
                  <select
                    className="f"
                    aria-label="光标"
                    value={prefs.cursor}
                    onChange={event => changePrefs({ cursor: event.target.value as TerminalPreferences['cursor'] })}
                  >
                    <option value="block">方块</option>
                    <option value="bar">竖线</option>
                    <option value="underline">下划线</option>
                  </select>
                </Setting>
                {(
                  [
                    ['blink', '光标闪烁'],
                    ['autoCopy', '选中即复制'],
                    ['ctrlCopy', '有选区时 Ctrl+C 复制'],
                    ['accessible', '辅助阅读'],
                  ] as const
                ).map(([key, label]) => (
                  <Setting key={key} label={label}>
                    <input
                      type="checkbox"
                      aria-label={label}
                      checked={prefs[key]}
                      onChange={event => changePrefs({ [key]: event.target.checked })}
                    />
                  </Setting>
                ))}
              </div>
            )}
            {panel === 'clips' && (
              <div className="cvm-terminal-clips">
                <p>仅存于当前窗口，关闭后清除。</p>
                {state.clips.length ? (
                  state.clips.map((text, i) => (
                    <button key={i} title="再次复制" onClick={() => void workspace.copy(text)}>
                      <code>{text}</code>
                      <small>{byteLabel(new TextEncoder().encode(text).length)}</small>
                    </button>
                  ))
                ) : (
                  <p>还没有复制记录</p>
                )}
              </div>
            )}
          </section>
        </DialogLayer>
      )}
      {endId && (
        <DialogLayer
          label="结束终端会话"
          mode="popover"
          anchor={endAnchor}
          align="end"
          className="cvm-webshell-popover"
          onClose={() => setEndId('')}
        >
          <section className="dialog-surface cvm-terminal-panel cvm-terminal-confirm" data-theme={prefs.color}>
            <h3>结束{labelOf(endId)}？</h3>
            <p>将结束这个 Shell 和运行中的命令。关闭窗口可保留会话。</p>
            <footer>
              <DialogClose className="btn small">取消</DialogClose>
              <button
                className="btn danger small"
                onClick={() => {
                  workspace.end(endId);
                  setEndId('');
                }}
              >
                结束会话
              </button>
            </footer>
          </section>
        </DialogLayer>
      )}
      {pending && (
        <DialogLayer label="多行粘贴确认" className="cvm-webshell-paste-layer" onClose={() => workspace.cancelPaste()}>
          <section className="dialog-surface cvm-terminal-paste-guard" data-theme={prefs.color}>
            <h3>粘贴 {pending.text.split(/\r\n|\r|\n/).length} 行内容？</h3>
            <p>当前程序未开启括号粘贴，换行可能逐行执行命令。</p>
            <pre>
              {pending.text.split(/\r\n|\r|\n/).map((line, i) => (
                <span key={i}>
                  <i>{i + 1}</i>
                  {line}
                  {'\n'}
                </span>
              ))}
            </pre>
            <footer>
              <DialogClose className="btn small">取消</DialogClose>
              <button
                className="btn primary small"
                onClick={() => {
                  if (workspace.paste(pending.text, pending.target, true))
                    setDraft(value => (value === pending.text ? '' : value));
                }}
              >
                确认粘贴
              </button>
            </footer>
          </section>
        </DialogLayer>
      )}
    </main>
  );
}

function Setting({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="cvm-terminal-setting">
      <span>{label}</span>
      <div>{children}</div>
    </div>
  );
}

function TerminalMenuItem({
  label,
  shortcut,
  onSelect,
  disabled = false,
  danger = false,
  phone,
  onClose,
}: {
  label: string;
  shortcut: string;
  onSelect: () => void;
  disabled?: boolean;
  danger?: boolean;
  phone: boolean;
  onClose: () => void;
}) {
  return (
    <button
      role="menuitem"
      aria-label={label}
      className={danger ? 'danger' : ''}
      disabled={disabled}
      onClick={() => {
        onClose();
        onSelect();
      }}
    >
      <span>{label}</span>
      {!phone && shortcut && <kbd>{shortcut}</kbd>}
    </button>
  );
}
