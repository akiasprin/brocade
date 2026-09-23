import type { SnapshotStep } from '../api';

/** Whether another step explicitly forwards into this machine. */
export function isForwardTargetInChain(args: { nodeId: string; steps: SnapshotStep[] }) {
  const { nodeId, steps } = args;
  return steps.some(
    step => step.node !== nodeId && step.rules.some(rule => rule.a.t === 'forward' && rule.a.to === nodeId),
  );
}
