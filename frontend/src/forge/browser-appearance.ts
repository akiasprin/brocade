/** 浏览器外沿与顶栏共用 CSS 底色，不维护第二套明暗 / 调色盘色值。 */
export function observeBrowserAppearance(): () => void {
  const root = document.documentElement;
  const scheme = document.querySelector<HTMLMetaElement>('meta[name="color-scheme"]');
  const color = document.querySelector<HTMLMetaElement>('meta[name="theme-color"]');
  if (!scheme || !color) throw new Error('Browser appearance metadata is missing from index.html');

  const sync = () => {
    scheme.content = root.dataset.theme === 'light' ? 'light' : 'dark';
    // 读取目标令牌而不是 backgroundColor：减弱动态效果的全局规则仍会产生
    // 极短的背景过渡，事件 / 动画帧内的渲染色可能还是上一套颜色。
    color.content = getComputedStyle(root).getPropertyValue('--surface').trim();
  };
  sync();

  // 只观察外观属性：覆盖首次载入、手动切换及跨标签页同步，不订阅业务状态或轮询。
  const observer = new MutationObserver(sync);
  observer.observe(root, { attributes: true, attributeFilter: ['data-theme', 'data-palette'] });
  return () => observer.disconnect();
}
