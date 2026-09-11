import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { DraftStore, type DraftEntry } from '../src/draft';
import { DraftStorage } from '../src/draft-storage';

const stores: DraftStore[] = [];
function tab(owner = 'concurrency') {
  const store = new DraftStore();
  stores.push(store);
  store.init(owner);
  return store;
}
const update = (store: DraftStore, id: string, name: string) =>
  store.push({ op: 'update_node', node_id: id, node: { name } });
const notify = () => window.dispatchEvent(new StorageEvent('storage', { key: null }));
beforeEach(() => localStorage.clear());
afterEach(() => {
  vi.restoreAllMocks();
  stores.forEach(store => store.dispose());
  stores.length = 0;
  localStorage.clear();
});

describe('submission batches', () => {
  it('retains a later edit of the same object and independent node fields', () => {
    const store = tab();
    store.push({ op: 'update_node', node_id: 'A', node: { name: 'first', mtu: 1380 } });
    const batch = store.beginSubmission();
    update(store, 'A', 'later');
    store.finishSubmission(batch, true);
    expect(store.ops()).toEqual([{ op: 'update_node', node_id: 'A', node: { name: 'later', mtu: 1380 } }]);
  });
  it('keeps all edits on failure and allows retry', () => {
    const store = tab();
    update(store, 'A', 'first');
    const batch = store.beginSubmission();
    update(store, 'B', 'later');
    store.finishSubmission(batch, false);
    expect(store.ops()).toHaveLength(2);
    const retry = store.beginSubmission();
    store.finishSubmission(retry, true);
    expect(store.ops()).toHaveLength(0);
  });
  it('turns a later edit of a successfully created app into an update', () => {
    const store = tab();
    store.push({ op: 'create_app', app: { id: 'a', label: 'first' } });
    const batch = store.beginSubmission();
    store.push({ op: 'upsert_app', app: { id: 'a', label: 'later' } });
    store.finishSubmission(batch, true);
    expect(store.ops()).toEqual([{ op: 'upsert_app', app: { id: 'a', label: 'later' } }]);
    expect(tab().ops()).toEqual(store.ops());
  });
  it('cleans the original operator only if identity changes while submitting', () => {
    const store = tab('first');
    update(store, 'A', 'first');
    const batch = store.beginSubmission();
    store.init('second');
    update(store, 'B', 'second');
    store.finishSubmission(batch, true);
    expect(store.ops()).toHaveLength(1);
    expect(tab('first').ops()).toHaveLength(0);
  });
});

describe('cross-tab persistence', () => {
  it('shares normal edits and merges edits before the storage event arrives', () => {
    const a = tab();
    const b = tab();
    update(a, 'A', 'first');
    update(b, 'B', 'second');
    notify();
    expect(a.ops()).toEqual(b.ops());
    expect(a.ops()).toHaveLength(2);
    expect(tab().ops()).toEqual(a.ops());
  });
  it('propagates a committed batch without erasing another tab’s new edit', () => {
    const a = tab();
    const b = tab();
    update(a, 'A', 'first');
    const batch = a.beginSubmission();
    update(b, 'B', 'later');
    a.finishSubmission(batch, true);
    notify();
    expect(a.ops()).toEqual(b.ops());
    expect(b.ops()).toEqual([{ op: 'update_node', node_id: 'B', node: { name: 'later' } }]);
  });
  it('propagates discard instead of resurrecting it from a stale tab', () => {
    const a = tab();
    const b = tab();
    update(a, 'A', 'first');
    notify();
    a.clear();
    update(b, 'B', 'new');
    notify();
    expect(a.ops()).toEqual([{ op: 'update_node', node_id: 'B', node: { name: 'new' } }]);
  });
  it('retains both truly simultaneous branch writes and blocks submission until explicitly resolved', () => {
    const a = tab();
    const disk = new DraftStorage('concurrency', localStorage);
    const parents = disk.read().map(branch => branch.id);
    const entry = (id: string): DraftEntry => ({
      key: `node:${id}`,
      label: id,
      editId: id,
      op: { op: 'update_node', node_id: id, node: { name: id } },
    });
    disk.write([entry('A')], parents);
    disk.write([entry('B')], parents);
    notify();
    expect(a.conflicts()).toHaveLength(2);
    expect(tab().conflicts()).toHaveLength(2);
    expect(() => a.beginSubmission()).toThrow(/同时修改/);
    expect(() => update(a, 'C', 'third')).toThrow(/同时修改/);
    const selected = a.conflicts().find(branch => branch.entries[0].editId === 'B')!;
    a.resolveConflict(selected.id);
    expect(a.conflicts()).toHaveLength(0);
    expect(a.ops()[0]).toMatchObject({ node_id: 'B' });
  });
  it('receipts prevent a late concurrent branch from resurrecting a submitted create', () => {
    const a = tab();
    a.push({ op: 'create_app', app: { id: 'a', label: 'created' } });
    const disk = new DraftStorage('concurrency', localStorage);
    const old = disk.read()[0];
    const batch = a.beginSubmission();
    a.finishSubmission(batch, true);
    disk.write(
      [
        ...old.entries,
        {
          key: 'node:B',
          label: 'B',
          editId: 'later',
          op: { op: 'update_node', node_id: 'B', node: { name: 'later' } },
        },
      ],
      [old.id],
    );
    notify();
    expect(a.ops()).toEqual([{ op: 'update_node', node_id: 'B', node: { name: 'later' } }]);
  });
});

