// The pinned-port preflight.
//
// Run with: node --test "scripts/*.test.mjs"

import { execFile } from "node:child_process";
import { createServer } from "node:net";
import { resolve } from "node:path";
import { describe, it } from "node:test";
import { promisify } from "node:util";
import assert from "node:assert/strict";

import {
  commandLineOf,
  diagnose,
  findHolders,
  isOurDevServer,
  parseLsof,
  portIsFree,
  workingDirectoryOf,
} from "./lib/port-holder.mjs";

const run = promisify(execFile);
const preflight = resolve(import.meta.dirname, "preflight-dev-port.mjs");

function listen() {
  return new Promise((resolve, reject) => {
    const server = createServer();
    server.on("error", reject);
    server.listen(0, "127.0.0.1", () => resolve(server));
  });
}

describe("portIsFree", () => {
  it("is true for a port nobody holds", async () => {
    const server = await listen();
    const port = server.address().port;
    await new Promise((done) => server.close(done));
    assert.equal(await portIsFree(port), true);
  });

  it("is false while something is listening", async () => {
    const server = await listen();
    assert.equal(await portIsFree(server.address().port), false);
    server.close();
  });
});

describe("parseLsof", () => {
  const output = [
    "COMMAND   PID  USER   FD   TYPE             DEVICE SIZE/OFF NODE NAME",
    "node    41234  kage   23u  IPv4 0x9a1b2c3d4e5f6789      0t0  TCP 127.0.0.1:1420 (LISTEN)",
    "node    41234  kage   24u  IPv6 0x9a1b2c3d4e5f6790      0t0  TCP [::1]:1420 (LISTEN)",
    "Postgres 512   kage    7u  IPv4 0x1111111111111111      0t0  TCP 127.0.0.1:1420 (LISTEN)",
    "",
  ].join("\n");

  it("reads the command and pid of each holder", () => {
    assert.deepEqual(parseLsof(output), [
      { command: "node", pid: 41_234 },
      { command: "Postgres", pid: 512 },
    ]);
  });

  it("reports one entry per process, not per socket", () => {
    // A dev server listening on both stacks is one process to kill, not two.
    assert.equal(parseLsof(output).filter((holder) => holder.pid === 41_234).length, 1);
  });

  it("returns nothing for empty or header-only output", () => {
    assert.deepEqual(parseLsof(""), []);
    assert.deepEqual(parseLsof("COMMAND PID USER FD TYPE DEVICE SIZE/OFF NODE NAME\n"), []);
  });
});

describe("findHolders", () => {
  it("survives an lsof that is missing or exits non-zero", () => {
    // lsof exits 1 when nothing matches, and is not installed everywhere.
    // Neither is a reason for the preflight itself to fail.
    assert.deepEqual(findHolders(1420, () => { throw new Error("lsof: command not found"); }), []);
  });
});

describe("isOurDevServer", () => {
  const root = "/Users/kage/code/VlogCutter";
  // What pnpm actually launches: no absolute path anywhere in argv, which is
  // why the working directory is what identifies the process and not the
  // command line.
  const vite = "node ./node_modules/.bin/../vite/bin/vite.js --host 127.0.0.1";

  it("recognises a vite running from inside this checkout", () => {
    assert.equal(isOurDevServer({ commandLine: vite, workingDirectory: `${root}/apps/desktop` }, root), true);
    assert.equal(isOurDevServer({ commandLine: vite, workingDirectory: root }, root), true);
  });

  it("does not claim a vite belonging to another project", () => {
    assert.equal(isOurDevServer({ commandLine: vite, workingDirectory: "/Users/kage/other" }, root), false);
  });

  it("is not fooled by a sibling directory with the same prefix", () => {
    assert.equal(isOurDevServer({ commandLine: vite, workingDirectory: `${root}-old` }, root), false);
  });

  it("does not claim some other process running out of this checkout", () => {
    assert.equal(
      isOurDevServer({ commandLine: "target/debug/rcdvlog serve", workingDirectory: root }, root),
      false,
    );
  });

  it("claims nothing when either fact could not be read", () => {
    assert.equal(isOurDevServer({ commandLine: "", workingDirectory: root }, root), false);
    assert.equal(isOurDevServer({ commandLine: vite, workingDirectory: "" }, root), false);
    assert.equal(isOurDevServer({ commandLine: vite, workingDirectory: root }, ""), false);
  });
});

