import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';
import type { DeploymentTargetDetail } from '../src/api';

window.matchMedia = ((query: string) => ({
  matches: false,
  media: query,
  onchange: null,
  addListener() {},
  removeListener() {},
  addEventListener() {},
  removeEventListener() {},
  dispatchEvent: () => false,
})) as unknown as typeof window.matchMedia;
const { RecordedArtifacts } = await import('../src/panes/deploy');

afterEach(cleanup);

function target(
  structure: Record<string, unknown>,
  before: Record<string, unknown> | null,
  after: Record<string, unknown> | null = null,
): DeploymentTargetDetail {
  return {
    node_id: 'n1',
    status: 'succeeded',
    error: null,
    wave: 0,
    disruptive: true,
    desired_structure: structure,
    observed_before: before,
    observed_after: after,
    verdict: null,
    dispatched_at: null,
  };
}

function show(...targets: DeploymentTargetDetail[]) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  client.setQueryData(['nodes'], { nodes: [{ node_id: 'n1', name: '测试节点' }] });
  return render(
    <QueryClientProvider client={client}>
      <RecordedArtifacts targets={targets} range="R10 → R11" />
    </QueryClientProvider>,
  );
}

const xray = (content: string | undefined, sha256: string) => ({
  xray: content === undefined ? { state: 'present', sha256 } : { state: 'present', sha256, content },
});
const json = (value: unknown) => `${JSON.stringify(value, null, 2)}\n`;
const rules = (generation: string, outbounds: string[]) =>
  json({
    routing: {
      rules: outbounds.map((outboundTag, index) => ({
        ruleTag: `r:${generation}:${String(index).padStart(3, '0')}`,
        outboundTag,
      })),
    },
  });

