import type { ReactNode } from 'react';

export interface WizardStage {
  label: string;
  state: 'done' | 'current' | 'next';
}

export function WizardHeader({
  eyebrow,
  title,
  context,
  description,
  stages,
  aside,
}: {
  eyebrow: string;
  title: string;
  context?: string;
  description: string;
  stages?: WizardStage[];
  aside?: ReactNode;
}) {
  return (
    <header className="wz-head">
      <div className="wz-head-copy">
        <span className="wz-eyebrow">{eyebrow}</span>
        <div className="wz-titleline">
          <h2>{title}</h2>
          {context && <span className="wz-context mono">{context}</span>}
        </div>
        <p>{description}</p>
      </div>
      {aside}
      {stages && (
        <ol className="wz-stages" aria-label={`${title}进度`}>
          {stages.map((stage, index) => (
            <li className={stage.state} key={stage.label} aria-current={stage.state === 'current' ? 'step' : undefined}>
              <span>{stage.state === 'done' ? '✓' : index + 1}</span>
              {stage.label}
            </li>
          ))}
        </ol>
      )}
    </header>
  );
}

export function WizardSummary({ children, label }: { children: ReactNode; label: string }) {
  return (
    <div className="wz-summary" aria-label={label}>
      {children}
    </div>
  );
}

export function WizardSummaryItem({ label, children }: { label: string; children: ReactNode }) {
  return (
    <span className="wz-summary-item">
      <small>{label}</small>
      <b>{children}</b>
    </span>
  );
}
