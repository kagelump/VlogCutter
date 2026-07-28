import { existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import { dirname, extname, join, resolve } from "node:path";

const root = process.cwd();
const ignoredDirectories = new Set([".git", "node_modules", "target", "dist"]);
const markdownFiles = [];

function walk(directory) {
  // Skip git worktrees — they contain a .git file (not directory) at their
  // root and are independent checkouts whose docs are not this tree's concern.
  if (existsSync(join(directory, ".git"))) {
    try {
      if (statSync(join(directory, ".git")).isFile()) return;
    } catch (_) { /* proceed — if we can't stat it, it's not a worktree marker */ }
  }
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    if (entry.isDirectory() && ignoredDirectories.has(entry.name)) continue;
    const path = join(directory, entry.name);
    if (entry.isDirectory()) walk(path);
    else if (extname(entry.name) === ".md") markdownFiles.push(path);
  }
}

walk(root);

const failures = [];
const linkPattern = /\[[^\]]*\]\(([^)]+)\)/g;
for (const file of markdownFiles) {
  const source = readFileSync(file, "utf8");
  const lines = source.split("\n");
  const fenceCount = lines.filter((line) => /^\s*```/.test(line)).length;
  if (fenceCount % 2 !== 0) failures.push(`${file}: unbalanced fenced code blocks`);

  // Build a set of line indices that are inside fenced code blocks.
  const insideFence = new Set();
  let inFence = false;
  for (let i = 0; i < lines.length; i++) {
    if (/^\s*```/.test(lines[i])) {
      inFence = !inFence;
    }
    if (inFence) insideFence.add(i);
  }

  for (const match of source.matchAll(linkPattern)) {
    // Skip links inside fenced code blocks — they are illustrative, not
    // files the repository is expected to contain.
    const linkLine = source.substring(0, match.index).split("\n").length - 1;
    if (insideFence.has(linkLine)) continue;

    let target = match[1].trim();
    if (target.startsWith("<") && target.endsWith(">")) target = target.slice(1, -1);
    target = target.split(/\s+["']/)[0];
    if (/^(?:https?:|mailto:|#)/.test(target)) continue;
    if (target.startsWith("file:")) {
      failures.push(`${file}: file URI is not portable: ${target}`);
      continue;
    }
    // Skip template placeholder links — e.g. {{TASK_ID}} or
    // {{TASK_SLUG}} that are designed to be expanded later.
    if (/\{\{/.test(target)) continue;
    const pathPart = decodeURIComponent(target.split("#", 1)[0]);
    if (!pathPart) continue;
    const absolute = resolve(dirname(file), pathPart);
    if (!existsSync(absolute)) failures.push(`${file}: broken relative link: ${target}`);
  }
}

const required = [
  "docs/server/README.md",
  "docs/server/deployment.md",
  "docs/server/security.md",
  "docs/server/storage-and-library.md",
  "docs/server/sync.md",
  "docs/server/jobs-and-media.md",
  "docs/server/backup-and-recovery.md",
  "docs/server/api.md",
  "docs/server/development-and-testing.md",
  "docs/server/troubleshooting.md",
  "docs/tasks/server/schemas/baseline-v1.schema.json",
  "docs/tasks/server/schemas/reconcile-v1.schema.json",
  "docs/tasks/server/schemas/reconcile-response-v1.schema.json",
];
for (const path of required) {
  if (!existsSync(join(root, path)) || !statSync(join(root, path)).isFile()) {
    failures.push(`missing required documentation artifact: ${path}`);
  }
}

for (const path of required.filter((value) => value.endsWith(".json"))) {
  try {
    JSON.parse(readFileSync(join(root, path), "utf8"));
  } catch (error) {
    failures.push(`${path}: invalid frozen JSON schema: ${error.message}`);
  }
}

if (failures.length > 0) {
  process.stderr.write(`${failures.join("\n")}\n`);
  process.exit(1);
}

process.stdout.write(`Documentation check passed (${markdownFiles.length} Markdown files).\n`);
