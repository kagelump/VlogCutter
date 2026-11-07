# Local gate, focused tests and publication

Relocated verbatim from the repository-root `AGENTS.md` section 12 ("Testing and verification") so the root file can stay a short index (issue #260). The gate table and the CI/doc drift rule are in [engineering-reference.md](engineering-reference.md).

Every gate scope starts with `pnpm preflight` (see
[dev-environment.md](dev-environment.md)) and stops before any slow check if a
pinned tool is missing or wrong. Use `pnpm gate --scope fast` for a quick
iteration check. The default local gate requires a JSON focused-check plan and runs the candidate diff checks,
pinned formatter/toolchain, workspace Clippy, frontend typecheck, and the
named checks in that plan. For example, save
`{"checks":[{"name":"migration test","command":"pnpm","args":["test:focused","rcdvlog-core","migrations::tests::the_schema_version_constant_tracks_the_migration_list"]}]}`
to `/absolute/path/focused-checks.json`, then run
`pnpm gate --checks /absolute/path/focused-checks.json`. Commands are spawned directly
with argument arrays; shell strings are not accepted. Local verification should
cover the changed behavior and its dependencies, including targeted integration
checks for migrations, integrity, concurrency, cancellation, or paid accounting
when those boundaries change. Use the full pre-PR scope only when a specific
risk or repeated CI failure justifies its broader cost:
`pnpm gate --scope pre-pr`. It follows all Linux CI checks in
`.github/workflows/ci.yml`, prints each command's output and status, and fails
if any prerequisite or command fails. `pnpm gate --scope fast` remains
iteration-only. Before merge, completed Linux and platform-relevant macOS CI
must pass on the exact candidate head; coverage and safety checks remain mandatory. The gate's
isolated regression suite is `node --test scripts/pre-pr-gate.test.mjs`, and
`pnpm test:scripts` includes it. Re-run relevant checks after each code change
and include results or exact-head CI links and failed-step logs in a handoff.

For one Rust test during iteration, run
`pnpm test:focused <cargo-package> <exact-test-name>` (for example,
`pnpm test:focused rcdvlog-core migrations::tests::the_schema_version_constant_tracks_the_migration_list`). It uses the CI Rust
pin and cargo-nextest with an exact `test(=name)` filter, then requires nextest to report
the named test as passed and at least one executed test. The fake-command regression cases are in
`scripts/run-focused-test.test.mjs`.

Use `pnpm publish:after-gate push --checks /absolute/path/focused-checks.json` or
`pnpm publish:after-gate pr --checks /absolute/path/focused-checks.json` to publish after
the focused local gate. Choose the broader policy explicitly with
`--scope pre-pr`; the fast scope is refused at the publication boundary. The
wrapper runs the actual gate from a clean tree, snapshots HEAD and tracked/untracked
working-tree content, and rechecks that identity before each external action.
It refuses publication if the gate fails or that identity changes. The `pr`
action pushes the current branch upstream and then creates the PR, stopping if
the push fails.

After publication, inspect CI on meaningful status transitions and wait for
jobs to finish before retrieving failure logs. Repair failures locally, rerun
the gate on the repaired code, and push the new commit. Confirm CI passes on
that exact commit before declaring an issue complete; neither an in-progress
run nor a green run for an older commit is confirmation.
