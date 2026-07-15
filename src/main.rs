//! Deck is a terminal cockpit for local development projects.
//!
//! The binary is organized as focused modules around project discovery,
//! command execution, sandboxing, context bundles, and dual JSON/human output.
//! See `docs/ARCHITECTURE.md` for the full module map and extension rules.

mod adapters;
mod cli;
mod commands;
mod config;
mod config_edit;
mod context;
mod contracts;
mod discover;
mod errors;
mod history;
mod init;
mod manifest;
mod model;
mod planner;
mod plugin;
mod process;
mod safety;
mod sandbox;
mod selection;
mod state;
mod summary;
mod tasks;
mod tools;
mod tui;
mod workflow;

fn main() -> anyhow::Result<()> {
    cli::run()
}
