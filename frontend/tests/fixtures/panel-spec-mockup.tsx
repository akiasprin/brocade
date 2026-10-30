// Mockup: 单个面板的规范样式，以机器详情「运行状态」为例。Synthetic data only; never imported by the app.
// 面板单独铺在台面上，不挂整个控制台。Serve frontend/ with Vite (configFile: false, plugins: [react()]) and open
//   tests/fixtures/panel-spec-mockup.html?option=a
// Query: option=a|b|c|d|current, scene=issues|calm, width=desktop|phone, theme=dark|light, nav=0 hides the review bar.
import { createRoot } from 'react-dom/client';
import { useLayoutEffect, useState } from 'react';
import { SegmentedControl } from '../../src/ui/bits';
import { runtimeOf, type Scene } from './panel-spec-mockup-data';
import { CurrentPanel, OptionPanel, type Option } from './panel-spec-mockup-ui';
import '../../src/styles.css';
import './panel-spec-mockup.css';

const params = new URLSearchParams(location.search);
const pick = <T extends string>(value: string | null, allowed: readonly T[], fallback: T): T =>
  value !== null && (allowed as readonly string[]).includes(value) ? (value as T) : fallback;

type Choice = Option | 'current';
type Width = 'desktop' | 'phone';
type Theme = 'dark' | 'light';

const OPTIONS: { value: Choice; name: string; note: string }[] = [
  {
    value: 'a',
    name: 'A 分组表单',
    note: '参照 macOS「系统设置」和 iOS「设置」。标题放在卡外，每个数据来源一组白色圆角框，组名和时效写在框上方；框内一行一项，标签在左，读数在右，行间是缩进的发丝线。判定置顶，单独一框。',
  },
  {
    value: 'b',
    name: 'B 规格表',
    note: '参照 apple.com 的技术规格页。一整张无边框白卡，24px 标题；左列是来源名和时效，右侧标签在上、读数在下，读数用 17px；来源之间用通栏发丝线分隔；判定作为脚注放在卡底。四套里留白最多。',
  },
  {
    value: 'c',
    name: 'C 摘要卡',
    note: '参照「健康」的摘要卡。标题放在卡外，每个来源一张卡，卡头是系统色图标和来源名；先给一组大号读数（已运行、核数与容量、发布号），其余字段在下面列成紧凑的两端对齐行；判定置顶。',
  },
  {
    value: 'd',
    name: 'D 检查器',
    note: '参照 Xcode 检查器和 Finder「显示简介」。发丝线外框，13px 标题行；三个来源并排成三栏，栏间是竖向发丝线；标签右对齐，读数左对齐，12px 字号；判定以问题行放在卡底。信息密度最高。',
  },
  { value: 'current', name: '现状', note: '生产当前样式。' },
];

const COMMON = [
  '去掉灰色标题条和大写等宽代号：标题用 SF Pro，来源名写作 Agent / 主机 / 配置，产物名用正常大小写（Xray、WireGuard、Phantun）。',
  '读数改用 SF Pro 的等宽数字；只有构建号这类哈希保留 SF Mono。',
  '颜色只给符号：橙色三角表示需要注意，红色八边形表示故障；判定的整段文字不再着色。',
  '配色用 iOS 系统灰与系统色（配色 mockup 的 iOS 底）；现状仍是生产调色盘。',
];

function Mockup() {
  const [option, setOption] = useState<Choice>(
    pick<Choice>(params.get('option'), ['a', 'b', 'c', 'd', 'current'], 'a'),
  );
  const [scene, setScene] = useState<Scene>(pick<Scene>(params.get('scene'), ['issues', 'calm'], 'issues'));
  const [width, setWidth] = useState<Width>(pick<Width>(params.get('width'), ['desktop', 'phone'], 'desktop'));
  const [theme, setTheme] = useState<Theme>(pick<Theme>(params.get('theme'), ['dark', 'light'], 'dark'));
  const nav = params.get('nav') !== '0';
  useLayoutEffect(() => {
    const root = document.documentElement;
    root.dataset.ps = option;
    root.dataset.theme = theme;
    document.querySelector<HTMLMetaElement>('meta[name="color-scheme"]')!.content = theme;
    const next = new URLSearchParams(location.search);
    next.set('option', option);
    next.set('scene', scene);
    next.set('width', width);
    next.set('theme', theme);
    history.replaceState(history.state, '', `?${next}`);
  }, [option, scene, width, theme]);
  const { sources, findings } = runtimeOf(scene);
  const entry = OPTIONS.find(item => item.value === option)!;
  return (
    <>
      {nav && (
        <div className="mk-bar">
          <div className="mk-bar-row">
            <span className="mk-title">
              面板规范<small>MOCKUP</small>
            </span>
            <div className="mk-controls">
              <label className="mk-ctl">
                <span className="mk-ctl-label">方案</span>
                <SegmentedControl
                  value={option}
                  options={OPTIONS.map(item => ({ value: item.value, label: item.name }))}
                  ariaLabel="方案"
                  onChange={setOption}
                />
              </label>
              <label className="mk-ctl">
                <span className="mk-ctl-label">场景</span>
                <SegmentedControl
                  value={scene}
                  options={[
                    { value: 'issues' as const, label: '有异常' },
                    { value: 'calm' as const, label: '正常' },
                  ]}
                  ariaLabel="场景"
                  onChange={setScene}
                />
              </label>
              <label className="mk-ctl">
                <span className="mk-ctl-label">宽度</span>
                <SegmentedControl
                  value={width}
                  options={[
                    { value: 'desktop' as const, label: '桌面' },
                    { value: 'phone' as const, label: '手机' },
                  ]}
                  ariaLabel="宽度"
                  onChange={setWidth}
                />
              </label>
              <label className="mk-ctl">
                <span className="mk-ctl-label">主题</span>
                <SegmentedControl
                  value={theme}
                  options={[
                    { value: 'dark' as const, label: '深' },
                    { value: 'light' as const, label: '浅' },
                  ]}
                  ariaLabel="主题"
                  onChange={setTheme}
                />
              </label>
            </div>
          </div>
          <p className="mk-scene-note">
            <b>{entry.name}</b>
            {entry.note}
          </p>
          {option !== 'current' && (
            <details className="mk-common">
              <summary>四个方案共同的改动</summary>
              <ol>
                {COMMON.map(item => (
                  <li key={item}>{item}</li>
                ))}
              </ol>
            </details>
          )}
        </div>
      )}
      <main className="ps-stage" data-width={width}>
        <div className="ps-frame">
          {option === 'current' ? (
            <CurrentPanel sources={sources} findings={findings} />
          ) : (
            <OptionPanel option={option} sources={sources} findings={findings} />
          )}
        </div>
      </main>
    </>
  );
}

createRoot(document.getElementById('mk-root')!).render(<Mockup />);
