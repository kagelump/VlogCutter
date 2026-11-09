import { useEffect, useRef, useState } from "react";

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

export function YouTubePlayer({ videoId, recording, onObservation }: {
  videoId: string;
  recording: boolean;
  onObservation: (position: number, state: number, rate: number, time: number) => void;
}) {
  const container = useRef<HTMLDivElement>(null);
  const player = useRef<Player | null>(null);
  const active = useRef(recording);
  const callback = useRef(onObservation);
  const [error, setError] = useState<string | null>(null);
  const [ready, setReady] = useState(false);
  active.current = recording;
  callback.current = onObservation;
  useEffect(() => {
    let cancelled = false;
    function sample() {
      const p = player.current;
      if (!active.current || !p) return;
      try {
        const position = p.getCurrentTime();
        const state = p.getPlayerState();
        const rate = p.getPlaybackRate();
        callback.current(position, state, rate, performance.now());
      } catch { /* A player that is not ready cannot supply an observation. */ }
    }
    void loadApi().then((api) => {
      if (cancelled || !container.current) return;
      const mount = document.createElement("div");
      container.current.appendChild(mount);
      player.current = new api.Player(mount, {
        videoId, width: "100%", height: "360", playerVars: { playsinline: 1, origin: window.location.origin },
        events: { onReady: () => setReady(true), onStateChange: sample, onPlaybackRateChange: sample,
          onError: () => setError("This video cannot be played in the embedded player. Use Record review with the video on another screen instead.") },
      });
    }).catch(() => { if (!cancelled) setError("The YouTube player could not load. Check your connection or use Record review instead."); });
    const timer = window.setInterval(sample, 5000);
    return () => { cancelled = true; window.clearInterval(timer); player.current?.destroy(); player.current = null; };
  }, [videoId]);
  useEffect(() => { if (!recording && player.current && ready) player.current.pauseVideo(); }, [recording, ready]);
  return <section aria-label="YouTube player"><div ref={container} />{error ? <p role="alert">{error}</p> : !ready ? <p>Loading YouTube player…</p> : null}</section>;
}
