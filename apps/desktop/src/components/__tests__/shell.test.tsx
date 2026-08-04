import { render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import App from "../../App";

/**
 * The shell's child count is the whole point of this file. `.app-shell`
 * declared three rows while rendering four children, so the fourth landed in
 * an implicit auto row and quietly took ~68px of workbench height.
 */
describe("app shell", () => {
  it("renders exactly three grid children, so no region can take an implicit row", async () => {
    const { container } = render(<App />);
    await waitFor(() => expect(container.querySelector(".app-shell")).not.toBeNull());

    const shell = container.querySelector(".app-shell")!;
    expect(shell.children).toHaveLength(3);
    expect(shell.children[0].tagName).toBe("HEADER");
    expect(shell.children[1]).toHaveClass("workbench");
    expect(shell.children[2]).toHaveClass("story-ribbon");
  });

  it("has no permanently reserved status line", async () => {
    const { container } = render(<App />);
    await waitFor(() => expect(container.querySelector(".app-shell")).not.toBeNull());
    expect(container.querySelector(".status-line")).toBeNull();
  });

  it("keeps the operation slot empty while nothing is running", async () => {
    render(<App />);
    const slot = await screen.findByTestId("operation-slot");
    expect(slot).toBeEmptyDOMElement();
  });

  it("does not restate the clip count as a status message", async () => {
    render(<App />);
    await screen.findByTestId("operation-slot");
    expect(screen.queryByText(/clips ready/i)).toBeNull();
  });

  it("shows labelling progress as a hairline rather than a readout", async () => {
    const { container } = render(<App />);
    await waitFor(() => expect(container.querySelector(".completion-hairline")).not.toBeNull());
    expect(container.querySelector(".session-readout")).toBeNull();
  });
});
