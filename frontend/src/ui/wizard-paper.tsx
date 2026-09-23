import { useState, type FormEventHandler, type ReactNode } from 'react';
import { Icon, PanelTitle, type IconName } from './icons';
import type { WizardStage } from './wizard';

// 纳管和建链共享机器详情式的连续内层纸；页面路由本身不得再包第二层 fg-sheet。
export function WizardPaper({
  children,
  onSubmit,
}: {
  children: ReactNode;
  onSubmit?: FormEventHandler<HTMLFormElement>;
}) {
  return (
    <div className="nd-sheet nd-page pv-page">
      {onSubmit ? (
        <form className="fg-sheet nd-paper" noValidate onSubmit={onSubmit}>
          {children}
        </form>
      ) : (
        <div className="fg-sheet nd-paper">{children}</div>
      )}
    </div>
  );
}

export function WizardPaperHeader({
  title,
  meta,
  icon,
  stages,
  aside,
  created = false,
  online = false,
}: {
  title: string;
  meta: ReactNode;
  icon: IconName;
  stages: WizardStage[];
  aside?: ReactNode;
  created?: boolean;
  online?: boolean;
}) {
  return (
    <header className="nd-page-head pv-head">
      <div className="nd-page-identity">
        <span className={`pv-plate${created ? ' created' : ''}`}>
          <Icon of={icon} size={15} />
          {created && <i className={`node-lamp ${online ? 'online' : 'idle'}`} aria-hidden="true" />}
        </span>
        <div className="nd-ident-text">
          <div className="nd-ident-row">
            <h1 className="nd-id nd-name">{title}</h1>
          </div>
          <span className="nd-ident-meta">{meta}</span>
        </div>
      </div>
      <ol className="pv-steps" aria-label={icon === 'nodes' ? '纳管进度' : '建链进度'}>
        {stages.map((stage, index) => (
          <li className={stage.state} key={stage.label} aria-current={stage.state === 'current' ? 'step' : undefined}>
            <i aria-hidden="true">{stage.state === 'done' ? '✓' : index + 1}</i>
            {stage.label}
          </li>
        ))}
      </ol>
      {aside && <div className="nd-acts">{aside}</div>}
    </header>
  );
}

export function WizardCard({
  title,
  icon,
  summary,
  invalid = false,
  hint,
  children,
  className = '',
}: {
  title: string;
  icon: IconName;
  summary?: ReactNode;
  invalid?: boolean;
  hint?: ReactNode;
  children: ReactNode;
  className?: string;
}) {
  const [expanded, setExpanded] = useState(false);
  const heading = <PanelTitle of={icon}>{title}</PanelTitle>;
  if (summary !== undefined) {
    return (
      <details
        className={`panel config-panel config-disclosure ${className}`}
        open={expanded || invalid}
        onToggle={event => setExpanded(event.currentTarget.open)}
      >
        <summary>
          {heading}
          <span className="config-disclosure-summary">{summary}</span>
          <span className="sp" />
          <span className="config-disclosure-toggle">{expanded || invalid ? '收起' : '展开'}</span>
        </summary>
        {children}
      </details>
    );
  }
  return (
    <section className={`panel config-panel ${className}`}>
      <header>
        {heading}
        {hint && <span className="hint">{hint}</span>}
      </header>
      {children}
    </section>
  );
}

export function WizardField({ label, htmlFor, children }: { label: string; htmlFor?: string; children: ReactNode }) {
  return (
    <div className="row">
      {htmlFor ? (
        <label className="k" htmlFor={htmlFor}>
          {label}
        </label>
      ) : (
        <span className="k">{label}</span>
      )}
      <div className="v">{children}</div>
    </div>
  );
}

export function WizardFooter({
  title,
  description,
  tone,
  id,
  children,
}: {
  title: string;
  description: ReactNode;
  tone: 'idle' | 'ready' | 'busy' | 'ok' | 'warn';
  id?: string;
  children: ReactNode;
}) {
  return (
    <footer className="pv-foot">
      <div className="pv-state" id={id} aria-live="polite">
        <i className={`pv-dot ${tone}`} aria-hidden="true" />
        <span className="pv-state-copy">
          <b>{title}</b>
          <small>{description}</small>
        </span>
      </div>
      {children}
    </footer>
  );
}
