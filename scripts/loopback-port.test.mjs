// The fixture port allocator.
//
// Run with: node --test "scripts/*.test.mjs"

import { createServer } from "node:net";
import { describe, it } from "node:test";
import assert from "node:assert/strict";

import { allocateLoopbackAddress, allocateLoopbackPort } from "./lib/loopback-port.mjs";

function listen(port) {
  return new Promise((resolve, reject) => {
    const server = createServer();
    server.on("error", reject);
    server.listen(port, "127.0.0.1", () => resolve(server));
  });
}

describe("allocateLoopbackPort", () => {
  it("reports a port the caller can then actually bind", async () => {
    const port = await allocateLoopbackPort();
    assert.ok(port > 0 && port < 65_536, `expected a usable port, got ${port}`);
    // The whole point of asking: the probe must be closed by the time it
    // answers, or every fixture using this would fail to start.
    const server = await listen(port);
    assert.equal(server.address().port, port);
    server.close();
  });

  it("does not hand out a port twice in a row", async () => {
    // Two runs starting together is the case this exists for. The kernel is
    // free to reuse a released port eventually, so this asserts only that two
    // allocations made while both are outstanding differ.
    const held = await listen(await allocateLoopbackPort());
    const second = await allocateLoopbackPort();
    assert.notEqual(second, held.address().port);
    held.close();
  });

  it("formats an address the server binary accepts", async () => {
    assert.match(await allocateLoopbackAddress(), /^127\.0\.0\.1:\d{1,5}$/);
  });
});
