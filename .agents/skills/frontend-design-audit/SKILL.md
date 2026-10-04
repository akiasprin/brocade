---
name: frontend-design-audit
description: Audit or standardize Brocade frontend visual design, interaction, accessibility, responsive behavior, and shared component usage.
---

# Frontend Design Audit

Use this workflow when the user asks for a frontend design review, consistency audit, interaction review, accessibility review, or extraction/enforcement of UI standards. Do not load it for an ordinary isolated frontend bug unless the request includes those concerns.

## Source of truth

Read `.agents/standards/frontend.md`, then inspect only the affected implementation and tests. Treat `frontend/src/styles.css`, `frontend/src/ui/`, and `frontend/tests/` as evidence of the current system, not as permission to preserve accidental inconsistencies.

## Workflow

1. Establish the reviewed surface and whether the request is read-only or includes implementation.
2. Inventory reused tokens, shared components, page-local variants, interaction states and responsive behavior.
3. Check semantic color roles, surface hierarchy, typography, spacing, keyboard/focus behavior, loading/empty/error states, reduced motion and narrow viewports.
4. Separate findings into system-level gaps and isolated defects. Prefer one shared correction over repeated page patches.
5. If implementation is requested, preserve existing state ownership and add behavioral tests next to the affected contract.
6. Run the smallest relevant checks from `.agents/standards/testing.md`, expanding to full frontend checks for shared tokens, components or routing.

## Output

For an audit, lead with findings ordered by user impact and include exact file references, affected patterns and a concrete standard or remediation. Distinguish verified defects from design recommendations. If no actionable issue is found, say so and identify any unverified browser or device coverage.
