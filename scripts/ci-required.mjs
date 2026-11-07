
export function requiredFailures(needs) {
  const failures = [];
  for (const name of ["changes", "linux-rust", "frontend", "browser", "web-e2e-build", "web-e2e"]) {
    if (needs?.[name]?.result !== "success") failures.push(name);
  }
  const macosRequired = needs?.changes?.outputs?.macos !== "false";
  if (needs?.macos?.result !== (macosRequired ? "success" : "skipped")) failures.push("macos");
  return failures;
}
// import.meta.main survives symlinked paths (macOS /var -> /private/var).
if (import.meta.main) {
  const needs = JSON.parse(process.env.CI_NEEDS ?? "{}");
  const failures = requiredFailures(needs);
  console.log(JSON.stringify(needs, null, 2));
  if (failures.length) { console.error(`Required jobs failed or missing: ${failures.join(", ")}`); process.exitCode = 1; }
  else console.log("All required gates passed");
}
