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

  it("handles source inspection with a declared fixture and camelCase request", async () => {
    const harness = desktopScenario().build().install();
    const request = {
      assetId: "asset_harness",
      representation: "original",
      purpose: "locator",
      sourceInSeconds: 1,
      sourceOutSeconds: 2,
      sampleCount: 1,
      maxWidth: 160,
    };
    try {
      const result = (await window.__TAURI_INTERNALS__!.invoke("inspect_source_window", {
        request,
      })) as {
        inspection: { evidenceId: string; limitations: string[] };
        reused: boolean;
      };
      expect(result.reused).toBe(false);
      expect(result.inspection.evidenceId).toBe("inspect_harness_fixture");
      expect(result.inspection.limitations.join(" ")).toContain("no media was probed");

      await expect(
        window.__TAURI_INTERNALS__!.invoke("inspect_source_window", {
          request: {
            asset_id: "asset_harness",
            representation: "original",
            purpose: "locator",
            source_in_seconds: 1,
            source_out_seconds: 2,
            sample_count: 1,
            max_width: 160,
          },
        }),
      ).rejects.toThrow(/request\.assetId must be a string/);
    } finally {
      harness.uninstall();
    }
  });

  it("handles preview artifact reads and controls from declared project-owned fixtures", async () => {
    const harness = desktopScenario().project("Preview artifacts", { id: "project_preview" }).build().install();
    try {
      const revision = (await window.__TAURI_INTERNALS__!.invoke("create_edit_plan_revision", {
        projectId: "project_preview",
        expectedActiveRevisionId: null,
        operations: [],
      })) as { revisionId: string; projectId: string };
      const artifactId = "preview_fixture_1";
      harness.state.previewArtifactPages[revision.revisionId] = {
        schemaReady: true,
        revisionId: revision.revisionId,
        total: 1,
        nextOffset: null,
        items: [],
      };
      const status = {
        artifact: { artifactId, projectId: "project_preview" },
        observedState: "running",
        latestAttempt: true,
        activeRevisionId: null,
        filePresent: false,
        currentForRequest: null,
        verification: null,
      } as import("../../chronologyTypes").PreviewArtifactStatus;
      harness.state.previewArtifactStatuses[artifactId] = status;

      await expect(window.__TAURI_INTERNALS__!.invoke("preview_artifacts", {
        projectId: "project_preview",
        revisionId: revision.revisionId,
      })).resolves.toMatchObject({ schemaReady: true, revisionId: revision.revisionId });
      await expect(window.__TAURI_INTERNALS__!.invoke("preview_artifact_status", {
        projectId: "project_preview", artifactId, verify: true,
      })).resolves.toBe(status);
      await expect(window.__TAURI_INTERNALS__!.invoke("cancel_preview_artifact", {
        projectId: "project_preview", artifactId,
      })).resolves.toBe(status);
      await expect(window.__TAURI_INTERNALS__!.invoke("wait_preview_artifact", {
        projectId: "project_preview", artifactId, timeoutSeconds: 1,
      })).resolves.toBe(status);
      await expect(window.__TAURI_INTERNALS__!.invoke("preview_artifact_status", {
        projectId: "other_project", artifactId,
      })).rejects.toThrow(/belongs to another project/);
      await expect(window.__TAURI_INTERNALS__!.invoke("preview_artifact_status", {
        project_id: "project_preview", artifact_id: artifactId,
      })).rejects.toThrow(/projectId.*received undefined/);
    } finally {
      harness.uninstall();
    }
  });

  it("handles human source-range commands with per-scenario revision state", async () => {
    const trimSuggestion: import("../../chronologyTypes").SourceTrimSuggestion = {
      inspectedEvidenceId: "inspect_declared_harness_trim",
      media: "audio",
      constraintsRevision: 0,
      cleanRanges: [],
      proposedRanges: [],
      conflicts: ["Declared harness fixture; no trim algorithm was run."],
      limitations: ["No source media, audio, or utterances were inspected."],
      listeningRequired: true,
    };
    const harness = desktopScenario()
      .project("Source range test", { id: "project_ranges" })
      .camera("Camera A", { clips: 1 })
      .sourceTrimSuggestion(trimSuggestion)
      .build()
      .install();
    const assetId = harness.state.assetOrder[0];
    const request = {
      assetId,
      expectedRevision: 0,
      expectedSourceFingerprint: harness.state.assets[assetId].fingerprint,
      decision: "exclude",
      media: "picture",
      decisionOwner: "human",
      sourceInSeconds: 0,
      sourceOutSeconds: 0.5,
      rationale: "Human-confirmed synthetic harness decision",
    };
    try {
      const before = (await window.__TAURI_INTERNALS__!.invoke("source_range_constraints", {
        assetId,
      })) as { revision: number; schemaReady: boolean; items: unknown[] };
      expect(before).toMatchObject({ revision: 0, schemaReady: true, items: [] });

      const confirmed = (await window.__TAURI_INTERNALS__!.invoke("confirm_source_range", {
        projectId: "project_ranges",
        request,
      })) as {
        revision: number;
        items: Array<{ constraintId: string; decisionOwner: string; active: boolean }>;
      };
      expect(confirmed.revision).toBe(1);
      expect(confirmed.items[0]).toMatchObject({ decisionOwner: "human", active: true });

      await expect(
        window.__TAURI_INTERNALS__!.invoke("confirm_source_range", {
          projectId: "project_ranges",
          request: { ...request, expectedRevision: 1, decisionOwner: "model" },
        }),
      ).rejects.toThrow(/request\.decisionOwner must be human/);
      await expect(
        window.__TAURI_INTERNALS__!.invoke("confirm_source_range", {
          projectId: "another_project",
          request: { ...request, expectedRevision: 1 },
        }),
      ).rejects.toThrow(/belongs to project project_ranges/);

      const revoked = (await window.__TAURI_INTERNALS__!.invoke("revoke_source_range", {
        projectId: "project_ranges",
        assetId,
        constraintId: confirmed.items[0].constraintId,
        expectedRevision: 1,
        rationale: "Human withdrew the synthetic decision",
      })) as { revision: number; items: Array<{ active: boolean }> };
      expect(revoked.revision).toBe(2);
      expect(revoked.items[0].active).toBe(false);

      const suggestion = await window.__TAURI_INTERNALS__!.invoke("suggest_source_trim", {
        request: {
          assetId,
          inspectedEvidenceId: trimSuggestion.inspectedEvidenceId,
          media: "audio",
          sourceInSeconds: 0.1,
          sourceOutSeconds: 0.2,
          preferUtterances: true,
        },
      });
      expect(suggestion).toEqual(trimSuggestion);
      await expect(
        window.__TAURI_INTERNALS__!.invoke("suggest_source_trim", {
          request: {
            asset_id: assetId,
            inspected_evidence_id: trimSuggestion.inspectedEvidenceId,
            media: "audio",
            source_in_seconds: 0.1,
            source_out_seconds: 0.2,
          },
        }),
      ).rejects.toThrow(/request\.assetId must be a string/);
    } finally {
      harness.uninstall();
    }
  });

  it("serves bounded chronology suggestions locally without mutating canonical chronology", async () => {
    const harness = desktopScenario()
      .project("Synthetic chronology test", { id: "project_test" })
      .camera("Camera A", { clips: 2 })
      .build()
      .install();
    try {
      const before = JSON.stringify(harness.state.chronology.overview);
      const result = (await window.__TAURI_INTERNALS__!.invoke(
        "analysis_suggest_chronology",
        {
          projectId: "project_test",
          assetIds: harness.state.assetOrder,
          primaryModel: "local-test-model",
          allowedModels: ["local-test-model"],
          allowFallback: false,
        },
      )) as {
        suggestion: { frameIds: string[]; matches: unknown[] };
        provenance: { model: string; attempts: number; attemptedModels: string[]; usageComplete: boolean; costUsd: number };
      };
      expect(result.suggestion.frameIds).toHaveLength(4);
      expect(result.suggestion.matches).toEqual([]);
      expect(result.provenance).toMatchObject({
        model: "local-test-model",
        attempts: 1,
        attemptedModels: ["local-test-model"],
        usageComplete: true,
        costUsd: 0,
      });
      expect(JSON.stringify(harness.state.chronology.overview)).toBe(before);
      await expect(
        window.__TAURI_INTERNALS__!.invoke("analysis_suggest_chronology", {
          projectId: "project_test",
          assetIds: [harness.state.assetOrder[0]],
          primaryModel: "local-test-model",
          allowedModels: ["local-test-model"],
          allowFallback: false,
        }),
      ).rejects.toThrow(/2–8 distinct clips/);
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
