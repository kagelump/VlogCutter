/**
 * Understanding and reclaiming local storage, and removing a project (#16).
 *
 * The invariant every case below is really about is the same one: a lifecycle
 * action removes VlogCutter's own regenerable data, or VlogCutter's own record
 * of a project — and never a frame of the user's footage. The destructive path
 * is guarded twice, once by an audit that refuses a folder holding anything
 * else, and once by a confirmation that makes the user type the project's name.
 *
 * The harness has no filesystem. Usage is declared, and what is proven here is
 * what the UI does with it: which numbers it shows, what it says will stop
 * working, what it refuses to delete, and what the library looks like after.
 */

import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { desktopScenario, renderApp } from "../harness";

function twoProjects() {
  return desktopScenario({ now: "2026-08-20T12:00:00.000Z" })
    .project("Hokkaido field test", { id: "project_hokkaido" })
    .camera("Sony A7C II", { clips: 2, withProxy: true })
    .storageUsage(
      {
        proxies: { bytes: 12_000_000_000, fileCount: 240 },
        thumbnails: { bytes: 40_000_000, fileCount: 240 },
        analysisModels: { bytes: 3_000_000_000, fileCount: 4 },
      },
      2_048_000,
    )
    .project("Abandoned test", { id: "project_abandoned" })
    .camera("DJI Pocket 3", { clips: 1 })
    .storageUsage({ proxies: { bytes: 500_000_000, fileCount: 12 } }, 16_384)
    .build();
}

async function openSettings(app: Awaited<ReturnType<typeof renderApp>>) {
  await app.user.click(screen.getByRole("button", { name: /Settings/ }));
  return screen.findByRole("region", { name: "Project storage" });
}

async function openLibrary(app: Awaited<ReturnType<typeof renderApp>>) {
  await app.user.click(screen.getByRole("button", { name: "New project" }));
  return screen.findByRole("dialog", { name: /Open or create a working set/ });
}

describe("seeing what a project costs", () => {
  it("breaks usage down by category and says what each one costs to replace", async () => {
    const harness = twoProjects();
    const app = await renderApp(harness);
    const panel = await openSettings(app);

    await app.user.click(within(panel).getByRole("button", { name: "Measure" }));

    await waitFor(() => expect(within(panel).getByText(/12.0 GB/)).toBeVisible());
    expect(within(panel).getByText("Review proxies")).toBeVisible();
    expect(within(panel).getByText(/Remade from the originals/)).toBeVisible();
    expect(within(panel).getByText(/offline clips cannot be reviewed at all/)).toBeVisible();
    // The irreplaceable half is shown beside the rest, and is not selectable.
    expect(within(panel).getByText(/of your own work/)).toBeVisible();
    expect(within(panel).queryByRole("checkbox", { name: /your own work/ })).toBeNull();
    // A download is not the same cost as an ffmpeg run, and says so.
    expect(within(panel).getByText(/gigabytes over the network/)).toBeVisible();
    expect(within(panel).getAllByText(/shared by every project in this catalog/).length).toBeGreaterThan(0);
  });

  it("clears only what was chosen, leaves the catalog alone, and re-measures", async () => {
    const harness = twoProjects();
    const app = await renderApp(harness);
    const panel = await openSettings(app);
    await app.user.click(within(panel).getByRole("button", { name: "Measure" }));
    await waitFor(() => expect(within(panel).getByText(/12.0 GB/)).toBeVisible());

    await app.user.click(within(panel).getByRole("checkbox", { name: "Clear Review proxies" }));
    // Remaking proxies needs the media back; the panel says so before the click.
    expect(within(panel).getByText(/needs the original media connected again/)).toBeVisible();
    await app.user.click(within(panel).getByRole("button", { name: /^Clear 12.0 GB$/ }));

    await waitFor(() => expect(within(panel).getByText(/Freed 12.0 GB across 240 files/)).toBeVisible());
    const stored = harness.state.storage.project_hokkaido;
    expect(stored.proxies).toEqual({ bytes: 0, fileCount: 0 });
    expect(stored.thumbnails).toEqual({ bytes: 40_000_000, fileCount: 240 });
    expect(stored.analysisModels).toEqual({ bytes: 3_000_000_000, fileCount: 4 });
    // The user's own work is untouched: the project and its clips are still here.
    expect(harness.state.projects.map((project) => project.id)).toContain("project_hokkaido");
    expect(Object.values(harness.state.assets).filter((asset) => asset.projectId === "project_hokkaido")).toHaveLength(2);
  });

  it("clears with the media disconnected, because it never reads the footage", async () => {
    const harness = twoProjects();
    // Every original offline. Cleanup must not care.
    for (const asset of Object.values(harness.state.assets)) {
      harness.asset(asset.id).setOriginalAvailable(false);
    }
    const app = await renderApp(harness);
    const panel = await openSettings(app);
    await app.user.click(within(panel).getByRole("button", { name: "Measure" }));
    await waitFor(() => expect(within(panel).getByText(/12.0 GB/)).toBeVisible());

    await app.user.click(within(panel).getByRole("checkbox", { name: "Clear Review proxies" }));
    await app.user.click(within(panel).getByRole("button", { name: /^Clear 12.0 GB$/ }));

    await waitFor(() => expect(within(panel).getByText(/Freed 12.0 GB/)).toBeVisible());
    expect(within(panel).queryByRole("alert")).toBeNull();
  });
});

