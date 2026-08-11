import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { NewProjectModal } from "../NewProjectModal";
import type { ProjectLibrary, ProjectLocation } from "../../types";

function location(name: string, id: string): ProjectLocation {
  return {
    catalogPath: `/root/${id}/catalog.sqlite3`,
    folderPath: `/root/${id}`,
    project: {
      id, name, createdAt: "2026-08-01T00:00:00Z", assetCount: 12, labeledCount: 4,
      removedCount: 0, originNodeId: null, pairedServerNodeId: null, pairedAt: null,
    },
  };
}

function renderModal(over: Partial<React.ComponentProps<typeof NewProjectModal>> = {}) {
  const props = {
    library: { rootPath: "/Movies/Projects", projects: [location("Hokkaido", "p1")] } as ProjectLibrary,
    isDesktop: true,
    busy: false,
    newProjectName: "",
    canCancel: true,
    onChangeRoot: vi.fn(),
    onOpenProject: vi.fn(),
    onNewProjectNameChange: vi.fn(),
    onOpenCatalog: vi.fn(),
    onCancel: vi.fn(),
    onCreate: vi.fn(),
    ...over,
  };
  return { props, ...render(<NewProjectModal {...props} />) };
}

describe("NewProjectModal", () => {
  it("opens an existing project from the library list", async () => {
    const { props } = renderModal();
    await userEvent.click(screen.getByRole("button", { name: /Hokkaido/ }));
    expect(props.onOpenProject).toHaveBeenCalledWith(props.library.projects[0]);
  });

  it("keeps Create disabled until a name is entered", () => {
    const { rerender, props } = renderModal({ newProjectName: "" });
    expect(screen.getByRole("button", { name: "Create project" })).toBeDisabled();
    rerender(<NewProjectModal {...props} newProjectName="Trip" />);
    expect(screen.getByRole("button", { name: "Create project" })).toBeEnabled();
  });

  it("hides the desktop-only catalog and folder controls in the browser", () => {
    renderModal({ isDesktop: false });
    expect(screen.queryByRole("button", { name: /open existing project/i })).toBeNull();
    expect(screen.queryByRole("button", { name: /change/i })).toBeNull();
  });
});
