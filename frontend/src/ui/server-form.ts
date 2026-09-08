import { useState } from 'react';

/** Rebase untouched fields while keeping edits made since the last source/save snapshot.
 * Arrays are atomic: replacing a target list must not splice new input into different rows. */
export function rebaseForm<T extends object>(current: T, base: T, next: T): T {
  const result = { ...next };
  for (const key of Object.keys(current) as (keyof T)[]) {
    if (JSON.stringify(current[key]) !== JSON.stringify(base[key])) result[key] = current[key];
  }
  return result;
}

export function useServerForm<T extends object>(source: T) {
  const [form, setForm] = useState(source);
  const [baseline, setBaseline] = useState(source);
  if (JSON.stringify(source) !== JSON.stringify(baseline)) {
    setBaseline(source);
    setForm(current => rebaseForm(current, baseline, source));
  }
  const accept = (saved: T, submitted: T) => setForm(current => rebaseForm(current, submitted, saved));
  return { form, setForm, accept };
}
