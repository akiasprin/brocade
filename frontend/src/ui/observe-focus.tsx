import { useState, type ButtonHTMLAttributes } from 'react';

/** Each plot owns its selection; an unavailable series cannot mute the remaining curves. */
export function useObserveFocus(names: readonly string[]) {
  const [chosen, setChosen] = useState<string | null>(null);
  return {
    focusedLine: chosen !== null && names.includes(chosen) ? chosen : null,
    onFocusLine: (name: string) => setChosen(current => (current === name ? null : name)),
  };
}

export function ObserveLegendItem({
  pressed,
  onToggle,
  className,
  ...props
}: ButtonHTMLAttributes<HTMLButtonElement> & { pressed: boolean; onToggle: () => void }) {
  return (
    <button
      {...props}
      type="button"
      className={`observe-legend-item${className ? ` ${className}` : ''}`}
      aria-pressed={pressed}
      onClick={onToggle}
    />
  );
}

/** Keep all samples, axes and stack totals stable while emphasizing the selected curve. */
export function observeSeriesFocus(name: string, focusedLine: string | null) {
  const focused = name === focusedLine;
  const muted = focusedLine !== null && !focused;
  return {
    z: focused ? 3 : 2,
    lineStyle: { width: focused ? 2.5 : 1.5, opacity: muted ? 0.14 : 1 },
    areaStyle: { opacity: muted ? 0.05 : 1 },
    itemStyle: { opacity: muted ? 0.14 : 1 },
    emphasis: { disabled: true },
  };
}
