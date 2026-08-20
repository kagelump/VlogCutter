import type { ReactNode } from "react";
import type { AutoplayMode, DesktopSettings, ModelInstallProgress, ModelStatus, ServerConnectionStatus } from "../types";
import { formatBytes } from "../review/summaries";

const autoplayOptions: Array<{ value: AutoplayMode; label: string; detail: string }> = [
  { value: "never", label: "Never", detail: "Keep each clip paused until you press Space." },
  { value: "proxyAvailable", label: "Only when proxy is available", detail: "Autoplay lightweight review media; keep camera originals paused." },
  { value: "always", label: "Always", detail: "Autoplay whichever version is available, including camera originals." },
];

/**
 * The global-settings modal (spec §3.9, §5, §14). Autoplay, the projects folder,
 * the speech models and OpenRouter key, and the server connection — the choices
 * that apply to every project on this Mac. Every value and action arrives as a
 * flat prop; the shell owns the state, the Keychain writes, and the wizards this
 * opens.
 */
export function SettingsModal({
  settings,
  libraryRootPath,
  isDesktop,
  busy,
  models,
  modelInstall,
  keyInput,
  keyPresent,
  serverStatus,
  onAutoplayModeChange,
  onChangeProjectsRoot,
  onInstallModels,
  onKeyInputChange,
  onSaveKey,
  onRemoveKey,
  onConnectServer,
  onDisconnectServer,
  onClose,
  storage,
}: {
  settings: DesktopSettings;
  libraryRootPath: string;
  isDesktop: boolean;
  busy: boolean;
  models: ModelStatus[];
  modelInstall: ModelInstallProgress | null;
  keyInput: string;
  keyPresent: boolean;
  serverStatus: ServerConnectionStatus | null;
  /**
   * The open project's storage panel (#16). Passed as a node rather than as a
   * dozen more flat props: it owns real state of its own, and Settings' job
   * here is only to give it a home the user can find.
   */
  storage?: ReactNode;
  onAutoplayModeChange(mode: AutoplayMode): void;
  onChangeProjectsRoot(): void;
  onInstallModels(): void;
  onKeyInputChange(value: string): void;
  onSaveKey(): void;
  onRemoveKey(): void;
  onConnectServer(): void;
  onDisconnectServer(): void;
  onClose(): void;
}) {
  return (
    <div className="modal-backdrop settings-backdrop" role="presentation">
      <section className="modal settings-modal" role="dialog" aria-modal="true" aria-labelledby="settings-title">
        <span className="eyebrow">Global settings</span>
        <div className="settings-modal-heading">
          <div>
            <h2 id="settings-title">Review behavior</h2>
            <p>These choices apply to every project on this Mac.</p>
          </div>
          <button className="text-button" onClick={onClose}>Close <kbd>Esc</kbd></button>
        </div>

        <fieldset className="settings-group">
          <legend>Autoplay clips</legend>
          <div className="settings-options">
            {autoplayOptions.map((option) => (
              <label className={`settings-option ${settings.autoplayMode === option.value ? "selected" : ""}`} key={option.value}>
                <input
                  type="radio"
                  name="autoplay-mode"
                  value={option.value}
                  checked={settings.autoplayMode === option.value}
                  autoFocus={settings.autoplayMode === option.value}
                  onChange={() => onAutoplayModeChange(option.value)}
                />
                <span>
                  <strong>{option.label}</strong>
                  <small>{option.detail}</small>
                </span>
              </label>
            ))}
          </div>
        </fieldset>

        <div className="settings-group project-folder-setting">
          <div>
            <span className="settings-legend">Default projects folder</span>
            <span className="project-root mono" title={settings.projectsRoot || libraryRootPath}>{settings.projectsRoot || libraryRootPath || "Loading…"}</span>
          </div>
          {isDesktop && <button className="button secondary" disabled={busy} onClick={onChangeProjectsRoot}>Change…</button>}
        </div>

        <fieldset className="settings-group analysis-setting">
          <legend>Clip analysis</legend>

          <div className="settings-row">
            <div>
              <span className="settings-legend">Speech models</span>
              <small>
                Downloaded once and shared by every project. Nothing else in the app downloads
                them: a batch fails with an explanation instead.
              </small>
              <ul className="model-list">
                {models.map((model) => (
                  <li key={model.name}>
                    <span className="mono">{model.name}</span>
                    <span>{model.installed ? "Installed" : `${Math.round(model.bytes / 1_000_000)} MB`}</span>
                  </li>
                ))}
              </ul>
              {modelInstall && (
                <div className="model-progress">
                  <div className="progress-track" role="progressbar" aria-valuemin={0} aria-valuemax={modelInstall.totalBytes} aria-valuenow={modelInstall.downloadedBytes}>
                    <span style={{ width: `${modelInstall.totalBytes ? (modelInstall.downloadedBytes / modelInstall.totalBytes) * 100 : 0}%` }} />
                  </div>
                  <span className="section-note">
                    {modelInstall.name || "Starting"} · {formatBytes(modelInstall.downloadedBytes)} of {formatBytes(modelInstall.totalBytes)}
                  </span>
                </div>
              )}
            </div>
            <button
              className="button secondary"
              disabled={Boolean(modelInstall) || models.every((model) => model.installed)}
              onClick={onInstallModels}
            >
              {models.every((model) => model.installed) ? "Installed" : modelInstall ? "Downloading…" : "Install"}
            </button>
          </div>

          <div className="settings-row">
            <div>
              <span className="settings-legend">OpenRouter key</span>
              <small>
                Stored in your macOS Keychain, never in a project catalog. Required only for the
                cloud analysis step; transcription runs offline without it.
              </small>
              <label className="field-label">
                <span className="sr-only">OpenRouter key</span>
                <input
                  type="password"
                  value={keyInput}
                  placeholder={keyPresent ? "A key is stored. Enter a new one to replace it." : "sk-or-…"}
                  autoComplete="off"
                  onChange={(event) => onKeyInputChange(event.target.value)}
                />
              </label>
            </div>
            <div className="settings-row-actions">
              <button className="button secondary" disabled={!keyInput.trim()} onClick={onSaveKey}>Save</button>
              {keyPresent && (
                <button className="text-button" onClick={onRemoveKey}>Remove</button>
              )}
            </div>
          </div>
        </fieldset>

        <div className="settings-group server-setting">
          <div>
            <span className="settings-legend">Server connection</span>
            {serverStatus?.paired ? (
              <div className="server-status connected">
                <span className="server-dot" aria-label="connected"></span>
                <span>{serverStatus.label || "Server"} — {serverStatus.nodeId?.slice(0, 12)}…</span>
                {serverStatus.endpoints && serverStatus.endpoints.length > 0 && (
                  <div className="endpoint-list">
                    {serverStatus.endpoints.map((ep) => (
                      <span className="mono endpoint-tag" key={ep.url}>{ep.kind}: {ep.url}</span>
                    ))}
                  </div>
                )}
              </div>
            ) : (
              <div className="server-status disconnected">
                <span className="server-dot" aria-label="disconnected"></span>
                <span>Not connected to a server</span>
              </div>
            )}
          </div>
          {isDesktop && (
            serverStatus?.paired ? (
              <button className="button secondary" disabled={busy} onClick={onDisconnectServer}>Disconnect</button>
            ) : (
              <button className="button primary" disabled={busy} onClick={onConnectServer}>Connect to server…</button>
            )
          )}
        </div>

        {storage}
      </section>
    </div>
  );
}
