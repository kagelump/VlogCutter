import { act, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ProjectReviewAsset } from "@rcdvlog/project-client";
import { renderApp } from "../../test/render";

function asset(id: string, overrides: Partial<ProjectReviewAsset> = {}): ProjectReviewAsset {
  return {
    projectAssetId: id, filename: `${id}.MP4`, camera: "DJI Pocket 3", mediaObjectId: null, mediaVerificationState: "verified",
    removed: false, rotationDegrees: 0, labels: [], activeNote: null, description: null, durationSeconds: 31, bestTime: null,
    materializedRevision: 4, activeNoteRevisionId: null, noteRevisionCount: 0, localProxyAvailable: false, proxy: null, thumbnail: null,
    originalAvailability: "accessible_on_another_node", unresolvedConflictCount: 0, ...overrides,
  };
}

function setup(assets = [asset("clip_a"), asset("clip_b"), asset("clip_c")]) {
  const byId = new Map(assets.map((item) => [item.projectAssetId, item]));
  return renderApp("/projects/p1", {
    listProjectAssets: vi.fn().mockResolvedValue({ items: assets, nextCursor: null }),
    getProjectReviewAsset: vi.fn(async (_project: string, id: string) => byId.get(id)!),
    createReviewNote: vi.fn(async (_p: string, id: string, text: string) => ({ asset: { ...byId.get(id)!, activeNote: text, materializedRevision: 5 }, staleBaseRevision: false })),
    addReviewLabel: vi.fn(async (_p: string, id: string, label: string) => ({ asset: { ...byId.get(id)!, labels: [label], materializedRevision: 5 }, staleBaseRevision: false })),
    removeReviewLabel: vi.fn(),
    setReviewRemoved: vi.fn(async (_p: string, id: string, removed: boolean) => ({ asset: { ...byId.get(id)!, removed, materializedRevision: 5 }, staleBaseRevision: false })),
    setReviewRotation: vi.fn(),
  });
}

test("Enter saves the note against the clip's current revision", async () => {
  const { client } = setup();
  expect(await screen.findByRole("heading", { name: "clip_a.MP4" })).toBeVisible();
  const note = screen.getByLabelText(/^Note/);
  await userEvent.type(note, "Use the second half{Enter}");
  await waitFor(() => expect(client.createReviewNote).toHaveBeenCalledWith("p1", "clip_a", "Use the second half", 4));
  expect(await screen.findByText("Saved")).toBeVisible();
  expect(note).toHaveValue("Use the second half");
});

test("Shift+Enter adds a line instead of saving, and Escape discards the draft", async () => {
  const { client } = setup([asset("clip_a", { activeNote: "Keep" })]);
  const note = await screen.findByLabelText(/^Note/);
  await userEvent.type(note, "{Shift>}{Enter}{/Shift}more");
  expect(note).toHaveValue("Keep\nmore");
  expect(client.createReviewNote).not.toHaveBeenCalled();
  await userEvent.keyboard("{Escape}");
  expect(note).toHaveValue("Keep");
});

test("a background refresh never overwrites a note being typed", async () => {
  const { queryClient } = setup();
  const note = await screen.findByLabelText(/^Note/);
  await userEvent.type(note, "half-written");
  await act(() => queryClient.invalidateQueries());
  expect(note).toHaveValue("half-written");
  expect(screen.getByRole("button", { name: "Save note" })).toBeVisible();
});

test("J and K move through the queue and the URL follows the clip", async () => {
  const { router, client } = setup();
  await screen.findByRole("heading", { name: "clip_a.MP4" });
  await userEvent.keyboard("j");
  expect(await screen.findByRole("heading", { name: "clip_b.MP4" })).toBeVisible();
  expect(router.state.location.search).toBe("?clip=clip_b");
  await userEvent.keyboard("jj");
  expect(await screen.findByRole("heading", { name: "clip_c.MP4" })).toBeVisible();
  await userEvent.keyboard("k");
  expect(await screen.findByRole("heading", { name: "clip_b.MP4" })).toBeVisible();
  expect(client.getProjectReviewAsset).toHaveBeenCalledWith("p1", "clip_c");
});

