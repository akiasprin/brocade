import { STUDIES, REFERENCE_TOKENS, candidateTokens, contrast, mix } from './palette-candidate-colors.mjs';

const query = new URLSearchParams(location.search);
const valid = (value, options, fallback) => (Object.hasOwn(options, value) ? value : fallback);
let palette = valid(query.get('palette'), STUDIES, 'jinzi');
let candidate = valid(
  query.get('candidate'),
  { current: true, ...STUDIES[palette].variants },
  STUDIES[palette].recommended,
);
let theme = query.get('theme') === 'light' ? 'light' : 'dark';
let mode = query.get('mode') === 'full' ? 'full' : 'compare';
if (palette === 'dailan') mode = 'full';
const scenes = {
  nodes: ['机器列表', '#/nodes'],
  detail: ['机器详情', '#/nodes/node/hk-hkix'],
  user: ['用户详情', '#/users/user/alice'],
};
let scene = valid(query.get('scene'), scenes, 'nodes');
let metricsOpen = query.get('metrics') === '1';
const root = document.documentElement;
root.dataset.theme = theme;
root.dataset.palette = palette;
document.querySelector('meta[name="color-scheme"]').content = theme;
const nameOf = key =>
  key === 'blue-reference' ? '黛蓝现况 · 固定' : key === 'current' ? '本色现况' : STUDIES[palette].variants[key].name;
const keysFor = () =>
  palette === 'dailan' ? ['current'] : ['blue-reference', ...Object.keys(STUDIES[palette].variants)];
const chromeRoles = {
  '--study-bg': '--ground',
  '--study-surface': '--surface',
  '--study-line': '--line',
  '--study-ink': '--ink',
  '--study-muted': '--ink-3',
  '--study-selected': '--band',
};

