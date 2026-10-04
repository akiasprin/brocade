import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { DiagTable } from '../src/ui/diag-table';

const names = {
  node: () => undefined,
  chain: () => undefined,
};

beforeEach(() => localStorage.clear());
afterEach(cleanup);

describe('diagnostic info copy', () => {
  it('describes info diagnostics as configuration details that may be overlooked', () => {
    render(
      <DiagTable
        diagnostics={[
          {
            level: 'info',
            code: 'node.dns-unused',
            location: 'n1',
            message: 'n1 配置了 Xray 内建 DNS 服务器，当前没有可承载其查询流量的落地出站',
          },
        ]}
        names={names}
      />,
    );

    expect(screen.getByText('另有 1 条配置提示，建议核对')).toBeTruthy();
    expect(screen.queryByText(/编译器判断不了/)).toBeNull();

    fireEvent.click(screen.getByRole('button', { name: /^1 提示/ }));

    expect(screen.getByText(/提示 · 请留意以下配置细节/)).toBeTruthy();
    expect(screen.getByText('node.dns-unused')).toBeTruthy();
  });
});
