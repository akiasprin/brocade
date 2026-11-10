import { useId, type ReactNode } from 'react';
import { type CvmInstance } from '../../api';
import { Icon, PanelTitle } from '../../ui/icons';

import { statusText } from './queries';

export function AppliedState({ instance }: { instance: CvmInstance }) {
  const label =
    instance.status === 'applied'
      ? instance.report?.power === 'running'
        ? '运行中'
        : instance.report?.power === 'stopped'
          ? '已关机'
          : instance.report?.power === 'deleted'
            ? '已删除'
            : '已应用'
      : statusText[instance.status];
  return (
    <span
      className={`st ${instance.status === 'applied' ? 'st-succeeded' : instance.status === 'failed' ? 'st-failed' : 'st-pending'}`}
      role="status"
    >
      {label}
    </span>
  );
}

export function Section({
  title,
  children,
  aside,
  className = '',
}: {
  title: string;
  children: ReactNode;
  aside?: ReactNode;
  className?: string;
}) {
  return (
    <section className={`panel titled cvm-section ${className}`}>
      <header>
        <PanelTitle of="servers">{title}</PanelTitle>
        {aside}
      </header>
      <div className="cvm-section-body">{children}</div>
    </section>
  );
}

export function CvmPageHeader({
  title,
  state,
  tabs,
  tools,
  detail = false,
}: {
  title: string;
  state?: ReactNode;
  tabs?: ReactNode;
  tools?: ReactNode;
  detail?: boolean;
}) {
  return (
    <header className={`nd-page-head cvm-page-head ${detail ? 'cvm-instance-head' : 'cvm-tabs-head'}`}>
      <div className="nd-page-identity">
        <span className="cg-plate">
          <Icon of="servers" size={18} />
        </span>
        <div className="cvm-page-heading">
          <h1 className="nd-id nd-name">{title}</h1>
          {state}
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
