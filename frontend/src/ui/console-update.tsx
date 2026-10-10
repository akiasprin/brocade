import { useCallback, useEffect, useRef, useState } from 'react';
import { useIsMutating } from '@tanstack/react-query';
import { prepareForDocumentReload } from './navigation-guard';

const DEFAULT_CHECK_INTERVAL_MS = 60_000;
const DEFAULT_RELOAD_DELAY_MS = 650;

type UpdatePhase = 'idle' | 'ready' | 'blocked' | 'reloading';
type VersionReader = (signal: AbortSignal) => Promise<string | null>;

interface ConsoleUpdateProps {
  checkIntervalMs?: number;
  reloadDelayMs?: number;
  reloadPage?: () => void;
  readLatestVersion?: VersionReader;
}

function documentVersion(): string | null {
  return document.querySelector<HTMLMetaElement>('meta[name="brocade-ui-version"]')?.content.trim() || null;
}

async function fetchLatestVersion(signal: AbortSignal): Promise<string | null> {
  const response = await fetch('/console/version', {
    cache: 'no-store',
    credentials: 'same-origin',
    headers: { Accept: 'application/json' },
    signal,
  });
  if (!response.ok) return null;
  const body = (await response.json()) as { ui_version?: unknown };
  return typeof body.ui_version === 'string' && body.ui_version.trim() ? body.ui_version.trim() : null;
}

/**
 * Detect a replaced embedded UI without disturbing the operator's current task. A detected update
 * stays invisible until the next safe click, which is consumed so old code cannot start another
 * action while the document is being replaced. Local-only edits and active mutations defer the
 * reload and receive a persistent status instead.
 */
export function ConsoleUpdate({
  checkIntervalMs = DEFAULT_CHECK_INTERVAL_MS,
  reloadDelayMs = DEFAULT_RELOAD_DELAY_MS,
  reloadPage = () => window.location.reload(),
  readLatestVersion = fetchLatestVersion,
}: ConsoleUpdateProps = {}) {
  const [currentVersion] = useState(documentVersion);
  const [phase, setPhase] = useState<UpdatePhase>('idle');
  const mutationCount = useIsMutating();
  const reloadTimerRef = useRef<number | null>(null);
  const reloadingRef = useRef(false);

  const requestReload = useCallback(
    (interaction?: Event) => {
      if (reloadingRef.current) return;
      if (mutationCount > 0 || !prepareForDocumentReload()) {
        setPhase('blocked');
        return;
      }

      // Consume the click that chose the old document. Replaying it after reload would be unsafe:
      // its target may no longer exist or may now mean something different.
      interaction?.preventDefault();
      interaction?.stopImmediatePropagation();
      reloadingRef.current = true;
      setPhase('reloading');
      reloadTimerRef.current = window.setTimeout(reloadPage, reloadDelayMs);
    },
    [mutationCount, reloadDelayMs, reloadPage],
  );

  useEffect(() => {
    // Only production HTML embedded by brocade-console carries this marker. In Vite development,
    // omitting it prevents a proxy pointed at production from causing an endless reload loop.
    if (!currentVersion) return;

    const controller = new AbortController();
    let inFlight = false;
    const check = async () => {
      if (inFlight || reloadingRef.current) return;
      inFlight = true;
      try {
        const latest = await readLatestVersion(controller.signal);
        if (latest && latest !== currentVersion) setPhase(previous => (previous === 'idle' ? 'ready' : previous));
      } catch {
        // Losing the version probe is not losing the console. Focus/online/interval checks retry
        // without surfacing a warning that would compete with the operator's actual work.
      } finally {
        inFlight = false;
      }
    };
    const onVisible = () => {
      if (document.visibilityState === 'visible') void check();
    };

    void check();
    const interval = window.setInterval(() => void check(), checkIntervalMs);
    window.addEventListener('focus', check);
    window.addEventListener('online', check);
    document.addEventListener('visibilitychange', onVisible);
    return () => {
      controller.abort();
      window.clearInterval(interval);
      window.removeEventListener('focus', check);
      window.removeEventListener('online', check);
      document.removeEventListener('visibilitychange', onVisible);
    };
  }, [checkIntervalMs, currentVersion, readLatestVersion]);

  useEffect(() => {
    if (phase !== 'ready' && phase !== 'blocked') return;
    const onClick = (event: MouseEvent) => requestReload(event);
    document.addEventListener('click', onClick, true);
    return () => document.removeEventListener('click', onClick, true);
  }, [phase, requestReload]);

  useEffect(() => {
    const onPreloadError = (event: Event) => {
      // Vite emits this when an old open page asks for a hashed lazy chunk removed by deployment.
      // Suppress the default exception and use the same edit-safe reload path.
      event.preventDefault();
      setPhase('ready');
      requestReload();
    };
    window.addEventListener('vite:preloadError', onPreloadError);
    return () => window.removeEventListener('vite:preloadError', onPreloadError);
  }, [requestReload]);

  useEffect(
    () => () => {
      if (reloadTimerRef.current !== null) window.clearTimeout(reloadTimerRef.current);
    },
    [],
  );

  if (phase !== 'blocked' && phase !== 'reloading') return null;
  return (
    <div className={`console-update-status ${phase}`} role="status" aria-live="assertive">
      <span aria-hidden="true" />
      {phase === 'reloading' ? '网站版本已更新，加载新版中…' : '新版可用，完成当前操作后加载'}
    </div>
  );
}