describe("workingDirectoryOf", () => {
  it("reads this process's own working directory", () => {
    assert.equal(workingDirectoryOf(process.pid), process.cwd());
  });

  it("returns an empty string rather than throwing for a pid that is gone", () => {
    assert.equal(workingDirectoryOf(2_147_483_646, () => { throw new Error("no such process"); }), "");
  });

  it("reads the cwd off lsof field output where there is no /proc", () => {
    const lsof = () => "p512\nfcwd\nn/Users/kage/code/VlogCutter/apps/desktop\n";
    assert.equal(workingDirectoryOf(2_147_483_646, lsof), "/Users/kage/code/VlogCutter/apps/desktop");
  });
});

describe("diagnose", () => {
  const root = "/Users/kage/code/VlogCutter";
  const base = { port: 1420, purpose: "the desktop dev server", repositoryRoot: root };

  it("offers the exact kill line for our own leftover dev server", () => {
    const result = diagnose({
      ...base,
      holders: [{ command: "node", pid: 41_234 }],
      commandLines: { 41_234: "node ./node_modules/.bin/../vite/bin/vite.js" },
      workingDirectories: { 41_234: `${root}/apps/desktop` },
    });
    assert.equal(result.ours, true);
    assert.match(result.message, /kill 41234/);
    assert.match(result.message, /this repository's dev server/);
  });

  it("names an unrecognised holder and offers nothing", () => {
    const result = diagnose({
      ...base,
      holders: [{ command: "Postgres", pid: 512 }],
      commandLines: { 512: "/usr/local/bin/postgres -D /usr/local/var/postgres" },
      workingDirectories: { 512: "/usr/local/var/postgres" },
    });
    assert.equal(result.ours, false);
    // Killing what it cannot identify is how a preflight takes down a database.
    assert.doesNotMatch(result.message, /kill 512/);
    assert.match(result.message, /Postgres \(pid 512\)/);
    assert.match(result.message, /stop it yourself/);
  });

  it("still says something useful when the holder could not be determined", () => {
    const result = diagnose({ ...base, holders: [] });
    assert.match(result.message, /already in use/);
    assert.match(result.message, /lsof -nP -iTCP:1420/);
  });

  it("names the port and what needs it, every time", () => {
    for (const holders of [[], [{ command: "node", pid: 7 }]]) {
      const message = diagnose({ ...base, holders, commandLines: {} }).message;
      assert.match(message, /1420/);
      assert.match(message, /the desktop dev server/);
    }
  });
});

describe("the preflight command", () => {
  it("exits zero when the port is free", async () => {
    const server = await listen();
    const port = server.address().port;
    await new Promise((done) => server.close(done));

    const { stdout } = await run(process.execPath, [preflight, "--port", String(port)]);
    assert.equal(stdout, "");
  });

  it("exits non-zero and names the port when it is held", async () => {
    const server = await listen();
    const port = server.address().port;

    await assert.rejects(
      run(process.execPath, [preflight, "--port", String(port), "--purpose", "the desktop dev server"]),
      (error) => {
        assert.equal(error.code, 1);
        assert.match(error.stderr, new RegExp(String(port)));
        assert.match(error.stderr, /the desktop dev server/);
        return true;
      },
    );
    server.close();
  });

  it("refuses a port argument that is not a port", async () => {
    await assert.rejects(
      run(process.execPath, [preflight, "--port", "not-a-port"]),
      (error) => {
        assert.equal(error.code, 2);
        return true;
      },
    );
  });
});

describe("commandLineOf", () => {
  it("returns something for this very process, or nothing it can be trusted on", () => {
    const line = commandLineOf(process.pid);
    if (line) assert.match(line, /node/i);
  });

  it("returns an empty string rather than throwing for a pid that is gone", () => {
    assert.equal(commandLineOf(2_147_483_646, () => { throw new Error("no such process"); }), "");
  });
});
