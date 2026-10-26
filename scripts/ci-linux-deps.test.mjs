import assert from "node:assert/strict";
import { test } from "node:test";
import { mkdtempSync, writeFileSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { spawnSync } from "node:child_process";

function setup({ installed = false, fail = "" } = {}) {
  const dir = mkdtempSync(join(tmpdir(), "ci-deps-"));
  const log = join(dir, "log");
  const fake = (name, script) => writeFileSync(join(dir, name), `#!/bin/bash\n${script}\n`, { mode: 0o755 });
  fake("dpkg-query", 'if [[ "$*" == *Status* ]]; then [[ "$INSTALLED" == yes ]] && echo "install ok installed"; else echo versions; fi');
  fake("timeout", 'echo "timeout $*" >> "$LOG"; shift 2; "$@"');
  fake("sudo", '"$@"');
  fake("apt-get", 'echo "apt-get $*" >> "$LOG"; [[ "$*" != *"$FAIL"* ]]');
  fake("ffmpeg", 'echo ffmpeg');
  fake("ffprobe", 'echo ffprobe');
  fake("pkg-config", 'echo "pkg-config $*" >> "$LOG"; echo versions');
  try {
    const result = spawnSync("bash", [new URL("./ci-linux-deps.sh", import.meta.url).pathname], {
      encoding: "utf8",
      env: { ...process.env, PATH: `${dir}:${process.env.PATH}`, LOG: log, INSTALLED: installed ? "yes" : "no", FAIL: fail || "never-match" },
    });
    return { ...result, log: readFileSync(log, "utf8") };
  } finally { rmSync(dir, { recursive: true, force: true }); }
}
test("installed packages bypass network setup but still verify media and headers", () => {
  const result = setup({ installed: true });
  assert.equal(result.status, 0, result.stderr);
  assert.doesNotMatch(result.log, /apt-get|timeout/);
  assert.match(result.log, /pkg-config --modversion gtk\+-3.0 webkit2gtk-4.1/);
});
test("missing dependencies use bounded apt with limited retries and no recommendations", () => {
  const result = setup();
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.log, /timeout --kill-after=10s 90s sudo apt-get/);
  assert.match(result.log, /timeout --kill-after=10s 180s sudo apt-get/);
  assert.match(result.log, /Acquire::Retries=1/);
  assert.match(result.log, /Acquire::http::Timeout=20/);
  assert.match(result.log, /Acquire::https::Timeout=20/);
  assert.match(result.log, /DPkg::Lock::Timeout=30/);
  assert.match(result.log, /install -y --no-install-recommends ffmpeg libwebkit2gtk-4.1-dev/);
});
test("an update or install failure fails setup rather than continuing as green", () => {
  for (const fail of ["update", "install"]) {
    const result = setup({ fail });
    assert.notEqual(result.status, 0);
    assert.doesNotMatch(result.log, /pkg-config/);
    if (fail === "update") assert.doesNotMatch(result.log, /install -y/);
  }
});
