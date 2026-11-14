import {
  Fragment,
  useEffect,
  useId,
  useLayoutEffect,
  useRef,
  useState,
  type KeyboardEvent,
  type ReactNode,
  type RefObject,
} from 'react';
import { createPortal } from 'react-dom';
import { RegionFlag } from '../../ui/region-flag';
import { SiteIcon } from '../../ui/site-icon';
import { useNarrow } from '../../ui/viewport';
import type { FlowMachine, FlowModel, FlowRule } from './model';
import type { FlowTarget, FlowTrace } from './trace';
import './trace-bar.css';
type Network = 'tcp' | 'udp';
type Outcome =
  | { kind: 'empty' }
  | { kind: 'invalid'; text: string }
  | { kind: 'pending'; path: FlowMachine[] }
  | { kind: 'uncertain'; path: FlowMachine[]; rule: FlowRule; reason: string }
  | { kind: 'done'; path: FlowMachine[]; action: 'egress' | 'block' | 'proxy'; label: string }
  | { kind: 'stop'; path: FlowMachine[]; text: string };

function outcomeOf(model: FlowModel, query: string, target: FlowTarget | null, trace: FlowTrace | null): Outcome {
  if (!query.trim()) return { kind: 'empty' };
  if (!target || !trace)
    return {
      kind: 'invalid',
      text:
        (query.match(/:/g)?.length ?? 0) >= 2 ? '不是有效的地址；IPv6 带端口写作 [地址]:端口' : '不是有效的域名或 IP',
    };
  const path = [...trace.machines].flatMap(id => model.machines.find(machine => machine.id === id) ?? []);
  const last = path.at(-1);
  if (trace.uncertain && last) {
    const rule = last.rules.find(row => row.id === trace.uncertain)!;
    const reason = trace.reason ?? trace.message;
    if (trace.pending) return { kind: 'pending', path };
    return { kind: 'uncertain', path, rule, reason };
  }
  const hit = last?.rules.filter(row => trace.rules.has(row.id)).at(-1);
  if (hit && !hit.target) {
    const action = hit.rule.a;
    if (action.t === 'egress') return { kind: 'done', path, action: 'egress', label: '本机出网' };
    if (action.t === 'block') return { kind: 'done', path, action: 'block', label: '拒绝' };
    if (action.t === 'proxy')
      return {
        kind: 'done',
        path,
        action: 'proxy',
        label: model.outbounds.find(outbound => outbound.id === action.outbound)?.name || action.outbound,
      };
  }
  return { kind: 'stop', path, text: trace.message };
}

function Chevron() {
  return (
    <svg className="flow-trace-chev" viewBox="0 0 10 10" aria-hidden="true">
      <path d="m3.5 1.8 3.2 3.2-3.2 3.2" />
    </svg>
  );
}

function Stops({ path }: { path: FlowMachine[] }) {
  return (
    <>
      {path.map((machine, index) => (
        <Fragment key={machine.id}>
          {index > 0 && <Chevron />}
          <span className="flow-trace-stop">
            {machine.country && <RegionFlag code={machine.country} />}
            <b>{machine.name}</b>
          </span>
        </Fragment>
      ))}
    </>
  );
}

function Route({ outcome, className = '' }: { outcome: Outcome; className?: string }) {
  if (outcome.kind === 'empty') return null;
  let body: ReactNode;
  let title = '';
  switch (outcome.kind) {
    case 'invalid':
      body = <span className="flow-trace-msg is-err">{outcome.text}</span>;
      break;
    case 'pending':
      body = (
        <>
          <Stops path={outcome.path} />
          <span className="flow-trace-msg">
            <i className="flow-trace-spin" aria-hidden="true" />
            正在匹配 GEOSITE
          </span>
        </>
      );
      break;
    case 'uncertain':
      title = `规则 ${outcome.rule.index + 1}：${outcome.reason}`;
      body = (
        <>
          <Stops path={outcome.path} />
          <span className="flow-trace-tag is-gold">规则 {outcome.rule.index + 1} 待确认</span>
          <span className="flow-trace-why">{outcome.reason}</span>
        </>
      );
      break;
    case 'done':
      body = (
        <>
          <Stops path={outcome.path} />
          <span className={`flow-trace-tag is-${outcome.action}`}>{outcome.label}</span>
        </>
      );
      break;
    case 'stop':
      body = (
        <>
          <Stops path={outcome.path} />
          <span className="flow-trace-msg is-gold">{outcome.text}</span>
        </>
      );
      break;
  }
  return (
    <span className={`flow-trace-route is-${outcome.kind} ${className}`} role="status" title={title || undefined}>
      {body}
    </span>
  );
}

