import { BinaryReleaseTab, binaryBadge, useBinaryRelease, type BinaryRelease } from './binary-release';

export const useAgentRelease = () => useBinaryRelease('agent');
export const agentBadge = binaryBadge;
export function AgentReleaseTab({
  agent,
  ...props
}: {
  agent: BinaryRelease;
  editable: boolean;
  editing: boolean;
  onEditingChange: (editing: boolean) => void;
}) {
  return <BinaryReleaseTab release={agent} {...props} />;
}
