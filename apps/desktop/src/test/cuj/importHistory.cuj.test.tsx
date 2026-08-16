/**
 * CUJ: undoing an import made in an earlier session.
 *
 * "I pointed it at the wrong folder last week" is the one import outcome the
 * user cannot fix by importing again, and until now the whole path was
 * untested: `openImportHistory`, `openImportSession`, `previewImportRollback`
 * and `rollbackImportSession` in `App.tsx` had no coverage, and
 * `ImportHistoryDialog` sat at 7.7% under the CUJ tier.
 *
 * The property that matters is the one the feature promises in its own copy:
 * removing an import removes *memberships*, and leaves the originals and the
 * archive copies alone. A rollback that quietly deleted footage would be the
 * worst defect this application could ship, so the preview step is asserted to
 * be genuinely read-only rather than merely labelled as one.
 *
 * The history is seeded as durable state rather than produced by running an
 * import in this test, because that is the real situation: the app was
 * restarted, the in-memory report is gone, and the only record is the catalog's.
 */

import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type { ImportSessionDetail } from "../../types";
import { desktopScenario, queueFilenames, renderApp } from "../harness";

const PROJECT = "project_hokkaido";
const dji = ["asset_dji_pocket_3_001", "asset_dji_pocket_3_002"];

/** The session record an import from a previous run left behind. */
function priorImport(overrides: Partial<ImportSessionDetail> = {}): ImportSessionDetail {
  return {
    id: "import_1",
    projectId: PROJECT,
    sourcePath: "/Volumes/DJI_SD",
    destinationMode: "archive_copy",
    destinationRootId: "archive_nas_video",
    destinationName: "NAS Video",
    destinationPath: "/Volumes/NAS/Video/Hokkaido",
    imported: 2,
    relinked: 0,
    skipped: 1,
    copied: 2,
    bytesCopied: 2_048_000,
    cancelled: false,
    failures: [],
    rolledBackAt: null,
    rolledBackAssetCount: 0,
    createdAt: "2026-08-10T12:00:00.000Z",
    files: [
      ...dji.map((assetId, index) => ({
        assetId,
        path: `/Volumes/DJI_SD/DJI_00${index + 1}.MP4`,
        filename: `DJI_00${index + 1}.MP4`,
        classification: "new",
        result: "completed",
        timestamp: null,
        durationSeconds: null,
        error: null,
      })),
      {
        assetId: "asset_sony_a7c_ii_001",
        path: "/Volumes/DJI_SD/SONY_001.MP4",
        filename: "SONY_001.MP4",
        classification: "known",
        result: "left_alone",
        timestamp: null,
        durationSeconds: null,
        error: null,
      },
    ],
    ...overrides,
  };
}

/** A project whose two DJI clips arrived in an import made before this launch. */
function withPriorImport(session: ImportSessionDetail = priorImport()) {
  return desktopScenario({ now: "2026-08-14T09:00:00.000Z" })
    .project("Hokkaido field test", { id: PROJECT })
    .camera("Sony A7C II", { clips: 1 })
    .camera("DJI Pocket 3", { clips: 2 })
    .tap((state) => {
      state.imports.sessions[session.id] = session;
    })
    .build();
}

async function openHistory(app: Awaited<ReturnType<typeof renderApp>>) {
  await app.user.click(screen.getByRole("button", { name: "More media generation actions" }));
  await app.user.click(await screen.findByRole("button", { name: /Review last import/ }));
  return screen.findByRole("dialog", { name: "Imports" });
}

async function openSession(app: Awaited<ReturnType<typeof renderApp>>, dialog: HTMLElement) {
  await app.user.click(within(dialog).getByRole("button", { name: /DJI_SD/ }));
  return within(dialog).findByText(/2 clips added/);
}

describe("reviewing imports from an earlier session", () => {
  it("lists the project's import history when there is no report in hand", async () => {
    const harness = withPriorImport();
    const app = await renderApp(harness);
    harness.clearLog();

    const dialog = await openHistory(app);

    // Reached the durable history, not the in-memory report of a run that
    // happened during this launch — there wasn't one.
    expect(harness.calls("list_import_sessions")).toEqual([{ projectId: PROJECT }]);
    expect(within(dialog).getByText(/2 added/)).toBeVisible();
    expect(within(dialog).getByText(/\/Volumes\/DJI_SD/)).toBeVisible();
  });

  it("opens one session's ledger, naming every file and what happened to it", async () => {
    const harness = withPriorImport();
    const app = await renderApp(harness);
    const dialog = await openHistory(app);

    await openSession(app, dialog);

    expect(harness.calls("import_session")[0]).toEqual({ projectId: PROJECT, sessionId: "import_1" });
    expect(within(dialog).getByText(/Copied to NAS Video/)).toBeVisible();
    const ledger = within(dialog).getByRole("group");
    expect(within(ledger).getByText(/DJI_001\.MP4 — completed/)).toBeVisible();
    // The clip the import recognised and left alone is in the ledger too:
    // "nothing happened to this one" is a result worth showing.
    expect(within(ledger).getByText(/SONY_001\.MP4 — left_alone/)).toBeVisible();
  });
});

