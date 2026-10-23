//! Local, air-gapped agent harness.
//!
//! The binary is intentionally small for now: the modules below establish the
//! foundation types and explicit extension points for later milestones.

#![forbid(unsafe_code)]

pub mod agent;
pub mod approval;
pub mod audit;
pub mod config;
pub mod error;
pub mod events;
pub mod jev;
pub mod llm;
pub mod mcp;
pub mod orchestrator;
pub mod policy;
pub mod state_machine;
pub mod tui;
pub mod wiki;
