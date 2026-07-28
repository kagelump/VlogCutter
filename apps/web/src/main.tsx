import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { HttpProjectClient } from "@rcdvlog/project-client/http";
import App from "./App";
import { AppErrorBoundary } from "./ErrorBoundary";
import "./styles.css";

const csrfStore = {
  // The HttpOnly session cookie is shared by same-origin tabs, so the
  // double-submit value must be shared as well. It is never placed in a URL,
  // process argument, or server-rendered markup.
  get: () => window.localStorage.getItem("rcdvlog.csrf"),
  set: (value: string) => window.localStorage.setItem("rcdvlog.csrf", value),
  clear: () => window.localStorage.removeItem("rcdvlog.csrf"),
};

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <AppErrorBoundary>
      <App client={new HttpProjectClient("", fetch, csrfStore)} />
    </AppErrorBoundary>
  </StrictMode>,
);
