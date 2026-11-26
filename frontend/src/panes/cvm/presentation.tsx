import { useId, type ReactNode } from 'react';
import { type CvmInstance } from '../../api';
import { Icon, type IconName } from '../../ui/icons';

import { hasPendingCvmChange, statusText } from './queries';

function stateLabel(instance: CvmInstance) {
  if (!instance.reserved && instance.operation_kind === 'undo_pending') return '已撤回创建';
  if (hasPendingCvmChange(instance)) return instance.blocking_deployment_id ? '发布中' : '待发布';
  if (instance.status !== 'applied') return statusText[instance.status];
  if (instance.report?.power === 'running') return '运行中';
  if (instance.report?.power === 'stopped') return '已关机';
  if (instance.report?.power === 'deleted')
    return instance.desired.spec.execution.kind === 'baremetal' ? '已解除纳管' : '已删除';
  return '已应用';
}

export function AppliedState({ instance }: { instance: CvmInstance }) {
  return (
    <span
      className={`st ${hasPendingCvmChange(instance) ? 'st-pending' : instance.status === 'applied' ? 'st-succeeded' : instance.status === 'failed' ? 'st-failed' : 'st-pending'}`}
      role="status"
    >
      {stateLabel(instance)}
    </span>
  );
}

/** 标题图标右下角的状态灯，与机器详情一致：绿色运行、灰色停止、黄色处理中、红色失败。 */
export function instanceLamp(instance: CvmInstance): { tone: string; why: string } {
  if (hasPendingCvmChange(instance) && !instance.blocking_deployment_id)
    return instance.report?.power === 'running'
      ? { tone: 'ok', why: '运行中' }
      : instance.report?.power === 'stopped'
        ? { tone: 'idle', why: '已关机' }
        : { tone: 'warn', why: '等待首次发布' };
  const why = stateLabel(instance);
  if (instance.status === 'failed') return { tone: 'bad', why };
  if (instance.status !== 'applied') return { tone: 'warn', why };
  return { tone: instance.report?.power === 'running' ? 'ok' : 'idle', why };
}

export function InstanceStateDot({ instance }: { instance: CvmInstance }) {
  const lamp = instanceLamp(instance);
  const state =
    lamp.tone === 'bad' ? 'failed' : lamp.tone === 'warn' ? 'pending' : lamp.tone === 'idle' ? 'stopped' : '';
  return <span className={`cz-dot ${state}`} role="img" aria-label={lamp.why} title={lamp.why} />;
}

/** 表单区块：粗体标题、灰色说明，操作靠右；纳管裸机等独立表单使用。 */
export function Section({
  title,
  children,
  aside,
  meta,
  icon,
  step,
  className = '',
  bare = false,
}: {
  title: string;
  children: ReactNode;
  aside?: ReactNode;
  meta?: ReactNode;
  icon?: IconName;
  step?: number;
  className?: string;
  /** 内容自带卡片（如多项出站方式）时不再套一层。 */
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
        <h2>
          {icon && <Icon of={icon} size={14} />}
          {title}
        </h2>
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
    <header className={`nd-page-head cvm-page-head${detail ? ' cvm-instance-head' : ''}`} data-tabbed={Boolean(tabs)}>
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
  items: readonly {
    key: T;
    label: string;
    shortLabel?: string;
    icon?: IconName;
    badge?: { count: number; label: string; tone?: 'dim' | 'gold' };
  }[];
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
            aria-label={item.label}
            aria-description={item.badge && item.badge.count > 0 ? item.badge.label : undefined}
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
              const workspace = event.currentTarget.closest('.fg-view');
              const targetId = `${id}-${items[next].key}`;
              onChange(items[next].key);
              document.getElementById(targetId)?.focus();
              if (workspace && next !== index) {
                // Routed pages focus their workspace after restoring scroll position.
                window.requestAnimationFrame(() => {
                  const target = document.getElementById(targetId);
                  if (document.activeElement === workspace && target?.getAttribute('aria-selected') === 'true')
                    target.focus({ preventScroll: true });
                });
              }
            }}
          >
            {item.icon && <Icon of={item.icon} size={14} />}
            {item.shortLabel ? (
              <>
                <span className="cvm-tab-full-label">{item.label}</span>
                <span className="cvm-tab-short-label">{item.shortLabel}</span>
              </>
            ) : (
              item.label
            )}
            {item.badge && item.badge.count > 0 && (
              <span className={`nd-tab-badge ${item.badge.tone ?? 'gold'}`} aria-hidden="true">
                {item.badge.count}
              </span>
            )}
          </button>
        ))}
      </div>
    </div>
  );
}
