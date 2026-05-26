use aish_shell::keyboard::{self, events, raw_mode, bindings};
use std::time::Duration;

/// Simple demonstration of the keyboard event handling system.
///
/// This example shows how to:
/// 1. Enter raw mode for direct keyboard input
/// 2. Read keyboard events with timeout
/// 3. Use key bindings to resolve actions
///
/// Press Esc to exit the demo.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Keyboard Event Demo");
    println!("Press keys to see their actions. Press Esc to exit.");
    println!("Try: Ctrl+C, Ctrl+D, Ctrl+L, Ctrl+R, Up, Down");
    println!();

    // Enter raw mode for direct key handling
    let _guard = raw_mode::InputRawGuard::enter()?;

    // Set up default key bindings
    let key_bindings = bindings::default_bindings();

    loop {
        // Read events with a 100ms timeout
        if let Some(event) = events::read_event(Some(Duration::from_millis(100)))? {
            match event {
                keyboard::ShellEvent::Key(key_event) => {
                    // Resolve the key event to an action
                    if let Some(action) = key_bindings.resolve(&key_event) {
                        println!("Action: {}", action);

                        // Exit on Esc (cancel action)
                        if action == "cancel" {
                            println!("Exiting demo...");
                            break;
                        }
                    } else {
                        // Show raw key info for unbound keys
                        println!("Key: {:?} (no action bound)", key_event);
                    }
                }
                keyboard::ShellEvent::Mouse(mouse_event) => {
                    println!("Mouse event: {:?}", mouse_event);
                }
                keyboard::ShellEvent::Resize(cols, rows) => {
                    println!("Terminal resized: {}x{}", cols, rows);
                }
            }
        }
    }

    println!("Demo complete. Raw mode will be exited automatically.");
    Ok(())
}
