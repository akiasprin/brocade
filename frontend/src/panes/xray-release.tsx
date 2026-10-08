import { BinaryReleaseTab, binaryBadge, useBinaryRelease, type BinaryRelease } from './binary-release';

export const useXrayRelease = () => useBinaryRelease('xray');
export const xrayBadge = binaryBadge;
export function XrayReleaseTab({
  xray,
  ...props
}: {
  xray: BinaryRelease;
  editable: boolean;
  editing: boolean;
  onEditingChange: (editing: boolean) => void;
}) {
  return <BinaryReleaseTab release={xray} {...props} />;
}
