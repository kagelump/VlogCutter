import { appendFileSync, readFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

// Only existing, modified Linux deployment files. Server Rust, Docker build
// recipes, docs and new/renamed paths deliberately remain platform-relevant.
export const linuxOnly = new Set([
  "deploy/server/Caddyfile",
  "deploy/server/compose.yaml",
  "deploy/server/example.env",
]);
export function classifyChanges(records) {
  if (!Array.isArray(records) || records.length === 0) return { macos: true, reason: "No reliable changed files; running macOS" };
  if (records.every(({ status, path }) => status === "M" && linuxOnly.has(path))) {
    return { macos: false, reason: "Only existing Linux deployment configuration modified; macOS skipped" };
  }
  return { macos: true, reason: "Shared, unknown, added, deleted or renamed path; running macOS" };
}
export function decidePlatform(eventName, event, git = (args) => {
  const result = spawnSync("git", args, { encoding: "utf8", timeout: 30_000 });
  if (result.status !== 0) throw new Error("Git change detection failed");
  return result.stdout;
}) {
  try {
    let base, head, range;
    if (eventName === "pull_request") {
      base = event.pull_request?.base?.sha;
      head = event.pull_request?.head?.sha;
      range = "..."; // Compare the PR against its merge base, including forks.
    } else if (eventName === "push") {
      base = event.before;
      head = event.after;
      range = "..";
    } else return { macos: true, reason: "Manual, scheduled or merge-queue full run" };
    if (![base, head].every((sha) => /^[a-f0-9]{40}$/.test(sha ?? "") && !/^0+$/.test(sha))) throw new Error("Missing SHA");
    const fields = git(["diff", "--no-renames", "--name-status", "-z", `${base}${range}${head}`]).split("\0");
    if (fields.pop() !== "" || fields.length % 2 !== 0) throw new Error("Invalid diff");
    const records = [];
    for (let i = 0; i < fields.length; i += 2) records.push({ status: fields[i], path: fields[i + 1] });
    return classifyChanges(records);
  } catch {
    return { macos: true, reason: "Change detection unavailable or ambiguous; running macOS" };
  }
}
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  let result;
  try { result = decidePlatform(process.env.GITHUB_EVENT_NAME, JSON.parse(readFileSync(process.env.GITHUB_EVENT_PATH, "utf8"))); }
  catch { result = { macos: true, reason: "Event unavailable; running macOS" }; }
  console.log(result.reason);
  if (process.env.GITHUB_OUTPUT) appendFileSync(process.env.GITHUB_OUTPUT, `macos=${result.macos}\nreason=${result.reason}\n`);
  if (process.env.GITHUB_STEP_SUMMARY) appendFileSync(process.env.GITHUB_STEP_SUMMARY, `${result.reason}\n`);
}
