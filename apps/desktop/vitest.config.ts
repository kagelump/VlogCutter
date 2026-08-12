import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  test: {
    environment: "jsdom",
    globals: true,
    setupFiles: "./src/test/setup.ts",
    // `e2e/` is the Playwright browser tier (`pnpm test:browser`); its *.spec.ts
    // files import @playwright/test and must never be collected by Vitest.
    exclude: ["node_modules/**", "dist/**", "src-tauri/**", "e2e/**"],

    /**
     * A floor, not a target.
     *
     * Coverage was unmeasured until this was added, and what the first
     * measurement showed was not the headline percentage but the gap beneath
     * it: 84.87% of statements against 68.20% of functions, meaning components
     * rendered while half their callbacks were never invoked. That gap is where
     * whole features were hiding — import rollback, project creation, the
     * baseline wizard — so `functions` is the threshold worth watching.
     *
     * The numbers below sit a little under what the suite currently reaches, so
     * an ordinary change does not fail on rounding. Raise them when the suite
     * moves up; do not lower them to make a red run green.
     *
     * Coverage says what executed, not what was asserted, and this tier fakes
     * the IPC destination — so it is never evidence about media, native
     * dialogs, or persistence. See `docs/integration-testing.md`.
     */
    coverage: {
      provider: "v8",
      reporter: ["text-summary", "html"],
      reportsDirectory: "./coverage",
      include: ["src/**"],
      exclude: [
        "src/test/**", // the harness itself
        "src/preview/**", // browser-preview wiring, driven by the Playwright tier
        "src/__independent__/**",
        "src/**/__tests__/**",
        "src/**/*.test.*",
        "src/main.tsx", // the mount point; running it is what a browser does
        "src/types.ts",
        "src/chronologyTypes.ts",
      ],
      thresholds: {
        statements: 89,
        branches: 84,
        functions: 81,
        lines: 89,
      },
    },
  },
});
