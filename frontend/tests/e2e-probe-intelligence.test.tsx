import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';
import type { E2eProbeItem } from '../src/api';
import { ProbeBanner } from '../src/ui/probe';

const probe = (intelligence: E2eProbeItem['exit_intelligence']): E2eProbeItem => ({
  app_id: 'app-main',
  chain_id: 'chain-jp',
  chain_name: '日本出口',
  node_id: 'hk-edge',
  status: 'ok',
  ttfb_ms: 98,
  exit_ip: '219.104.18.20',
  exit_loc: 'JP',
  exit_intelligence: intelligence,
  exit_verdict: 'unknown',
  detail: null,
  probed_at: '2026-09-16T05:15:53Z',
  samples: { probed_at_unix_secs: [], status: [], ttfb_ms: [] },
});

afterEach(cleanup);

describe('E2E probe exit IP intelligence', () => {
  it('shows the provider-attributed scores and compact network identity from the shared record', () => {
    render(
      <ProbeBanner
        item={probe({
          country_code: 'JP',
          scores: [
            { provider: 'proxycheck', score: 0, country_code: 'JP' },
            { provider: 'ffraud', score: 0, country_code: 'JP' },
            { provider: 'iplogs', score: 0, country_code: 'JP' },
          ],
          networks: [
            { provider: 'proxycheck', isp: 'Sony Network Communications Inc.', network_type: 'business' },
            { provider: 'ffraud', isp: 'Sony Network Communications Inc.', network_type: 'residential' },
            { provider: 'iplogs', isp: 'Sony Network Communications Inc.', network_type: 'unknown' },
          ],
          verified_at: '2026-09-16T05:01:27Z',
        })}
      />,
    );

    expect(screen.getByText('PC 0 · FF 0 · IL 0')).toBeTruthy();
    expect(screen.getByText('Sony Network Communications Inc. · 商宽 / 家宽')).toBeTruthy();
    expect(screen.getByLabelText('IP 情报').getAttribute('title')).toContain('ProxyCheck 0 / 100');
  });

  it('keeps a compact empty state when the exit IP has no verified intelligence', () => {
    render(<ProbeBanner item={probe(null)} />);
    expect(screen.getByLabelText('IP 情报').textContent).toBe('IP 情报暂无记录');
  });
});
