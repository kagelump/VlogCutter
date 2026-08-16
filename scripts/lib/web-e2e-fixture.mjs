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
