import { render } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryRouter, RouterProvider } from "react-router";
import type { BrowserProjectClient, ServerStatus } from "@rcdvlog/project-client";
import { HttpProjectClient } from "@rcdvlog/project-client/http";
import { ClientProvider } from "../app/client";
import { routes } from "../app/routes";
import { MemoryOutbox } from "../features/tvReview/recording/outbox";
import { VoiceDepsProvider, type VoiceDeps } from "../features/tvReview/shared";

export const readyStatus: ServerStatus = {
  version: { requestId: "r", applicationVersion: "0.1.0", protocolVersion: { major: 1, minor: 0 }, controlSchemaVersion: 8, nodeId: "node_fixture" },
  readiness: { requestId: "r", status: "ready", checks: [{ name: "control_catalog", status: "ok" }] },
};

export function memoryVoice(overrides: Partial<VoiceDeps> = {}): VoiceDeps {
  return { outbox: new MemoryOutbox(), getUserMedia: vi.fn(), listMicrophones: vi.fn(), startRecording: vi.fn(), ...overrides };
}

/** Mount the real route table at `path` against a mocked client. Unlisted client I/O methods are absent; thumbnail URLs use the real builder. */
export function renderApp(path: string, overrides: Partial<BrowserProjectClient> = {}, voice: VoiceDeps = memoryVoice()) {
  const client = {
    getLiveReviewTranscript: vi.fn().mockResolvedValue({ enabled: false, state: "disabled", lines: [], model: "ggml-tiny.bin", modelAvailable: true, revision: 0, processedSeconds: 0, receivedSeconds: 0, errorCode: null, finalRunId: null }),
    enableLiveReviewTranscript: vi.fn().mockResolvedValue({ enabled: true, state: "live", lines: [], model: "ggml-tiny.bin", modelAvailable: true, revision: 0, processedSeconds: 0, receivedSeconds: 0, errorCode: null, finalRunId: null }),
    reviewSessionAudioUrl: (projectId: string, sessionId: string) => new HttpProjectClient().reviewSessionAudioUrl(projectId, sessionId),
    reviewReportUrl: (projectId: string, runId: string) => new HttpProjectClient().reviewReportUrl(projectId, runId),
    projectAssetThumbnailUrl: (projectId: string, assetId: string) => new HttpProjectClient().projectAssetThumbnailUrl(projectId, assetId),
    getServerStatus: vi.fn().mockResolvedValue(readyStatus),
    getSession: vi.fn().mockResolvedValue({ authenticated: true, sessions: [] }),
    ...overrides,
  } as unknown as BrowserProjectClient;
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity }, mutations: { retry: false } } });
  const router = createMemoryRouter(routes, { initialEntries: [path] });
  const result = render(
    <ClientProvider client={client}>
      <QueryClientProvider client={queryClient}>
        <VoiceDepsProvider deps={voice}>
          <RouterProvider router={router} />
        </VoiceDepsProvider>
      </QueryClientProvider>
    </ClientProvider>,
  );
  return { ...result, client, router, queryClient };
}
