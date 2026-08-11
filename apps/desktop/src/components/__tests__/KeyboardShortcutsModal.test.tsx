import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { KeyboardShortcutsModal } from "../KeyboardShortcutsModal";

describe("KeyboardShortcutsModal", () => {
  it("is a labelled dialog that closes on the one control it offers", async () => {
    const onClose = vi.fn();
    const user = userEvent.setup();
    render(<KeyboardShortcutsModal onClose={onClose} />);

    const dialog = screen.getByRole("dialog", { name: "Review without leaving the keys" });
    expect(dialog).toBeInTheDocument();

    await user.click(within(dialog).getByRole("button", { name: /close/i }));
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("prints the P proxy binding, which is the single-clip proxy's only affordance since #25", () => {
    // The offline-slate button was removed in #25; the keyboard map is now the
    // legend that keeps that binding discoverable, so its presence is asserted.
    render(<KeyboardShortcutsModal onClose={vi.fn()} />);
    const row = screen.getByText("Proxy selected clip").closest("div")!;
    expect(within(row).getByText("P")).toBeInTheDocument();
  });
});