describe("deleting a project from the library", () => {
  it("draws the boundary before anything else, and separates lost from remakeable", async () => {
    const harness = twoProjects();
    const app = await renderApp(harness);
    const library = await openLibrary(app);

    await app.user.click(within(library).getByRole("button", { name: "Delete Abandoned test" }));

    const dialog = await screen.findByRole("dialog", { name: /Delete “Abandoned test”/ });
    expect(within(dialog).getByText(/Your footage is not touched/)).toBeVisible();
    expect(within(dialog).getByText(/Originals stay where they are/)).toBeVisible();
    expect(within(dialog).getByText(/None of this can be regenerated/)).toBeVisible();
    expect(within(dialog).getByText(/VlogCutter can make these again/)).toBeVisible();
  });

  it("will not delete until the project's own name is typed", async () => {
    const harness = twoProjects();
    const app = await renderApp(harness);
    const library = await openLibrary(app);
    await app.user.click(within(library).getByRole("button", { name: "Delete Abandoned test" }));
    const dialog = await screen.findByRole("dialog", { name: /Delete “Abandoned test”/ });

    const confirm = within(dialog).getByRole("button", { name: "Delete project" });
    expect(confirm).toBeDisabled();
    await app.user.type(within(dialog).getByRole("textbox"), "Abandoned");
    expect(confirm).toBeDisabled();
    await app.user.type(within(dialog).getByRole("textbox"), " test");
    expect(confirm).toBeEnabled();
  });

  it("removes the row immediately and leaves every other project intact", async () => {
    const harness = twoProjects();
    const app = await renderApp(harness);
    const library = await openLibrary(app);
    await app.user.click(within(library).getByRole("button", { name: "Delete Abandoned test" }));
    const dialog = await screen.findByRole("dialog", { name: /Delete “Abandoned test”/ });
    await app.user.type(within(dialog).getByRole("textbox"), "Abandoned test");
    await app.user.click(within(dialog).getByRole("button", { name: "Delete project" }));

    await waitFor(() => expect(screen.queryByRole("button", { name: "Delete Abandoned test" })).toBeNull());
    expect(screen.getByRole("button", { name: "Delete Hokkaido field test" })).toBeVisible();
    expect(harness.state.projects.map((project) => project.id)).toEqual(["project_hokkaido"]);
    expect(screen.getByText(/Deleted Abandoned test and its folder/)).toBeVisible();
  });

  it("keeps the folder, and says why, when something else is inside it", async () => {
    const harness = desktopScenario({ now: "2026-08-20T12:00:00.000Z" })
      .project("Hokkaido field test", { id: "project_hokkaido" })
      .camera("Sony A7C II", { clips: 1 })
      .project("Archive in here", { id: "project_archive" })
      .camera("DJI Pocket 3", { clips: 1 })
      .storageUsage({ proxies: { bytes: 1_000, fileCount: 1 } })
      .removalBlockedBy({ archiveRootsInside: ["NAS Video"], originalsInside: 42 })
      .build();
    const app = await renderApp(harness);
    const library = await openLibrary(app);
    await app.user.click(within(library).getByRole("button", { name: "Delete Archive in here" }));

    const dialog = await screen.findByRole("dialog", { name: /Delete “Archive in here”/ });
    const kept = within(dialog).getByRole("status");
    expect(kept).toHaveTextContent(/kept/);
    expect(kept).toHaveTextContent(/archive root “NAS Video”/);
    expect(kept).toHaveTextContent(/42 originals live inside it/);

    await app.user.type(within(dialog).getByRole("textbox"), "Archive in here");
    await app.user.click(within(dialog).getByRole("button", { name: "Delete project" }));
    await waitFor(() => expect(screen.getByText(/its folder was kept/)).toBeVisible());
  });

  it("names the other project that keeps the folder alive", async () => {
    const harness = twoProjects();
    harness.state.removalBlockers.project_abandoned = { otherProjectsInCatalog: ["Hokkaido field test"] };
    const app = await renderApp(harness);
    const library = await openLibrary(app);
    await app.user.click(within(library).getByRole("button", { name: "Delete Abandoned test" }));

    const dialog = await screen.findByRole("dialog", { name: /Delete “Abandoned test”/ });
    expect(within(dialog).getByRole("status")).toHaveTextContent(/“Hokkaido field test” shares the same catalog/);
  });

  it("backs out without touching anything", async () => {
    const harness = twoProjects();
    const app = await renderApp(harness);
    const library = await openLibrary(app);
    await app.user.click(within(library).getByRole("button", { name: "Delete Abandoned test" }));
    const dialog = await screen.findByRole("dialog", { name: /Delete “Abandoned test”/ });

    await app.user.click(within(dialog).getByRole("button", { name: "Keep project" }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: /Delete “/ })).toBeNull());
    expect(harness.state.projects).toHaveLength(2);
    expect(harness.calls("delete_project")).toHaveLength(0);
  });
});
