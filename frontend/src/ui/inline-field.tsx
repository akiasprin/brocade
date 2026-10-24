import type { ReactNode } from 'react';

export function InlineField({ label, htmlFor, children }: { label: string; htmlFor?: string; children: ReactNode }) {
  return (
    <div className="fwe-field">
      {htmlFor ? (
        <label className="fwe-k" htmlFor={htmlFor}>
          {label}
        </label>
      ) : (
        <span className="fwe-k">{label}</span>
      )}
      <div className="fwe-v">{children}</div>
    </div>
  );
}
