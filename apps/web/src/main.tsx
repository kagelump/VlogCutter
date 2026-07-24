import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { HttpProjectClient } from "@rcdvlog/project-client/http";
import App from "./App";
import { AppErrorBoundary } from "./ErrorBoundary";
import "./styles.css";

const csrfStore = {
  get: () => window.sessionStorage.getItem("rcdvlog.csrf"),
  set: (value: string) => window.sessionStorage.setItem("rcdvlog.csrf", value),
  clear: () => window.sessionStorage.removeItem("rcdvlog.csrf"),
};

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <AppErrorBoundary>
      <App client={new HttpProjectClient("", fetch, csrfStore)} />
    </AppErrorBoundary>
  </StrictMode>,
);
