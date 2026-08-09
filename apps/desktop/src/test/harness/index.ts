export { DesktopHarness, commandTable, EVENT_COMMANDS, type CommandLogEntry } from "./DesktopHarness";
export { desktopScenario, ScenarioBuilder, type ScenarioOptions } from "./scenario";
export { HarnessClock } from "./clock";
export { renderApp, settle, queueFilenames, type BootedApp } from "./renderApp";
export {
  buildAnalysis,
  buildArchiveRoot,
  buildCameraAssets,
  buildCard,
  buildProject,
  buildSyncStatus,
  buildTranscript,
  filenamePrefix,
  slugify,
  type AnalysisSpec,
  type CameraSpec,
  type TranscriptSpec,
} from "./builders";
export {
  createInitialState,
  patchAsset,
  projectAssets,
  projectSummary,
  putAsset,
  requireAsset,
  EMPTY_CHRONOLOGY_OVERVIEW,
  type DesktopHarnessState,
} from "./state";
export type { CommandArgs, CommandHandler, CommandTable, HarnessContext } from "./registry";
export type { JobController } from "./jobs";
