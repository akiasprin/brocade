---
name: release-preflight
description: Prepare or verify a Brocade Console, configuration, authorization, Agent, or Xray release and its rollback readiness.
---

# Release Preflight

Use this workflow when the user asks to prepare, check, or diagnose release readiness. Loading this Skill does not authorize production deployment, restart, publication, rollback, database restore, or other external mutation.

## Route by release domain

Read `.agents/runbooks/release-and-rollback.md` first and identify the release domain. For Console host deployment also read `.agents/runbooks/console-deployment.md`. Use `.agents/standards/testing.md` to select checks and `.agents/architecture/system-overview.md` when the change crosses protocol or component boundaries.

## Preflight

1. Record the intended environment, release object, immutable identifier, affected scope and requested stopping point.
2. Inspect the working tree and diff without modifying unrelated user work.
3. Map changed files to Rust crates, frontend, PostgreSQL, protocol, installer, embedded Agent/Xray and documentation impact.
4. Run targeted checks first. Expand to the standard matrix only where impact or the user's release gate requires it.
5. Verify artifact identity, configuration changes, compatibility, backup/recovery readiness, gray target, success signals and halt/rollback criteria.
6. Stop before external mutation unless the user has explicitly authorized the exact environment and action.

## Report

Return a concise go/no-go/conditional verdict with:

- release domain and scope;
- artifact identifiers or missing identity evidence;
- checks actually run and their results;
- compatibility, migration, secret and operational risks;
- backup, gray, observation and rollback readiness;
- blockers and the next authorized action.

Never mark a release ready solely because compilation passed. Never claim checks that were not run.
