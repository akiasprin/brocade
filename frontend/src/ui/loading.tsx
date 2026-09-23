export const LOADING_TEXT = '加载中…';
export const LOADING_SPINNER_RADIUS = 8;
export const LOADING_LINE_WIDTH = 2;

function LoadingIndicator({
  kind,
  label = LOADING_TEXT,
  announce = true,
}: {
  kind: 'panel' | 'field';
  label?: string;
  /** Disable the nested live region when an owning surface already exposes its busy state. */
  announce?: boolean;
}) {
  return (
    <span
      className={`loading-mark ${kind}`}
      role={announce ? 'status' : undefined}
      aria-live={announce ? 'polite' : undefined}
      aria-label={announce ? label : undefined}
    >
      <i
        aria-hidden="true"
        style={{
          width: LOADING_SPINNER_RADIUS * 2,
          height: LOADING_SPINNER_RADIUS * 2,
          borderWidth: LOADING_LINE_WIDTH,
        }}
      />
      <span>{label}</span>
    </span>
  );
}

/** Progress for a whole panel or content region whose data has not arrived yet. */
export function PanelLoading({ label, announce }: { label?: string; announce?: boolean }) {
  return <LoadingIndicator kind="panel" label={label} announce={announce} />;
}

/** Compact progress for one value, count, KPI, or other field inside an already visible panel. */
export function FieldLoading({ label, announce }: { label?: string; announce?: boolean }) {
  return <LoadingIndicator kind="field" label={label} announce={announce} />;
}
