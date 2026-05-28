//! Keyboard event handling module for the AISH shell.
//!
//! This module provides input-raw mode terminal handling, keyboard event reading,
//! and key binding registration for shell interaction.
//!
//! # Example
//!
//! ```ignore
//! use aish_shell::keyboard::{self, events, raw_mode, bindings};
//! use std::time::Duration;
//!
//! // Enter input-raw mode for direct key handling
//! let _guard = raw_mode::InputRawGuard::enter().unwrap();
//!
//! // Set up default key bindings
//! let key_bindings = bindings::default_bindings();
//!
//! // Read events with a timeout
//! loop {
//!     if let Some(event) = events::read_event(Some(Duration::from_millis(100))).unwrap() {
//!         match event {
//!             keyboard::events::ShellEvent::Key(key_event) => {
//!                 if let Some(action) = key_bindings.resolve(&key_event) {
//!                     println!("Action: {}", action);
//!                 }
//!             }
//!             _ => {}
//!         }
//!     }
//! }
//! ```

pub mod bindings;
pub mod events;
#[cfg(unix)]
pub mod raw_mode;

// Re-export commonly used types at the module level
pub use bindings::default_bindings;
pub use events::{read_event, ShellEvent};
#[cfg(unix)]
pub use raw_mode::InputRawGuard;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_module_exports() {
        let _bindings = default_bindings();
    }
}
