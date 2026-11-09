import { useEffect, useImperativeHandle, useRef, useState, type Ref } from "react";

interface Player {
  getCurrentTime(): number;
  getPlayerState(): number;
  getPlaybackRate(): number;
  pauseVideo(): void;
  destroy(): void;
}
interface YoutubeApi {
  Player: new (element: HTMLElement, options: Record<string, unknown>) => Player;
}
declare global {
  interface Window { YT?: YoutubeApi; onYouTubeIframeAPIReady?: () => void }
}
let apiPromise: Promise<YoutubeApi> | undefined;
function loadApi(): Promise<YoutubeApi> {
  if (window.YT?.Player) return Promise.resolve(window.YT);
  if (apiPromise) return apiPromise;
  apiPromise = new Promise<YoutubeApi>((resolve, reject) => {
    const previous = window.onYouTubeIframeAPIReady;
    const timer = window.setTimeout(() => reject(new Error("YouTube player timed out")), 15000);
    window.onYouTubeIframeAPIReady = () => {
      previous?.();
      window.clearTimeout(timer);
      if (window.YT?.Player) resolve(window.YT);
      else reject(new Error("YouTube player unavailable"));
    };
    const script = document.createElement("script");
    script.src = "https://www.youtube.com/iframe_api";
    script.onerror = () => { window.clearTimeout(timer); reject(new Error("YouTube player failed to load")); };
    document.head.appendChild(script);
  }).catch((error) => { apiPromise = undefined; throw error; });
  return apiPromise!;
}

export interface YouTubePlayerHandle { sample(): void }

export function YouTubePlayer({ videoId, recording, captureReady = recording, onObservation, ref }: {
  videoId: string;
  recording: boolean;
  /** The microphone has delivered its first native audio frame. */
  captureReady?: boolean;
  onObservation: (position: number, state: number, rate: number, time: number) => void;
  ref?: Ref<YouTubePlayerHandle>;
}) {
  const container = useRef<HTMLDivElement>(null);
  const player = useRef<Player | null>(null);
  const active = useRef(recording);
  const callback = useRef(onObservation);
  const sampleNow = useRef<(() => void) | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [ready, setReady] = useState(false);
  active.current = recording;
  callback.current = onObservation;
  useImperativeHandle(ref, () => ({ sample: () => sampleNow.current?.() }), []);
  useEffect(() => {
    let cancelled = false;
    setError(null);
    setReady(false);
    function sample() {
      const p = player.current;
      if (!p || cancelled) return;
      try {
        const position = p.getCurrentTime();
        const state = p.getPlayerState();
        const rate = p.getPlaybackRate();
        if ([1, 2, 3, 5].includes(state)) { setReady(true); setError(null); }
        if (active.current) callback.current(position, state, rate, performance.now());
      } catch { /* A player that is not ready cannot supply an observation. */ }
    }
    sampleNow.current = sample;
    void loadApi().then((api) => {
      if (cancelled || !container.current) return;
      const mount = document.createElement("div");
      container.current.appendChild(mount);
      player.current = new api.Player(mount, {
        videoId, width: "100%", height: "360", playerVars: { playsinline: 1, origin: window.location.origin },
        events: { onReady: () => { if (!cancelled) { setReady(true); setError(null); } }, onStateChange: sample, onPlaybackRateChange: sample,
          // The iframe shows the actual error (including account/private-video
          // prompts). API error codes alone cannot tell whether this viewer can
          // play it, and may fire transiently before successful playback.
          onError: () => { if (!cancelled) setReady(true); } },
      });
    }).catch(() => { if (!cancelled) setError("The YouTube player could not load. Check your connection or use Record review instead."); });
    return () => { cancelled = true; sampleNow.current = null; player.current?.destroy(); player.current = null; };
  }, [videoId]);
  // Starting an already-playing video produces no state-change event. Sample
  // as soon as both capture and the player are ready, then keep polling.
  useEffect(() => {
    if (!recording || !captureReady || !ready) return;
    sampleNow.current?.();
    const timer = window.setInterval(() => sampleNow.current?.(), 5000);
    return () => window.clearInterval(timer);
  }, [recording, captureReady, ready, videoId]);
  useEffect(() => { if (!recording && player.current && ready) player.current.pauseVideo(); }, [recording, ready]);
  return <section aria-label="YouTube player"><div ref={container} />{error ? <p role="alert">{error}</p> : !ready ? <p>Loading YouTube player…</p> : null}</section>;
}
