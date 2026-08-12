/**
 * The ratchet.
 *
 * The suite above this file is only as honest as the harness, and the harness
 * is only honest while every command the frontend can issue is accounted for.
 * This compares three sets — what the frontend calls, what the harness handles,
 * what Rust registers — and fails on any difference that matters.
 *
 * The practical effect: adding a production `invoke()` fails here immediately,
 * with the command named, before anyone writes a CUJ that would have exercised
 * it. That is the property that keeps this tier from rotting.
 */

import { describe, expect, it } from "vitest";
import { desktopScenario } from "../harness";
import {
  CHRONOLOGY_COMMANDS_SOURCE,
  CHRONOLOGY_TYPES_SOURCE,
  backendCommands,
  contractExemptions,
  dynamicInvocations,
  exemptCommands,
  frontendCommands,
  harnessCommands,
  rustStructFields,
  typescriptInterfaceFields,
  uncalledBackendCommands,
} from "../harness/contract";

describe("the frontend's command surface", () => {
  it("is fully handled by the harness", () => {
    const unhandled = [...frontendCommands()].filter(
      (command) => !harnessCommands().has(command) && !exemptCommands().has(command),
    );
    expect(unhandled, "add a handler in src/test/harness/commands/, or an exemption with a reason").toEqual([]);
  });

  it("is fully registered on the Rust side", () => {
    // A frontend call to a command Rust does not register fails at runtime with
    // an unhelpful error. Catching it in jsdom costs nothing.
    const unregistered = [...frontendCommands()].filter(
      (command) => !backendCommands().has(command) && !command.startsWith("plugin:"),
    );
    expect(unregistered, "these commands are invoked but not registered in src-tauri/src/lib.rs").toEqual([]);
  });

  it("contains no command name the extractor cannot see", () => {
    // A templated or computed name would be invisible above, so it fails here
    // rather than passing as coverage.
    expect(dynamicInvocations()).toEqual([]);
  });

  it("is not empty, so a broken extractor cannot pass silently", () => {
    expect(frontendCommands().size).toBeGreaterThan(60);
    expect(backendCommands().size).toBeGreaterThan(60);
  });
});

describe("the harness's own guarantees", () => {
  it("rejects a command it does not implement, naming it", async () => {
    const harness = desktopScenario().build().install();
    try {
      await expect(window.__TAURI_INTERNALS__!.invoke("a_command_nobody_implemented")).rejects.toThrow(
        /Unhandled Tauri command in test harness: a_command_nobody_implemented/,
      );
    } finally {
      harness.uninstall();
    }
  });

  it("refuses to install twice, so no test can leak a runtime into the next", () => {
    const first = desktopScenario().build().install();
    try {
      expect(() => desktopScenario().build().install()).toThrow(/already installed/);
    } finally {
      first.uninstall();
    }
  });

  it("rejects an argument sent under the wrong name rather than guessing", async () => {
    const harness = desktopScenario()
      .project("Hokkaido field test", { id: "project_hokkaido" })
      .camera("Sony A7C II", { clips: 1 })
      .build()
      .install();
    try {
      // snake_case is what the bridge would produce if an adapter stopped
      // camelCasing; accepting it would hide exactly that regression.
      await expect(
        window.__TAURI_INTERNALS__!.invoke("list_assets", { project_id: "project_hokkaido" }),
      ).rejects.toThrow(/must be a string/);
    } finally {
      harness.uninstall();
    }
  });

  it("states a reason for every exemption", () => {
    for (const entry of contractExemptions) {
      expect(entry.command, "an exemption needs a command").toBeTruthy();
      expect(entry.reason.trim().length, `exemption for ${entry.command} has no reason`).toBeGreaterThan(20);
    }
  });
});

describe("the backend's command surface", () => {
  it("reports commands no frontend call reaches", () => {
    // Not a failure: the CLI and the Rust tests are legitimate callers, and a
    // command may land before the UI that uses it. This is a visible inventory
    // so the gap is a decision rather than an accident.
    const uncalled = [...backendCommands()].filter((command) => !frontendCommands().has(command)).sort();
    expect(uncalled).toEqual(uncalledBackendCommands.map((entry) => entry.command));
  });

  it("states the decision behind every command the interface does not reach", () => {
    for (const entry of uncalledBackendCommands) {
      expect(entry.reason.trim().length, `no decision recorded for ${entry.command}`).toBeGreaterThan(40);
    }
  });
});

describe("the chronology overview DTO", () => {
  // Caught for real: `declinedSplits` was added to the TypeScript type and to
  // the harness but never to the Rust struct, so the field arrived `undefined`
  // and reading `.length` blanked the whole app. No type error, and the CUJs
  // passed because the harness builds its payload from the TypeScript side.
  it("has the same fields in Rust and in TypeScript", () => {
    const rust = rustStructFields(CHRONOLOGY_COMMANDS_SOURCE, "ChronologyOverview");
    const typescript = typescriptInterfaceFields(CHRONOLOGY_TYPES_SOURCE, "ChronologyOverview");

    expect(
      [...typescript].filter((field) => !rust.has(field)).sort(),
      "the frontend reads these but Rust never sends them",
    ).toEqual([]);
    expect(
      [...rust].filter((field) => !typescript.has(field)).sort(),
      "Rust sends these but the frontend type does not declare them",
    ).toEqual([]);
  });
});
