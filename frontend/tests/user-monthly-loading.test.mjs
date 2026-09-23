import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const source = readFileSync(new URL('../src/panes/users.tsx', import.meta.url), 'utf8');

test('月度统计不阻塞用户页，且只在用量位置投影读取状态', () => {
  const initialLoadingGuard = source.match(
    /if \(\s*users\.isPending[\s\S]*?return <Loading variant="users"[\s\S]*?;/,
  )?.[0];

  assert.ok(initialLoadingGuard, '应保留用户页必要元数据的首屏等待边界');
  assert.doesNotMatch(initialLoadingGuard, /monthly\.isPending/);
  assert.match(source, /usageState=\{monthlyState\}/);
  assert.match(source, /monthlyUsageText\(monthlyState,/);
});
