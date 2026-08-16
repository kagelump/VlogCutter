/**
 * CUJ: Escape, across every surface that can be on screen.
 *
 * Escape used to be resolved in two places that did not know about each other,
 * and the surfaces neither one knew about got whatever fell out. The property
 * that matters is not "Escape closes things" but "Escape closes *one* thing,
 * the one on top" — the old handler cleared four flags at once, so dismissing a
 * dialog could also drop the user out of Focus, and a running import could be
 * left by a keystroke that was aimed at the dialog in front of it.
 *
 * These cases press one key and then assert what is still there, which is the
 * half the previous implementation got wrong.
 */

import { act, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type { ImportSessionDetail } from "../../types";
import { desktopScenario, renderApp } from "../harness";

const PROJECT = "project_hokkaido";

function project() {
  return desktopScenario({ now: "2026-08-09T12:00:00.000Z" })
    .project("Hokkaido field test", { id: PROJECT })
    .camera("Sony A7C II", { clips: 2, withProxy: true });
}

function cardInSlot() {
  return project()
    .archiveRoot("NAS Video", { availableBytes: 1_200_000_000_000 })
    .cardInserted("DJI_SD", { clips: 3, newClips: 3, camera: "DJI Pocket 3", cameraHint: "DJI Pocket 3" });
}

/** The record an import from a previous run left behind, so history has content. */
function priorImport(): ImportSessionDetail {
  return {
    id: "import_1", projectId: PROJECT, sourcePath: "/Volumes/DJI_SD",
    destinationMode: "reference", destinationRootId: "", destinationName: "",
    destinationPath: "", imported: 2, relinked: 0, skipped: 0, copied: 0, bytesCopied: 0,
    cancelled: false, failures: [], rolledBackAt: null, rolledBackAssetCount: 0,
    createdAt: "2026-08-08T12:00:00.000Z", files: [],
  };
}

const inFocus = () => document.querySelector(".app-shell")?.classList.contains("is-focus") ?? false;

async function enterFocus(app: Awaited<ReturnType<typeof renderApp>>) {
  const notes = screen.getByLabelText("Notes");
  await act(async () => { notes.blur(); });
  await app.user.keyboard("f");
  await waitFor(() => expect(inFocus()).toBe(true));
  // Focus puts the caret in the notes field; leave it so the review shortcuts
  // are reachable, exactly as they are for a user who has stopped typing.
  await act(async () => { (document.activeElement as HTMLElement | null)?.blur(); });
}

describe("Escape closes exactly what is on top", () => {
  it("closes a modal opened over Focus and leaves Focus running", async () => {
    const app = await renderApp(project().build());
    await enterFocus(app);
    await app.user.keyboard("?");
    await screen.findByRole("dialog", { name: /Review without leaving the keys/ });

    await app.user.keyboard("{Escape}");

    await waitFor(() => expect(screen.queryByRole("dialog", { name: /Review without leaving the keys/ })).toBeNull());
    // The old handler cleared the modal flags and then fell through to Focus,
    // so one key did two things.
    expect(inFocus()).toBe(true);
  });

  it("takes the wizard off Settings before it takes Settings", async () => {
    const app = await renderApp(project().build());
    await app.user.keyboard("{Meta>},{/Meta}");
    const settings = await screen.findByRole("dialog", { name: "Review behavior" });
    await app.user.click(within(settings).getByRole("button", { name: "Connect to server…" }));
    await screen.findByRole("dialog", { name: "Enter server address" });

    await app.user.keyboard("{Escape}");

    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Enter server address" })).toBeNull());
    // Two surfaces really are stacked here; the one underneath stays.
    expect(screen.getByRole("dialog", { name: "Review behavior" })).toBeVisible();

    await app.user.keyboard("{Escape}");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Review behavior" })).toBeNull());
  });

  it("leaves Focus when nothing is over it", async () => {
    const app = await renderApp(project().build());
    await enterFocus(app);

    await app.user.keyboard("{Escape}");

    await waitFor(() => expect(inFocus()).toBe(false));
  });

  it("returns to Review from the chronology desk", async () => {
    const app = await renderApp(project().build());
    await app.user.click(screen.getByRole("button", { name: "Chronology" }));
    await screen.findByRole("region", { name: "Chronology calibration" });

    await app.user.keyboard("{Escape}");

    await waitFor(() =>
      expect(screen.queryByRole("region", { name: "Chronology calibration" })).toBeNull(),
    );
    expect(screen.getByRole("listbox", { name: "Clips" })).toBeVisible();
  });

  it("closes the import history rather than the mode behind it", async () => {
    const app = await renderApp(project().tap((state) => {
      state.imports.sessions.import_1 = priorImport();
    }).build());
    await app.user.click(screen.getByRole("button", { name: "Chronology" }));
    await screen.findByRole("region", { name: "Chronology calibration" });
    await app.user.click(screen.getByRole("button", { name: "More media generation actions" }));
    await app.user.click(await screen.findByRole("button", { name: /Review last import/ }));
    await screen.findByRole("dialog", { name: "Imports" });

    await app.user.keyboard("{Escape}");

    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Imports" })).toBeNull());
    expect(screen.getByRole("region", { name: "Chronology calibration" })).toBeVisible();
  });
});

describe("Escape when the surface on top refuses", () => {
  it("does not abandon a baseline upload in flight, and does not leave Focus instead", async () => {
    const harness = desktopScenario({ now: "2026-08-14T09:00:00.000Z" })
      .project("Hokkaido field test", { id: PROJECT })
      .camera("Sony A7C II", { clips: 2, withProxy: true })
      .paired({ label: "Studio server" })
      .build();
    const app = await renderApp(harness);
    await enterFocus(app);
    await app.user.click(await screen.findByRole("button", { name: "Pair with server" }));
    const wizard = await screen.findByRole("dialog", { name: /Step 1: Map media paths/ });
    await app.user.click(within(wizard).getByRole("button", { name: "Preview baseline" }));
    await screen.findByRole("dialog", { name: /Step 2: Review baseline preview/ });
    harness.command("confirm_baseline_import").delayUntilReleased();
    await app.user.click(within(wizard).getByRole("button", { name: /Confirm & upload baseline/ }));

    await app.user.keyboard("{Escape}");

    // An upload half-made is not something a keystroke gets to abandon — and
    // the key must not travel past the wizard to something it was never aimed
    // at, which is what the previous handler did with everything it did not
    // know about.
    expect(screen.getByRole("dialog", { name: "Uploading baseline…" })).toBeVisible();
    expect(inFocus()).toBe(true);
    expect(harness.commandNames()).not.toContain("abandon_baseline_attempt");
    harness.release("confirm_baseline_import");
  });

  it("keeps the project picker up when there is no project to go back to", async () => {
    const harness = desktopScenario({ now: "2026-08-09T12:00:00.000Z" }).build();
    const app = await renderApp(harness);
    const picker = await screen.findByRole("dialog");

    await app.user.keyboard("{Escape}");

    expect(picker).toBeVisible();
  });
});
