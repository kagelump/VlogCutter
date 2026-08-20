/**
 * CUJ: getting into a working set — first launch, the library, and catalogs
 * that live outside the projects folder.
 *
 * `createProject`, `openLibraryProject`, `configureProjectsRoot` and
 * `openCatalog` in `App.tsx` had no coverage: every existing CUJ starts with a
 * project already open, so the four ways a user *reaches* one were only ever
 * exercised by hand. `NewProjectModal` sat at 1.75% under the CUJ tier.
 *
 * The native picker itself is not under test and cannot be — `plugin:dialog|open`
 * is a declared seam, and whether macOS shows a folder chooser stays a native
 * acceptance item. What is under test is everything the app does with the
 * answer, including the answer "the user pressed cancel", which is the branch
 * that silently does the wrong thing when it is missed.
 */

import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { desktopScenario, queueFilenames, renderApp } from "../harness";

const PROJECT = "project_hokkaido";

/** First launch: a projects folder with nothing in it. */
function emptyLibrary() {
  return desktopScenario({ now: "2026-08-14T09:00:00.000Z" }).build();
}

/** A projects folder holding one project, with another already open. */
function populatedLibrary() {
  return desktopScenario({ now: "2026-08-14T09:00:00.000Z" })
    .project("Hokkaido field test", { id: PROJECT })
    .camera("Sony A7C II", { clips: 2 })
    .build();
}

function libraryModal() {
  return screen.findByRole("dialog", { name: "Open or create a working set" });
}

async function openLibraryModal(app: Awaited<ReturnType<typeof renderApp>>) {
  await app.user.click(screen.getByRole("button", { name: "New project" }));
  return libraryModal();
}

describe("first launch", () => {
  it("opens the library by itself when the folder holds no projects", async () => {
    const harness = emptyLibrary();
    await renderApp(harness);

    const modal = await libraryModal();

    expect(within(modal).getByText("No VlogCutter projects found in this folder.")).toBeVisible();
    // Nothing to go back to, so there is no way to dismiss it into an empty app.
    expect(within(modal).queryByRole("button", { name: "Cancel" })).toBeNull();
  });

  it("creates a project, opens it, and leaves the library behind", async () => {
    const harness = emptyLibrary();
    const app = await renderApp(harness);
    const modal = await libraryModal();

    await app.user.type(within(modal).getByRole("textbox"), "Hokkaido winter 2026");
    await app.user.click(within(modal).getByRole("button", { name: "Create project" }));

    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(harness.calls("create_project")).toEqual([{ name: "Hokkaido winter 2026" }]);
    expect(harness.state.projects.map((project) => project.name)).toEqual(["Hokkaido winter 2026"]);
    // The created project is also the open one, which is what the empty queue
    // prompt reports. "Project created" is a transient toast this state
    // replaces immediately; the destination is the assertion worth making.
    expect(await screen.findByText("Choose a media folder to begin")).toBeVisible();
    expect(queueFilenames()).toEqual([]);
  });

  it("will not create an unnamed project", async () => {
    const harness = emptyLibrary();
    const app = await renderApp(harness);
    const modal = await libraryModal();

    // Whitespace is not a name. The button stays disabled rather than creating
    // a project nobody can identify later.
    await app.user.type(within(modal).getByRole("textbox"), "   ");
    expect(within(modal).getByRole("button", { name: "Create project" })).toBeDisabled();
    expect(harness.commandNames()).not.toContain("create_project");
  });

  it("creates on Enter as well as on the button", async () => {
    const harness = emptyLibrary();
    const app = await renderApp(harness);
    const modal = await libraryModal();

    await app.user.type(within(modal).getByRole("textbox"), "Nagano spring{Enter}");

    await waitFor(() => expect(harness.calls("create_project")).toHaveLength(1));
    expect(harness.calls("create_project")[0]).toEqual({ name: "Nagano spring" });
  });

  it("reports a rejected creation without closing the library", async () => {
    // No id override, so the new project's derived id genuinely collides with
    // this one's — the same clash a user hits by reusing a trip name.
    const harness = desktopScenario({ now: "2026-08-14T09:00:00.000Z" })
      .project("Hokkaido field test")
      .camera("Sony A7C II", { clips: 2 })
      .build();
    const app = await renderApp(harness);
    const modal = await openLibraryModal(app);

    await app.user.type(within(modal).getByRole("textbox"), "Hokkaido field test");
    await app.user.click(within(modal).getByRole("button", { name: "Create project" }));

    expect(await screen.findByRole("alert")).toHaveTextContent(/already exists/);
    expect(await libraryModal()).toBeVisible();
    expect(harness.state.projects).toHaveLength(1);
  });
});

