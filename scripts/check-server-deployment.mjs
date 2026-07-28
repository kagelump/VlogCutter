import { readFileSync } from "node:fs";
import { resolve } from "node:path";

const repository = resolve(import.meta.dirname, "..");
const compose = readFileSync(resolve(repository, "deploy/server/compose.yaml"), "utf8");

const checks = [
  ["host networking is explicit", /^\s{4}network_mode:\s*host\s*$/m],
  ["container filesystem is read-only", /^\s{4}read_only:\s*true\s*$/m],
  ["backend listens only on loopback", /^\s{6}RCDVLOG_LISTEN_ADDR:\s*127\.0\.0\.1:8090\s*$/m],
  [
    "original media mount is read-only",
    /source:\s*\$\{RCDVLOG_ORIGINALS_HOST_PATH[^\n]*\}\s*\n\s*target:\s*\/media\/originals\s*\n\s*read_only:\s*true/m,
  ],
];

for (const [description, pattern] of checks) {
  if (!pattern.test(compose)) throw new Error(`deployment invariant failed: ${description}`);
}
if (/^\s{4}ports:\s*$/m.test(compose)) {
  throw new Error("deployment invariant failed: backend must not publish container ports");
}
if (/tailscale\s+funnel/i.test(compose)) {
  throw new Error("deployment invariant failed: Tailscale Funnel must not be configured");
}

console.log("server deployment invariants passed (loopback backend, read-only originals, no published ports)");
