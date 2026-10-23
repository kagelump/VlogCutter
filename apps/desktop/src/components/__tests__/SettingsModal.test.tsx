import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { SettingsModal } from "../SettingsModal";
import type { DesktopSettings, ModelStatus, ServerConnectionStatus } from "../../types";

const settings: DesktopSettings = {
  projectsRoot: "/Movies/Projects",
  nodeId: "node_mac",
  autoplayMode: "proxyAvailable",
  lastCatalogPath: null,
  lastProjectId: null,
  lastAssetId: null,
  lastContextUpdatedAt: null,
  makeProxiesOnImport: true,
  lastImportArchiveRootId: null,
  importArchiveRootIdsByCardSession: {},
  mediaOrigin: null,
};

const models: ModelStatus[] = [{
  name: "whisper-base",
  role: "speech_to_text",
  installed: false,
  ready: false,
  present: false,
  integrity: "missing",
  detail: null,
  bytes: 148_000_000,
  url: "",
  directory: "",
  path: "",
  expectedBlake3: null,
}];

function renderSettings(over: Partial<React.ComponentProps<typeof SettingsModal>> = {}) {
  const props = {
    settings,
    libraryRootPath: "/Movies/Projects",
    isDesktop: true,
    busy: false,
    models,
    modelInstall: null,
    keyInput: "",
    keyPresent: false,
    serverStatus: { paired: false, nodeId: null, endpoint: null, label: null, pairedAt: null, endpoints: null } as ServerConnectionStatus,
    onAutoplayModeChange: vi.fn(),
    onChangeProjectsRoot: vi.fn(),
    onInstallModels: vi.fn(),
    onKeyInputChange: vi.fn(),
    onSaveKey: vi.fn(),
    onRemoveKey: vi.fn(),
    onConnectServer: vi.fn(),
    onDisconnectServer: vi.fn(),
    onClose: vi.fn(),
    ...over,
  };
  return { props, ...render(<SettingsModal {...props} />) };
}

describe("SettingsModal", () => {
  it("changes the autoplay mode from the radio group", async () => {
    const { props } = renderSettings();
    await userEvent.click(screen.getByRole("radio", { name: /Never/ }));
    expect(props.onAutoplayModeChange).toHaveBeenCalledWith("never");
  });

  it("keeps Save disabled until a key is typed, and hides Remove without a stored key", () => {
    const { rerender, props } = renderSettings({ keyInput: "", keyPresent: false });
    expect(screen.getByRole("button", { name: "Save" })).toBeDisabled();
    expect(screen.queryByRole("button", { name: "Remove" })).toBeNull();
    rerender(<SettingsModal {...props} keyInput="sk-or-abc" keyPresent />);
    expect(screen.getByRole("button", { name: "Save" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Remove" })).toBeInTheDocument();
  });

  it("offers Connect when unpaired and Disconnect when paired", async () => {
    const { props, rerender } = renderSettings();
    await userEvent.click(screen.getByRole("button", { name: /connect to server/i }));
    expect(props.onConnectServer).toHaveBeenCalledTimes(1);

    rerender(
      <SettingsModal
        {...props}
        serverStatus={{ paired: true, nodeId: "node_server_1234", endpoint: null, label: "Home server", pairedAt: null, endpoints: null }}
      />,
    );
    await userEvent.click(screen.getByRole("button", { name: "Disconnect" }));
    expect(props.onDisconnectServer).toHaveBeenCalledTimes(1);
  });

  it("hides the desktop-only folder and server controls in the browser", () => {
    renderSettings({ isDesktop: false });
    expect(screen.queryByRole("button", { name: /change/i })).toBeNull();
    expect(screen.queryByRole("button", { name: /connect to server/i })).toBeNull();
  });
});
