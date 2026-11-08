import { useEffect, useRef, useState } from "react";
import { useQueryClient, type QueryKey } from "@tanstack/react-query";

/**
 * Subscribe to the server's change feed for one scope (a project ID or
 * `__admin__`) and invalidate `invalidate` whenever anything changes.
 * Returns whether the socket is live so callers can poll faster when it isn't.
 */
export function useLiveEvents(scope: string | null, invalidate: QueryKey): boolean {
  const queryClient = useQueryClient();
  const [connected, setConnected] = useState(false);
  const keyRef = useRef(invalidate);
  keyRef.current = invalidate;

  useEffect(() => {
    if (!scope || typeof WebSocket === "undefined") return;
    const protocol = window.location.protocol === "https:" ? "wss:" : "ws:";
    let socket: WebSocket;
    try {
      socket = new WebSocket(`${protocol}//${window.location.host}/api/v1/ws/events`);
    } catch {
      return;
    }
    socket.addEventListener("open", () => {
      setConnected(true);
      socket.send(JSON.stringify({ type: "subscribe", projectId: scope }));
    });
    // Bursts of events become one refresh, and a refresh already under way is not restarted.
    let timer: number | undefined;
    socket.addEventListener("message", () => {
      window.clearTimeout(timer);
      timer = window.setTimeout(() => { void queryClient.invalidateQueries({ queryKey: keyRef.current }, { cancelRefetch: false }); }, 250);
    });
    socket.addEventListener("close", () => setConnected(false));
    socket.addEventListener("error", () => setConnected(false));
    return () => { window.clearTimeout(timer); setConnected(false); socket.close(); };
  }, [queryClient, scope]);

  return connected;
}

export type HotkeyMap = Record<string, (event: KeyboardEvent) => void>;

/**
 * Single-key shortcuts that never fire while typing in a field or with a
 * modifier held. Keys are matched case-insensitively against `event.key`.
 */
export function useHotkeys(map: HotkeyMap, enabled = true) {
  const mapRef = useRef(map);
  mapRef.current = map;
  useEffect(() => {
    if (!enabled) return;
    const handler = (event: KeyboardEvent) => {
      if (event.metaKey || event.ctrlKey || event.altKey || event.defaultPrevented) return;
      const target = event.target as HTMLElement | null;
      if (target?.closest("input, textarea, select, [contenteditable='true'], dialog[open]")) return;
      const key = event.key.length === 1 ? event.key.toLowerCase() : event.key;
      const action = mapRef.current[key];
      if (!action) return;
      event.preventDefault();
      action(event);
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [enabled]);
}

export function useMediaQuery(query: string) {
  const [matches, setMatches] = useState(() => typeof window !== "undefined" && typeof window.matchMedia === "function" && window.matchMedia(query).matches);
  useEffect(() => {
    if (typeof window.matchMedia !== "function") return;
    const list = window.matchMedia(query);
    const update = () => setMatches(list.matches);
    update();
    list.addEventListener("change", update);
    return () => list.removeEventListener("change", update);
  }, [query]);
  return matches;
}

export function useDocumentTitle(title: string | null | undefined) {
  useEffect(() => {
    document.title = title ? `${title} · VlogCutter` : "VlogCutter";
  }, [title]);
}
