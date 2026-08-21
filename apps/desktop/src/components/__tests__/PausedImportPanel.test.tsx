/**
 * The paused import surface (#79).
 *
 * The behaviour under test is mostly about restraint: what this panel must
 * *not* offer. There is no "Check again" — detection is automatic, and a button
 * that only re-asks a question already being asked every few seconds is a
 * placebo. There is no countdown and no elapsed time, because continuing is a
 * question about conditions and never about a clock.
 */

import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { PausedImportPanel } from "../PausedImportPanel";
import type { EndpointProbe, PausedImport } from "../../import/pausedImport";

function paused(overrides: Partial<PausedImport> = {}): PausedImport {
  return {
    session: {
      projectId: "project_eclipse",
      sourcePath: "/Volumes/SD_Card",
      card: null,
      archiveRootId: "root_acasis",
      cameraOverride: null,
      verifyCopies: true,
      makeProxies: false,
    },
    endpoint: "destination",
    volumeLabel: "Acasis",
    reachability: "not-connected",
    committed: 77,
    failed: 0,
    pausedAt: 1_000,
    ...overrides,
  };
}

function renderPanel(
  state: PausedImport = paused(),
  probe: EndpointProbe | null = { present: false, identityVerified: false },
) {
  const onCancel = vi.fn();
  const onCorrective = vi.fn();
  render(
    <PausedImportPanel
      paused={state}
      probe={probe}
      onCancel={onCancel}
      onCorrective={onCorrective}
    />,
  );
  return { onCancel, onCorrective };
}

describe("PausedImportPanel", () => {
  it("says the import is paused rather than failed", () => {
    renderPanel();

    expect(screen.getByRole("heading", { name: "Import paused" })).toBeInTheDocument();
  });

  /// The whole point of the pause: committed work is safe, and the user needs
  /// to see that before anything else.
  it("says how much is already committed", () => {
    renderPanel();

    expect(screen.getByText(/77 clips are already in the project/i)).toBeInTheDocument();
  });

  it("names the condition that is still unmet", () => {
    renderPanel();

    expect(screen.getByText("Acasis isn't connected")).toBeInTheDocument();
  });

  /// A mounted-but-mute volume, with its icon still in Finder. Telling this
  /// user the drive "isn't connected" spends trust in the whole surface.
  it("says a stalled volume is not responding rather than missing", () => {
    renderPanel(paused({ reachability: "not-responding" }));

    expect(screen.getByText("Acasis isn't responding")).toBeInTheDocument();
  });

  /// There is nothing to press to hurry detection along, because detection is
  /// automatic. A button that re-asks a question already on a timer is a lie
  /// about how the system works.
  it("offers no way to re-check, because checking is automatic", () => {
    renderPanel();

    expect(screen.queryByRole("button", { name: /check again/i })).toBeNull();
    expect(screen.queryByRole("button", { name: /retry|refresh|resume/i })).toBeNull();
  });

  it("offers cancelling as the one routine action", async () => {
    const { onCancel } = renderPanel();

    await userEvent.click(screen.getByRole("button", { name: /cancel import/i }));

    expect(onCancel).toHaveBeenCalled();
  });

  /// An absent volume needs plugging in, not a dialog. Offering "Locate
  /// archive…" to someone whose drive is simply unplugged sends them into a
  /// file picker to solve a problem a cable solves.
  it("offers no corrective while the volume is merely absent", () => {
    renderPanel();

    expect(screen.queryByRole("button", { name: /locate archive/i })).toBeNull();
  });

  /// The escalation case: something came back and cannot prove it is ours.
  it("offers to locate the archive when what returned cannot prove itself", async () => {
    const { onCorrective } = renderPanel(paused(), { present: true, identityVerified: false });

    expect(
      screen.getByText("The drive connected as Acasis isn't the one this import was using"),
    ).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: /locate archive/i }));

    expect(onCorrective).toHaveBeenCalledWith("locate-archive");
  });

  it("offers to choose the source again when a paused source cannot prove itself", async () => {
    const { onCorrective } = renderPanel(
      paused({ endpoint: "source", volumeLabel: "SD_Card" }),
      { present: true, identityVerified: false },
    );

    await userEvent.click(screen.getByRole("button", { name: /choose source/i }));

    expect(onCorrective).toHaveBeenCalledWith("choose-source");
  });

  /// A clock is not a safety property, so the surface must not imply one is
  /// running out. Nothing here counts up, counts down, or expires.
  it("shows no elapsed time or countdown", () => {
    renderPanel(paused({ pausedAt: Date.now() - 8 * 60 * 60 * 1_000 }));

    expect(screen.queryByText(/hour|minute|ago|remaining|expires/i)).toBeNull();
  });

  /// Real ledger work must not disappear behind the paused surface.
  it("surfaces genuine per-file failures alongside the pause", () => {
    renderPanel(paused({ failed: 2 }));

    expect(screen.getByText(/2 files need attention/i)).toBeInTheDocument();
  });

  it("keeps the run's choices inspectable without putting them in the way", async () => {
    renderPanel();

    const details = screen.getByText(/what this import was doing/i);
    await userEvent.click(details);

    expect(screen.getByText(/verifying copies/i)).toBeInTheDocument();
  });
});
