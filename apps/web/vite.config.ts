import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  server: {
    host: "127.0.0.1",
    port: 1421,
    proxy: { "/api": "http://127.0.0.1:8090" },
  },
  build: {
    sourcemap: true,
    assetsDir: "assets",
  },
  test: {
    environment: "jsdom",
    globals: true,
    setupFiles: "./src/test/setup.ts",
    exclude: ["e2e/**", "node_modules/**", "dist/**"],
  },
});