describe("removing an import from the project", () => {
  it("previews without removing anything", async () => {
    const harness = withPriorImport();
    const app = await renderApp(harness);
    const dialog = await openHistory(app);
    await openSession(app, dialog);
    const before = JSON.stringify(harness.state.assets);

    await app.user.click(within(dialog).getByRole("button", { name: /Remove from project…/ }));

    await within(dialog).findByText(/Remove 2 clip memberships\?/);
    expect(harness.calls("import_rollback_preview")[0]).toEqual({
      projectId: PROJECT,
      sessionId: "import_1",
    });
    // The step that asks the question must not also answer it: the catalog is
    // byte-identical, the queue is untouched, and no write command was issued.
    expect(JSON.stringify(harness.state.assets)).toBe(before);
    expect(queueFilenames()).toHaveLength(3);
    expect(harness.commandNames()).not.toContain("rollback_import_membership");
  });

  it("says the originals survive, and only removes memberships when confirmed", async () => {
    const harness = withPriorImport();
    const app = await renderApp(harness);
    const dialog = await openHistory(app);
    await openSession(app, dialog);
    await app.user.click(within(dialog).getByRole("button", { name: /Remove from project…/ }));
    expect(within(dialog).getByText(/Original files and archive copies stay untouched\./)).toBeVisible();

    await app.user.click(within(dialog).getByRole("button", { name: /Confirm remove from project/ }));

    await waitFor(() => expect(queueFilenames()).toEqual(["SONY_001.MP4"]));
    expect(harness.calls("rollback_import_membership")[0]).toEqual({
      projectId: PROJECT,
      sessionId: "import_1",
    });
    // Only the clips this session added. The clip it recognised and left alone
    // was in the project beforehand and stays.
    expect(Object.keys(harness.state.assets)).toEqual(["asset_sony_a7c_ii_001"]);
    expect(await screen.findByText(/original media was left untouched/i)).toBeVisible();
  });

  it("refreshes the queue and the history from the backend rather than guessing", async () => {
    const harness = withPriorImport();
    const app = await renderApp(harness);
    const dialog = await openHistory(app);
    await openSession(app, dialog);
    await app.user.click(within(dialog).getByRole("button", { name: /Remove from project…/ }));
    harness.clearLog();

    await app.user.click(within(dialog).getByRole("button", { name: /Confirm remove from project/ }));

    // After a destructive write the app re-reads: the queue, the history list,
    // and the session it is showing. A local edit of React state would look
    // identical here and be wrong the moment the backend disagreed.
    await waitFor(() => expect(harness.commandNames()).toContain("import_session"));
    expect(harness.commandNames()).toContain("list_assets");
    expect(harness.commandNames()).toContain("list_import_sessions");
  });

  it("shows an already-removed session as done and offers no second removal", async () => {
    const harness = withPriorImport(
      priorImport({ rolledBackAt: "2026-08-12T08:00:00.000Z", rolledBackAssetCount: 2 }),
    );
    const app = await renderApp(harness);
    const dialog = await openHistory(app);

    await openSession(app, dialog);

    expect(within(dialog).getByText(/memberships were removed/)).toBeVisible();
    expect(within(dialog).queryByRole("button", { name: /Remove from project…/ })).toBeNull();
    expect(within(dialog).queryByRole("button", { name: /Confirm remove/ })).toBeNull();
  });

  it("leaves the project alone when the dialog is closed mid-preview", async () => {
    const harness = withPriorImport();
    const app = await renderApp(harness);
    const dialog = await openHistory(app);
    await openSession(app, dialog);
    await app.user.click(within(dialog).getByRole("button", { name: /Remove from project…/ }));

    await app.user.click(within(dialog).getByRole("button", { name: "Done" }));

    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Imports" })).toBeNull());
    expect(harness.commandNames()).not.toContain("rollback_import_membership");
    expect(queueFilenames()).toHaveLength(3);
  });

  it("starts a reopened history at the list, with no preview carried over", async () => {
    const harness = withPriorImport();
    const app = await renderApp(harness);
    const first = await openHistory(app);
    await openSession(app, first);
    await app.user.click(within(first).getByRole("button", { name: /Remove from project…/ }));
    await app.user.click(within(first).getByRole("button", { name: "Done" }));

    const second = await openHistory(app);

    // A confirmation left armed behind a closed dialog is one stray click from
    // a removal the user never asked for.
    expect(within(second).queryByText(/Remove 2 clip memberships\?/)).toBeNull();
    expect(within(second).getByText(/2 added/)).toBeVisible();
  });
});

