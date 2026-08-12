# CI workflows

## `ci.yml`

Runs on every push to `main` and every pull request targeting `main`.

### `ubuntu` job (ubuntu-latest)

Proves the full headless verification gate passes on Linux:

- Rust: formatting, clippy (deny warnings), and the full workspace test suite
- TypeScript: type checking across all frontend packages, the desktop vitest
  suite (extracted by D01-T003), and the web vitest suite
- CLI integration: the process-level `cli_workflow` test, which creates a
  real FFmpeg fixture and validates persistence, WAL reads, ingest, metadata,
  deduplication, labels/notes/removal, clock correction, thumbnails/proxies,
  and offline-original review
- Documentation: OpenAPI spec freshness, link and fence validation in
  Markdown, and deployment configuration checks
- Repository tooling: the `node --test` suites under `scripts/`, covering the
  docs checker and the eval-bridge address resolution that `pnpm eval` and the
  desktop harness share

Every gate step after the first carries `if: ${{ !cancelled() }}`. A gate that
fails still fails the job, but it no longer skips the gates behind it, so one
run reports every problem instead of the first one. This is not cosmetic: a
`cargo fmt` nit left `main` red from 2026-08-03 to 2026-08-09 while nine later
checks — clippy, the whole workspace test suite, both frontend suites, the CLI
integration test and every docs check — reported nothing at all. Add the guard
to any gate step added here.

### Pinning, and why everything here is immutable

Two kinds of drift can turn this gate red without anyone changing the code, and
both are closed.

**The toolchain.** `dtolnay/rust-toolchain@stable` followed whatever stable
release existed at run time, so a new `rustfmt` or `clippy` could reject code
that was fine the day before — which is exactly the six-day breakage above. The
version is now the `RUST_TOOLCHAIN` env value at the top of `ci.yml`, used by
both jobs. Raise it deliberately: run the gate locally on the new toolchain
first, change it in its own commit, and keep `AGENTS.md` §6 in step. The pin
also makes `components: rustfmt, clippy` load-bearing — the action installs a
minimal profile, and a pinned version is one the runner image does not already
carry with those components attached.

The other remedy the D01-T004 review offered for this — a root
`rust-toolchain.toml` read by the action — was not taken. It would pin every
contributor's local `cargo` to one version as a side effect of hardening CI,
which contradicts the `Rust 1.85+` floor in `AGENTS.md` §6 and is a wider
decision than this gate should make on its own. The env value keeps the pin
where the drift was.

**The actions.** Every `uses:` is a commit SHA with the human-readable version
in a trailing comment. A tag is mutable: whoever owns the action's repository
can repoint `v4` at new code, and a workflow that trusts a tag runs whatever
that account publishes next, with this repository's checkout in front of it. A
SHA cannot be repointed. `.github/dependabot.yml` proposes the upgrades so the
pins do not quietly rot; it rewrites both the SHA and the version comment.

### Untrusted pull requests

The repository's destination is public, so anyone can fork it and open a pull
request, and PR code must be treated as untrusted.

- The trigger stays `pull_request`. It runs fork code with a token scoped to
  the fork and no access to this repository's secrets. `pull_request_target`
  runs the same untrusted code with the *base* repository's write-capable
  token; do not introduce it here for any convenience.
- `permissions: contents: read` is declared at the workflow level, so the
  token is read-only even on `main` pushes. Nothing in this gate writes to the
  repository. A future step that genuinely needs more takes it at the job
  level, at the smallest scope that works.
- No step reads a secret, so there is nothing for a fork PR to exfiltrate
  through a build script or a test.
- Caches are written by the base repository's runs and are read-only to fork
  PRs, so a fork cannot poison a cache entry that a later trusted run restores.
- Artifacts are per-run, and this workflow uploads none.

### Disk

The job reclaims disk before it builds anything, and this is load-bearing rather
than tidy. A full debug `target/` for this workspace is about 14 GB: whisper.cpp
through `whisper-rs-sys`, the webkit2gtk bindings, a bundled SQLite, axum and
reqwest — and several of those are built more than once, because each cargo
invocation that narrows the package graph (`cargo run -p rcdvlog-server`, as
`check:openapi` does) resolves features differently from `cargo test
--workspace` and gets its own metadata hash. A local tree carries ten separate
`whisper-rs-sys` build directories for exactly that reason.

That is close enough to the runner image's free space that a job can finish nine
gates and then fail on the tenth with `No space left on device` from `ar`, which
reads as a broken step rather than a full disk. Removing the Android SDK, .NET,
GHC, Boost and Swift frees roughly 15 GB and is the difference between "barely
fits" and "comfortable". `/opt/hostedtoolcache` is left alone because Node lives
there. The step prints `df -h` on both sides so the next disk problem is one
glance rather than log archaeology.

### The Rust cache

`Cache Rust` is `Swatinem/rust-cache`, not a bare `actions/cache` over
`target/`, and the difference is that it prunes before it saves.

A plain cache of `target/` only grows. It restores under a `Cargo.lock` hash
with a broad fallback key, so any lockfile change restores the *previous*
target directory, builds the new artifacts alongside the old ones, and saves
both. Nothing ever removes the artifacts of a dependency version no longer in
the graph, and nothing removes the workspace's own binaries, which are rebuilt
every run anyway and are the largest single thing in the tree — a local
`target/debug/deps` holds 5.3 GB of them across 190 files, including three
separate ~300 MB `rcdvlog_desktop` test binaries left over from earlier builds
and one 891 MB `librcdvlog_desktop_lib.a`. GitHub caps a repository at 10 GB of
cache and evicts least-recently-used entries to stay under it, so an
ever-growing Rust entry does not just waste space, it pushes every other cache
out and eventually cannot be stored at all.

`rust-cache` cleans the target directory in its post step: it drops workspace
member artifacts, `examples`, incremental state, and dependency artifacts that
are no longer in the resolved graph, keeping the expensive external builds —
`whisper-rs-sys`'s compiled whisper.cpp among them. Its key also includes the
rustc version, so a toolchain bump gets a fresh entry instead of layering on an
incompatible one.

`cache-on-failure: true` is set deliberately. Now that every gate runs even
after one fails, a single red gate would otherwise cost the following run its
cache as well, turning one failure into two slow runs.

Expect the first run after this change to be slow: it starts from a cold cache
under a new key. The disk reclaim above stays regardless — the working
`target/` is still ~14 GB during a run whatever ends up cached.

### `macos` job (macos-latest)

Proves what the `ubuntu` job cannot: that the native macOS desktop bundle
compiles and that the Tauri-specific Rust crate (`rcdvlog-desktop`) passes its
own test suite. This job exists because the desktop is the product, and a gate
that never compiles it is not a gate.

The `cargo test -p rcdvlog-desktop` step runs before `tauri build` so that a
test failure surfaces faster than waiting through a full native compile.

### What CI does not prove

These items from `AGENTS.md` §12 ("do not pretend headless tests validate")
are not and cannot be proven by CI:

- DaVinci Resolve import
- macOS dialogs and filesystem permission prompts
- Native media playback in the Tauri webview
- Sleep/wake or source-volume disconnect/reconnect
- Expo-scale NAS performance
- Ubuntu/Compose restart and real NFS/SMB mount behavior
- LAN Caddy trust, Tailscale Serve/ACLs, and endpoint failover
- Target-laptop CPU/RSS/thermal behavior
- Playwright web end-to-end tests (`pnpm test:web:e2e` needs browser downloads
  and a running server, and stays a local manual check)
