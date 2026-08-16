import { render, screen, fireEvent } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { PairingWizardModal, type PairingWizardStep } from "../PairingWizardModal";
import type { ServerVersionInfo } from "../../types";

const identity: ServerVersionInfo = { nodeId: "node_abcdef0123456789", applicationVersion: "1.2.3", protocolMajor: 1, protocolMinor: 0 };

function renderWizard(over: Partial<React.ComponentProps<typeof PairingWizardModal>> = {}) {
  const props = {
    step: "endpoint" as PairingWizardStep,
    serverEndpoint: "",
    serverIdentity: null as ServerVersionInfo | null,
    pairingCode: "",
    deviceLabel: "MacBook Pro",
    busy: false,
    onServerEndpointChange: vi.fn(),
    onTestConnection: vi.fn(),
    onPairingCodeChange: vi.fn(),
    onDeviceLabelChange: vi.fn(),
    onPair: vi.fn(),
    onBack: vi.fn(),
    onCancel: vi.fn(),
    ...over,
  };
  return { props, ...render(<PairingWizardModal {...props} />) };
}

describe("PairingWizardModal", () => {
  it("tests the connection only once an endpoint is entered", async () => {
    const { props, rerender } = renderWizard({ serverEndpoint: "" });
    expect(screen.getByRole("button", { name: "Test connection" })).toBeDisabled();
    rerender(<PairingWizardModal {...props} serverEndpoint="https://rcdvlog.local" />);
    await userEvent.click(screen.getByRole("button", { name: "Test connection" }));
    expect(props.onTestConnection).toHaveBeenCalledTimes(1);
  });

  it("shows the server identity and pairs from the code step", async () => {
    const { props } = renderWizard({ step: "code", serverIdentity: identity, pairingCode: "ABCDEFGH" });
    expect(screen.getByText(/node_abcdef01234…/)).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Pair with server" }));
    expect(props.onPair).toHaveBeenCalledTimes(1);
    await userEvent.click(screen.getByRole("button", { name: "Back" }));
    expect(props.onBack).toHaveBeenCalledTimes(1);
  });

  it("resets through onCancel from its own button", () => {
    // Escape reaches the same reset through the shell's escape stack, which is
    // where that case is now asserted (issue #52).
    const { props } = renderWizard();
    fireEvent.click(screen.getByRole("button", { name: /cancel/i }));
    expect(props.onCancel).toHaveBeenCalledTimes(1);
  });

  it("gates the pairing action on a non-empty code", () => {
    renderWizard({ step: "code", serverIdentity: identity, pairingCode: "" });
    expect(screen.getByRole("button", { name: "Pair with server" })).toBeDisabled();
  });
});
