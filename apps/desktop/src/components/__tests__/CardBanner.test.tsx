import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { CardBanner } from "../CardBanner";

describe("the card mount banner", () => {
  it("offers review and dismissal without starting an import", async () => {
    const onReview = vi.fn();
    const onDismiss = vi.fn();
    render(<CardBanner card={{
      sessionId: "card_one",
      label: "DJI_SD",
      mountPath: "/Volumes/DJI_SD",
      cameraHint: "DJI Pocket 3",
      clipCount: 243,
      totalBytes: 12,
    }} onReview={onReview} onDismiss={onDismiss} />);
    expect(screen.getByText("DJI_SD detected")).toBeTruthy();
    expect(screen.getByText("DJI Pocket 3 · 243 clips")).toBeTruthy();
    await userEvent.click(screen.getByRole("button", { name: "Review import" }));
    expect(onReview).toHaveBeenCalledOnce();
    expect(onDismiss).not.toHaveBeenCalled();
  });
});
