import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const login = readFileSync(new URL('../src/ui/login.tsx', import.meta.url), 'utf8');
const styles = readFileSync(new URL('../src/styles.css', import.meta.url), 'utf8');
const rule = selector => {
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  return styles.match(new RegExp(`^${escaped} \\{([^}]*)\\}`, 'm'))?.[1] ?? '';
};

test('登录、初始化与直达登录共用分栏版式', () => {
  // Login 与 DirectLogin 都经由 LoginLayout 渲染，不再使用浮窗 .fw。
  assert.equal((login.match(/<LoginLayout\b/g) ?? []).length, 2);
  assert.doesNotMatch(login, /className="fw|login-fw|fw-kind|fw-title/);
  assert.match(login, /<BrandIcon branding=\{branding\} className="login-mark" \/>/);
  assert.match(login, /<span className="login-word">\{branding\.site_name\}<\/span>/);
  assert.match(login, /<div className="login-tagline">跨境网络小管家<\/div>/);
  assert.match(login, /<svg className="login-routes" viewBox="0 0 440 300" aria-hidden="true">/);
});

test('访客模式排在登录按钮之下，且不会提交登录表单', () => {
  const form = login.slice(
    login.indexOf('export function PasswordLogin'),
    login.indexOf('export function InitializeAdmin'),
  );
  const submit = form.indexOf('className="btn primary login-submit" type="submit"');
  const guest = form.indexOf('className="login-alt" type="button"');
  assert.ok(submit > 0 && guest > submit);
});

test('登录页铺满视口但不注册 fixed 贴顶容器，窄屏上下排列', () => {
  const page = rule('.login-page');
  // 与 #stage、.forge 一致：全屏容器不用 fixed，避免 Safari 按贴顶的 fixed 容器给工具栏取色。
  assert.match(page, /position:\s*absolute;/);
  assert.match(page, /inset:\s*0;/);
  assert.match(page, /overflow:\s*auto;/);
  assert.doesNotMatch(page, /position:\s*fixed/);
  assert.match(
    styles,
    /@media \(max-width: 820px\)\s*\{\s*\.login-page\s*\{[^}]*grid-template-columns:\s*minmax\(0, 1fr\);/,
  );
  assert.match(styles, /\.login-routes,\s*\.login-tagline\s*\{\s*display:\s*none;/);
  assert.match(styles, /\.login-panel\s*\{\s*animation:\s*motion-login-in 360ms/);
  assert.doesNotMatch(styles, /\.login-fw/);
});
