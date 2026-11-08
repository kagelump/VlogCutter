// Global tokens first so component modules can override them.
import "./styles/global.css";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { createBrowserRouter, RouterProvider } from "react-router";
import { QueryClientProvider } from "@tanstack/react-query";
import { HttpProjectClient } from "@rcdvlog/project-client/http";
import { ClientProvider, createQueryClient } from "./app/client";
import { routes } from "./app/routes";

const csrfStore = {
  // The HttpOnly session cookie is shared by same-origin tabs, so the
  // double-submit value must be shared as well. It is never placed in a URL,
  // process argument, or server-rendered markup.
  get: () => window.localStorage.getItem("rcdvlog.csrf"),
  set: (value: string) => window.localStorage.setItem("rcdvlog.csrf", value),
  clear: () => window.localStorage.removeItem("rcdvlog.csrf"),
};

const client = new HttpProjectClient("", fetch, csrfStore);
const queryClient = createQueryClient();
const router = createBrowserRouter(routes);

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <ClientProvider client={client}>
      <QueryClientProvider client={queryClient}>
        <RouterProvider router={router} />
      </QueryClientProvider>
    </ClientProvider>
  </StrictMode>,
);
