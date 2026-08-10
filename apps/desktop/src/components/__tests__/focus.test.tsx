import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import App from "../../App";

async function ready(container: HTMLElement) {
  await screen.findByTestId("operation-slot");
  // The panel only mounts once a clip is selected, which happens an effect
  // later than the shell itself.
  await waitFor(() => expect(container.querySelector("#notes")).not.toBeNull());
  return container.querySelector<HTMLElement>(".app-shell")!;
}

/** Entering focuses the notes field, so leave it before pressing a shortcut. */
async function enterFocus(container: HTMLElement) {
  const shell = await ready(container);
  await userEvent.keyboard("f");
  await waitFor(() => expect(shell).toHaveClass("is-focus"));
  return shell;
}

async function blurNotes(container: HTMLElement) {
  container.querySelector<HTMLTextAreaElement>("#notes")?.blur();
  await waitFor(() => expect(document.activeElement?.id).not.toBe("notes"));
}

/**
 * Focus is a presentation of Review rather than a separate component tree.
 * That is not an implementation detail: keeping one tree is what makes the
 * caret and the playback position survive the transition, because the
 * textarea and the video element are never unmounted.
 */
describe("entering and leaving Focus", () => {
  it("marks the shell rather than swapping the tree, so nothing unmounts", async () => {
    const { container } = render(<App />);
    await ready(container);
    const notesBefore = container.querySelector("#notes");
    const videoBefore = container.querySelector("video");
    expect(notesBefore).not.toBeNull();

    await enterFocus(container);

    expect(container.querySelector("#notes")).toBe(notesBefore);
    expect(container.querySelector("video")).toBe(videoBefore);
  });

  it("puts the caret in the notes field on entry, since writing is why you came", async () => {
    const { container } = render(<App />);
    await enterFocus(container);
    expect(document.activeElement?.id).toBe("notes");
  });

  it("leaves on a second F once the notes field no longer has the caret", async () => {
    const { container } = render(<App />);
    const shell = await enterFocus(container);
    await blurNotes(container);
    await userEvent.keyboard("f");
    await waitFor(() => expect(shell).not.toHaveClass("is-focus"));
  });

  it("types rather than leaving when F is pressed inside the notes field", async () => {
    const { container } = render(<App />);
    const shell = await enterFocus(container);
    const notes = container.querySelector<HTMLTextAreaElement>("#notes")!;
    const before = notes.value;

    // The caret lands at offset 0 on entry, so the character prepends.
    await userEvent.keyboard("f");
    expect(shell).toHaveClass("is-focus");
    expect(notes.value).toBe(`f${before}`);
  });

  it("leaves on Escape straight from the notes field, keeping what was typed", async () => {
    const { container } = render(<App />);
    const shell = await enterFocus(container);
    const notes = container.querySelector<HTMLTextAreaElement>("#notes")!;

    await userEvent.keyboard("harbour");
    await userEvent.keyboard("{Escape}");

    await waitFor(() => expect(shell).not.toHaveClass("is-focus"));
    // Escape cancels in Review; in Focus it saves, because discarding a
    // paragraph you just wrote is the wrong reading of "take me back".
    expect(container.querySelector<HTMLTextAreaElement>("#notes")!.value).toContain("harbour");
  });
});

describe("overlays", () => {
  it("hides the right stack by default", async () => {
    const { container } = render(<App />);
    const shell = await enterFocus(container);
    expect(shell).not.toHaveClass("show-right-stack");
  });

  it("toggles every overlay with H", async () => {
    const { container } = render(<App />);
    const shell = await enterFocus(container);
    await blurNotes(container);

    await userEvent.keyboard("h");
    await waitFor(() => expect(shell).toHaveClass("overlays-hidden"));
    await userEvent.keyboard("h");
    await waitFor(() => expect(shell).not.toHaveClass("overlays-hidden"));
  });

  it("toggles the right stack with D, the same key the shelf uses in Review", async () => {
    const { container } = render(<App />);
    const shell = await enterFocus(container);
    await blurNotes(container);

    await userEvent.keyboard("d");
    await waitFor(() => expect(shell).toHaveClass("show-right-stack"));
  });

  it("never changes overlay visibility on pointer movement", async () => {
    const { container } = render(<App />);
    const shell = await enterFocus(container);
    await blurNotes(container);

    const before = shell.className;
    await userEvent.hover(shell);
    await userEvent.unhover(shell);
    expect(shell.className).toBe(before);
  });

  it("renders the ribbon as a single merged lane", async () => {
    const { container } = render(<App />);
    await enterFocus(container);
    await waitFor(() => expect(container.querySelectorAll(".lane-track")).toHaveLength(1));
    expect(container.querySelectorAll(".lane-name")).toHaveLength(0);
  });
});

describe("queue navigation stays available", () => {
  it("advances clips without leaving Focus", async () => {
    const { container } = render(<App />);
    const shell = await enterFocus(container);
    await blurNotes(container);

    const first = container.querySelector(".queue-item.selected")?.textContent;
    await userEvent.keyboard("j");
    await waitFor(() => {
      expect(container.querySelector(".queue-item.selected")?.textContent).not.toBe(first);
    });
    expect(shell).toHaveClass("is-focus");
  });
});

describe("frame orientation stays keyboard-first", () => {
  it("rotates with O and flips with Shift+O outside text fields", async () => {
    const { container } = render(<App />);
    await ready(container);
    const orientation = screen.getByRole("button", { name: "Frame orientation" });

    await userEvent.keyboard("o");
    await waitFor(() => expect(orientation.getAttribute("title")).toContain("90°"));

    await userEvent.keyboard("{Shift>}o{/Shift}");
    await waitFor(() => expect(orientation.getAttribute("title")).toContain("270°"));
  });
});

describe("modes leave Focus", () => {
  it("lands in Chronology rather than a focused Chronology", async () => {
    const { container } = render(<App />);
    const shell = await enterFocus(container);
    await blurNotes(container);

    await userEvent.keyboard("{Meta>}2{/Meta}");
    await waitFor(() => expect(shell).not.toHaveClass("is-focus"));
    expect(container.querySelector(".mode-surface")).not.toBeNull();
  });
});
