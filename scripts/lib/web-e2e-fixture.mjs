// Where the web e2e fixture leaves its one-time bootstrap codes.
//
// The server writes them and the Chrome suite reads them from a different
// process, so the path has to be derivable from something both ends already
// know. That is the run's port: it is unique to the run by construction, which
// makes concurrent runs — two checkouts, or a local run beside a CI
// reproduction — read their own codes instead of each other's, and makes the
// exit cleanup incapable of deleting another run's files.
//
// Run the tests with: node --test "scripts/*.test.mjs"

import { tmpdir } from "node:os";
import { join } from "node:path";

/** The port a `127.0.0.1:<port>` listen address or an `http://…` base URL names. */
export function fixturePort(addressOrUrl) {
  const port = addressOrUrl.includes("//")
    ? new URL(addressOrUrl).port
    : addressOrUrl.slice(addressOrUrl.lastIndexOf(":") + 1);
  if (!/^\d{1,5}$/.test(port)) {
    throw new Error(`web e2e fixture: no port in ${addressOrUrl}`);
  }
  return port;
}

/** The file holding bootstrap code `index` (1-based) for the run at that address. */
export function bootstrapCodePath(addressOrUrl, index) {
  return join(tmpdir(), `rcdvlog-web-e2e-bootstrap-${fixturePort(addressOrUrl)}-${index}.secret`);
}

/** The synthetic TV review-target handoff the seed writes for the run at that address. */
export function reviewTargetHandoffPath(addressOrUrl) {
  return join(tmpdir(), `rcdvlog-web-e2e-review-target-${fixturePort(addressOrUrl)}.json`);
}

/** The synthetic #232 historical rendered-artifact review the seed writes for the run at that address. */
export function historicalReviewPath(addressOrUrl) {
  return join(tmpdir(), `rcdvlog-web-e2e-historical-review-${fixturePort(addressOrUrl)}.json`);
}

/** The web path of the seeded, transcribed voice review for the run at that address. */
export function reviewRunPath(addressOrUrl) {
  return join(tmpdir(), `rcdvlog-web-e2e-review-run-${fixturePort(addressOrUrl)}.txt`);
}

/**
 * How to launch the server or the seed: `cargo run` from source by default, or
 * a prebuilt binary when CI hands one in. CI builds both once on a Rust runner
 * and runs the browser tier in the pinned Playwright image, which has no Rust
 * toolchain; a prebuilt path must be absolute so it cannot resolve against
 * whatever directory the fixture happens to start in.
 */
export function rustLaunch(environment, kind, args) {
  const variable = { server: "RCDVLOG_WEB_E2E_SERVER_BIN", seed: "RCDVLOG_WEB_E2E_SEED_BIN" }[kind];
  if (!variable) throw new Error(`web e2e fixture: unknown launch kind ${kind}`);
  const prebuilt = environment[variable];
  if (prebuilt) {
    if (!prebuilt.startsWith("/")) throw new Error(`web e2e fixture: ${variable} must be an absolute path`);
    return { command: prebuilt, args };
  }
  const target = kind === "seed" ? ["--example", "web_e2e_seed"] : [];
  return {
    command: "cargo",
    args: ["run", "--quiet", "-p", "rcdvlog-server", ...target, ...(args.length ? ["--", ...args] : [])],
  };
}
