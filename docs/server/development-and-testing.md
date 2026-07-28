# Server, web, and desktop development

Install workspace dependencies from the repository root with `pnpm install`. Build and run the native desktop with `pnpm dev`; build the CLI and server with `pnpm build:cli` and `pnpm build:server`.

Run a local server only with disposable roots:

```bash
RCDVLOG_DATA_DIR=/absolute/disposable/state \
RCDVLOG_DERIVED_DIR=/absolute/disposable/derived \
RCDVLOG_BACKUP_DIR=/absolute/disposable/backups \
RCDVLOG_ALLOWED_ORIGINAL_MOUNTS=/absolute/disposable/originals \
RCDVLOG_LISTEN_ADDR=127.0.0.1:8090 \
target/debug/rcdvlog-server
```

The production server serves `apps/web/dist` when `RCDVLOG_WEB_DIR` points at that directory. `pnpm build:web` builds it. The desktop Vite preview uses browser mocks and cannot prove Tauri, Keychain, catalog, media, or sync behavior. The web Playwright suite starts a real disposable server and creates its bootstrap credential locally.

## Gates

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
pnpm check:frontend
pnpm test:web
pnpm test:web:e2e
pnpm check:openapi
pnpm check:deployment
pnpm check:docs
docker compose -f deploy/server/compose.yaml config --quiet
```

The OpenAPI document is Rust-owned. Run `pnpm generate:openapi` only after an intentional API change, then run `pnpm check:openapi`. Shared browser types and requests belong in `packages/project-client`; shared review UI belongs in `packages/review-ui`. Desktop React calls only its exported `api` boundary, while CLI/Tauri/server adapters delegate domain behavior to the owning Rust crate.

Tests must use temporary directories and explicit catalog/server selection. Never aim a test at a real project root, `/Volumes`, a live server data root, or a NAS original tree. Keep original mounts read-only even in fixtures that exercise failure paths.

See [integration-testing.md](../integration-testing.md) for the exact automated/manual boundary and the [Task 19 report](../tasks/server/ACCEPTANCE_TASK_19.md) for the last accepted environment.
