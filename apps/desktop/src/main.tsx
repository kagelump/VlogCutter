import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import App from "./App";
import "./styles.css";

/**
 * Outside Tauri there is no backend, so the browser preview installs the same
 * fake one the CUJ suite uses. The import is dynamic so the preview's demo data
 * and its timer-driven progress land in their own chunk, which the desktop
 * build never loads.
 */
async function boot() {
  if (!("__TAURI_INTERNALS__" in window)) {
    const { installPreviewBackend } = await import("./preview");
    installPreviewBackend();
  }
  createRoot(document.getElementById("root")!).render(
    <StrictMode>
      <App />
    </StrictMode>,
  );
}

void boot();
