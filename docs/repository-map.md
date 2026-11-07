# Repository map

```text
.
├── AGENTS.md                         # This operational handoff
├── README.md                         # Short setup and usage guide
├── Cargo.toml                        # Rust workspace
├── package.json                      # Root pnpm command aliases
├── apps/
│   ├── cli/
│   │   ├── src/main.rs               # clap CLI, JSON envelopes, error mapping
│   │   ├── src/preparation_commands.rs # Thin bounded preparation adapter
│   │   ├── tests/cli_workflow.rs     # Process-level CLI/media integration tests
│   │   └── tests/preparation_workflow.rs # Preparation CLI contracts
│   ├── desktop/
│   │   ├── src/                      # React UI, API boundary, types, styles
│   │   └── src-tauri/                # Tauri commands, settings, native entry point
│   ├── server/                       # Axum runtime, admin CLI, HTTP/process tests
│   └── web/                          # Authenticated Chrome review/admin app
├── core/
│   ├── rcdvlog-core/src/lib.rs       # Canonical catalog, media, annotation, chronology logic
│   ├── rcdvlog-core/src/operations.rs # Typed operations, HLC, materialization, conflicts
│   ├── rcdvlog-server/               # Server catalog, storage, jobs, auth, media, backup
│   ├── rcdvlog-sync/                 # Baseline/reconciliation DTOs and apply logic
│   └── batch-estimator/src/lib.rs    # Domain-independent weighted ETA library
├── packages/
│   ├── project-client/               # OpenAPI snapshot and typed browser client
│   └── review-ui/                    # Shared review components/types
├── deploy/server/                    # Dockerfile, Compose, Caddy, Tailscale, example env
└── docs/
    ├── agent-cli.md                  # Full CLI and JSON contract
    ├── batch-estimation.md           # Estimator semantics and persistence
    ├── integration-testing.md        # Automated versus manual acceptance boundaries
    └── server/                       # Current server operator/developer contracts
```

Generated directories such as `target/`, `node_modules/`, `apps/desktop/dist/`, TypeScript build info, and Tauri schema output are not source.
