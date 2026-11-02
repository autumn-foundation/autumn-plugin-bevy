//! Integration tests for `autumn-plugin-bevy`. One binary, one link step.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::missing_panics_doc,
    clippy::significant_drop_tightening,
    missing_docs
)]

mod support;

mod config;
mod health;
mod http;
mod metrics;
mod plugin;
mod repo_hygiene;
mod runtime;
#[cfg(feature = "time")]
mod time;
