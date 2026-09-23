import type { DraftEntry, ModelOp } from './draft';
import { randomKey } from './ui/platform';

export interface DraftBranch {
  id: string;
  parents: string[];
  entries: DraftEntry[];
}

export interface DraftBatch {
  owner: string | null;
  entries: readonly DraftEntry[];
}

type SubmittedEntry = Pick<DraftEntry, 'key' | 'editId'> & { op: { op: ModelOp['op'] } };

/** A successful create is now an existing object. Later edits must not replay create-only ops. */
export function afterSubmission(entries: readonly DraftEntry[], submitted: readonly SubmittedEntry[]): DraftEntry[] {
  const sent = new Map(submitted.map(entry => [entry.key, entry]));
  return entries.flatMap(entry => {
    const previous = sent.get(entry.key);
    if (!previous) return [entry];
    if (entry.editId === previous.editId) return [];
    const op = entry.op;
    if (previous.op.op === 'create_app' && op.op === 'create_app') {
      return [{ ...entry, op: { ...op, op: 'upsert_app' as const } }];
    }
    if (previous.op.op === 'create_chain' && op.op === 'create_chain') {
      return [{ ...entry, op: { ...op, op: 'upsert_chain' as const } }];
    }
    if (previous.op.op === 'create_ingress' && op.op === 'create_ingress') {
      return [{ ...entry, op: { ...op, op: 'upsert_ingress' as const } }];
    }
    return [entry];
  });
}

function readEntries(value: unknown): DraftEntry[] {
  if (!Array.isArray(value)) return [];
  return value.filter(
    (entry): entry is DraftEntry =>
      !!entry &&
      typeof entry.key === 'string' &&
      typeof entry.label === 'string' &&
      typeof entry.editId === 'string' &&
      !!entry.op &&
      typeof entry.op.op === 'string',
  );
}

/** Immutable branch heads avoid localStorage's read/modify/write lost-update race. Two writes
 * from the same parent keep two heads until the operator explicitly selects a version.
 * Successful submissions are separate receipts: even a late write based on an old head cannot
 * resurrect submitted operations. Each edit has an ID, so newer edits survive those receipts. */
export class DraftStorage {
  readonly prefix: string;
  constructor(
    readonly owner: string,
    private readonly storage: Storage,
  ) {
    this.prefix = `brocade-console:draft:v1:${owner}:`;
  }

  private records(kind: string): string[] {
    const prefix = `${this.prefix}${kind}:`;
    const keys: string[] = [];
    for (let i = 0; i < this.storage.length; i++) {
      const key = this.storage.key(i);
      if (key?.startsWith(prefix)) keys.push(key);
    }
    return keys.sort();
  }

  private values(kind: string): string[] {
    // Another tab can replace a head between key enumeration and getItem. Retry that read;
    // a disappearing parent is not evidence that the operator discarded the whole draft.
    for (let attempt = 0; attempt < 8; attempt++) {
      const keys = this.records(kind);
      const values = keys.map(key => this.storage.getItem(key));
      if (
        values.every((value): value is string => value !== null) &&
        JSON.stringify(keys) === JSON.stringify(this.records(kind))
      )
        return values;
    }
    throw new Error('其他标签页正在更新草稿，请稍后重试。');
  }

  read(): DraftBranch[] {
    const receipts = this.values('receipt').map(raw => JSON.parse(raw) as SubmittedEntry[]);
    const records: DraftBranch[] = this.values('head').map(raw => {
      const branch = JSON.parse(raw) as DraftBranch;
      let entries = readEntries(branch.entries);
      for (const receipt of receipts) entries = afterSubmission(entries, receipt);
      return { ...branch, entries };
    });
    const replaced = new Set(records.flatMap(branch => branch.parents));
    return records.filter(branch => !replaced.has(branch.id));
  }

  write(entries: readonly DraftEntry[], parents: string[]): void {
    const id = randomKey();
    const branch: DraftBranch = { id, parents, entries: [...entries] };
    // Publish first. Removing parents before this write would lose data on quota/storage errors.
    this.storage.setItem(`${this.prefix}head:${id}`, JSON.stringify(branch));
    for (const parent of parents) this.storage.removeItem(`${this.prefix}head:${parent}`);
  }

  acknowledge(entries: readonly DraftEntry[]): void {
    // Receipts need identifiers only; don't retain submitted credentials or full settings bodies.
    const receipt: SubmittedEntry[] = entries.map(({ key, editId, op }) => ({ key, editId, op: { op: op.op } }));
    this.storage.setItem(`${this.prefix}receipt:${randomKey()}`, JSON.stringify(receipt));
  }
}
