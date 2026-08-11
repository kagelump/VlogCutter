import { defineConfig, devices } from "@playwright/test";

/**
 * The desktop real-browser tier (issue #22).
 *
 * This tier boots the *same* app the CUJ suite boots — the real `App`, the real
 * adapters, the same in-memory harness — but in headless Chromium through the
 * Vite preview (`src/main.tsx` installs the harness when `__TAURI_INTERNALS__`
 * is absent). Everything here must prove a property a jsdom CUJ cannot:
 * computed layout, real scrolling, real focus and caret, or real pointer
 * events. If a test does not need a layout engine, it belongs in the fast tier.
 *
 * It is never evidence for native behaviour — no WKWebView, no IPC, no media
 * decoding, no Keychain. See `docs/integration-testing.md`.
 */
export default defineConfig({
  testDir: "./e2e",
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI ? [["list"], ["html", { open: "never" }]] : "list",
  use: {
    baseURL: process.env.RCDVLOG_DESKTOP_BASE_URL ?? "http://127.0.0.1:1420",
    ...devices["Desktop Chrome"],
    trace: "on-first-retry",
  },
  webServer: process.env.RCDVLOG_DESKTOP_BASE_URL ? undefined : {
    // Bind IPv4 explicitly: Vite's default `localhost` resolves to ::1 first on
    // macOS, which the 127.0.0.1 health check below never reaches.
    command: "pnpm dev --host 127.0.0.1",
    url: "http://127.0.0.1:1420",
    reuseExistingServer: !process.env.CI,
    timeout: 120_000,
  },
});
