import type { RuleIssue } from './api/geodata';
let latest: RuleIssue[] = [];
export const currentRuleIssues = () => latest;
export function reportRuleIssues(issues: RuleIssue[]) {
  latest = issues;
  window.dispatchEvent(new CustomEvent('rule-validation-error', { detail: issues }));
}
