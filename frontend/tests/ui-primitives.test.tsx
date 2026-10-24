import { act, cleanup, fireEvent, render } from '@testing-library/react';
import { useRef, useState } from 'react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { SegmentedControl } from '../src/ui/bits';
import { CopyButton } from '../src/ui/copy-button';
import { DialogClose, DialogLayer } from '../src/ui/dialog';
import { bytes, fileBytes } from '../src/ui/format';
import { Icon } from '../src/ui/icons';

const clipboardDescriptor = Object.getOwnPropertyDescriptor(navigator, 'clipboard');

beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal(
    'matchMedia',
    vi.fn((query: string) => ({
      matches: false,
      media: query,
      onchange: null,
      addListener() {},
      removeListener() {},
      addEventListener() {},
      removeEventListener() {},
      dispatchEvent: () => true,
    })),
  );
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.unstubAllGlobals();
  if (clipboardDescriptor) Object.defineProperty(navigator, 'clipboard', clipboardDescriptor);
  else Reflect.deleteProperty(navigator, 'clipboard');
  document.body.style.overflow = '';
});

it('共享分段选择保留选中、禁用和可访问名称', () => {
  const onChange = vi.fn();
  const view = render(
    <SegmentedControl
      value="global"
      ariaLabel="参数来源"
      options={[
        { value: 'global', label: '跟随全局' },
        { value: 'custom', label: '单独配置' },
      ]}
      onChange={onChange}
    />,
  );

  const group = view.getByRole('group', { name: '参数来源' });
  expect(view.getByRole('button', { name: '跟随全局' }).getAttribute('aria-pressed')).toBe('true');
  fireEvent.click(view.getByRole('button', { name: '单独配置' }));
  expect(onChange).toHaveBeenCalledWith('custom');
  expect(group.classList.contains('segsw')).toBe(true);
});

it('共享复制按钮明确反馈成功并自动复位', async () => {
  const writeText = vi.fn().mockResolvedValue(undefined);
  Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText } });
  const view = render(<CopyButton text="brocade" />);
  const button = view.getByRole('button', { name: '复制' });

  await act(async () => fireEvent.click(button));
  expect(writeText).toHaveBeenCalledWith('brocade');
  expect(button.dataset.copyState).toBe('done');
  expect(button.textContent).toBe('已复制');

  act(() => vi.advanceTimersByTime(1_600));
  expect(button.dataset.copyState).toBe('idle');
  expect(button.textContent).toBe('复制');
});

it('图标复制按钮用可访问名称反馈 UUID 复制结果', async () => {
  const writeText = vi.fn().mockResolvedValue(undefined);
  Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText } });
  const view = render(
    <CopyButton text="user-uuid" label="复制 UUID" successLabel="UUID 已复制" failureLabel="UUID 复制失败" iconOnly />,
  );
  const button = view.getByRole('button', { name: '复制 UUID' });

  expect(button.querySelector('svg')).toBeTruthy();
  await act(async () => fireEvent.click(button));
  expect(writeText).toHaveBeenCalledWith('user-uuid');
  expect(button.dataset.copyState).toBe('done');
  expect(button.getAttribute('aria-label')).toBe('UUID 已复制');
});

it('共享图标保留调用方类名并统一使用无基线偏移的容器', () => {
  const view = render(<Icon of="copy" size={13} className="caller-icon" />);
  const wrapper = view.container.firstElementChild;

  expect(wrapper?.classList.contains('ui-icon')).toBe(true);
  expect(wrapper?.classList.contains('caller-icon')).toBe(true);
  expect(wrapper?.getAttribute('aria-hidden')).toBe('true');
  expect(wrapper?.querySelector('svg')?.getAttribute('width')).toBe('13');
});

it('复制按钮的自定义内容随复制状态更新，可访问名称保持调用方给定的值', async () => {
  const writeText = vi.fn().mockResolvedValue(undefined);
  Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText } });
  const view = render(
    <CopyButton text="vless://link" aria-label="复制 东京 IIJ（IPv4）">
      {(state, label) => (
        <>
          <span className="endpoint">203.0.113.21:443</span>
          <span className="state">{`${state}:${label}`}</span>
        </>
      )}
    </CopyButton>,
  );
  const button = view.getByRole('button', { name: '复制 东京 IIJ（IPv4）' });
  expect(button.querySelector('.state')?.textContent).toBe('idle:复制');

  await act(async () => fireEvent.click(button));
  expect(writeText).toHaveBeenCalledWith('vless://link');
  expect(button.querySelector('.endpoint')?.textContent).toBe('203.0.113.21:443');
  expect(button.querySelector('.state')?.textContent).toBe('done:已复制');
  expect(button.getAttribute('aria-label')).toBe('复制 东京 IIJ（IPv4）');

  act(() => vi.advanceTimersByTime(1_600));
  expect(button.querySelector('.state')?.textContent).toBe('idle:复制');
});