function Protocol({
  network,
  setNetwork,
  className = '',
}: {
  network: Network;
  setNetwork: (value: Network) => void;
  className?: string;
}) {
  const group = useRef<HTMLSpanElement>(null);
  const move = (event: KeyboardEvent) => {
    if (!['ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown'].includes(event.key)) return;
    event.preventDefault();
    const next = network === 'tcp' ? 'udp' : 'tcp';
    setNetwork(next);
    group.current?.querySelector<HTMLButtonElement>(`[data-value="${next}"]`)?.focus();
  };
  return (
    <span
      ref={group}
      className={`flow-trace-seg${network === 'udp' ? ' is-second' : ''} ${className}`}
      role="radiogroup"
      aria-label="追踪传输层"
      onKeyDown={move}
    >
      <span className="flow-trace-seg-thumb" aria-hidden="true" />
      {(['tcp', 'udp'] as const).map(value => (
        <button
          key={value}
          type="button"
          role="radio"
          data-value={value}
          aria-checked={network === value}
          tabIndex={network === value ? 0 : -1}
          onClick={() => setNetwork(value)}
        >
          {value.toUpperCase()}
        </button>
      ))}
    </span>
  );
}

/* Muse 官网 favicon：https://muse.ai/images/favicon/app-squiggle.svg。
   保留原路径，改用 currentColor，与生产 SiteIcon 的单色标识一致。 */
function MuseIcon() {
  return (
    <svg viewBox="0 0 100 100" fill="currentColor" aria-hidden="true" focusable="false">
      <path d="M24.6257 16.1214C28.2365 16.1214 30.8187 18.9408 30.8187 23.3643C30.8185 27.7868 30.1055 31.7765 28.9225 40.2466C28.0743 46.1813 26.62 55.2418 26.0132 59.0088C27.1782 56.7815 29.9074 51.5757 30.3914 50.5982C36.1081 39.061 42.0657 28.7544 43.7826 26.0254C46.0594 22.382 48.062 20.9229 51.1597 20.9229C52.7685 20.923 54.6826 21.7357 55.6315 22.819C56.9178 24.2895 57.1859 26.6602 57.1411 29.244C57.0738 33.2073 56.5599 39.5264 55.8553 46.5698C55.5416 49.7057 54.9365 56.1066 54.5288 60.4004C58.1883 53.8532 64.9609 41.8109 67.6921 37.6058C70.0727 33.9402 72.466 31.5797 75.6836 31.5796C78.4595 31.5796 81.8809 33.5416 81.6772 38.7573C81.3774 46.4556 80.2673 63.2232 79.7607 70.7479C83.1662 65.7913 87.6377 58.4543 89.3148 55.6966C91.0104 52.908 92.75 50.6542 95.3898 50.5778C98.0987 50.4995 99.913 52.341 99.9959 55.1839C100.078 58.0277 98.3835 61.2256 96.1344 64.856C93.6937 68.7957 90.9621 73.0123 87.6383 76.9491C84.3148 80.8856 81.2604 83.8782 76.5991 83.8786C72.4243 83.8786 69.4869 81.1043 69.283 76.0254C69.1768 73.364 69.6804 67.6405 70.0602 63.1673C70.5 57.9875 71.0315 52.9861 71.403 49.6786C67.7337 56.3979 60.0101 70.4676 56.5348 75.9603C53.1343 81.3339 51.4774 83.2269 47.8882 83.2275C44.2977 83.2275 42.7226 80.6416 42.6717 76.7497C42.6142 72.5285 45.7852 47.1821 46.8587 38.737C45.1183 42.4616 40.839 51.6056 38.1551 57.0435C35.5587 62.3055 31.204 71.3978 28.0436 76.6357C25.7668 80.4094 23.2092 83.2273 19.4173 83.2275C15.6252 83.2275 13.4847 80.6784 13.4847 76.5788C13.4847 72.4794 14.6716 65.6492 15.9465 57.1777C16.5839 53.0781 17.3108 48.6133 18.1315 43.8761C18.7324 40.448 19.7764 34.4169 20.4183 30.7129C17.502 35.2539 12.2536 43.4278 10.6567 45.8903C8.88113 48.6291 7.28747 50.626 4.64681 50.6266C1.7329 50.6266 0.000532901 48.3522 0 45.7113C0 43.0703 1.34323 41.0344 3.52376 37.4512C6.32403 32.8465 11.3578 26.57 14.1154 23.2747C17.3656 19.3912 20.6139 16.1219 24.6257 16.1214Z" />
    </svg>
  );
}

