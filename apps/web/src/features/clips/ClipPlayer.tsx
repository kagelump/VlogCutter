import { forwardRef, useImperativeHandle, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import type { ProjectReviewAsset } from "@rcdvlog/project-client";
import { keys, useClient } from "../../app/client";
import { Loading } from "../../ui";
import styles from "./Clips.module.css";

export interface PlayerHandle { toggle(): void }

const mediaUrl = (representationId: string) => `/api/v1/media/${encodeURIComponent(representationId)}/content`;

export const ClipPlayer = forwardRef<PlayerHandle, { asset: ProjectReviewAsset | null; loading: boolean }>(function ClipPlayer({ asset, loading }, ref) {
  const client = useClient();
  const queryClient = useQueryClient();
  const video = useRef<HTMLVideoElement>(null);
  const [failed, setFailed] = useState<string | null>(null);

  useImperativeHandle(ref, () => ({
    toggle() {
      const element = video.current;
      if (!element) return;
      if (element.paused) void element.play().catch(() => undefined);
      else element.pause();
    },
  }), []);

  async function diagnose(assetId: string) {
    try {
      const session = await client.getSession();
      if (!session.authenticated) { void queryClient.invalidateQueries({ queryKey: keys.auth }); return; }
    } catch { /* fall through to the media message */ }
    setFailed(assetId);
  }

  if (!asset) return <div className={styles.stage}>{loading ? <Loading center /> : null}</div>;

  const proxyReady = asset.proxy?.state === "available" && asset.proxy.availableOnServer;
  const poster = asset.thumbnail?.state === "available" && asset.thumbnail.availableOnServer ? mediaUrl(asset.thumbnail.representationId) : undefined;
  const rotation = asset.rotationDegrees % 360;

  return (
    <div className={styles.stage}>
      {proxyReady ? (
        <video
          key={asset.proxy!.representationId}
          ref={video}
          className={styles.video}
          data-rotation={rotation || undefined}
          src={mediaUrl(asset.proxy!.representationId)}
          poster={poster}
          controls
          playsInline
          preload="metadata"
          onError={() => void diagnose(asset.projectAssetId)}
        />
      ) : poster ? <img className={styles.video} data-rotation={rotation || undefined} src={poster} alt="" /> : null}

      {!proxyReady ? (
        <div className={styles.stageMessage}>
          <strong>{previewTitle(asset)}</strong>
          <p>{previewDetail(asset)}</p>
        </div>
      ) : failed === asset.projectAssetId ? (
        <div className={styles.stageMessage} role="alert">
          <strong>Preview stopped loading</strong>
          <p>The preview file is no longer available on the server.</p>
        </div>
      ) : null}
    </div>
  );
});

function previewTitle(asset: ProjectReviewAsset) {
  const proxy = asset.proxy;
  if (proxy?.state === "queued" || proxy?.state === "running") return "Preview is being made";
  if (proxy?.state === "failed") return "Preview failed";
  if (proxy?.state === "available" && !proxy.availableOnServer) return "Preview is on another Mac";
  return "No preview";
}

/** Browser playback only ever uses a server-side preview (proxy); originals are never streamed. */
function previewDetail(asset: ProjectReviewAsset) {
  const proxy = asset.proxy;
  if (proxy?.state === "queued" || proxy?.state === "running") return "It will play here when it’s ready.";
  if (proxy?.state === "available" && !proxy.availableOnServer) return "Transfer it from the paired Mac, or generate one where the original is available.";
  if (asset.mediaVerificationState === "mismatch") return "The original’s identity doesn’t match. Fix that before making a preview.";
  switch (asset.originalAvailability) {
    case "accessible_here": return "The server can read the original. Request a preview from Admin → Jobs.";
    case "accessible_on_another_node": return "The original is only on a paired Mac. Generate the preview there.";
    case "temporarily_unavailable":
    case "missing_after_completed_observation": return "The original’s storage is offline. Reconnect it, then generate a preview.";
    default: return "This server can’t reach the original, so no preview can be made here.";
  }
}
