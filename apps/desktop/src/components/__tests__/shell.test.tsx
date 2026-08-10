import { screen, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { desktopScenario, renderApp } from "../../test/harness";

/**
 * The shell's child count is the whole point of this file. `.app-shell`
 * declared three rows while rendering four children, so the fourth landed in
 * an implicit auto row and quietly took ~68px of workbench height.
 *
 * The app boots against the CUJ harness, so what fills these regions is a
 * declared scenario rather than demo data hidden in an adapter.
 */
function bootApp() {
  const harness = desktopScenario()
    .project("Hokkaido field test")
    .camera("Sony A7C II", { clips: 3, withProxy: true })
    .withLabels("asset_sony_a7c_ii_001", "arrival")
    .build();
  return renderApp(harness);
}

describe("app shell", () => {
  it("renders exactly three grid children, so no region can take an implicit row", async () => {
    const { container } = await bootApp();
    await waitFor(() => expect(container.querySelector(".app-shell")).not.toBeNull());

    const shell = container.querySelector(".app-shell")!;
    expect(shell.children).toHaveLength(3);
    expect(shell.children[0].tagName).toBe("HEADER");
    expect(shell.children[1]).toHaveClass("workbench");
    expect(shell.children[2]).toHaveClass("story-ribbon");
  });

  it("has no permanently reserved status line", async () => {
    const { container } = await bootApp();
    await waitFor(() => expect(container.querySelector(".app-shell")).not.toBeNull());
    expect(container.querySelector(".status-line")).toBeNull();
  });

  it("keeps the operation slot empty while nothing is running", async () => {
    await bootApp();
    const slot = await screen.findByTestId("operation-slot");
    expect(slot).toBeEmptyDOMElement();
  });

  it("does not restate the clip count as a status message", async () => {
    await bootApp();
    await screen.findByTestId("operation-slot");
    expect(screen.queryByText(/clips ready/i)).toBeNull();
  });

  it("shows labelling progress as a hairline rather than a readout", async () => {
    const { container } = await bootApp();
    await waitFor(() => expect(container.querySelector(".completion-hairline")).not.toBeNull());
    expect(container.querySelector(".session-readout")).toBeNull();
  });
});