/* 菜单里的方形视频标识，播放键镂空，随明暗主题显示。 */
function YoutubeSquareIcon() {
  return (
    <svg viewBox="0 0 24 24" fill="currentColor" aria-hidden="true" focusable="false">
      <path
        fillRule="evenodd"
        d="M7 2h10a5 5 0 0 1 5 5v10a5 5 0 0 1-5 5H7a5 5 0 0 1-5-5V7a5 5 0 0 1 5-5Zm3 5.5v9l7-4.5Z"
      />
    </svg>
  );
}

const COMMON_DOMAINS = [
  { value: 'google.com', to: 'Google 搜索', icon: <SiteIcon siteId="google" /> },
  { value: 'youtube.com', to: 'YouTube', icon: <YoutubeSquareIcon /> },
  { value: 'chatgpt.com', to: 'ChatGPT', icon: <SiteIcon siteId="openai" /> },
  { value: 'claude.ai', to: 'Claude', icon: <SiteIcon siteId="anthropic" /> },
  { value: 'muse.ai', to: 'Muse', icon: <MuseIcon /> },
  { value: 'github.com', to: 'GitHub', icon: <SiteIcon siteId="github" /> },
];

/** Keeps the input focused while the listbox is rendered outside the scrolling canvas. */
function Suggestions({
  id,
  anchor,
  active,
  pick,
  close,
}: {
  id: string;
  anchor: RefObject<HTMLDivElement | null>;
  active: number;
  pick: (value: string) => void;
  close: () => void;
}) {
  const menu = useRef<HTMLDivElement>(null);
  const [box, setBox] = useState<{ left: number; top: number; width: number; maxHeight: number } | null>(null);
  useLayoutEffect(() => {
    const place = () => {
      const rect = anchor.current?.getBoundingClientRect();
      const list = menu.current;
      if (!rect || !list) return;
      const viewport = window.visualViewport;
      const leftEdge = (viewport?.offsetLeft ?? 0) + 10;
      const topEdge = (viewport?.offsetTop ?? 0) + 10;
      const rightEdge = leftEdge + (viewport?.width ?? innerWidth) - 20;
      const bottomEdge = topEdge + (viewport?.height ?? innerHeight) - 20;
      const width = Math.min(Math.max(rect.width, 300), rightEdge - leftEdge);
      const below = bottomEdge - rect.bottom - 6;
      const above = rect.top - topEdge - 6;
      const naturalHeight = list.scrollHeight + 2;
      const flip = below < naturalHeight && above > below;
      const maxHeight = Math.max(44, flip ? above : below);
      const height = Math.min(naturalHeight, maxHeight);
      setBox({
        left: Math.max(leftEdge, Math.min(rect.left, rightEdge - width)),
        top: Math.max(topEdge, flip ? rect.top - height - 6 : rect.bottom + 6),
        width,
        maxHeight,
      });
    };
    place();
    const observer = new ResizeObserver(place);
    if (anchor.current) observer.observe(anchor.current);
    addEventListener('resize', place);
    addEventListener('scroll', place, true);
    window.visualViewport?.addEventListener('resize', place);
    return () => {
      observer.disconnect();
      removeEventListener('resize', place);
      removeEventListener('scroll', place, true);
      window.visualViewport?.removeEventListener('resize', place);
    };
  }, [anchor]);
  useEffect(() => {
    const outside = (event: PointerEvent) => {
      if (
        event.target instanceof Node &&
        !anchor.current?.contains(event.target) &&
        !menu.current?.contains(event.target)
      )
        close();
    };
    document.addEventListener('pointerdown', outside, true);
    return () => document.removeEventListener('pointerdown', outside, true);
  }, [anchor, close]);
  useLayoutEffect(() => {
    if (active >= 0) menu.current?.querySelectorAll('[role="option"]')[active]?.scrollIntoView({ block: 'nearest' });
  }, [active]);
  return createPortal(
    <div
      ref={menu}
      className="flow-trace-suggest"
      id={id}
      role="listbox"
      aria-label="常用域名"
      style={box ?? { visibility: 'hidden' }}
    >
      <h6 aria-hidden="true">常用域名</h6>
      {COMMON_DOMAINS.map((row, index) => (
        <div
          key={row.value}
          id={`${id}-${index}`}
          role="option"
          aria-selected={index === active}
          onMouseDown={event => event.preventDefault()}
          onClick={() => pick(row.value)}
        >
          <span className="flow-trace-suggest-domain">
            <span className="flow-trace-domain-icon" aria-hidden="true">
              {row.icon}
            </span>
            <span>{row.value}</span>
          </span>
          <small>{row.to}</small>
        </div>
      ))}
    </div>,
    document.body,
  );
}

