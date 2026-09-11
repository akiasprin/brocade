import type { ReactNode } from 'react';

export interface SettingsParameterMetric {
  label: string;
  value: ReactNode;
}

/** 设置页参数组的统一摘要与展开控件。只读态仍可展开查看，因此不能使用禁用的 button。 */
export function SettingsParameterSummary({
  label,
  metrics,
  expanded,
  editable,
  controls,
  onToggle,
}: {
  label: string;
  metrics: SettingsParameterMetric[];
  expanded: boolean;
  editable: boolean;
  controls: string;
  onToggle: () => void;
}) {
  const action = expanded ? '收起' : editable ? '配置' : '查看';
  const spacing = /[A-Za-z0-9]$/.test(label) ? ' ' : '';
  const toggleProps = {
    className: 'btn sm settings-disclosure-toggle',
    'aria-expanded': expanded,
    'aria-controls': controls,
    'aria-label': `${action}${label}${spacing}参数`,
  } as const;

  return (
    <div className="settings-disclosure-summary">
      <div className="settings-summary-copy" aria-label={`${label}${spacing}当前参数`}>
        {metrics.map(metric => (
          <span className="settings-summary-metric" key={metric.label}>
            <span>{metric.label}</span>
            <b>{metric.value}</b>
          </span>
        ))}
      </div>
      {editable ? (
        <button type="button" {...toggleProps} onClick={onToggle}>
          {action}
        </button>
      ) : (
        <span
          {...toggleProps}
          role="button"
          tabIndex={0}
          onClick={onToggle}
          onKeyDown={event => {
            if (event.key === 'Enter' || event.key === ' ') {
              event.preventDefault();
              onToggle();
            }
          }}
        >
          {action}
        </span>
      )}
    </div>
  );
}