function DialogHarness() {
  const [open, setOpen] = useState(false);
  return (
    <>
      <button type="button" onClick={() => setOpen(true)}>
        打开弹层
      </button>
      {open && (
        <DialogLayer label="共享弹层" onClose={() => setOpen(false)}>
          <section className="dialog-surface">
            <button type="button" autoFocus>
              主要操作
            </button>
            <DialogClose>关闭</DialogClose>
          </section>
        </DialogLayer>
      )}
    </>
  );
}

function ChainedDialogHarness() {
  const [stage, setStage] = useState<'choice' | 'editor' | null>(null);
  return (
    <>
      <button type="button" onClick={() => setStage('choice')}>
        新建隧道
      </button>
      {stage === 'choice' && (
        <DialogLayer label="选择类型" onClose={() => setStage(null)}>
          <section className="dialog-surface">
            <button type="button" autoFocus onClick={() => setStage('editor')}>
              选择 WARP
            </button>
          </section>
        </DialogLayer>
      )}
      {stage === 'editor' && (
        <DialogLayer label="编辑 WARP" mode="drawer" onClose={() => setStage(null)}>
          <section className="dialog-surface dialog-drawer">
            <DialogClose autoFocus>关闭编辑</DialogClose>
          </section>
        </DialogLayer>
      )}
    </>
  );
}

function GuardedDialogHarness({ canClose }: { canClose: () => boolean }) {
  const [open, setOpen] = useState(true);
  return open ? (
    <DialogLayer label="有未保存修改" onClose={() => setOpen(false)} canClose={canClose}>
      <section className="dialog-surface">
        <DialogClose>取消编辑</DialogClose>
      </section>
    </DialogLayer>
  ) : null;
}

it('共享弹层锁定滚动、处理 Escape、播放退出并恢复焦点', async () => {
  const view = render(<DialogHarness />);
  const opener = view.getByRole('button', { name: '打开弹层' });
  opener.focus();
  fireEvent.click(opener);

  expect(document.body.style.overflow).toBe('hidden');
  expect(document.activeElement).toBe(view.getByRole('button', { name: '主要操作' }));
  fireEvent.keyDown(document, { key: 'Escape' });
  expect(document.body.querySelector('.dialog-layer')?.getAttribute('data-motion-state')).toBe('exiting');
  expect(view.queryByRole('dialog')).toBeNull();

  await act(async () => Promise.resolve());
  act(() => vi.advanceTimersByTime(260));
  await act(async () => Promise.resolve());
  expect(document.body.querySelector('.dialog-layer')).toBeNull();
  expect(document.body.style.overflow).toBe('');
  expect(document.activeElement).toBe(opener);
});

it('弹层切换为抽屉时保留最初触发按钮作为焦点回退点', async () => {
  const view = render(<ChainedDialogHarness />);
  const opener = view.getByRole('button', { name: '新建隧道' });
  opener.focus();
  fireEvent.click(opener);
  fireEvent.click(view.getByRole('button', { name: '选择 WARP' }));
  await act(async () => Promise.resolve());

  expect(document.activeElement).toBe(view.getByRole('button', { name: '关闭编辑' }));
  expect(document.body.style.overflow).toBe('hidden');
  fireEvent.click(view.getByRole('button', { name: '关闭编辑' }));
  await act(async () => Promise.resolve());
  act(() => vi.advanceTimersByTime(300));
  await act(async () => Promise.resolve());

  expect(document.body.querySelector('.dialog-layer')).toBeNull();
  expect(document.activeElement).toBe(opener);
});

it('弹层在退出动画前检查未保存保护，拒绝时保持可交互', async () => {
  const canClose = vi.fn(() => false);
  const view = render(<GuardedDialogHarness canClose={canClose} />);
  const closeButton = view.getByRole('button', { name: '取消编辑' });

  fireEvent.click(closeButton);
  expect(canClose).toHaveBeenCalledOnce();
  expect(view.getByRole('dialog')).toBeTruthy();
  expect(document.body.querySelector('.dialog-layer')?.getAttribute('data-motion-state')).not.toBe('exiting');

  canClose.mockReturnValue(true);
  fireEvent.click(closeButton);
  fireEvent.click(closeButton);
  expect(canClose).toHaveBeenCalledTimes(2);
  expect(document.body.querySelector('.dialog-layer')?.getAttribute('data-motion-state')).toBe('exiting');
  await act(async () => Promise.resolve());
  act(() => vi.advanceTimersByTime(260));
  await act(async () => Promise.resolve());
  expect(document.body.querySelector('.dialog-layer')).toBeNull();
});

