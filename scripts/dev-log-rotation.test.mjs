// Run with: node --test "scripts/*.test.mjs"
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import {
  planPrune,
  sessionFileName,
  TOTAL_CAP_BYTES,
  FILE_CAP_BYTES,
} from "./lib/dev-log-rotation.mjs";

describe("planPrune", () => {
  it("deletes nothing when total is under cap", () => {
    const files = [
      { name: "a.log", size: 10, mtimeMs: 1 },
      { name: "b.log", size: 10, mtimeMs: 2 },
    ];
    assert.deepEqual(planPrune(files, 100), []);
  });

  it("deletes nothing when total is exactly at cap", () => {
    const files = [
      { name: "a.log", size: 50, mtimeMs: 1 },
      { name: "b.log", size: 50, mtimeMs: 2 },
    ];
    assert.deepEqual(planPrune(files, 100), []);
  });

  it("deletes oldest-first until remaining is under cap", () => {
    const files = [
      { name: "new.log", size: 60, mtimeMs: 3 },
      { name: "old.log", size: 60, mtimeMs: 1 },
      { name: "mid.log", size: 60, mtimeMs: 2 },
    ];
    // total 180 > 100; delete old(60)->120, mid(60)->60 <= 100. Stop.
    assert.deepEqual(planPrune(files, 100), ["old.log", "mid.log"]);
  });

  it("handles empty input", () => {
    assert.deepEqual(planPrune([], 100), []);
  });

  it("caps are the documented sizes", () => {
    assert.equal(TOTAL_CAP_BYTES, 100 * 1024 * 1024);
    assert.equal(FILE_CAP_BYTES, 90 * 1024 * 1024);
  });
});

describe("sessionFileName", () => {
  it("formats a zero-padded local timestamp", () => {
    const d = new Date(2026, 7, 20, 9, 5, 3); // 2026-08-20 09:05:03 local
    assert.equal(sessionFileName(d), "dev-20260820-090503.log");
  });
});