describe('storage failures', () => {
  it('keeps unpersisted input when another tab changes, then restores both as a conflict', () => {
    const a = tab();
    const b = tab();
    const original = Storage.prototype.setItem;
    const failure = vi.spyOn(Storage.prototype, 'setItem').mockImplementation(function (this: Storage, key, value) {
      if (key.includes(':head:')) throw new DOMException('full', 'QuotaExceededError');
      original.call(this, key, value);
    });
    update(a, 'A', 'local');
    expect(a.storageError()).toMatch(/尚未保存/);
    failure.mockRestore();
    update(b, 'B', 'remote');
    notify();
    expect(a.ops()[0]).toMatchObject({ node_id: 'A' });
    a.retryPersistence();
    expect(a.conflicts()).toHaveLength(2);
  });
  it('does not re-submit a successful batch when saving its receipt failed', () => {
    const a = tab();
    update(a, 'A', 'submitted');
    const batch = a.beginSubmission();
    const original = Storage.prototype.setItem;
    const failure = vi.spyOn(Storage.prototype, 'setItem').mockImplementation(function (this: Storage, key, value) {
      if (key.includes(':receipt:')) throw new DOMException('full', 'QuotaExceededError');
      original.call(this, key, value);
    });
    a.finishSubmission(batch, true);
    expect(a.ops()).toEqual([]);
    expect(a.storageError()).toMatch(/提交已成功/);
    expect(() => a.beginSubmission()).toThrow(/提交已成功/);
    failure.mockRestore();
    a.retryPersistence();
    expect(a.storageError()).toBeNull();
    expect(tab().ops()).toEqual([]);
  });
});

it('retries when a head is replaced between enumeration and reading', () => {
  const disk = new DraftStorage('concurrency', localStorage);
  disk.write([], []);
  const old = disk.read()[0];
  const original = Storage.prototype.getItem;
  let replaced = false;
  vi.spyOn(Storage.prototype, 'getItem').mockImplementation(function (this: Storage, key) {
    if (!replaced && key.endsWith(`head:${old.id}`)) {
      replaced = true;
      disk.write(
        [{ key: 'node:B', label: 'B', editId: 'B', op: { op: 'update_node', node_id: 'B', node: { name: 'latest' } } }],
        [old.id],
      );
    }
    return original.call(this, key);
  });
  expect(disk.read()[0].entries[0].op).toMatchObject({ node_id: 'B' });
});

it('keeps both versions when another tab saves the same target before its event arrives', () => {
  const a = tab();
  const b = tab();
  update(a, 'A', 'remote');
  update(b, 'A', 'local from old view');
  notify();
  expect(a.conflicts()).toHaveLength(2);
  expect(a.conflicts().flatMap(branch => branch.entries.map(entry => entry.op))).toEqual(
    expect.arrayContaining([
      { op: 'update_node', node_id: 'A', node: { name: 'remote' } },
      { op: 'update_node', node_id: 'A', node: { name: 'local from old view' } },
    ]),
  );
});
it('discard only removes the reviewed edits, not an unseen remote update', () => {
  const a = tab();
  const b = tab();
  update(a, 'A', 'first');
  notify();
  update(a, 'A', 'new remote');
  b.clear();
  notify();
  expect(b.ops()).toEqual([{ op: 'update_node', node_id: 'A', node: { name: 'new remote' } }]);
});
it('requires reviewing an unseen remote update before submission', () => {
  const a = tab();
  const b = tab();
  update(a, 'A', 'unseen');
  expect(() => b.beginSubmission()).toThrow(/查看最新内容/);
  expect(b.isSubmitting()).toBe(false);
  const batch = b.beginSubmission();
  b.finishSubmission(batch, false);
});