function PopoverHarness({ align }: { align?: 'start' | 'end' }) {
  const anchor = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  return (
    <>
      <button ref={anchor} type="button" onClick={() => setOpen(true)}>
        账期
      </button>
      {open && (
        <DialogLayer mode="popover" anchor={anchor} align={align} label="贴边弹层" onClose={() => setOpen(false)}>
          <section className="dialog-surface">
            <input aria-label="重置日" />
            <DialogClose>取消</DialogClose>
          </section>
        </DialogLayer>
      )}
    </>
  );
}

it('贴边弹层优先放在锚点右侧，放不下时换到下方或上方，并沿用弹层的焦点与关闭', async () => {
  // jsdom 没有布局：给面板一个尺寸、给锚点一个位置。
  vi.spyOn(HTMLElement.prototype, 'offsetWidth', 'get').mockReturnValue(380);
  vi.spyOn(HTMLElement.prototype, 'offsetHeight', 'get').mockReturnValue(300);
  vi.stubGlobal('innerWidth', 1200);
  vi.stubGlobal('innerHeight', 800);
  const view = render(<PopoverHarness />);
  const opener = view.getByRole('button', { name: '账期' });
  let box = new DOMRect(200, 600, 236, 46);
  opener.getBoundingClientRect = () => box;
  opener.focus();
  fireEvent.click(opener);

  const layer = document.body.querySelector<HTMLElement>('.dialog-layer.popover')!;
  const place = () => [layer.style.getPropertyValue('--popover-left'), layer.style.getPropertyValue('--popover-top')];
  // 右侧放得下：贴右边，顶边上移到面板底不出视口。
  expect(place()).toEqual(['444px', '492px']);
  expect(document.body.style.overflow).toBe('hidden');
  expect(document.activeElement).toBe(view.getByRole('textbox', { name: '重置日' }));

  // 窄视口：右侧放不下，下方也放不下，放在上方并与锚点左对齐。
  vi.stubGlobal('innerWidth', 600);
  act(() => void window.dispatchEvent(new Event('resize')));
  expect(place()).toEqual(['200px', '292px']);
  // 锚点靠上时放在下方；左边不越过视口右侧留白。
  box = new DOMRect(300, 80, 236, 46);
  act(() => void window.dispatchEvent(new Event('resize')));
  expect(place()).toEqual(['212px', '134px']);

  fireEvent.keyDown(document, { key: 'Escape' });
  await act(async () => Promise.resolve());
  act(() => vi.advanceTimersByTime(180));
  await act(async () => Promise.resolve());
  expect(document.body.querySelector('.dialog-layer')).toBeNull();
  expect(document.activeElement).toBe(opener);
});

it('行尾的弹层放在锚点下方并与它右缘对齐，下方放不下时换到上方', () => {
  vi.spyOn(HTMLElement.prototype, 'offsetWidth', 'get').mockReturnValue(300);
  vi.spyOn(HTMLElement.prototype, 'offsetHeight', 'get').mockReturnValue(200);
  vi.stubGlobal('innerWidth', 1200);
  vi.stubGlobal('innerHeight', 800);
  const view = render(<PopoverHarness align="end" />);
  const opener = view.getByRole('button', { name: '账期' });
  // 右侧本来放得下，也不放到右侧：面板右缘对齐锚点右缘（1100 - 300）。
  let box = new DOMRect(1000, 100, 100, 24);
  opener.getBoundingClientRect = () => box;
  fireEvent.click(opener);
  const layer = document.body.querySelector<HTMLElement>('.dialog-layer.popover')!;
  const place = () => [layer.style.getPropertyValue('--popover-left'), layer.style.getPropertyValue('--popover-top')];
  expect(place()).toEqual(['800px', '132px']);
  // 锚点靠下：换到上方；锚点靠左时左缘不越过视口留白。
  box = new DOMRect(40, 700, 100, 24);
  act(() => void window.dispatchEvent(new Event('resize')));
  expect(place()).toEqual(['8px', '492px']);
});

it('流量和文件大小共用 IEC 单位名称', () => {
  expect(bytes(0)).toBe('0 B');
  expect(bytes(1024)).toBe('1 KiB');
  expect(bytes(1024 ** 2)).toBe('1.0 MiB');
  expect(fileBytes(1536)).toBe('1.5 KiB');
  expect(fileBytes(null)).toBe('—');
});
