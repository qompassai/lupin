//! `lupin-tui` — the fleet-pattern TUI for the lupin MCP server
//! (fleet TUI programme D-1562, DESIGN D2).
//!
//! Lupin is the Revenue Alignment Engine: an opportunity ledger plus
//! a capability registry, lane-scoped by product rule. This crate is
//! an operator console for that server. It is a separate crate that
//! programs against a narrow [`facade::LupinFacade`] trait; the real
//! facade is an MCP stdio client spawning the member's own `lupin`
//! server binary (rmcp client machinery, following phlow's MCP
//! client), and tests drive an in-memory
//! [`facade::FakeFacade`].
//!
//! Lane isolation is a product rule, made visible: this TUI is built
//! for the `qompass` / `sfw` lane. Its view lane is `sfw`, the lane
//! in view is named on every screen, and records from any other lane
//! are filtered out client-side as defence in depth — even if a
//! server (or a fake) hands them over, they never reach a panel.
//!
//! # Bounds
//!
//! Every queue, buffer, and render carries a named `*_MAX` constant:
//! [`LOG_ENTRIES_MAX`], [`AUDIT_ENTRIES_MAX`] (in
//! [`triggers`]), [`panels::TITLE_CHARS_MAX`],
//! [`panels::LINE_CHARS_MAX`], [`app::INPUT_LINE_BYTES_MAX`], and
//! [`facade::RESULT_BYTES_MAX`]. No Media screen exists: lupin's use
//! case has no images, video, or diagrams (DESIGN D2/D5).

#![forbid(unsafe_code)]

pub mod app;
pub mod facade;
pub mod panels;
pub mod screens;
pub mod triggers;

pub use app::{App, LineAction, classify_line, run_line_mode};
pub use facade::{FacadeError, FakeFacade, Lane, LupinFacade, McpFacade};
pub use screens::{Mode, Screen, StagedCall, Workflow, workflows};
pub use triggers::{TriggerEngine, TriggerRule};

/// Maximum number of lines kept in the Log / Live buffer.
pub const LOG_ENTRIES_MAX: usize = 512;

/// Maximum number of audit lines kept by the trigger engine.
pub const AUDIT_ENTRIES_MAX: usize = 512;
