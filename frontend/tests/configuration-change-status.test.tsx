import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import type { ArtifactIndexEntry } from '../src/api';
import { changedConfigurationNodes, useRevisionDiff } from '../src/forge/artifacts';

const entry = (kind: string, sha: string, node = 'edge'): ArtifactIndexEntry => ({
  target_kind: 'node',
  target_id: node,
  artifact_kind: kind,
  state: 'present',
  sha256: sha.repeat(64),
  byte_len: 100,
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

it('ignores grants and new user subscriptions when node configuration files are identical', () => {
  const before = [entry('xray', 'a'), entry('grants', 'b')];
  const after = [entry('xray', 'a'), entry('grants', 'c'), { ...entry('uri', 'd'), target_kind: 'user' }];
  expect([...changedConfigurationNodes(after, before)]).toEqual([]);
});

it.each(['xray', 'wireguard', 'phantun', 'hy2_port_hop'])('detects changes and removal of %s configuration', kind => {
  const before = [entry(kind, 'a')];
  expect([...changedConfigurationNodes([entry(kind, 'b')], before)]).toEqual(['edge']);
  expect([...changedConfigurationNodes([], before)]).toEqual(['edge']);
  expect([...changedConfigurationNodes([{ ...entry(kind, 'a'), state: 'disabled', sha256: null }], before)]).toEqual([
    'edge',
  ]);
});

it('does not treat an absent disabled workload as a configuration update', () => {
  const disabled = { ...entry('xray', 'a'), state: 'disabled', sha256: null };
  expect([...changedConfigurationNodes([disabled], [])]).toEqual([]);
  expect([...changedConfigurationNodes([], [disabled])]).toEqual([]);
});

it('keeps configuration status unknown until both committed indexes are available', async () => {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  client.setQueryData(['artifact-index', 1], { revision: 1, artifacts: [entry('xray', 'a')] });
  let resolve!: (value: Response) => void;
  vi.stubGlobal(
    'fetch',
    vi.fn(
      () =>
        new Promise<Response>(done => {
          resolve = done;
        }),
    ),
  );
  function Status() {
    const diff = useRevisionDiff(2, 1);
    return <output>{diff.configurationNodes == null ? 'unknown' : diff.configurationNodes.size}</output>;
  }
  render(
    <QueryClientProvider client={client}>
      <Status />
    </QueryClientProvider>,
  );
  expect(screen.getByRole('status').textContent).toBe('unknown');
  resolve(Response.json({ revision: 2, artifacts: [entry('xray', 'a'), entry('grants', 'b')] }));
  await waitFor(() => expect(screen.getByRole('status').textContent).toBe('0'));
  client.clear();
});
