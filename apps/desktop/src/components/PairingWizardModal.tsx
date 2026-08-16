import type { ServerVersionInfo } from "../types";

export type PairingWizardStep = "endpoint" | "code" | "connecting";

/**
 * The connect-to-server wizard (spec §14). Three steps: reach the server, enter
 * a pairing code, connect.
 *
 * Step state and the reset it needs live in `App` and arrive as props (the
 * decision recorded in HANDOFF_APP_DECOMPOSITION.md: wizards receive their step
 * rather than own it, so the shell can resolve Escape and Cancel to one reset).
 * `onCancel` is that reset; the backdrop's own Escape calls it too.
 */
export function PairingWizardModal({
  step,
  serverEndpoint,
  serverIdentity,
  pairingCode,
  deviceLabel,
  busy,
  onServerEndpointChange,
  onTestConnection,
  onPairingCodeChange,
  onDeviceLabelChange,
  onPair,
  onBack,
  onCancel,
}: {
  step: PairingWizardStep;
  serverEndpoint: string;
  serverIdentity: ServerVersionInfo | null;
  pairingCode: string;
  deviceLabel: string;
  busy: boolean;
  onServerEndpointChange(value: string): void;
  onTestConnection(): void;
  onPairingCodeChange(value: string): void;
  onDeviceLabelChange(value: string): void;
  onPair(): void;
  onBack(): void;
  onCancel(): void;
}) {
  return (
    <div className="modal-backdrop pairing-backdrop" role="presentation">
      <section className="modal pairing-modal" role="dialog" aria-modal="true" aria-labelledby="pairing-title">
        <span className="eyebrow">Connect to server</span>
        <div className="settings-modal-heading">
          <h2 id="pairing-title">
            {step === "endpoint" && "Enter server address"}
            {step === "code" && "Enter pairing code"}
            {step === "connecting" && "Connecting…"}
          </h2>
          <button className="text-button" onClick={onCancel}>Cancel <kbd>Esc</kbd></button>
        </div>

        {step === "endpoint" && (
          <div className="wizard-step">
            <p>Enter the HTTPS URL of your RCD VlogCutter server on the home network or Tailscale.</p>
            <label className="field">
              <span>Server URL</span>
              <input
                type="text"
                className="mono"
                placeholder="https://rcdvlog.local"
                value={serverEndpoint}
                onChange={(e) => onServerEndpointChange(e.target.value)}
                onKeyDown={(e) => { if (e.key === "Enter") onTestConnection(); }}
                autoFocus
              />
            </label>
            <div className="modal-actions">
              <button className="button primary" disabled={busy || !serverEndpoint.trim()} onClick={onTestConnection}>Test connection</button>
            </div>
          </div>
        )}

        {step === "code" && serverIdentity && (
          <div className="wizard-step">
            <div className="server-identity">
              <span className="server-dot connected"></span>
              <span>Server: <strong>{serverIdentity.nodeId.slice(0, 16)}…</strong></span>
              <span className="mono quiet">v{serverIdentity.applicationVersion} · protocol {serverIdentity.protocolMajor}.{serverIdentity.protocolMinor}</span>
            </div>
            <p>Open the RCD VlogCutter web interface in Chrome, go to <strong>Settings → Devices</strong>, and generate a pairing code. Paste it below:</p>
            <label className="field">
              <span>Pairing code</span>
              <input
                type="text"
                className="mono"
                placeholder="ABCDEFGHIJKLMNOP"
                value={pairingCode}
                onChange={(e) => onPairingCodeChange(e.target.value)}
                onKeyDown={(e) => { if (e.key === "Enter") onPair(); }}
                autoFocus
                maxLength={20}
              />
            </label>
            <label className="field">
              <span>This device label</span>
              <input
                type="text"
                value={deviceLabel}
                onChange={(e) => onDeviceLabelChange(e.target.value)}
                placeholder="MacBook Pro"
              />
            </label>
            <div className="modal-actions">
              <button className="button secondary" onClick={onBack}>Back</button>
              <button className="button primary" disabled={busy || !pairingCode.trim()} onClick={onPair}>Pair with server</button>
            </div>
          </div>
        )}

        {step === "connecting" && (
          <div className="wizard-step">
            <p>Connecting to server…</p>
          </div>
        )}
      </section>
    </div>
  );
}
