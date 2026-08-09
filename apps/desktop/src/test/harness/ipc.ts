/**
 * The fake Tauri runtime.
 *
 * `@tauri-apps/api` is a thin shim over three window globals:
 *
 *   invoke(cmd, args)       -> window.__TAURI_INTERNALS__.invoke(cmd, args)
 *   convertFileSrc(path)    -> window.__TAURI_INTERNALS__.convertFileSrc(path)
 *   transformCallback(fn)   -> window.__TAURI_INTERNALS__.transformCallback(fn)
 *
 * Installing that object is the whole seam. The production `api.ts` and
 * `chronologyApi.ts` then take their Tauri branches — `inTauri()` is exactly
 * `"__TAURI_INTERNALS__" in window` — so a CUJ exercises the real command names,
 * the real argument shapes, and the real event subscriptions, and only the
 * destination is fake.
 */

export interface TauriInternals {
  invoke(command: string, args?: Record<string, unknown>, options?: unknown): Promise<unknown>;
  transformCallback(callback: (payload: unknown) => void, once?: boolean): number;
  convertFileSrc(filePath: string, protocol?: string): string;
  metadata?: Record<string, unknown>;
}

export interface EventPluginInternals {
  unregisterListener(event: string, eventId: number): void;
}

declare global {
  interface Window {
    __TAURI_INTERNALS__?: TauriInternals;
  }
}

// `@tauri-apps/api` declares `__TAURI_EVENT_PLUGIN_INTERNALS__` as required, so
// it is reached through a cast rather than redeclared with a looser type.
const eventPluginSlot = "__TAURI_EVENT_PLUGIN_INTERNALS__";
type MutableWindow = Record<string, unknown>;

export interface InstallOptions {
  invoke: TauriInternals["invoke"];
  transformCallback: TauriInternals["transformCallback"];
  unregisterListener: EventPluginInternals["unregisterListener"];
  /** The asset-protocol URL builder, used for thumbnails. */
  convertFileSrc?: TauriInternals["convertFileSrc"];
}

/**
 * jsdom implements no media pipeline, and an unimplemented `play()` throws a
 * "Not implemented" error into the console on every autoplay. These stubs make
 * the element's *interface* exist; the harness drives its state with real DOM
 * events, and nothing here claims a frame was ever decoded.
 */
function stubMediaElement(): () => void {
  const proto = window.HTMLMediaElement.prototype as unknown as Record<string, unknown>;
  const original = {
    play: proto.play,
    pause: proto.pause,
    load: proto.load,
  };
  proto.play = function play(this: HTMLMediaElement) {
    this.dispatchEvent(new Event("play"));
    return Promise.resolve();
  };
  proto.pause = function pause(this: HTMLMediaElement) {
    this.dispatchEvent(new Event("pause"));
  };
  proto.load = function load() {};
  return () => {
    proto.play = original.play;
    proto.pause = original.pause;
    proto.load = original.load;
  };
}

export function installTauriRuntime(options: InstallOptions): () => void {
  if (window.__TAURI_INTERNALS__) {
    throw new Error("A Tauri runtime is already installed. Call harness.uninstall() first.");
  }

  window.__TAURI_INTERNALS__ = {
    invoke: options.invoke,
    transformCallback: options.transformCallback,
    convertFileSrc:
      options.convertFileSrc ??
      ((filePath: string, protocol = "asset") => `${protocol}://localhost/${encodeURIComponent(filePath)}`),
    metadata: {},
  };
  (window as unknown as MutableWindow)[eventPluginSlot] = {
    unregisterListener: options.unregisterListener,
  } satisfies EventPluginInternals;

  const restoreMedia = stubMediaElement();

  return () => {
    const mutable = window as unknown as MutableWindow;
    delete mutable.__TAURI_INTERNALS__;
    delete mutable[eventPluginSlot];
    delete mutable.__RCDVLOG_MEDIA_ORIGIN__;
    restoreMedia();
  };
}
