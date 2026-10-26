import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

export function requiredFailures(needs) {
  const failures = [];
  for (const name of ["changes", "linux-rust", "frontend", "browser"]) {
    if (needs?.[name]?.result !== "success") failures.push(name);
  }
  const macosRequired = needs?.changes?.outputs?.macos !== "false";
  if (needs?.macos?.result !== (macosRequired ? "success" : "skipped")) failures.push("macos");
  return failures;
}
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const needs = JSON.parse(process.env.CI_NEEDS ?? "{}");
  const failures = requiredFailures(needs);
  console.log(JSON.stringify(needs, null, 2));
  if (failures.length) { console.error(`Required jobs failed or missing: ${failures.join(", ")}`); process.exitCode = 1; }
  else console.log("All required gates passed");
}
