import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import type { CvmEntry } from '../../src/api';
import type { NetworkEdits } from '../../src/panes/cvm/network-draft';
import { NetworkEntries } from '../../src/panes/cvm/network-entries';
import { cvmInstance, cvmWorkspace } from '../fixtures/cvm/data';
import { entryBoard } from '../fixtures/cvm/entry-data';

const clients: QueryClient[] = [];
afterEach(() => {
  cleanup();
  clients.splice(0).forEach(client => client.clear());
  vi.unstubAllGlobals();
});

function setup(status: 409 | 503, edits: NetworkEdits = {}) {
  const workspace = cvmWorkspace('detail');
  const instance = workspace.instances[0];
  const eligible = cvmInstance('eligible');
  eligible.desired.spec.name = 'same-owner';
  const stopped = cvmInstance('stopped');
  stopped.desired.spec.name = 'same-owner-stopped';
  stopped.desired.power = 'stopped';
  const otherUser = { ...cvmInstance('wrong-owner'), user_id: 'bob' };
  otherUser.desired.spec.name = 'other-owner';
  const otherTenant = { ...cvmInstance('wrong-tenant'), tenant_id: 'other' };
  otherTenant.desired.spec.name = 'other-tenant';
  const deleted = cvmInstance('deleted');
  deleted.desired.spec.name = 'pending-deletion';
  deleted.desired.power = 'deleted';
  workspace.instances = [instance, eligible, stopped, otherUser, otherTenant, deleted];
  const [entry] = entryBoard(workspace, 'detail').entries;
  let persisted: CvmEntry = entry;
  const requests: { url: string; method: string; body: unknown }[] = [];
  vi.stubGlobal(
    'fetch',
    vi.fn(async (url: string, init?: RequestInit) => {
      if (url === `/cvm/${instance.desired.id}/entries` && !init?.method)
        return Response.json(persisted.instance_id === instance.desired.id ? [persisted] : []);
      if (url !== `/cvm/entries/${entry.id}/target` || init?.method !== 'PUT') throw Error(`unexpected ${url}`);
      requests.push({ url, method: init.method, body: JSON.parse(String(init.body)) });
      if (requests.length === 1) {
        persisted = { ...entry, instance_id: status === 503 ? eligible.desired.id : instance.desired.id, revision: 2 };
        return Response.json({ error: status === 503 ? '保存响应丢失' : '入口已变化，请刷新后操作' }, { status });
      }
      return Response.json(persisted);
    }),
  );
  const client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  clients.push(client);
  const onChange = vi.fn();
  render(
    <QueryClientProvider client={client}>
      <NetworkEntries
        instance={instance}
        workspace={workspace}
        edits={edits}
        onChange={onChange}
        writable
        providerName={() => '香港宿主'}
      />
    </QueryClientProvider>,
  );
  return { entry, requests, onChange };
}

async function selectTarget() {
  fireEvent.click(await screen.findByRole('button', { name: '改派' }));
  const form = screen.getByRole('form', { name: '改派访问入口' });
  const select = within(form).getByRole('combobox', { name: '目标容器' }) as HTMLSelectElement;
  expect(Array.from(select.options, option => option.value)).toEqual(['', 'eligible', 'stopped']);
  expect(within(form).getByText('same-owner · 香港宿主')).toBeTruthy();
  fireEvent.change(select, { target: { value: 'eligible' } });
  fireEvent.click(within(form).getByRole('button', { name: '保存改派' }));
  return { form, select };
}

it('moves only the existing claim and retries the original revision and request ID after an uncertain response', async () => {
  const { entry, requests, onChange } = setup(503);
  const { form, select } = await selectTarget();
  await within(form).findByText('保存响应丢失');
  await screen.findByText(/暂无访问入口/);
  expect(select.disabled).toBe(true);
  expect(requests[0]).toEqual({
    url: `/cvm/entries/${entry.id}/target`,
    method: 'PUT',
    body: { instance_id: 'eligible', expected_revision: entry.revision, request_id: expect.any(String) },
  });
  fireEvent.click(within(form).getByRole('button', { name: '重试原改派' }));
  await waitFor(() => expect(screen.queryByRole('form', { name: '改派访问入口' })).toBeNull());
  expect(requests).toEqual([requests[0], requests[0]]);
  expect(onChange).not.toHaveBeenCalled();
});

it('does not silently rebase after a definite revision conflict', async () => {
  const { requests } = setup(409);
  const { form } = await selectTarget();
  await within(form).findByText('入口已变化，请取消后重新选择。');
  expect((within(form).getByRole('button', { name: '保存改派' }) as HTMLButtonElement).disabled).toBe(true);
  expect(within(form).queryByRole('button', { name: '重试原改派' })).toBeNull();
  expect(requests).toHaveLength(1);
});

it('does not mix reassignment into an existing network draft', async () => {
  const { requests } = setup(503, { detach_interface_ids: ['nic-business'] });
  const button = (await screen.findByRole('button', { name: '改派' })) as HTMLButtonElement;
  expect(button.disabled).toBe(true);
  expect(button.title).toContain('网络草稿');
  expect(requests).toHaveLength(0);
});
