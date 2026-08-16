import { defineConfig, devices } from "@playwright/test";
import { existsSync } from "node:fs";

import { allocateLoopbackPort } from "../../scripts/lib/loopback-port.mjs";

const localChrome = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
const executablePath = process.env.RCDVLOG_CHROME_PATH
  ?? (existsSync(localChrome) ? localChrome : undefined);

/** A server the caller is already running and wants this suite pointed at. */
const external = process.env.RCDVLOG_WEB_BASE_URL;

// Otherwise every run gets its own port, and with it its own `mkdtemp` fixture
// state and its own bootstrap codes. That is the real fix for the stale-server
// hazard: reuse of a leftover server seeded with a previous run's data is not
// merely discouraged, it has become impossible. Allocated here rather than in
// the fixture script because Playwright needs the health-check URL before it
// spawns the script.
//
// One allocation per *run*, not per process: Playwright evaluates this file in
// the runner and again in every worker, so the address is published to the
// environment and an already-published one is reused. Allocating per process
// would point each worker at a port nothing is listening on.
const baseURL = external
  ?? process.env.RCDVLOG_WEB_E2E_BASE_URL
  ?? `http://127.0.0.1:${await allocateLoopbackPort()}`;
process.env.RCDVLOG_WEB_E2E_BASE_URL = baseURL;
const listenAddress = new URL(baseURL).host;

export default defineConfig({
  testDir: "./e2e",
  use: {
    baseURL,
    ...devices["Desktop Chrome"],
    launchOptions: executablePath ? { executablePath } : undefined,
  },
  webServer: external ? undefined : {
    command: "node ../../scripts/start-web-e2e-server.mjs",
    url: `${baseURL}/api/v1/health`,
    env: { RCDVLOG_LISTEN_ADDR: listenAddress },
    // Belt and braces. A run now has a port nobody else can be holding, so
    // there is nothing to reuse; leaving this on under CI would only mask a
    // fixture that failed to shut down.
    reuseExistingServer: !process.env.CI,
    // The script seeds the fixture through `cargo run` before the server can
    // answer a health check, and on a cold target directory that is a build,
    // not a start. Playwright's 60s default fails the suite on the compile.
    timeout: 300_000,
  },
});
