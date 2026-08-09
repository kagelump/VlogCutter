/**
 * Logical playback state.
 *
 * jsdom has no media pipeline and this makes no attempt to pretend otherwise.
 * What it does is dispatch the *real* DOM events at the *real* `<video>` the
 * app mounted, with the readonly properties the handlers read set first, so
 * play/pause orchestration, buffering, position bookkeeping, keyboard
 * transport, and error recovery are all exercised for real.
 *
 * What this can never establish: whether WKWebView paints a first frame,
 * whether a codec decodes, whether an unbuffered seek works. Those are the
 * three acceptance criteria in `docs/tasks/desktop/MEDIA_DELIVERY.md`, and they
 * stay in the native tier.
 */

export interface LoadedOptions {
  duration: number;
  currentTime?: number;
}

/** Overwrite a readonly media property the way a real load would set it. */
function define(element: HTMLMediaElement, property: string, value: unknown) {
  Object.defineProperty(element, property, { configurable: true, writable: true, value });
}

export class MediaController {
  constructor(
    private readonly assetId: string,
    private readonly wrap: <T>(action: () => Promise<T> | T) => Promise<T>,
  ) {}

  /** The element the app mounted for this clip, active slot or preloaded neighbour. */
  element(): HTMLVideoElement {
    const element = document.querySelector<HTMLVideoElement>(`video[data-asset-id="${this.assetId}"]`);
    if (!element) {
      const mounted = [...document.querySelectorAll("video")]
        .map((video) => video.getAttribute("data-asset-id"))
        .join(", ");
      throw new Error(`No media element for ${this.assetId}. Mounted: ${mounted || "(none)"}`);
    }
    return element;
  }

  private async dispatch(type: string) {
    const element = this.element();
    await this.wrap(() => {
      element.dispatchEvent(new Event(type));
    });
  }

  /** Metadata arrived: the clip now has a duration and a position. */
  async loaded(options: LoadedOptions) {
    const element = this.element();
    define(element, "duration", options.duration);
    define(element, "currentTime", options.currentTime ?? 0);
    await this.dispatch("loadedmetadata");
    await this.dispatch("canplay");
  }

  async playing() {
    await this.dispatch("play");
    await this.dispatch("playing");
  }

  async paused() {
    await this.dispatch("pause");
  }

  async buffering() {
    await this.dispatch("waiting");
  }

  async seeked(seconds: number) {
    define(this.element(), "currentTime", seconds);
    await this.dispatch("seeked");
    await this.dispatch("timeupdate");
  }

  async advanceTo(seconds: number) {
    define(this.element(), "currentTime", seconds);
    await this.dispatch("timeupdate");
  }

  async ended() {
    await this.dispatch("ended");
  }

  /**
   * The element could not load or decode. The app cannot see why — that is the
   * whole point of its recovery path — so no reason is passed to it; the string
   * is for the test's own readability.
   */
  async failed(_reason: "decode" | "network" | "origin" = "decode") {
    await this.dispatch("error");
  }
}
