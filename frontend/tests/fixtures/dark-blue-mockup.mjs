// Review-only HTML entry. Each frame mounts the real App against the existing synthetic backend.
// Start with frontend/scripts/visual-preview.mjs, then open dark-blue-mockup.html.
const params = new URLSearchParams(location.search);
const schemes = {
  current: {
    label: 'A · 原有黛蓝',
    title: '原有配色 · 对照',
    note: '保留调整前的黛蓝作为对照。重点看主按钮文字、选中导航，以及图表中的蓝色。',
  },
  soft: {
    label: 'B · 柔和黛蓝',
    title: '柔和黛蓝',
    note: '链接蓝降低饱和度，图表蓝与按钮文字降低亮度；保留清楚的蓝色识别。',
  },
  mist: {
    label: 'C · 灰雾蓝',
    title: '灰雾蓝 · 更克制',
    note: '进一步收低蓝色的饱和度，交互和曲线更接近灰蓝。明暗主题均已采用此方案。',
  },
};
const routes = {
  nodes: ['机器列表', '#/nodes'],
  detail: ['机器详情', '#/nodes/node/hk-hkix'],
  user: ['用户详情', '#/users/user/alice'],
};
const palettes = { dailan: '黛蓝', jinzi: '堇紫', songlv: '松绿', oufen: '藕粉', xuanmo: '玄墨' };
const valid = (value, options, fallback) => (Object.hasOwn(options, value) ? value : fallback);
let scheme = valid(params.get('scheme'), schemes, 'mist');
let palette = valid(params.get('palette'), palettes, 'dailan');
let scene = valid(params.get('scene'), routes, 'nodes');
let mode = params.get('mode') === 'compare' ? 'compare' : 'single';
let theme = params.get('theme') === 'light' ? 'light' : 'dark';
document.documentElement.dataset.theme = theme;
document.querySelector('meta[name="color-scheme"]').content = theme;

