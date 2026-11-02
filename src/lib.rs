//! `aoe-llm-tags` — the Agent of Empires "LLM Tags" plugin.
//!
//! Two entrypoints share this library: the CLI the `[host_hooks]` lines run
//! (`hook`, `value`, `setup`, `doctor`) and the plugin worker (`status`). Both
//! go through [`resolver::snapshot`], so they cannot drift.
//!
//! Nothing here links aoe; the only contact with the host is a subprocess call
//! to `aoe session show --json` / `aoe list --json` behind an injectable runner.

pub mod adapters;
pub mod cli;
pub mod doctor;
pub mod env;
pub mod overlay;
pub mod resolver;
pub mod settings;
pub mod setup;
pub mod worker;
pub mod writer;
