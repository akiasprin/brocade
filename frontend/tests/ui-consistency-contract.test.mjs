import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import { extname, join, relative } from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('../src/', import.meta.url));
const source = path => readFileSync(new URL(path, import.meta.url), 'utf8');

const sourceFiles = directory =>
  readdirSync(directory, { withFileTypes: true }).flatMap(entry => {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) return sourceFiles(path);
    return ['.ts', '.tsx'].includes(extname(entry.name)) ? [path] : [];
  });

const files = new Map(
  sourceFiles(root).map(path => [relative(root, path).replaceAll('\\', '/'), readFileSync(path, 'utf8')]),
);
const styles = source('../src/styles.css');

test('分段选择、复制和弹层各自只有一个实现边界', () => {
  for (const [file, contents] of files) {
    if (file !== 'ui/bits.tsx') assert.doesNotMatch(contents, /className=["'`]segsw["'`]/, `${file} 重写了分段选择`);
    if (file !== 'ui/platform.ts') assert.doesNotMatch(contents, /navigator\.clipboard/, `${file} 绕过复制适配层`);
    if (!['ui/platform.ts', 'ui/copy-button.tsx'].includes(file)) {
      assert.doesNotMatch(contents, /\bcopyText\s*\(/, `${file} 绕过 CopyButton`);
    }
    assert.doesNotMatch(
      contents,
      /(?:confirm-mask|tunnel-dialog-wrap|external-outbound-wrap|external-outbound-scrim)/,
      `${file} 恢复了页面私有弹层外壳`,
    );
  }

  assert.match(files.get('ui/dialog.tsx'), /role="dialog"[\s\S]*aria-modal="true"/);
  assert.match(files.get('ui/dialog.tsx'), /event\.key === 'Escape'/);
  assert.match(files.get('ui/dialog.tsx'), /focusableElements\(dialog\)/);
  assert.match(files.get('ui/copy-button.tsx'), /data-copy-state=\{state\}/);
});

test('状态语义不再使用无效修饰符，按钮不保留无效果的 ghost 变体', () => {
  for (const [file, contents] of files) {
    assert.doesNotMatch(contents, /className=["'`][^"'`]*(?:note err|callout red|sub err|hint err)/, file);
    assert.doesNotMatch(contents, /className=["'`][^"'`]*\bbtn ghost\b/, file);
  }
  assert.match(styles, /\.sub\.bad,\s*\.hint\.bad\s*\{[^}]*color:\s*var\(--err\)/s);
  assert.match(styles, /\.callout\.err\s*\{[^}]*var\(--err\)/s);
});

test('用户页桌面按钮沿用全站控件高度', () => {
  for (const selector of [
    'user-roster-head-actions \\.btn',
    'user-dacts \\.btn',
    'user-account-type \\.segsw',
    'grant-probe-actions \\.btn',
    'grant-probe-one',
  ]) {
    assert.match(styles, new RegExp(`\\.${selector}\\s*\\{[^}]*min-height:\\s*var\\(--ctl-h\\)`, 's'));
  }
});

test('历史样式骨架已删除，关键组件不再靠后置重复规则改写', () => {
  assert.doesNotMatch(styles, /\.b-(?:split|col)(?:\b|[. >])/);
  assert.doesNotMatch(styles, /^\.seg\s*\{/m);

  for (const selector of ['cfg-grp', 'cfg-owner', 'cfg-leaf', 'cfg-bar', 'cfg-fmt', 'sh', 'sh h4', 'certgrp']) {
    const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, '\\$&').replaceAll(' ', '\\s+');
    assert.equal(
      [...styles.matchAll(new RegExp(`^\\.${escaped}\\s*\\{`, 'gm'))].length,
      1,
      `.${selector} 应只有一份基础规则`,
    );
  }
});

test('容量单位与密码约束使用共享来源', () => {
  assert.match(files.get('panes/subscription.tsx'), /import \{ bytes \} from '\.\.\/ui\/format'/);
  assert.match(files.get('topo/canvas.tsx'), /const fmtBytes = bytes/);
  assert.match(files.get('forge/diff.ts'), /return fileBytes\(n\)/);
  assert.match(files.get('ui/password-policy.ts'), /export const PASSWORD_MIN_LENGTH = 8/);

  for (const [file, contents] of files) {
    if (file === 'ui/password-policy.ts') continue;
    assert.doesNotMatch(contents, /(?:PASSWORD_MIN_LENGTH|MIN_PASSWORD_LENGTH)\s*=\s*\d+/, `${file} 重复定义密码长度`);
  }
});

test('写操作不渲染额外的生效提示组件', () => {
  assert.equal(files.has('ui/change-effect.tsx'), false);
  assert.doesNotMatch(styles, /\.change-effect\b/);
  for (const [file, contents] of files) {
    assert.doesNotMatch(contents, /\bChangeEffect\b/, `${file} 仍引用生效提示组件`);
  }
});

test('受管实体在页面级文案中统一称为机器', () => {
  assert.match(files.get('panes/nodes.tsx'), /title="纳管机器"/);
  assert.match(files.get('panes/tenants.tsx'), /<th>机器<\/th>/);
  assert.match(files.get('panes/artifact.tsx'), /grp: '机器产物'/);
  assert.match(files.get('preview/provision.tsx'), /title="创建预览机器"/);
});
