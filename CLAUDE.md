# CLAUDE.md - autumn-plugin-bevy

An Autumn plugin that runs one headless Bevy app on its own thread. Read
`README.md` for the user view and `docs/adr/` for the decisions.

## Commands

- Build: `cargo build --all-targets`
- Format: `cargo fmt --all`
- Lint: `cargo clippy --all-targets --all-features -- -D warnings`
- Lint without features: `cargo clippy --all-targets --no-default-features -- -D warnings`
- Test: `cargo test --all-features` (the doctests run too)
- Docs: `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features`
- Coverage: `cargo llvm-cov --all-features --fail-under-lines 85`
- Verus: `scripts/verify-verus.sh` (needs `verus` on `PATH`)

## Rules

- **Never bump the version** in `Cargo.toml` unless the user asks for a
  release. Write the change in `CHANGELOG.md` under `## [Unreleased]`.
- **The thread rule.** Only the `autumn-bevy` thread touches the `App` and
  the `World` (`src/driver.rs`). The async side sends messages. It never
  blocks a tokio worker: waits go through `spawn_blocking`.
- **The lifecycle rule.** A status changes only through `lifecycle::next`.
  A change to the lifecycle, the queue or the tick schedule needs the same
  change in `verification/bevy_runtime.rs`, then in the tests.
- **Count before reply.** Update a counter before the reply goes to the
  caller, so a caller never reads an old count.
- **Safe HTTP errors.** Handlers use `Bevy` methods, which return
  `AutumnResult`. Do not put panic text or internal data in a response body.
- **Security defaults, not blocks.** The plugin adds no Bevy plugin with a
  process-wide effect and does not set the Bevy runner or error handler. The
  app owns these choices.
- **No `unwrap`, `expect` or `panic!` in `src/`.** Clippy denies them. Tests
  may use them.
- **Docs and comments use ASD-STE100 style**: short sentences, active voice,
  simple tenses, one word for one meaning.
- **Tests.** No behavior without a test. A test that depends on time asserts
  a lower bound with a wide margin, or uses `TickMode::Manual`. A test that
  can hang runs under the `within` or `current_thread` watchdog in
  `tests/integration/support.rs`.

## Layout

| Path | Contents |
| --- | --- |
| `src/plugin.rs` | `BevyPlugin`: the Autumn wiring and the hooks |
| `src/runtime.rs` | `BevyRuntime`: start, wait for ready, shutdown |
| `src/driver.rs` | The loop on the Bevy thread |
| `src/handle.rs` | `BevyHandle` and the `Bevy` extractor |
| `src/shared.rs` | Status, flags and counters that both sides read |
| `src/lifecycle.rs`, `src/schedule.rs` | Pure cores, mirrored by the Verus model |
| `src/health.rs`, `src/metrics.rs` | The `bevy` indicator and metrics source |
| `src/config.rs`, `src/error.rs` | `BevyConfig` and the errors |
| `tests/integration/` | One test binary |
| `verification/` | The Verus model |
