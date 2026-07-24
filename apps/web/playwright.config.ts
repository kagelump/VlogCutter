import { defineConfig, devices } from "@playwright/test";
import { existsSync } from "node:fs";

const localChrome = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
const executablePath = process.env.RCDVLOG_CHROME_PATH
  ?? (existsSync(localChrome) ? localChrome : undefined);

export default defineConfig({
  testDir: "./e2e",
  use: {
    baseURL: process.env.RCDVLOG_WEB_BASE_URL ?? "http://127.0.0.1:8090",
    ...devices["Desktop Chrome"],
    launchOptions: executablePath ? { executablePath } : undefined,
  },
  webServer: process.env.RCDVLOG_WEB_BASE_URL ? undefined : {
    command: "node ../../scripts/start-web-e2e-server.mjs",
    url: "http://127.0.0.1:8090/api/v1/health",
    reuseExistingServer: true,
  },
});
