// The eval bridge address contract, from the client's side.
//
// The app-side half of this contract has Rust tests in
// `apps/desktop/src-tauri/src/eval_bridge.rs`; these cover the resolution the
// shell client and the native runner share.
//
// Run with: node --test "scripts/*.test.mjs"

import { execFile } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:net";
import { join, resolve } from "node:path";
import { tmpdir } from "node:os";
import { Readable } from "node:stream";
import { after, describe, it } from "node:test";
import { promisify } from "node:util";
import assert from "node:assert/strict";

import {
  addressFilePath,
  parseMarkerLine,
  readMarkerAddress,
  readPublishedAddress,
  resolveAddress,
} from "./lib/eval-bridge.mjs";

const root = mkdtempSync(join(tmpdir(), "eval-bridge-test-"));
after(() => rmSync(root, { recursive: true, force: true }));

function publish(name, document) {
  const path = join(root, name);
  writeFileSync(path, typeof document === "string" ? document : JSON.stringify(document));
  return path;
}

describe("marker line", () => {
  it("yields the address the app printed", () => {
    assert.equal(
      parseMarkerLine("[RCDVLOG_EVAL_BRIDGE_COMPILED] listening on ws://127.0.0.1:53125"),
      "ws://127.0.0.1:53125",
    );
  });

  it("ignores every other line the app prints", () => {
    for (const line of [
      "[media-origin] serving review media on http://127.0.0.1:53125",
      "[eval-bridge] evaluating:",
      "[RCDVLOG_EVAL_BRIDGE_COMPILED] listening on ws://10.0.0.4:53125",
      "",
    ]) {
      assert.equal(parseMarkerLine(line), null, line);
    }
  });

  it("finds the address in a stream that has other output around it", async () => {
    const stdout = Readable.from([
      "[media-origin] serving review media on http://127.0.0.1:1\n[RCDVLOG_EVAL_BR",
      "IDGE_COMPILED] listening on ws://127.0.0.1:53125\nready\n",
    ]);
    assert.equal(await readMarkerAddress(stdout), "ws://127.0.0.1:53125");
  });

  it("resolves to null when the app exits without ever listening", async () => {
    assert.equal(await readMarkerAddress(Readable.from(["boom\n"])), null);
  });
});

describe("published address", () => {
  it("is usable while the publishing process is alive", () => {
    const path = publish("live.json", {
      address: "ws://127.0.0.1:53125",
      port: 53125,
      pid: process.pid,
    });
    assert.deepEqual(readPublishedAddress(path), {
      state: "running",
      address: "ws://127.0.0.1:53125",
      pid: process.pid,
      path,
    });
  });

  it("is stale, not usable, once that process is gone", () => {
    // PID 1 exists everywhere; a freed high PID reliably does not.
    const path = publish("stale.json", {
      address: "ws://127.0.0.1:53125",
      port: 53125,
      pid: 0x7ffffffe,
    });
    assert.equal(readPublishedAddress(path).state, "stale");
  });

  it("reports a missing or malformed file rather than guessing", () => {
    assert.equal(readPublishedAddress(join(root, "absent.json")).state, "missing");
    assert.equal(readPublishedAddress(publish("garbage.json", "{")).state, "unreadable");
    assert.equal(readPublishedAddress(publish("partial.json", { port: 1 })).state, "unreadable");
  });
});

describe("address resolution", () => {
  it("prefers an explicit port, then the app's pinned override", async () => {
    assert.deepEqual(await resolveAddress({ env: {}, port: 4000 }), {
      address: "ws://127.0.0.1:4000",
      source: "--port",
    });
    const env = { RCDVLOG_EVAL_BRIDGE_PORT: "47961" };
    assert.equal((await resolveAddress({ env })).address, "ws://127.0.0.1:47961");
  });

  it("falls back to the address the running app published", async () => {
    const path = publish("resolve.json", {
      address: "ws://127.0.0.1:53125",
      port: 53125,
      pid: process.pid,
    });
    const env = { RCDVLOG_EVAL_BRIDGE_ADDRESS_FILE: path };
    assert.equal(addressFilePath(env), path);
    assert.equal((await resolveAddress({ env })).address, "ws://127.0.0.1:53125");
  });

  it("fails with an actionable message when no bridge is running", async () => {
    const env = { RCDVLOG_EVAL_BRIDGE_ADDRESS_FILE: join(root, "nothing.json") };
    await assert.rejects(resolveAddress({ env }), /RCDVLOG_EVAL_BRIDGE=1/);

    const stale = publish("gone.json", {
      address: "ws://127.0.0.1:53125",
      port: 53125,
      pid: 0x7ffffffe,
    });
    await assert.rejects(
      resolveAddress({ env: { RCDVLOG_EVAL_BRIDGE_ADDRESS_FILE: stale } }),
      /which is gone/,
    );
  });

  it("gives up waiting rather than hanging when the app never comes up", async () => {
    const env = { RCDVLOG_EVAL_BRIDGE_ADDRESS_FILE: join(root, "never.json") };
    const started = Date.now();
    await assert.rejects(resolveAddress({ env, waitMs: 250 }));
    assert.ok(Date.now() - started >= 200, "the wait should have been honoured");
  });
});

describe("the rcdvlog-eval client", () => {
  const CLIENT = resolve(import.meta.dirname, "rcdvlog-eval");
  const run = async (args, env = {}) => {
    try {
      await promisify(execFile)(process.execPath, [CLIENT, ...args], {
        env: { ...process.env, RCDVLOG_EVAL_BRIDGE_PORT: "", ...env },
        timeout: 20_000,
      });
      return { code: 0, stderr: "" };
    } catch (error) {
      return { code: error.code, stderr: String(error.stderr) };
    }
  };

  it("names the missing bridge instead of a hardcoded address", async () => {
    const { code, stderr } = await run(["1 + 1"], {
      RCDVLOG_EVAL_BRIDGE_ADDRESS_FILE: join(root, "cli-absent.json"),
    });
    assert.equal(code, 1);
    assert.match(stderr, /No eval bridge address at .*cli-absent\.json/);
  });

  it("connects to the port it was given", async () => {
    // A port nothing is listening on: the client should report that exact
    // address, which is what proves --port reached the connection.
    const idle = createServer();
    await new Promise((done) => idle.listen(0, "127.0.0.1", done));
    const port = idle.address().port;
    await new Promise((done) => idle.close(done));

    const { code, stderr } = await run(["--port", String(port), "1 + 1"]);
    assert.equal(code, 1);
    assert.match(stderr, new RegExp(`Could not connect to ws://127\\.0\\.0\\.1:${port}`));
  });

  it("rejects a malformed connection option before evaluating anything", async () => {
    for (const args of [["--port", "0", "1"], ["--port"], ["--wait", "-1", "1"]]) {
      assert.equal((await run(args)).code, 2, args.join(" "));
    }
  });
});
