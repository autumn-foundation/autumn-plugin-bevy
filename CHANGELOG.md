# Changelog

All notable changes to this crate are in this file. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/). The crate follows
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- `BevyPlugin`: an Autumn plugin that runs one headless Bevy app on its own
  thread, `autumn-bevy`. The startup hook starts the thread and waits for the
  first update. The shutdown hook stops it.
- `BevyRuntime` and `BevyHandle`: async access to the Bevy `World` through a
  bounded command queue. A full queue fails at once with `Busy`.
- The `Bevy` handler extractor. Its methods return `AutumnResult` with a
  503 or 500 error and a fixed body.
- `TickMode::Fixed` (default 60 Hz, late ticks are skipped) and
  `TickMode::Manual` with `step(n)` for tests.
- Panic isolation: a panic in a command fails that call only. A panic in an
  update stops the world, and the server continues.
- The `bevy` health indicator (`Readiness` group) and the `bevy_*` metrics.
- `BevyConfig` with the `[bevy]` table of `autumn.toml`. An unknown key is an
  error.
- The `time` feature (on by default): adds `TimePlugin` when setup did not
  add it, with a fixed time step in manual mode.
- A Verus model of the lifecycle, the queue and the tick schedule in
  `verification/bevy_runtime.rs`.
