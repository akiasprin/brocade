---
name: frontend-design-audit
description: Audit or standardize Brocade frontend visual design, interaction, accessibility, responsive behavior, and shared component usage.
---

# Frontend Design Audit

Use this workflow when the user asks for a frontend design review, consistency audit, interaction review, accessibility review, or extraction/enforcement of UI standards. Do not load it for an ordinary isolated frontend bug unless the request includes those concerns.

## Source of truth

Read `.agents/standards/frontend.md`, then inspect only the affected implementation and tests. Treat `frontend/src/styles.css`, `frontend/src/ui/`, and `frontend/tests/` as evidence of the current system, not as permission to preserve accidental inconsistencies.

## Review guidance

- Establish the reviewed surface and whether the request is read-only or includes implementation; choose coverage according to the affected experience.
- Use shared tokens, components and comparable project surfaces to understand the design. Assess intentional changes by user needs, coherent rules and usability.
- Ground visual findings in rendered pages or supplied screenshots. For implemented visual changes, perform the visual acceptance required by `.agents/standards/frontend.md`; identify unverified coverage.
- Consider hierarchy, semantic color, typography, spacing, interaction states, accessibility and responsive behavior where relevant.
- Separate system-level gaps from isolated defects. Prefer shared corrections when they address the actual cause, preserving existing state ownership and affected behavior contracts.
- Select checks through `.agents/standards/testing.md`, expanding coverage when the impact warrants it.

## Output

For an audit, lead with findings ordered by user impact and include exact file references, affected patterns and a concrete standard or remediation. Distinguish verified defects from design recommendations. If no actionable issue is found, say so and identify any unverified browser or device coverage.