test("shortcuts are ignored while typing in a field", async () => {
  const { client } = setup();
  await userEvent.type(await screen.findByLabelText("Add label"), "jr");
  expect(screen.getByRole("heading", { name: "clip_a.MP4" })).toBeVisible();
  expect(client.setReviewRemoved).not.toHaveBeenCalled();
});

test("labels and removal update the clip and its queue entry", async () => {
  const { client } = setup();
  await userEvent.type(await screen.findByLabelText("Add label"), "penguins{Enter}");
  expect(await screen.findByRole("button", { name: "Remove label penguins" })).toBeVisible();
  expect(client.addReviewLabel).toHaveBeenCalledWith("p1", "clip_a", "penguins", 4);
  await userEvent.keyboard("{Escape}r");
  await waitFor(() => expect(client.setReviewRemoved).toHaveBeenCalledWith("p1", "clip_a", true, 5));
  expect(await screen.findByRole("button", { name: "Restore clip" })).toBeVisible();
});

test("the clip list opens as a sheet on narrow screens", async () => {
  setup();
  await screen.findByRole("heading", { name: "clip_a.MP4" });
  await userEvent.click(screen.getByRole("button", { name: "All clips" }));
  const sheet = await screen.findByRole("dialog", { name: "Clips (3)" });
  await userEvent.click(within(sheet).getByRole("button", { name: /clip_c\.MP4/ }));
  expect(await screen.findByRole("heading", { name: "clip_c.MP4" })).toBeVisible();
  expect(screen.queryByRole("dialog")).toBeNull();
});

test("a clip without a server preview says why", async () => {
  setup([asset("clip_a", { proxy: { representationId: "rep", state: "available", availableOnServer: false } })]);
  expect(await screen.findByText("Preview is on another device")).toBeVisible();
});


test("an attached CLI proxy plays without a server media-library representation", async () => {
  const { container } = setup([asset("clip_a", { localProxyAvailable: true, rotationDegrees: 90 })]);
  await screen.findByRole("heading", { name: "clip_a.MP4" });
  const video = container.querySelector("video")!;
  expect(video).toHaveAttribute("src", "/api/v1/projects/p1/assets/clip_a/proxy");
  expect(video).toHaveAttribute("data-rotation", "90");
  expect(screen.queryByText("No preview")).toBeNull();
});

test("unknown original availability is not presented as unreachable", async () => {
  setup([asset("clip_a", { originalAvailability: null })]);
  expect(await screen.findByText("The original’s availability has not been established.")).toBeVisible();
});

test("a reachable attached original is reported even when its proxy is missing", async () => {
  setup([asset("clip_a", { originalAvailability: "accessible_here" })]);
  expect(await screen.findByText("The original is available on this server.")).toBeVisible();
});


test("a shared link opens its clip even beyond the loaded queue page", async () => {
  const selected = asset("later_clip", { localProxyAvailable: true, rotationDegrees: 180 });
  const { router, client, container } = renderApp("/projects/p1?clip=later_clip", {
    listProjectAssets: vi.fn().mockResolvedValue({ items: [asset("first_clip")], nextCursor: "next-page" }),
    getProjectReviewAsset: vi.fn().mockResolvedValue(selected),
  });
  expect(await screen.findByRole("heading", { name: "later_clip.MP4" })).toBeVisible();
  expect(client.getProjectReviewAsset).toHaveBeenCalledWith("p1", "later_clip");
  expect(client.getProjectReviewAsset).not.toHaveBeenCalledWith("p1", "first_clip");
  expect(router.state.location.search).toBe("?clip=later_clip");
  expect(container.querySelector("video")).toHaveAttribute("src", "/api/v1/projects/p1/assets/later_clip/proxy");
  expect(container.querySelector("video")).toHaveAttribute("data-rotation", "180");
  expect(screen.getByText("Selected clip")).toBeVisible();
  expect(screen.getByRole("button", { name: "Next clip (J)" })).toBeDisabled();
});
