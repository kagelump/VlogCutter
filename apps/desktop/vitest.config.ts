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
  },
});
