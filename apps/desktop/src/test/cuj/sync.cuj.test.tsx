/**
 * Pairing and sync.
 *
 * No server is running and none is needed: what the desktop owes the user here
 * is an honest account of where its work is, and a way out of every state it
 * can get into. Whether the protocol converges is the server suite's business
 * (`pnpm test:server`, and the two-node process tests).
 */

import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { buildSyncStatus, desktopScenario, renderApp } from "../harness";

function project() {
  return desktopScenario({ now: "2026-08-09T12:00:00.000Z" })
    .project("Hokkaido field test", { id: "project_hokkaido" })
    .camera("Sony A7C II", { clips: 3, withProxy: true, startAt: "2026-02-08T00:10:00.000Z" });
}

async function openSettings(app: Awaited<ReturnType<typeof renderApp>>) {
  await app.user.click(screen.getByRole("button", { name: /Settings/ }));
  return screen.findByRole("dialog");
}

describe("pairing with a server", () => {
  it("reaches the server, takes the code, and reports itself paired", async () => {
    const harness = project().build();
    const app = await renderApp(harness);

    const settings = await openSettings(app);
    await app.user.click(within(settings).getByRole("button", { name: /Connect to server/i }));
    await app.user.type(await screen.findByLabelText("Server URL"), "https://studio.local");
    await app.user.click(screen.getByRole("button", { name: "Test connection" }));

    // The identity check happens before any secret is sent.
    await waitFor(() => expect(harness.calls("fetch_server_info")).toHaveLength(1));

    await app.user.type(await screen.findByLabelText("Pairing code"), "123456");
    await app.user.click(screen.getByRole("button", { name: "Pair with server" }));

    await waitFor(() => expect(harness.state.settings.pairedServerNodeId).toBe("node_server_harness"));
    expect(harness.state.settings.pairedServerEndpoint).toBe("https://studio.local");
    expect(harness.state.settings.pairedAt).toBe("2026-08-09T12:00:00.000Z");
  });

  it("refuses a bad code without recording a pairing", async () => {
    const harness = project().build();
    const app = await renderApp(harness);

    const settings = await openSettings(app);
    await app.user.click(within(settings).getByRole("button", { name: /Connect to server/i }));
    await app.user.type(await screen.findByLabelText("Server URL"), "https://studio.local");
    await app.user.click(screen.getByRole("button", { name: "Test connection" }));

    await app.user.type(await screen.findByLabelText("Pairing code"), "000000");
    await app.user.click(screen.getByRole("button", { name: "Pair with server" }));

    expect(await screen.findByText(/not valid or has expired/)).toBeVisible();
    expect(harness.state.settings.pairedServerNodeId).toBeNull();
  });

  it("says it cannot reach a server that will not answer", async () => {
    const harness = project().build();
    const app = await renderApp(harness);
    harness.next("fetch_server_info").reject("connection refused");

    const settings = await openSettings(app);
    await app.user.click(within(settings).getByRole("button", { name: /Connect to server/i }));
    await app.user.type(await screen.findByLabelText("Server URL"), "https://studio.local");
    await app.user.click(screen.getByRole("button", { name: "Test connection" }));

    expect(await screen.findByText(/Cannot reach server.*connection refused/)).toBeVisible();
    expect(harness.state.settings.pairedServerNodeId).toBeNull();
  });

  it("lets the user disconnect again", async () => {
    const harness = project().paired().build();
    const app = await renderApp(harness);

    const settings = await openSettings(app);
    await app.user.click(within(settings).getByRole("button", { name: /Disconnect/i }));

    await waitFor(() => expect(harness.state.settings.pairedServerNodeId).toBeNull());
    expect(await screen.findByText(/Disconnected from server/)).toBeVisible();
  });
});

describe("sync status", () => {
  it("shows the state the backend reports and its detail", async () => {
    const harness = project()
      .paired()
      .syncStatus({
        state: "attention",
        conflictCount: 2,
        localPendingOperations: 5,
        lastError: "the server rejected two operations",
        activeEndpoint: "https://studio.local",
      })
      .build();
    const app = await renderApp(harness);

    await app.user.click(await screen.findByRole("button", { name: /Needs attention/i }));

    const diagnostics = await screen.findByRole("dialog");
    expect(within(diagnostics).getByText("2 unresolved")).toBeVisible();
    expect(within(diagnostics).getByText("the server rejected two operations")).toBeVisible();
    expect(within(diagnostics).getByText("https://studio.local")).toBeVisible();
  });

  it("follows a status the backend pushes while the user watches", async () => {
    const harness = project().paired().syncStatus({ state: "offline" }).build();
    await renderApp(harness);

    expect(await screen.findByRole("button", { name: /Offline/i })).toBeVisible();

    await harness.emit(
      "sync-status-changed",
      buildSyncStatus("project_hokkaido", { state: "synced", lastSyncAt: "2026-08-09T12:05:00.000Z" }),
    );

    expect(await screen.findByRole("button", { name: /Synced/i })).toBeVisible();
  });

  it("ignores a status pushed for a project the user is not in", async () => {
    const harness = project().paired().syncStatus({ state: "offline" }).build();
    await renderApp(harness);
    await screen.findByRole("button", { name: /Offline/i });

    await harness.emit("sync-status-changed", buildSyncStatus("project_other", { state: "synced" }));

    expect(screen.getByRole("button", { name: /Offline/i })).toBeVisible();
  });

  it("syncs on demand and reports the result", async () => {
    const harness = project().paired().syncStatus({ state: "offline" }).build();
    const app = await renderApp(harness);

    await app.user.click(await screen.findByRole("button", { name: /Offline/i }));
    await app.user.click(await screen.findByRole("button", { name: "Sync now" }));

    await waitFor(() => expect(harness.state.sync.project_hokkaido.state).toBe("synced"));
    expect(harness.state.sync.project_hokkaido.lastSyncAt).toBe("2026-08-09T12:00:00.000Z");
  });

  it("survives a sync that fails, and succeeds on the retry", async () => {
    const harness = project().paired().syncStatus({ state: "offline" }).build();
    const app = await renderApp(harness);
    harness.next("sync_now").reject("the server is not reachable from this network");

    await app.user.click(await screen.findByRole("button", { name: /Offline/i }));
    const dialog = await screen.findByRole("dialog");
    await app.user.click(within(dialog).getByRole("button", { name: "Sync now" }));

    // The failure does not close the panel or lose the user's place.
    await waitFor(() => expect(harness.state.sync.project_hokkaido.state).toBe("offline"));
    expect(within(dialog).getByRole("button", { name: "Sync now" })).toBeVisible();

    await app.user.click(within(dialog).getByRole("button", { name: "Sync now" }));
    await waitFor(() => expect(harness.state.sync.project_hokkaido.state).toBe("synced"));
  });
});
