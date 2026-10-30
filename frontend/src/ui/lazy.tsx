import { Component, lazy as reactLazy, createElement, type ComponentProps, type ReactNode } from 'react';

export const MODULE_ERROR_EVENT = 'brocade:module-error';
export const MODULE_RELOAD_EVENT = 'brocade:module-reload';

/** Keep a failed split module from unmounting the shell, other editors and the reload coordinator. */
export class ModuleBoundary extends Component<{ children: ReactNode }, { failed: boolean }> {
  state = { failed: false };

  static getDerivedStateFromError() {
    return { failed: true };
  }

  componentDidCatch() {
    window.dispatchEvent(new Event(MODULE_ERROR_EVENT));
  }

  render() {
    if (!this.state.failed) return this.props.children;
    return (
      <section className="callout err module-load-error" role="alert">
        <b>这部分界面未能加载</b>
        <p>请检查网络后重新加载。已保存的草稿会保留。</p>
        <button type="button" className="btn" onClick={() => window.dispatchEvent(new Event(MODULE_RELOAD_EVENT))}>
          重新加载界面
        </button>
      </section>
    );
  }
}

export const lazy: typeof reactLazy = load => {
  const Content = reactLazy(load);
  function RecoverableModule(props: ComponentProps<typeof Content>) {
    return <ModuleBoundary>{createElement(Content, props)}</ModuleBoundary>;
  }
  return RecoverableModule as unknown as typeof Content;
};
