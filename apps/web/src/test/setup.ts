import "@testing-library/jest-dom/vitest";

// Components open a live change feed when WebSocket exists; tests drive data through the mocked client instead.
vi.stubGlobal("WebSocket", undefined);

// React Router builds a `Request` per navigation with an AbortSignal. Under jsdom the signal comes from
// jsdom while `Request` is Node's own (undici), which rejects foreign signals on newer Node versions.
// Tests never abort navigations, so the signal is dropped rather than mixing the two implementations.
const NodeRequest = globalThis.Request;
class TestRequest extends NodeRequest {
  constructor(input: RequestInfo | URL, init?: RequestInit) {
    if (init?.signal) {
      const { signal: _signal, ...rest } = init;
      super(input, rest);
    } else {
      super(input, init);
    }
  }
}
vi.stubGlobal("Request", TestRequest);