if (params.get('frame') === '1') {
  document.documentElement.dataset.blue = scheme;
  // Set stores before importing production modules. This fixture runs only on the local preview origin.
  localStorage.setItem('brocade-console:theme', theme);
  localStorage.setItem('brocade-console:palette', palette);
  const { default: RefreshRuntime } = await import('/@react-refresh');
  RefreshRuntime.injectIntoGlobalHook(window);
  window.$RefreshReg$ = () => {};
  window.$RefreshSig$ = () => type => type;
  window.__vite_plugin_react_preamble_installed__ = true;
  await import('../../src/styles.css');
  const [react, dom, query, app, backend, appearance] = await Promise.all([
    import('react'),
    import('react-dom/client'),
    import('@tanstack/react-query'),
    import('../../src/app'),
    import('./palette-mockup-data'),
    import('../../src/forge/browser-appearance'),
  ]);
  backend.installBackend();
  appearance.observeBrowserAppearance();
  const client = new query.QueryClient({
    defaultOptions: { queries: { retry: false, refetchOnWindowFocus: false, refetchOnReconnect: false } },
  });
  dom
    .createRoot(document.getElementById('root'))
    .render(react.createElement(query.QueryClientProvider, { client }, react.createElement(app.App)));
} else {
  document.documentElement.dataset.review = '';
  document.getElementById('root').innerHTML = `
    <main class="blue-review">
      <header class="review-head">
        <div>
          <h1>主题配色检查</h1>
          <p>同一组示例数据，查看五套配色的明暗主题与控件状态。</p>
        </div>
        <span class="review-label">BROCADE / COLOR STUDY</span>
      </header>
      <div class="review-controls">
        <label class="review-palette">调色盘 <select aria-label="调色盘">${Object.entries(palettes)
          .map(([key, label]) => `<option value="${key}">${label}</option>`)
          .join('')}</select></label>
        <div class="review-options" id="review-schemes" role="group" aria-label="蓝色方案">
          ${Object.entries(schemes)
            .map(
              ([key, value]) =>
                `<button type="button" data-scheme="${key}" aria-pressed="false">${value.label}</button>`,
            )
            .join('')}
        </div>
        <div class="review-options" role="group" aria-label="明暗主题">
          <button type="button" data-theme-choice="dark" aria-pressed="false">暗色</button>
          <button type="button" data-theme-choice="light" aria-pressed="false">亮色</button>
        </div>
        <div class="review-options" id="review-modes" role="group" aria-label="预览方式">
          <button type="button" data-mode="single" aria-pressed="false">完整预览</button>
          <button type="button" data-mode="compare" aria-pressed="false">并排对照</button>
        </div>
        <label class="review-page">查看页面 <select aria-label="查看页面">${Object.entries(routes)
          .map(([key, [label]]) => `<option value="${key}">${label}</option>`)
          .join('')}</select></label>
      </div>
      <section class="review-notes" aria-label="方案说明">
        <div aria-live="polite"><strong id="scheme-title"></strong><p id="scheme-note"></p></div>
        <div class="review-swatches" aria-label="实际预览色值"></div>
      </section>
      <section class="review-contrast" aria-label="颜色对比度检查"></section>
      <div class="review-stage"></div>
      <footer class="review-foot">本地 mockup · 示例数据 · 底色、布局和状态色沿用当前产品。可在预览中点击导航、按钮查看悬停与选中效果。</footer>
    </main>`;

  function frameFor(key) {
    const pane = document.createElement('section');
    pane.className = 'review-pane';
    const header = document.createElement('header');
    const label = document.createElement('span');
    label.textContent = palette === 'dailan' ? schemes[key].label : `${palettes[palette]} · 正式主题`;
    const pageLabel = document.createElement('span');
    pageLabel.textContent = routes[scene][0];
    header.append(label, pageLabel);
    const frame = document.createElement('iframe');
    frame.title = label.textContent;
    frame.dataset.scheme = key;
    const url = new URL(location.href);
    url.search = new URLSearchParams({ frame: '1', scheme: key, theme, palette });
    url.hash = routes[scene][1];
    frame.src = url.href;
    frame.addEventListener('load', () => {
      // Read the actual frame colors so light and dark swatches match what is rendered.
      const root = frame.contentDocument.documentElement;
      const observer = new MutationObserver(() => readSwatches(frame, key, observer));
      observer.observe(root, { attributes: true, attributeFilter: ['data-palette'] });
      readSwatches(frame, key, observer);
    });
    pane.append(header, frame);
    return pane;
  }

  function readSwatches(frame, key, observer) {
    if (key !== scheme) return;
    const style = getComputedStyle(frame.contentDocument.documentElement);
    const values = ['--action', '--data', '--primary-on'].map(token => style.getPropertyValue(token).trim());
    // Candidate overrides can exist before the imported production stylesheet has loaded.
    if (values.some(value => !value) || !style.getPropertyValue('--sheet').trim()) return;
    observer.disconnect();
    const labels = ['链接 / 焦点', '图表数据', '主按钮文字'];
    const swatches = document.querySelector('.review-swatches');
    swatches.replaceChildren(
      ...values.map((value, index) => {
        const item = document.createElement('div');
        item.className = 'review-swatch';
        item.style.setProperty('--chip', value);
        const dot = document.createElement('i');
        const label = document.createElement('span');
        label.textContent = labels[index];
        const code = document.createElement('code');
        code.textContent = value;
        item.append(dot, label, code);
        return item;
      }),
    );
    const luminance = token => {
      const hex = style.getPropertyValue(token).trim();
      if (!/^#[0-9a-f]{6}$/i.test(hex)) throw new Error(`Expected a direct color for ${token}: ${hex}`);
      return hex
        .slice(1)
        .match(/../g)
        .map(channel => Number.parseInt(channel, 16) / 255)
        .map(channel => (channel <= 0.04045 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4))
        .reduce((sum, channel, index) => sum + channel * [0.2126, 0.7152, 0.0722][index], 0);
    };
    const checks = [
      ['链接 / 纸面', '--action', '--sheet'],
      ['链接 / 卡片', '--action', '--block'],
      ['主按钮悬停', '--primary-on', '--primary-hover'],
      ['主按钮按下', '--primary-on', '--primary-active'],
      ['实色控件选中', '--action-on', '--action'],
      ['主数据 / 卡片', '--data', '--block'],
    ];
    document.querySelector('.review-contrast').replaceChildren(
      ...checks.map(([label, foreground, background]) => {
        const [high, low] = [luminance(foreground), luminance(background)].sort((a, b) => b - a);
        const ratio = (high + 0.05) / (low + 0.05);
        const item = document.createElement('div');
        item.className = 'review-ratio';
        item.dataset.pass = String(ratio >= 4.5);
        const name = document.createElement('span');
        name.textContent = label;
        const value = document.createElement('b');
        value.textContent = `${ratio.toFixed(2)}${ratio < 4.5 ? ' · 低于 AA' : ''}`;
        item.append(name, value);
        return item;
      }),
    );
  }

  function render() {
    document.documentElement.dataset.theme = theme;
    document.querySelector('meta[name="color-scheme"]').content = theme;
    document.querySelector('meta[name="theme-color"]').content = theme === 'light' ? '#f4f5f7' : '#111317';
    document.querySelectorAll('[data-scheme]').forEach(button => {
      if (button.tagName === 'BUTTON') button.setAttribute('aria-pressed', String(button.dataset.scheme === scheme));
    });
    document
      .querySelectorAll('button[data-mode]')
      .forEach(button => button.setAttribute('aria-pressed', String(button.dataset.mode === mode)));
    document
      .querySelectorAll('button[data-theme-choice]')
      .forEach(button => button.setAttribute('aria-pressed', String(button.dataset.themeChoice === theme)));
    document.querySelector('[aria-label="查看页面"]').value = scene;
    document.querySelector('[aria-label="调色盘"]').value = palette;
    document.getElementById('review-schemes').hidden = palette !== 'dailan';
    document.getElementById('review-modes').hidden = palette !== 'dailan';
    const themeLabel = theme === 'light' ? '亮色' : '暗色';
    document.getElementById('scheme-title').textContent =
      `${schemes[scheme].title} · ${themeLabel}${scheme === 'mist' ? '已落地' : ''}`;
    document.getElementById('scheme-note').textContent =
      theme === 'light'
        ? scheme === 'current'
          ? '保留调整前的亮色黛蓝作为对照。底色和布局沿用正式主题。'
          : scheme === 'soft'
            ? '降低亮色界面中链接、实底按钮与数据曲线的蓝色饱和度，保留适量蓝感。'
            : '亮色使用更中性的灰蓝，按钮、链接和曲线同族；保留对白底的文字对比度。'
        : schemes[scheme].note;
    if (palette !== 'dailan') {
      document.getElementById('scheme-title').textContent = `${palettes[palette]} · ${themeLabel}正式主题`;
      document.getElementById('scheme-note').textContent =
        '检查原有配色的真实页面。下方显示原色对比度，低于 4.5 的文字组合标出供审查。';
    }
    document.querySelector('.review-swatches').replaceChildren();
    document.querySelector('.review-contrast').replaceChildren();
    const stage = document.querySelector('.review-stage');
    const comparing = palette === 'dailan' && mode === 'compare' && scheme !== 'current';
    stage.dataset.mode = comparing ? 'compare' : 'single';
    stage.replaceChildren(...(comparing ? ['current', scheme] : [scheme]).map(frameFor));
    const next = new URL(location.href);
    next.search = new URLSearchParams({ scheme, mode, scene, theme, palette });
    history.replaceState(null, '', next);
  }

  document.querySelectorAll('button[data-scheme]').forEach(button =>
    button.addEventListener('click', () => {
      scheme = button.dataset.scheme;
      render();
    }),
  );
  document.querySelectorAll('button[data-mode]').forEach(button =>
    button.addEventListener('click', () => {
      mode = button.dataset.mode;
      render();
    }),
  );
  document.querySelectorAll('button[data-theme-choice]').forEach(button =>
    button.addEventListener('click', () => {
      theme = button.dataset.themeChoice;
      render();
    }),
  );
  document.querySelector('[aria-label="查看页面"]').addEventListener('change', event => {
    scene = event.target.value;
    render();
  });
  document.querySelector('[aria-label="调色盘"]').addEventListener('change', event => {
    palette = event.target.value;
    render();
  });
  render();
}
