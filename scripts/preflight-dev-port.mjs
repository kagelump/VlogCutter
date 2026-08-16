#!/usr/bin/env node
// Fail a dev-server start with a diagnosis instead of a bare EADDRINUSE.
//
//   node scripts/preflight-dev-port.mjs --port 1420 --purpose "the desktop dev server"
//
// Exits 0 when the port is free. Exits 1 with the holder named otherwise —
// naming it only; see `scripts/lib/port-holder.mjs` for why nothing is killed.

import { resolve } from "node:path";

import { commandLineOf, diagnose, findHolders, portIsFree, workingDirectoryOf } from "./lib/port-holder.mjs";

function option(name, fallback) {
  const at = process.argv.indexOf(`--${name}`);
  return at === -1 ? fallback : process.argv[at + 1];
}

const port = Number(option("port", "1420"));
const purpose = option("purpose", "the dev server");
if (!Number.isInteger(port) || port < 1 || port > 65_535) {
  console.error(`preflight-dev-port: --port must be a port number, got ${option("port", "")}`);
  process.exit(2);
}

if (await portIsFree(port)) process.exit(0);

const repositoryRoot = resolve(import.meta.dirname, "..");
const holders = findHolders(port);
const commandLines = Object.fromEntries(holders.map((holder) => [holder.pid, commandLineOf(holder.pid)]));
const workingDirectories = Object.fromEntries(holders.map((holder) => [holder.pid, workingDirectoryOf(holder.pid)]));

console.error(diagnose({ port, purpose, holders, repositoryRoot, commandLines, workingDirectories }).message);
process.exit(1);