describe("when the backend refuses", () => {
  it("says so on the main banner when the history itself cannot be read", async () => {
    const harness = withPriorImport();
    const app = await renderApp(harness);
    harness.next("list_import_sessions").reject("catalog is locked by another process");

    await app.user.click(screen.getByRole("button", { name: "More media generation actions" }));
    await app.user.click(await screen.findByRole("button", { name: /Review last import/ }));

    // No dialog opened, so the failure has nowhere else to go. A click that
    // silently does nothing is the defect being prevented here.
    expect(await screen.findByText(/catalog is locked by another process/)).toBeVisible();
    expect(screen.queryByRole("dialog", { name: "Imports" })).toBeNull();
  });

  it("reports a ledger that will not open, inside the dialog that stayed up", async () => {
    const harness = withPriorImport();
    const app = await renderApp(harness);
    const dialog = await openHistory(app);
    harness.next("import_session").reject("session record is unreadable");

    await app.user.click(within(dialog).getByRole("button", { name: /DJI_SD/ }));

    expect(await within(dialog).findByRole("alert")).toHaveTextContent(/session record is unreadable/);
  });

  it("reports a preview that failed rather than showing no confirmation", async () => {
    const harness = withPriorImport();
    const app = await renderApp(harness);
    const dialog = await openHistory(app);
    await openSession(app, dialog);
    harness.next("import_rollback_preview").reject("project was closed");

    await app.user.click(within(dialog).getByRole("button", { name: /Remove from project…/ }));

    expect(await within(dialog).findByRole("alert")).toHaveTextContent(/project was closed/);
    expect(within(dialog).queryByText(/Remove 2 clip memberships\?/)).toBeNull();
  });

  it("never lets a failed rollback read as a completed one", async () => {
    const harness = withPriorImport();
    const app = await renderApp(harness);
    const dialog = await openHistory(app);
    await openSession(app, dialog);
    await app.user.click(within(dialog).getByRole("button", { name: /Remove from project…/ }));
    harness.next("rollback_import_membership").reject("membership write rejected");

    await app.user.click(within(dialog).getByRole("button", { name: /Confirm remove from project/ }));

    // The one operation where silence is least acceptable: the user has to be
    // able to tell "it failed" from "it removed nothing visible".
    expect(await within(dialog).findByRole("alert")).toHaveTextContent(/membership write rejected/);
    expect(screen.queryByText(/original media was left untouched/i)).toBeNull();
    expect(queueFilenames()).toHaveLength(3);
  });

  it("lets the same rollback be retried once the backend recovers", async () => {
    const harness = withPriorImport();
    const app = await renderApp(harness);
    const dialog = await openHistory(app);
    await openSession(app, dialog);
    await app.user.click(within(dialog).getByRole("button", { name: /Remove from project…/ }));
    harness.next("rollback_import_membership").reject("membership write rejected");
    await app.user.click(within(dialog).getByRole("button", { name: /Confirm remove from project/ }));
    await within(dialog).findByRole("alert");

    // The fault was one-shot: the confirmation is still armed, and pressing it
    // again runs the real write.
    await app.user.click(within(dialog).getByRole("button", { name: /Confirm remove from project/ }));

    await waitFor(() => expect(queueFilenames()).toEqual(["SONY_001.MP4"]));
    expect(within(dialog).queryByRole("alert")).toBeNull();
  });
});

describe("the detected-card banner", () => {
  it("stays dismissed for that card rather than reappearing on the next poll", async () => {
    const harness = desktopScenario({ now: "2026-08-14T09:00:00.000Z" })
      .project("Hokkaido field test", { id: PROJECT })
      .camera("Sony A7C II", { clips: 1 })
      .cardInserted("DJI_SD", { clips: 2, newClips: 2, camera: "DJI Pocket 3" })
      .build();
    const app = await renderApp(harness);
    const banner = await screen.findByRole("complementary", { name: "Camera card detected" });

    await app.user.click(within(banner).getByRole("button", { name: "Dismiss DJI_SD" }));

    await waitFor(() =>
      expect(screen.queryByRole("complementary", { name: "Camera card detected" })).toBeNull(),
    );
    // The card is still in the slot and still detected; the dismissal is per
    // card session, so a later detection of the same card must stay quiet.
    await harness.emit("cards-detected", harness.state.cards);
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(screen.queryByRole("complementary", { name: "Camera card detected" })).toBeNull();
  });
});
