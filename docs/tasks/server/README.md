# Companion-server engineering records

This directory preserves the accepted architecture, task specifications, implementation handoffs, frozen protocol schemas, and Task 19 evidence for the completed companion-server epic.

These files are engineering history, not the primary operator contract. Current installation, security, storage, synchronization, job/media, recovery, development, and troubleshooting guidance lives in [`docs/server`](../../server/README.md). Task status lines record delivered implementation honestly; external release-topology gaps remain in [`ACCEPTANCE_TASK_19.md`](ACCEPTANCE_TASK_19.md).

`schemas/baseline-v1.schema.json`, `schemas/reconcile-v1.schema.json`, and `schemas/reconcile-response-v1.schema.json` are frozen protocol-v1 compatibility artifacts. Do not update them in place when DTOs change. Introduce a new protocol/baseline version and compatibility path.
