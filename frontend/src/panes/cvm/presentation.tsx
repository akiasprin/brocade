import { useId, type ReactNode } from 'react';
import { type CvmInstance } from '../../api';
import { Icon } from '../../ui/icons';

import { statusText } from './queries';

function stateLabel(instance: CvmInstance) {
  if (instance.status !== 'applied') return statusText[instance.status];
  if (instance.report?.power === 'running') return '运行中';
  if (instance.report?.power === 'stopped') return '已关机';
  if (instance.report?.power === 'deleted')
    return instance.desired.spec.execution?.kind === 'baremetal' ? '已解除纳管' : '已删除';
  return '已应用';
}

export function AppliedState({ instance }: { instance: CvmInstance }) {
  return (
    <span
      className={`st ${instance.status === 'applied' ? 'st-succeeded' : instance.status === 'failed' ? 'st-failed' : 'st-pending'}`}
      role="status"
    >
      {stateLabel(instance)}
    </span>
  );
}

/** 标题图标右下角的状态灯，与机器详情一致：绿色运行、灰色停止、黄色处理中、红色失败。 */
export function instanceLamp(instance: CvmInstance): { tone: string; why: string } {
  const why = stateLabel(instance);
  if (instance.status === 'failed') return { tone: 'bad', why };
  if (instance.status !== 'applied') return { tone: 'warn', why };
  return { tone: instance.report?.power === 'running' ? 'ok' : 'idle', why };
}

/** 组合台的栏头语言：粗体标题、灰色计数，操作靠右；内容落在无边框卡片上。 */
export function Section({
  title,
  children,
  aside,
  meta,
  step,
  className = '',
  bare = false,
}: {
  title: string;
  children: ReactNode;
  aside?: ReactNode;
  meta?: ReactNode;
  step?: number;
  className?: string;
  /** 内容自带卡片（如多张网卡卡片）时不再套一层。 */
  bare?: boolean;
}) {
  return (
    <section className={`cvm-section ${className}`} aria-label={title}>
      <header className="cvm-lane-head">
        {step !== undefined && (
          <span className="cvm-step-no" aria-hidden="true">
            {step}
          </span>
        )}
        <h2>{title}</h2>
        {meta && <small>{meta}</small>}
        {aside && <span className="cvm-lane-aside">{aside}</span>}
      </header>
      <div className={`cvm-section-body${bare ? '' : ' cvm-tile'}`}>{children}</div>
    </section>
  );
}

export function CvmPageHeader({
  title,
  state,
  lamp,
  meta,
  tabs,
  tools,
  detail = false,
}: {
  title: string;
  state?: ReactNode;
  lamp?: { tone: string; why: string };
  meta?: ReactNode;
  tabs?: ReactNode;
  tools?: ReactNode;
  detail?: boolean;
}) {
  return (
    <header className={`nd-page-head cvm-page-head ${detail ? 'cvm-instance-head' : 'cvm-tabs-head'}`}>
      <div className="nd-page-identity">
        <span className="cg-plate cvm-plate">
          <Icon of="servers" size={18} />
          {lamp && <i className={`node-lamp ${lamp.tone}`} role="img" aria-label={lamp.why} title={lamp.why} />}
        </span>
        <div className="nd-ident-text">
          <div className="nd-ident-row cvm-page-heading">
            <h1 className="nd-id nd-name">{title}</h1>
            {state}
          </div>
          {meta && <span className="nd-ident-meta cvm-ident-meta">{meta}</span>}
        </div>
      </div>
      {tabs}
      {tools}
    </header>
  );
}

export function CvmTabs<T extends string>({
  label,
  value,
  items,
  onChange,
  panelId,
}: {
  label: string;
  value: T;
  items: { key: T; label: string }[];
  onChange: (key: T) => void;
  panelId: string;
}) {
  const id = useId();
  return (
    <div className="nd-tabs" role="tablist" aria-label={label}>
      <div className="nd-tabs-seg">
        {items.map((item, index) => (
          <button
            key={item.key}
            type="button"
            role="tab"
            id={`${id}-${item.key}`}
            aria-selected={value === item.key}
            aria-controls={panelId}
            tabIndex={value === item.key ? 0 : -1}
            onClick={() => onChange(item.key)}
            onKeyDown={event => {
              const move = ['ArrowLeft', 'ArrowUp'].includes(event.key)
                ? -1
                : ['ArrowRight', 'ArrowDown'].includes(event.key)
                  ? 1
                  : 0;
              const next =
                event.key === 'Home'
                  ? 0
                  : event.key === 'End'
                    ? items.length - 1
                    : move
                      ? (index + move + items.length) % items.length
                      : null;
              if (next === null) return;
              event.preventDefault();
              onChange(items[next].key);
              document.getElementById(`${id}-${items[next].key}`)?.focus();
            }}
          >
            {item.label}
          </button>
        ))}
      </div>
    </div>
  );
}
