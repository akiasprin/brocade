import js from '@eslint/js';
import tseslint from 'typescript-eslint';
import reactHooks from 'eslint-plugin-react-hooks';
import reactRefresh from 'eslint-plugin-react-refresh';

export default tseslint.config(
  { ignores: ['dist', 'node_modules'] },
  {
    files: ['**/*.{ts,tsx}'],
    extends: [js.configs.recommended, ...tseslint.configs.recommended],
    plugins: { 'react-hooks': reactHooks, 'react-refresh': reactRefresh },
    rules: {
      ...reactHooks.configs.recommended.rules,

      // no-undef 交给 tsc。ESLint 不看类型，全局符号（document / window / setTimeout）
      // 它一个都不认识，开着只会满屏假警报——而 tsconfig 已经是 strict 了。
      'no-undef': 'off',

      // 下划线开头 = 明确声明「知道它没用到」。tsconfig 的 noUnusedLocals 不认这个约定，
      // 所以这条规则不是重复：它负责那些故意留着的占位参数。
      '@typescript-eslint/no-unused-vars': ['warn', { argsIgnorePattern: '^_', varsIgnorePattern: '^_' }],

      // Vite 热更新的粒度问题，不是正确性问题。这个仓库刻意把小工具函数跟用它的组件放一起，
      // 为了热更新把它们拆开会让相关的东西散到更多文件里——那跟「让人读得懂」是反的。
      'react-refresh/only-export-components': 'off',

      // React Compiler 那套规则（eslint-plugin-react-hooks v6 收进了 recommended）
      // 一律按 error 收：渲染期读写 ref、渲染期读时钟、在 effect 里 setState，
      // 在并发渲染和 StrictMode 双渲染下都会真出问题。
      // 唯一的例外在下面单列，不用 inline disable——那种散在代码里没人会回头看。
    },
  },
  {
    // panes/rules.tsx 是唯一还没清的。那 10 条是一个完整机制不是 10 个独立问题：
    // 子组件渲染期往 handle.current 写状态，父组件渲染期读 handles.current。机制本身
    // 有防护（handles 一变就 bump()，那个 useMemo 的依赖里有 tick），不会算出陈旧结果,
    // 但读写 ref 仍然不是并发安全的写法。正解是 useSyncExternalStore，前置条件是
    // 先有组件级测试——它管的是草稿保存。计划见 CODE_REVIEW.md 第二面。
    files: ['src/panes/rules.tsx'],
    rules: {
      'react-hooks/refs': 'warn',
      'react-hooks/set-state-in-effect': 'warn',
    },
  },
);
