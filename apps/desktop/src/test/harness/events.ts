/**
 * The event transport.
 *
 * `listen()` in `@tauri-apps/api` is not a special API — it registers a callback
 * through `transformCallback` and then invokes `plugin:event|listen` with the id
 * it got back. `unlisten()` calls
 * `__TAURI_EVENT_PLUGIN_INTERNALS__.unregisterListener` and invokes
 * `plugin:event|unlisten`. Serving both here means a CUJ exercises the real
 * subscription and the real teardown, so a listener leaked on unmount is a test
 * failure rather than a slow memory problem in production.
 */

interface Registration {
  event: string;
  callbackId: number;
}

export interface TauriEventPayload<T> {
  event: string;
  id: number;
  payload: T;
}

export class EventBus {
  private callbacks = new Map<number, (payload: unknown) => void>();
  private onceCallbacks = new Set<number>();
  private registrations = new Map<number, Registration>();
  private nextId = 1;

  /** `window.__TAURI_INTERNALS__.transformCallback` */
  transformCallback(callback: (payload: unknown) => void, once = false): number {
    const id = this.nextId++;
    this.callbacks.set(id, callback);
    if (once) this.onceCallbacks.add(id);
    return id;
  }

  /** `plugin:event|listen` */
  listen(event: string, callbackId: number): number {
    const eventId = this.nextId++;
    this.registrations.set(eventId, { event, callbackId });
    return eventId;
  }

  /** `plugin:event|unlisten` and `__TAURI_EVENT_PLUGIN_INTERNALS__.unregisterListener` */
  unlisten(event: string, eventId: number) {
    const registration = this.registrations.get(eventId);
    if (!registration || registration.event !== event) return;
    this.registrations.delete(eventId);
    this.callbacks.delete(registration.callbackId);
    this.onceCallbacks.delete(registration.callbackId);
  }

  /** How many live listeners an event has. A CUJ asserts this drops to zero on unmount. */
  listenerCount(event: string): number {
    let count = 0;
    for (const registration of this.registrations.values()) {
      if (registration.event === event) count += 1;
    }
    return count;
  }

  /**
   * Deliver an event to every live listener, in the envelope Tauri uses. Returns
   * the number of listeners reached, so a test can prove an emit landed rather
   * than assuming it did.
   */
  emit<T>(event: string, payload: T): number {
    let delivered = 0;
    for (const [eventId, registration] of [...this.registrations.entries()]) {
      if (registration.event !== event) continue;
      const callback = this.callbacks.get(registration.callbackId);
      if (!callback) continue;
      const envelope: TauriEventPayload<T> = { event, id: eventId, payload };
      callback(envelope);
      delivered += 1;
      if (this.onceCallbacks.has(registration.callbackId)) this.unlisten(event, eventId);
    }
    return delivered;
  }

  reset() {
    this.callbacks.clear();
    this.onceCallbacks.clear();
    this.registrations.clear();
    this.nextId = 1;
  }
}
