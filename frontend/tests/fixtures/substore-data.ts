import type { SubStoreView, SubStoreRun, SubStoreWrite } from '../../src/api';
export function connectorFixture(scene = 'empty', writable = true): SubStoreView {
  const at = '2026-11-06T11:58:00Z';
  const events = ['读回来源', '主动拉取订阅', '验证转换结果'].map((name, checkpoint) => ({
    name,
    checkpoint,
    state: 'done',
    ms: 120,
    http: 200,
    detail: null,
  }));
  const run: SubStoreRun = {
    id: 1049,
    kind: 'sync',
    status: 'succeeded',
    trigger: '手动同步',
    actor: 'demo',
    config_version: 1,
    source_version: 58,
    started_at: at,
    ended_at: at,
    duration_ms: 1380,
    parent: null,
    attempts: [{ number: 1, status: 'succeeded', events }],
    error: null,
    output_count: 16,
  };
  const c = {
    version: 1,
    host: 'substore.example.test',
    title: 'Brocade · alice',
    auth: false,
    login: '',
    has_password: false,
    auto: true,
    registered: true,
    verified: true,
    last_confirmed: at,
    next_check: '2026-11-06T12:03:00Z',
    source_version: 58,
  };
  if (scene === 'running') {
    run.status = 'running';
    run.ended_at = null;
    run.attempts[0].status = 'running';
    run.attempts[0].events = events.slice(0, 2);
    events[1].state = 'running';
    c.verified = false;
  }
  if (scene === 'partial') {
    run.status = 'partial';
    run.error = 'Sub-Store 拉取返回错误';
    events[1].state = 'failed';
    run.attempts[0].events = events.slice(0, 2);
    c.verified = false;
  }
  if (scene === 'failed') {
    run.status = 'failed';
    run.error = '连接 Sub-Store 超时';
    run.attempts[0].events = [{ ...events[0], name: '连接 Sub-Store', state: 'failed' }];
    c.verified = false;
  }
  if (scene === 'stale') c.verified = false;
  return {
    writable: writable && scene !== 'readonly',
    version: scene === 'empty' ? 0 : 1,
    config: scene === 'empty' ? null : c,
    inactive: scene === 'inactive',
    active: scene === 'running' ? run : null,
    latest: scene === 'empty' ? null : run,
  };
}
/** Synthetic transport only. Every view renders the production SubscriptionViewer. */
export function subStoreTransport(scene: string, writable: boolean) {
  let view = connectorFixture(scene, writable);
  let runs = view.latest ? [view.latest] : [];
  return async (url: URL, init?: RequestInit): Promise<Response | null> => {
    if (!url.pathname.includes('/subscription-connectors/sub-store')) return null;
    const method = init?.method ?? 'GET';
    const tail = url.pathname.split('/sub-store')[1];
    if (method === 'GET' && tail === '') return Response.json(view);
    if (method === 'PUT') {
      const input = JSON.parse(String(init?.body)) as SubStoreWrite;
      const baseline = connectorFixture('ready', writable).config!;
      view = {
        ...view,
        version: view.version + 1,
        config: {
          ...baseline,
          ...view.config,
          title: input.title,
          auth: input.auth,
          login: '',
          has_password: input.auth,
          auto: input.auto,
          version: view.version + 1,
          host: input.endpoint ? new URL(input.endpoint).host : baseline.host,
          registered: view.config?.registered ?? false,
          verified: false,
        },
      };
      return Response.json(view);
    }
    if (method === 'DELETE' && tail === '') {
      view = { ...view, version: view.version + 1, config: null, active: null };
      return Response.json(view);
    }
    if (tail === '/test') return Response.json({ ok: true });
    if (tail === '/runs') {
      const filtered =
        url.searchParams.get('notable') === 'true'
          ? runs.filter(r => r.kind !== 'inspect' || r.status !== 'succeeded')
          : runs;
      return Response.json({ runs: filtered, next: null });
    }
    const id = /^\/runs\/(\d+)$/.exec(tail);
    if (id) {
      const r = runs.find(r => r.id === Number(id[1]));
      if (!r) return Response.json({ error: 'record not found' }, { status: 404 });
      if (method === 'DELETE') {
        r.status = 'canceled';
        view.active = null;
      }
      return Response.json(r);
    }
    if (method === 'POST') {
      const r = { ...connectorFixture('running').active!, id: (runs[0]?.id ?? 1049) + 1, config_version: view.version };
      runs = [r, ...runs];
      view = { ...view, active: r, latest: r };
      setTimeout(() => {
        if (r.status === 'canceled') return;
        r.status = 'succeeded';
        r.ended_at = '2026-11-06T12:00:00Z';
        r.attempts = connectorFixture('ready').latest!.attempts;
        view = {
          ...view,
          active: null,
          latest: r,
          config: view.config
            ? {
                ...view.config,
                registered: tail !== '/cleanup',
                verified: tail !== '/cleanup',
                last_confirmed: r.ended_at,
              }
            : null,
        };
      }, 700);
      return Response.json({ id: r.id }, { status: 202 });
    }
    return Response.json({ error: 'unhandled fixture operation' }, { status: 400 });
  };
}