describe("opening a project from the library", () => {
  it("loads that project's own catalog, not merely the id", async () => {
    const harness = populatedLibrary();
    const app = await renderApp(harness);
    const modal = await openLibraryModal(app);
    harness.clearLog();

    // The row carries two controls now: open, and delete (#16).
    await app.user.click(within(modal).getByRole("button", { name: "Open Hokkaido field test" }));

    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    // Each project owns a catalog file; opening one by id alone would read the
    // wrong database the moment two projects live in different folders.
    expect(harness.calls("load_catalog")).toEqual([
      { path: "/Users/harness/Movies/VideoProjects/Hokkaido field test/catalog.sqlite3" },
    ]);
    expect(harness.state.catalogPath).toContain("Hokkaido field test");
    expect(await screen.findByText("Loaded Hokkaido field test")).toBeVisible();
  });

  it("keeps the library open and says so when the catalog will not load", async () => {
    const harness = populatedLibrary();
    harness.faults.always("load_catalog", { kind: "reject", message: "catalog is locked by another process" });
    const app = await renderApp(harness);
    const modal = await openLibraryModal(app);

    // The row carries two controls now: open, and delete (#16).
    await app.user.click(within(modal).getByRole("button", { name: "Open Hokkaido field test" }));

    // The failure reaches the user as an alert, which outranks the status
    // toast the handler also sets — so the alert is what has to carry it.
    expect(await screen.findByRole("alert")).toHaveTextContent(/catalog is locked by another process/);
    expect(await libraryModal()).toBeVisible();
  });
});

describe("choosing where projects live", () => {
  it("moves the library to the folder the picker returned", async () => {
    const harness = populatedLibrary();
    harness.state.dialogResponses.push("/Volumes/Travel SSD/Projects");
    const app = await renderApp(harness);
    const modal = await openLibraryModal(app);

    await app.user.click(within(modal).getByRole("button", { name: "Change…" }));

    await waitFor(() =>
      expect(harness.calls("set_projects_root")).toEqual([{ path: "/Volumes/Travel SSD/Projects" }]),
    );
    expect(await screen.findByText(/Default project folder: \/Volumes\/Travel SSD\/Projects/)).toBeVisible();
    expect(harness.state.settings.projectsRoot).toBe("/Volumes/Travel SSD/Projects");
  });

  it("changes nothing when the picker is dismissed", async () => {
    const harness = populatedLibrary();
    const rootBefore = harness.state.settings.projectsRoot;
    const app = await renderApp(harness);
    const modal = await openLibraryModal(app);

    // No queued answer: the user pressed Cancel.
    await app.user.click(within(modal).getByRole("button", { name: "Change…" }));

    await waitFor(() => expect(harness.commandNames()).toContain("plugin:dialog|open"));
    expect(harness.commandNames()).not.toContain("set_projects_root");
    expect(harness.state.settings.projectsRoot).toBe(rootBefore);
  });
});

describe("opening a catalog from outside the projects folder", () => {
  it("loads it and selects its first project", async () => {
    const harness = populatedLibrary();
    harness.state.dialogResponses.push("/Volumes/Archive/2026/catalog.sqlite3");
    const app = await renderApp(harness);
    harness.clearLog();

    await app.user.click(screen.getByRole("button", { name: "Open catalog" }));

    await waitFor(() =>
      expect(harness.calls("load_catalog")).toEqual([{ path: "/Volumes/Archive/2026/catalog.sqlite3" }]),
    );
    expect(await screen.findByText(/Loaded 1 project from \/Volumes\/Archive\/2026/)).toBeVisible();
    expect(queueFilenames()).toHaveLength(2);
  });

  it("offers to create the first project when the catalog is empty", async () => {
    const harness = emptyLibrary();
    harness.state.dialogResponses.push("/Volumes/Archive/blank/catalog.sqlite3");
    const app = await renderApp(harness);
    const modal = await libraryModal();

    await app.user.click(within(modal).getByRole("button", { name: "Open existing project…" }));

    expect(await screen.findByText(/is empty; create its first project/)).toBeVisible();
    expect(await libraryModal()).toBeVisible();
  });

  it("changes nothing when the catalog picker is dismissed", async () => {
    const harness = populatedLibrary();
    const app = await renderApp(harness);
    harness.clearLog();

    await app.user.click(screen.getByRole("button", { name: "Open catalog" }));

    await waitFor(() => expect(harness.commandNames()).toContain("plugin:dialog|open"));
    expect(harness.commandNames()).not.toContain("load_catalog");
    expect(queueFilenames()).toHaveLength(2);
  });

  it("says so when the chosen catalog cannot be opened", async () => {
    const harness = populatedLibrary();
    harness.state.dialogResponses.push("/Volumes/Archive/corrupt/catalog.sqlite3");
    harness.faults.always("load_catalog", { kind: "reject", message: "file is not a database" });
    const app = await renderApp(harness);

    await app.user.click(screen.getByRole("button", { name: "Open catalog" }));

    expect(await screen.findByRole("alert")).toHaveTextContent(/file is not a database/);
    // The project that was open stays open; a failed catalog switch is not a
    // reason to leave the user with nothing.
    expect(queueFilenames()).toHaveLength(2);
  });
});