if (query.get('frame')) {
  const sample = query.get('frame') === 'sample';
  if (sample) root.dataset.sample = '';
  root.dataset.palette = 'dailan';
  await import('../../src/styles.css');
  const css = getComputedStyle(root);
  const reference = Object.fromEntries(REFERENCE_TOKENS.map(key => [key, css.getPropertyValue(key).trim()]));
  root.dataset.palette = palette;
  for (const [name, value] of Object.entries(candidateTokens(palette, candidate, theme, reference)))
    root.style.setProperty(name, value);
  root.dataset.candidate = candidate;
  const resolved = name => getComputedStyle(root).getPropertyValue(name).trim();
  const ratio = (a, b) => contrast(resolved(a), resolved(b));
  const metrics = {
    body: ratio('--ink', '--card'),
    action: ratio('--action', '--card'),
    button: ratio('--primary-on', '--primary'),
    hover: ratio('--primary-on', '--primary-hover'),
    active: ratio('--primary-on', '--primary-active'),
    line: contrast(mix(resolved('--card'), resolved('--data'), 0.72), resolved('--card')),
    actionColor: resolved('--action'),
    dataColor: resolved('--data'),
    paper: resolved('--card'),
  };
  if (sample) {
    document.getElementById('root').innerHTML = `
      <main class="color-specimen">
        <header class="spec-top"><b>Brocade</b><button class="fg-nv" aria-current="true">机器</button><button class="fg-nv">线路</button><button class="fg-nv">用户</button></header>
        <div class="spec-paper">
          <div class="spec-title"><span>机器观测</span><span>近 1 小时</span></div>
          <section class="spec-card">
            <div class="spec-identity"><b>香港 HKIX</b><span class="spec-status">● 正常</span></div>
            <p class="spec-number">182.4 <span>Mbit/s</span></p>
            <svg class="spec-plot" viewBox="0 0 300 92" role="img" aria-label="接收与发送速率示例">
              <path d="M0 65H300 M0 37H300" stroke="var(--line-soft)" fill="none" />
              <path d="M0 58L12 54L24 56L36 43L48 46L60 49L72 44L84 48L96 38L108 42L120 35L132 37L144 30L156 33L168 26L180 36L192 31L204 37L216 30L228 35L240 28L252 32L264 24L276 30L288 25L300 28V73H0Z" fill="var(--data-wash)" />
              <path d="M0 58L12 54L24 56L36 43L48 46L60 49L72 44L84 48L96 38L108 42L120 35L132 37L144 30L156 33L168 26L180 36L192 31L204 37L216 30L228 35L240 28L252 32L264 24L276 30L288 25L300 28" stroke="var(--data)" stroke-opacity=".72" stroke-width="1.4" stroke-linecap="round" fill="none" />
              <path d="M0 67L18 64L36 66L54 59L72 61L90 55L108 57L126 52L144 56L162 47L180 51L198 48L216 54L234 46L252 48L270 42L288 46L300 42" stroke="var(--observe-2)" stroke-opacity=".72" stroke-width="1.2" fill="none" />
              <text x="0" y="88">11:00</text><text x="132" y="88">11:30</text><text x="270" y="88">12:00</text>
            </svg>
            <div class="spec-legend"><span><i></i>接收 182.4</span><span><i></i>发送 168.2</span></div>
            <div class="spec-stats"><span>本月用量<b>440.44 GiB</b></span><span>TCP 延迟<b>39 ms</b></span></div>
          </section>
          <p class="spec-copy">机器运行正常，最近一次状态报告在 8 秒前。</p>
          <div class="spec-actions"><button class="btn primary">纳管机器</button><button class="btn">多选</button></div>
          <label class="spec-label">机器名称<input class="f" value="hk-hkix.example" aria-label="机器名称示例" /></label>
          <button class="spec-link" type="button">查看机器详情 →</button>
          <div class="spec-actions" aria-label="主按钮状态"><button class="btn primary">默认</button><button class="btn primary is-hover">悬停</button><button class="btn primary is-active">按下</button></div>
          <div class="spec-bottom"><span class="good">正常</span><span class="warning">待发布</span><span class="error">失败</span></div>
        </div>
      </main>`;
    document
      .querySelector('.spec-link')
      .addEventListener('click', () =>
        parent.postMessage({ type: 'study-open', candidate, scene: 'detail' }, location.origin),
      );
    document.querySelectorAll('.spec-top button').forEach(button =>
      button.addEventListener('click', () => {
        document.querySelectorAll('.spec-top button').forEach(item => item.removeAttribute('aria-current'));
        button.setAttribute('aria-current', 'true');
      }),
    );
  } else {
    // Review tabs must retain their explicit theme and palette independently.
    Object.defineProperty(window, 'localStorage', { value: window.sessionStorage });
    localStorage.setItem('brocade-console:theme', theme);
    localStorage.setItem('brocade-console:palette', palette);
    const { default: RefreshRuntime } = await import('/@react-refresh');
    RefreshRuntime.injectIntoGlobalHook(window);
    window.$RefreshReg$ = () => {};
    window.$RefreshSig$ = () => type => type;
    window.__vite_plugin_react_preamble_installed__ = true;
    const [react, dom, queryClient, app, backend, appearance] = await Promise.all([
      import('react'),
      import('react-dom/client'),
      import('@tanstack/react-query'),
      import('../../src/app'),
      import('./palette-mockup-data'),
      import('../../src/forge/browser-appearance'),
    ]);
    backend.installBackend();
    appearance.observeBrowserAppearance();
    const client = new queryClient.QueryClient({
      defaultOptions: { queries: { retry: false, refetchOnWindowFocus: false, refetchOnReconnect: false } },
    });
    dom
      .createRoot(document.getElementById('root'))
      .render(react.createElement(queryClient.QueryClientProvider, { client }, react.createElement(app.App)));
  }
  root.dataset.ready = 'true';
  const chrome = Object.fromEntries(Object.values(chromeRoles).map(key => [key, reference[key]]));
  parent.postMessage({ type: 'study-ready', candidate, metrics, chrome }, location.origin);
} else {
  root.dataset.candidateReview = '';
  const reports = new Map();
  function frameUrl(key, frameMode) {
    const url = new URL(location.href);
    url.search = new URLSearchParams({
      palette: key === 'blue-reference' ? 'dailan' : palette,
      candidate: key === 'blue-reference' ? 'current' : key,
      theme,
      frame: frameMode,
    });
    url.hash = frameMode === 'app' ? scenes[scene][1] : '';
    return url.href;
  }
  function saveQuery() {
    const url = new URL(location.href);
    url.search = new URLSearchParams({
      palette,
      candidate,
      theme,
      mode,
      scene,
      ...(metricsOpen ? { metrics: '1' } : {}),
    });
    history.replaceState(null, '', url);
  }
  function frame(key, frameMode) {
    const element = document.createElement('iframe');
    element.title = `${STUDIES[palette].name} · ${nameOf(key)} · ${theme === 'dark' ? '暗色' : '亮色'}`;
    element.dataset.candidate = key;
    element.dataset.value = key === 'blue-reference' ? 'current' : key;
    element.src = frameUrl(key, frameMode);
    return element;
  }
  function renderMetrics() {
    const list = document.querySelector('.study-metrics dl');
    if (!list) return;
    list.replaceChildren(
      ...keysFor()
        .filter(key => reports.has(key))
        .flatMap(key => {
          const data = reports.get(key);
          const title = document.createElement('dt');
          title.textContent = `${nameOf(key)} · 操作 ${data.actionColor} / 数据 ${data.dataColor} / 卡片 ${data.paper}`;
          const line = document.createElement('dd');
          line.textContent = `正文 ${data.body.toFixed(2)} · 操作文字 ${data.action.toFixed(2)} · 按钮 ${data.button.toFixed(2)} / 悬停 ${data.hover.toFixed(2)} / 按下 ${data.active.toFixed(2)} · 72% 曲线 ${data.line.toFixed(2)}`;
          return [title, line];
        }),
    );
  }
  function render() {
    reports.clear();
    for (const role of Object.keys(chromeRoles)) root.style.removeProperty(role);
    root.dataset.theme = theme;
    document.querySelector('meta[name="color-scheme"]').content = theme;
    const study = STUDIES[palette];
    const recommend = study.variants[study.recommended];
    document.getElementById('root').innerHTML = `
      <main class="candidate-review">
        <header class="study-heading"><div><h1>以当前黛蓝为基准 · 配色候选</h1><p>黛蓝保持现况。其他四套各 5 个候选，共 20 套明暗方案。</p></div><span class="study-kicker">BROCADE / PALETTE STUDIES</span></header>
        <div class="study-controls">
          <label>调色盘 <select aria-label="调色盘">${Object.entries(STUDIES)
            .map(
              ([key, value]) =>
                `<option value="${key}" ${key === palette ? 'selected' : ''}>${value.name}${key === 'dailan' ? ' · 现况固定' : ''}</option>`,
            )
            .join('')}</select></label>
          <div class="study-switch" role="group" aria-label="明暗主题"><button data-theme="dark" aria-pressed="${theme === 'dark'}">暗色</button><button data-theme="light" aria-pressed="${theme === 'light'}">亮色</button></div>
          <div class="study-switch" role="group" aria-label="查看方式"><button data-mode="compare" ${palette === 'dailan' ? 'disabled' : ''} aria-pressed="${mode === 'compare'}">方案对照</button><button data-mode="full" aria-pressed="${mode === 'full'}">完整界面</button></div>
          <label ${mode !== 'full' || palette === 'dailan' ? 'hidden' : ''}>方案 <select aria-label="完整页面方案">${['current', ...Object.keys(study.variants)].map(key => `<option value="${key}" ${key === candidate ? 'selected' : ''}>${nameOf(key)}${key === study.recommended && palette !== 'dailan' ? ' · 推荐' : ''}</option>`).join('')}</select></label>
          <label ${mode !== 'full' ? 'hidden' : ''}>页面 <select aria-label="查看页面">${Object.entries(scenes)
            .map(([key, [name]]) => `<option value="${key}" ${key === scene ? 'selected' : ''}>${name}</option>`)
            .join('')}</select></label>
        </div>
        <p class="study-note"><strong>${palette === 'dailan' ? '黛蓝现况 · 固定参照' : `${study.name} · 我推荐「${recommend.name}」`}</strong>　${recommend?.note ?? '直接显示当前正式黛蓝，不提供蓝色改版。'}<br>底色深浅、正文、状态色、按钮画法和图表明暗关系沿用当前黛蓝，只比较色相、冷暖和灰度。</p>
        <div id="study-stage"></div>
        <details class="study-metrics" ${metricsOpen ? 'open' : ''}><summary>展开色值与对比度</summary><p>所有候选以当前黛蓝同一角色的明暗关系为参照，图表透明度也保持一致。</p><dl></dl></details>
        <footer class="study-footer">组件样张用于并排看色彩关系；完整界面可查看真实页面与示例数据。所有候选仅用于本地评审。</footer>
      </main>`;
    const stage = document.getElementById('study-stage');
    if (mode === 'compare') {
      stage.className = 'study-grid';
      for (const [index, key] of keysFor().entries()) {
        const spec = study.variants[key];
        const card = document.createElement('section');
        card.className = `study-card${key === study.recommended ? ' recommended' : ''}`;
        card.dataset.candidate = key;
        card.innerHTML = `<header class="study-card-head"><h2>${index ? `${index} · ${spec.name}` : '黛蓝现况 · 固定'}${key === study.recommended ? '<small>推荐</small>' : ''}</h2><p>${spec?.note ?? '当前正式黛蓝。底色、文字、按钮与图表均直接读取现况。'}</p></header>`;
        card.append(frame(key, 'sample'));
        const foot = document.createElement('div');
        foot.className = 'study-card-foot';
        foot.innerHTML = `<span>${key === 'blue-reference' ? '固定参照' : '同黛蓝：暗色洗色 · 亮色实底'}</span><button type="button">查看完整界面</button>`;
        foot.querySelector('button').addEventListener('click', () => {
          if (key === 'blue-reference') {
            palette = 'dailan';
            candidate = 'current';
          } else candidate = key;
          mode = 'full';
          render();
        });
        card.append(foot);
        stage.append(card);
      }
    } else {
      stage.className = 'study-full';
      stage.innerHTML = `<header><span>${study.name} / ${nameOf(candidate)} / ${theme === 'dark' ? '暗色' : '亮色'}</span><span>${candidate === 'current' ? '当前正式配色' : study.variants[candidate].note}</span></header>`;
      stage.append(frame(candidate, 'app'));
    }
    document.querySelector('[aria-label="调色盘"]').addEventListener('change', event => {
      const wasBlue = palette === 'dailan';
      palette = event.target.value;
      candidate = STUDIES[palette].recommended;
      if (palette === 'dailan') mode = 'full';
      else if (wasBlue) mode = 'compare';
      render();
    });
    document.querySelectorAll('button[data-theme]').forEach(button =>
      button.addEventListener('click', () => {
        theme = button.dataset.theme;
        render();
      }),
    );
    document.querySelectorAll('button[data-mode]').forEach(button =>
      button.addEventListener('click', () => {
        mode = button.dataset.mode;
        render();
      }),
    );
    document.querySelector('[aria-label="完整页面方案"]').addEventListener('change', event => {
      candidate = event.target.value;
      render();
    });
    document.querySelector('[aria-label="查看页面"]').addEventListener('change', event => {
      scene = event.target.value;
      render();
    });
    document.querySelector('.study-metrics').addEventListener('toggle', event => {
      metricsOpen = event.target.open;
      saveQuery();
    });
    saveQuery();
  }
  window.addEventListener('message', event => {
    if (event.origin !== location.origin) return;
    const source = [...document.querySelectorAll('iframe')].find(frame => frame.contentWindow === event.source);
    if (!source || source.dataset.value !== event.data?.candidate) return;
    if (event.data.type === 'study-ready') {
      reports.set(source.dataset.candidate, event.data.metrics);
      for (const [role, source] of Object.entries(chromeRoles)) root.style.setProperty(role, event.data.chrome[source]);
      renderMetrics();
    }
    if (event.data.type === 'study-open') {
      if (source.dataset.candidate === 'blue-reference') {
        palette = 'dailan';
        candidate = 'current';
      } else candidate = source.dataset.candidate;
      scene = 'detail';
      mode = 'full';
      render();
    }
  });
  render();
}
