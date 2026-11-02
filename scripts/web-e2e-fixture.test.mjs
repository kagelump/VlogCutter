// Where the web e2e fixture's bootstrap codes live.
//
// Run with: node --test "scripts/*.test.mjs"

import { tmpdir } from "node:os";
import { describe, it } from "node:test";
import assert from "node:assert/strict";

import { bootstrapCodePath, fixturePort, reviewRunPath, reviewTargetHandoffPath, rustLaunch } from "./lib/web-e2e-fixture.mjs";

describe("fixturePort", () => {
  it("reads the port off the listen address the server was given", () => {
    assert.equal(fixturePort("127.0.0.1:41235"), "41235");
  });

  it("reads it off the base URL the suite was given", () => {
    assert.equal(fixturePort("http://127.0.0.1:41235"), "41235");
  });

  it("refuses an address with no port rather than guessing one", () => {
    // Silently falling back to a default is how two runs end up sharing codes.
    assert.throws(() => fixturePort("http://127.0.0.1"), /no port/);
    assert.throws(() => fixturePort("127.0.0.1"), /no port/);
  });
});

describe("bootstrapCodePath", () => {
  it("keys the file to the run's port, so concurrent runs cannot collide", () => {
    const mine = bootstrapCodePath("127.0.0.1:41235", 1);
    const theirs = bootstrapCodePath("127.0.0.1:41236", 1);
    assert.ok(mine.startsWith(tmpdir()));
    assert.notEqual(mine, theirs);
  });

  it("gives each of a run's codes its own file", () => {
    assert.notEqual(bootstrapCodePath("127.0.0.1:41235", 1), bootstrapCodePath("127.0.0.1:41235", 2));
  });

  it("agrees between the address the server binds and the URL the suite loads", () => {
    // The server and the Chrome suite are separate processes holding different
    // spellings of the same fact; they have to resolve to one path.
    assert.equal(
      bootstrapCodePath("127.0.0.1:41235", 3),
      bootstrapCodePath("http://127.0.0.1:41235", 3),
    );
  });
});

describe("reviewTargetHandoffPath", () => {
  it("keys the synthetic handoff to the run's port", () => {
    assert.notEqual(reviewTargetHandoffPath("127.0.0.1:41235"), reviewTargetHandoffPath("127.0.0.1:41236"));
    assert.equal(reviewTargetHandoffPath("http://127.0.0.1:41235"), reviewTargetHandoffPath("127.0.0.1:41235"));
  });
});

describe("reviewRunPath", () => {
  it("keys the seeded review path to the run's port", () => {
    assert.notEqual(reviewRunPath("127.0.0.1:41235"), reviewRunPath("127.0.0.1:41236"));
  });
});

describe("rustLaunch", () => {
  it("builds from source with cargo by default", () => {
    assert.deepEqual(rustLaunch({}, "server", []), { command: "cargo", args: ["run", "--quiet", "-p", "rcdvlog-server"] });
    assert.deepEqual(rustLaunch({}, "server", ["admin", "bootstrap"]), {
      command: "cargo",
      args: ["run", "--quiet", "-p", "rcdvlog-server", "--", "admin", "bootstrap"],
    });
    assert.deepEqual(rustLaunch({}, "seed", []), {
      command: "cargo",
      args: ["run", "--quiet", "-p", "rcdvlog-server", "--example", "web_e2e_seed"],
    });
  });

  it("uses the prebuilt binaries CI hands in", () => {
    const environment = { RCDVLOG_WEB_E2E_SERVER_BIN: "/opt/bin/rcdvlog-server", RCDVLOG_WEB_E2E_SEED_BIN: "/opt/bin/seed" };
    assert.deepEqual(rustLaunch(environment, "server", ["admin"]), { command: "/opt/bin/rcdvlog-server", args: ["admin"] });
    assert.deepEqual(rustLaunch(environment, "seed", []), { command: "/opt/bin/seed", args: [] });
  });

  it("refuses a relative prebuilt path", () => {
    assert.throws(() => rustLaunch({ RCDVLOG_WEB_E2E_SERVER_BIN: "target/debug/rcdvlog-server" }, "server", []), /absolute/);
  });
});
