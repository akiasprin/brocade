import { useCallback, useEffect, useRef, useState } from 'react';
import { useIsMutating } from '@tanstack/react-query';
import { prepareForDocumentReload } from './navigation-guard';
import { MODULE_ERROR_EVENT, MODULE_RELOAD_EVENT } from './lazy';

const DEFAULT_CHECK_INTERVAL_MS = 60_000;
const DEFAULT_RELOAD_DELAY_MS = 650;

type UpdatePhase = 'idle' | 'ready' | 'blocked' | 'reloading' | 'failed';
const MODULE_RELOAD_KEY = 'brocade.module-reload-at';
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
  const [moduleFailed, setModuleFailed] = useState(false);
  const mutationCount = useIsMutating();
  const reloadTimerRef = useRef<number | null>(null);
  const repairControllerRef = useRef<AbortController | null>(null);
  const failedPreloadsRef = useRef(new Set<string>());
  const reloadingRef = useRef(false);

  const requestReload = useCallback(
    (interaction?: Event, automaticModuleRecovery = false) => {
      if (reloadingRef.current) return;
      if (automaticModuleRecovery) {
        // A persistent chunk/network failure must leave a readable retry surface, not a reload loop.
        try {
          const previous = Number(sessionStorage.getItem(MODULE_RELOAD_KEY));
          if (previous && Date.now() - previous < 60_000) {
            setPhase('failed');
            return;
          }
        } catch {
          setPhase('failed');
          return;
        }
      }
      if (mutationCount > 0 || !prepareForDocumentReload()) {
        setPhase('blocked');
        return;
      }

      // Consume the click that chose the old document. Replaying it after reload would be unsafe:
      // its target may no longer exist or may now mean something different.
      interaction?.preventDefault();
      interaction?.stopImmediatePropagation();
      if (automaticModuleRecovery) {
        try {
          sessionStorage.setItem(MODULE_RELOAD_KEY, String(Date.now()));
        } catch {
          setPhase('failed');
          return;
        }
      }
      reloadingRef.current = true;
      setPhase('reloading');
      reloadTimerRef.current = window.setTimeout(() => {
        const urls = [...failedPreloadsRef.current];
        failedPreloadsRef.current.clear();
        if (urls.length === 0) {
          reloadPage();
          return;
        }
        // WebKit can retain failed preloads across reloads: https://bugs.webkit.org/show_bug.cgi?id=270357.
        // A reload-mode fetch evicts the poisoned resource before the fresh document imports it.
        const controller = new AbortController();
        repairControllerRef.current = controller;
        const timeout = window.setTimeout(() => controller.abort(), 2_000);
        void Promise.allSettled(
          urls.map(url =>
            fetch(url, { cache: 'reload', credentials: 'same-origin', signal: controller.signal }).then(response =>
              response.arrayBuffer(),
            ),
          ),
        ).finally(() => {
          window.clearTimeout(timeout);
          if (repairControllerRef.current !== controller) return;
          repairControllerRef.current = null;
          reloadPage();
        });
      }, reloadDelayMs);
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
    const onResourceError = (event: Event) => {
      const link = event.target;
      if (!(link instanceof HTMLLinkElement) || link.rel !== 'modulepreload') return;
      const url = new URL(link.href, window.location.href);
      if (url.origin !== window.location.origin || !url.pathname.endsWith('.js')) return;
      const failed = failedPreloadsRef.current;
      failed.add(url.href);
      if (failed.size > 64) failed.delete(failed.values().next().value!);
    };
    const onPreloadError = () => {
      // Vite emits this when an old open page asks for a hashed lazy chunk removed by deployment.
      // Let the import reject into its local boundary; suppressing it returns an undefined module.
      setModuleFailed(true);
      requestReload(undefined, true);
    };
    const onManualReload = () => requestReload();
    window.addEventListener('vite:preloadError', onPreloadError);
    document.addEventListener('error', onResourceError, true);
    window.addEventListener(MODULE_ERROR_EVENT, onPreloadError);
    window.addEventListener(MODULE_RELOAD_EVENT, onManualReload);
    return () => {
      window.removeEventListener('vite:preloadError', onPreloadError);
      document.removeEventListener('error', onResourceError, true);
      window.removeEventListener(MODULE_ERROR_EVENT, onPreloadError);
      window.removeEventListener(MODULE_RELOAD_EVENT, onManualReload);
    };
  }, [requestReload]);

  useEffect(
    () => () => {
      if (reloadTimerRef.current !== null) window.clearTimeout(reloadTimerRef.current);
      repairControllerRef.current?.abort();
      repairControllerRef.current = null;
    },
    [],
  );

  if (phase !== 'blocked' && phase !== 'reloading' && phase !== 'failed') return null;
  return (
    <div className={`console-update-status ${phase}`} role="status" aria-live="assertive">
      <span aria-hidden="true" />
      {phase === 'reloading'
        ? moduleFailed
          ? '界面加载失败，重新加载中…'
          : '网站版本已更新，加载新版中…'
        : phase === 'failed'
          ? '界面仍无法加载，请检查网络后重试'
          : moduleFailed
            ? '界面加载失败，完成当前操作后重新加载'
            : '新版可用，完成当前操作后加载'}
    </div>
  );
}