export function FlowTraceBar({
  model,
  query,
  onQueryChange,
  network,
  onNetworkChange,
  target,
  trace,
}: {
  model: FlowModel;
  query: string;
  onQueryChange: (value: string) => void;
  network: Network;
  onNetworkChange: (value: Network) => void;
  target: FlowTarget | null;
  trace: FlowTrace | null;
}) {
  const outcome = outcomeOf(model, query, target, trace);
  const field = useRef<HTMLDivElement>(null);
  const input = useRef<HTMLInputElement>(null);
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(-1);
  const list = useId();
  const narrow = useNarrow();
  const show = () => {
    setActive(-1);
    setOpen(true);
  };
  const pick = (value: string) => {
    onQueryChange(value);
    setOpen(false);
    input.current?.focus();
  };
  return (
    <div className="flow-trace">
      <div className="flow-trace-capsule" ref={field} data-invalid={outcome.kind === 'invalid' || undefined}>
        <label className="flow-trace-field">
          <svg className="flow-trace-glass" viewBox="0 0 16 16" aria-hidden="true">
            <circle cx="6.8" cy="6.8" r="4.3" />
            <path d="m10.1 10.1 3.6 3.6" />
          </svg>
          <input
            ref={input}
            aria-label="追踪域名或 IP"
            placeholder={narrow ? '追踪域名或 IP' : '追踪域名或 IP，可带端口'}
            value={query}
            spellCheck={false}
            autoCapitalize="off"
            autoCorrect="off"
            autoComplete="off"
            enterKeyHint="search"
            aria-invalid={outcome.kind === 'invalid'}
            role="combobox"
            aria-expanded={open}
            aria-controls={open ? list : undefined}
            aria-autocomplete="list"
            aria-activedescendant={open && active >= 0 ? `${list}-${active}` : undefined}
            onChange={event => {
              onQueryChange(event.target.value);
              setOpen(false);
            }}
            onFocus={show}
            onClick={show}
            onBlur={() => setOpen(false)}
            onKeyDown={event => {
              if (event.nativeEvent.isComposing) return;
              if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
                event.preventDefault();
                setOpen(true);
                const last = COMMON_DOMAINS.length - 1;
                setActive(index =>
                  event.key === 'ArrowDown' ? (index >= last ? 0 : index + 1) : index <= 0 ? last : index - 1,
                );
              } else if (open && event.key === 'Enter' && active >= 0) {
                event.preventDefault();
                pick(COMMON_DOMAINS[active].value);
              } else if (event.key === 'Escape' && (open || query)) {
                event.preventDefault();
                event.stopPropagation();
                if (open) setOpen(false);
                else onQueryChange('');
              }
            }}
          />
          {query && (
            <button
              type="button"
              className="flow-trace-clear"
              aria-label="清除追踪"
              onMouseDown={event => event.preventDefault()}
              onClick={() => {
                onQueryChange('');
                setOpen(false);
                input.current?.focus();
              }}
            >
              <svg viewBox="0 0 10 10" aria-hidden="true">
                <path d="m3 3 4 4m0-4-4 4" />
              </svg>
            </button>
          )}
        </label>
        <span className="flow-trace-sep" aria-hidden="true" />
        <Protocol network={network} setNetwork={onNetworkChange} />
      </div>
      {open && <Suggestions id={list} anchor={field} active={active} pick={pick} close={() => setOpen(false)} />}
      <div className="flow-trace-under">
        <Route outcome={outcome} className="flow-trace-pill" />
      </div>
    </div>
  );
}
