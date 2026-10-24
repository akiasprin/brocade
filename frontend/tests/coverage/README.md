# Visual coverage checks

Run `npm run test:coverage` from `frontend/`. The entry first type-checks the fixtures, then runs screenshot comparisons and interaction checks in Chromium and WebKit. Install those Playwright browsers if absent. Results are written to `.tools/playwright-coverage/` at the repository root.

These fixtures mount production components with synthetic query data. The rule fixture includes the production rule container so its named container queries apply. Rule save tests apply a Mux change, stage the actual local draft, reload the document and reopen the parameters. They stop at local draft persistence; they do not commit to the server or prove deployment.

The suite covers forward and referenced-listener callers separately, readonly access, long unbroken titles, long confirmation bodies, keyboard focus and return, popover scrolling and viewport resizing, long error URLs, and large overages in the capacity chart. Shared dialog tests verify the mechanism; each business caller still needs its own preconditions and evidence.

Known audit issues UI-011, UI-012, UI-014 and UI-016 use `test.fail()` immediately before the specific failing assertion. Setup and screenshot comparison happen first, so a broken fixture or unexpected screenshot difference is not accepted as the known failure. A repaired issue produces an unexpected pass and must have its annotation removed when the repair is reviewed. Snapshot baselines retain the current visible defects and are not evidence that those defects are acceptable.

The frontend visual acceptance requirements remain in [the frontend standard](../../../.agents/standards/frontend.md). Real device keyboards, browser chrome, system text scaling, screen readers and Agent execution require separate verification.
