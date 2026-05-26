//! Async event stream module for terminal and multiplexed events.
//!
//! This module provides async infrastructure for handling terminal events
//! and multiplexing multiple event sources using tokio.

pub mod multiplexer;
pub mod reader;

pub use multiplexer::{EventMultiplexer, MultiplexedEvent};
pub use reader::{AsyncTerminalEventReader, TerminalEvent};
