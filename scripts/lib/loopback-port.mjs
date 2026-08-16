// A loopback port nobody else is using.
//
// Fixed ports are contracts; ephemeral ports are implementation (AGENTS.md §7).
// A test fixture is never a contract, so it asks the kernel for a port instead
// of hoping a hard-coded one is free. Two checkouts, or a run alongside a
// leftover server from an interrupted one, then cannot collide — and, more
// importantly, cannot silently talk to each other's fixture data.
//
// The port is released before the caller's real listener claims it, so this is
// a hint rather than a reservation. That is deliberate and it is not papered
// over: there is no retry loop and no fallback range here, because a fixture
// that comes up somewhere its client is not looking is worse than a loud bind
// error.
//
// Run the tests with: node --test "scripts/*.test.mjs"

import { createServer } from "node:net";

/** A free TCP port on 127.0.0.1, as reported by the kernel. */
export function allocateLoopbackPort() {
  return new Promise((resolve, reject) => {
    const probe = createServer();
    probe.on("error", reject);
    probe.listen(0, "127.0.0.1", () => {
      const address = probe.address();
      if (address === null || typeof address === "string") {
        probe.close();
        reject(new Error("allocate loopback port: the probe reported no numeric address"));
        return;
      }
      probe.close((error) => (error ? reject(error) : resolve(address.port)));
    });
  });
}

/** The same port as a `127.0.0.1:<port>` listen address. */
export async function allocateLoopbackAddress() {
  return `127.0.0.1:${await allocateLoopbackPort()}`;
}