describe('产物记录', () => {
  it('面板默认展开、机器行默认收起；点文件展开新旧双行号、增删标记与行内改动段，再点收起', () => {
    const view = show(
      target(
        xray(json({ serverName: 'new.example', port: 443 }), 'after'),
        xray(json({ serverName: 'old.example', port: 443 }), 'before'),
      ),
    );
    expect(view.container.querySelector('details')?.open).toBe(true);
    const machine = view.getByRole('button', { name: /测试节点/ });
    const file = view.getByRole('button', { name: /xray\.json/ });
    // 收起的行照样读出增删，差异区不渲染
    expect(machine.getAttribute('aria-expanded')).toBe('false');
    expect(file.getAttribute('aria-pressed')).toBe('false');
    expect(file.textContent).toContain('+1');
    expect(file.textContent).toContain('−1');
    expect(view.container.querySelector('.cga-detail')).toBeNull();
    expect(view.container.querySelector('.cg-meta')?.textContent).toBe('R10 → R11 · 1 台机器 · 1 份文件 +1 −1');

    fireEvent.click(file);
    expect(file.getAttribute('aria-pressed')).toBe('true');
    expect(machine.getAttribute('aria-expanded')).toBe('true');
    const removed = view.container.querySelector('tr.del');
    const added = view.container.querySelector('tr.add');
    expect([...removed!.querySelectorAll('td.ln')].map(cell => cell.textContent)).toEqual(['2', '']);
    expect([...added!.querySelectorAll('td.ln')].map(cell => cell.textContent)).toEqual(['', '2']);
    expect(removed!.querySelector('mark')?.textContent).toBe('old');
    expect(added!.querySelector('mark')?.textContent).toBe('new');

    fireEvent.click(file);
    expect(file.getAttribute('aria-pressed')).toBe('false');
    expect(view.container.querySelector('tr.del')).toBeNull();
    expect(machine.getAttribute('aria-expanded')).toBe('false');

    // 点机器名展开该行的第一份文件
    fireEvent.click(machine);
    expect(file.getAttribute('aria-pressed')).toBe('true');
    expect(view.container.querySelector('tr.del')?.textContent).toContain('old.example');
  });

  it('规则表重算产生的 ruleTag 重新编号默认按未改动处理，可以切换显示', () => {
    const view = show(
      target(
        xray(rules('bbbbbbbb', ['direct', 'proxy-b', 'block']), 'after'),
        xray(rules('aaaaaaaa', ['direct', 'proxy-a', 'block']), 'before'),
      ),
    );
    const file = view.getByRole('button', { name: /xray\.json/ });
    expect(file.textContent).toContain('+1');
    expect(file.textContent).toContain('−1');
    fireEvent.click(file);
    expect(view.getByText(/ruleTag 重新编号 3 处，已折叠/)).toBeTruthy();
    // 改动前后各留三行：第二、三条规则的 ruleTag 在其中，按未改动显示、不带增删标记
    const retagged = [...view.container.querySelectorAll('tr.tag')];
    expect(retagged).toHaveLength(2);
    expect(retagged.every(row => row.querySelector('td.mk')?.textContent === '')).toBe(true);

    fireEvent.click(view.getByRole('button', { name: '显示' }));
    expect(file.textContent).toContain('+4');
    expect(file.textContent).toContain('−4');
    expect(view.container.querySelectorAll('tr.tag')).toHaveLength(0);
    expect(view.container.querySelector('.cg-meta')?.textContent).toContain('1 份文件 +4 −4');
    expect(view.getByRole('button', { name: '折叠' })).toBeTruthy();
  });

  it('远离改动的未改动行收成一行，写明行数与所在位置，点按展开', () => {
    const view = show(
      target(
        xray(rules('aaaaaaaa', ['direct', 'proxy-b', 'block']), 'after'),
        xray(rules('aaaaaaaa', ['direct', 'proxy-a', 'block']), 'before'),
      ),
    );
    fireEvent.click(view.getByRole('button', { name: /xray\.json/ }));
    const head = view.getByRole('button', { name: /未改动 6 行/ });
    expect(head.textContent).toContain('routing › rules[0]');
    expect(view.getByRole('button', { name: /未改动 5 行/ }).textContent).toContain('文件末尾');
    expect(view.container.textContent).not.toContain('"direct"');

    fireEvent.click(head);
    expect(view.queryByRole('button', { name: /未改动 6 行/ })).toBeNull();
    expect(view.container.textContent).toContain('"direct"');
  });

  it('旧原文缺失时写出两边的哈希，不把未知基线当成没有变化', () => {
    const view = show(target(xray('{}', 'after-hash'), xray(undefined, 'before-hash')));
    const file = view.getByRole('button', { name: /xray\.json/ });
    expect(file.textContent).toContain('不可比较');
    fireEvent.click(file);
    expect(view.getByText('before-hash')).toBeTruthy();
    expect(view.getByText('after-hash')).toBeTruthy();
    expect(view.container.querySelectorAll('tr.add')).toHaveLength(0);
    expect(view.container.querySelector('.cg-meta')?.textContent).toBe('R10 → R11 · 1 台机器 · 1 份文件');

    fireEvent.click(view.getByRole('button', { name: '查看本次写入的内容 · 1 行' }));
    expect(view.container.querySelector('.cga-code td.src')?.textContent).toBe('{}');
  });

  it('授权名单与文件差异同一种行，按入站分组；凭据只写「已更换」，不显示 UUID', () => {
    const grants = (clients: { email: string; uuid: string; flow: string | null }[]) => ({
      grants: { state: 'present', inbounds: [{ tag: 'in-a', clients }] },
    });
    const view = show(
      target(
        { actions: ['sync-grants'] },
        grants([
          { email: 'a@example', uuid: 'uuid-a-old', flow: null },
          { email: 'b@example', uuid: 'uuid-b', flow: null },
          { email: 'c@example', uuid: 'uuid-c', flow: 'xtls-rprx-vision' },
        ]),
        grants([
          { email: 'a@example', uuid: 'uuid-a-new', flow: null },
          { email: 'c@example', uuid: 'uuid-c', flow: null },
          { email: 'd@example', uuid: 'uuid-d', flow: null },
        ]),
      ),
    );
    const chip = view.getByRole('button', { name: /授权名单/ });
    expect(chip.textContent).toBe('授权名单+1−1~2');
    fireEvent.click(chip);
    const rows = [...view.container.querySelectorAll('.cga-grants tr')].map(row => row.textContent);
    expect(rows).toEqual([
      'in-a4 项',
      '~a@example凭据已更换',
      '−b@example移除',
      '~c@exampleflow xtls-rprx-vision → 无',
      '+d@example新增',
    ]);
    expect(view.container.textContent).not.toMatch(/uuid-/);
    expect(view.container.querySelector('.cg-meta')?.textContent).toBe('R10 → R11 · 1 台机器 · 授权 4 项');
  });

  it('没有文件内容变化也没有授权变更时写明原因', () => {
    const view = show(target(xray('{}', 'same'), xray('{}', 'same')));
    expect(view.getByText('没有文件内容变化；本次执行的重应用操作见上方动作记录。')).toBeTruthy();
  });
});
