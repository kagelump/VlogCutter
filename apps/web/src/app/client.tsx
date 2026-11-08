import { createContext, useContext, type ReactNode } from "react";
import { QueryCache, MutationCache, QueryClient } from "@tanstack/react-query";
import type { BrowserProjectClient } from "@rcdvlog/project-client";
import { isAuthError } from "../lib/errors";

const ClientContext = createContext<BrowserProjectClient | null>(null);

export function ClientProvider({ client, children }: { client: BrowserProjectClient; children: ReactNode }) {
  return <ClientContext.Provider value={client}>{children}</ClientContext.Provider>;
}

export function useClient(): BrowserProjectClient {
  const client = useContext(ClientContext);
  if (!client) throw new Error("useClient must be used inside <ClientProvider>");
  return client;
}

/**
 * Query keys, grouped by scope so a project's data can be invalidated as a
 * unit when the server pushes a change.
 */
export const keys = {
  status: ["status"] as const,
  auth: ["auth"] as const,
  projects: ["projects"] as const,
  library: (filters: object) => ["library", filters] as const,
  project: (projectId: string) => ["project", projectId] as const,
  assets: (projectId: string, search: string) => ["project", projectId, "assets", search] as const,
  asset: (projectId: string, assetId: string) => ["project", projectId, "asset", assetId] as const,
  targets: (projectId: string) => ["project", projectId, "targets"] as const,
  target: (projectId: string, targetId: string) => ["project", projectId, "target", targetId] as const,
  sessions: (projectId: string, targetId: string) => ["project", projectId, "sessions", targetId] as const,
  session: (projectId: string, sessionId: string) => ["project", projectId, "session", sessionId] as const,
  transcriptions: (projectId: string, sessionId: string) => ["project", projectId, "transcriptions", sessionId] as const,
  workspace: (projectId: string, runId: string) => ["project", projectId, "workspace", runId] as const,
  admin: ["admin"] as const,
};

export function createQueryClient() {
  const queryClient: QueryClient = new QueryClient({
    // An expired or revoked session anywhere sends the whole app back to sign-in.
    queryCache: new QueryCache({ onError: (error) => signOutOnAuthError(error) }),
    mutationCache: new MutationCache({ onError: (error) => signOutOnAuthError(error) }),
    defaultOptions: {
      queries: {
        staleTime: 10_000,
        refetchOnWindowFocus: true,
        retry: (count, error) => !isAuthError(error) && count < 2,
      },
      mutations: { retry: false },
    },
  });
  function signOutOnAuthError(error: unknown) {
    if (isAuthError(error)) void queryClient.invalidateQueries({ queryKey: keys.auth });
  }
  return queryClient;
}
