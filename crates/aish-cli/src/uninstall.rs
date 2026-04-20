//! Uninstall aish binary and optionally purge data.

use std::io::{self, Write};
use std::path::PathBuf;

use aish_core::AishError;

pub fn detect_installation_method() -> String {
    let exe = std::env::current_exe().unwrap_or_default();
    let exe_str = exe.to_string_lossy();

    if exe_str.contains("/.local/bin/") || exe_str.contains("/usr/local/bin/") {
        "archive".to_string()
    } else if exe_str.contains("/.cargo/bin/") {
        "cargo".to_string()
    } else {
        "unknown".to_string()
    }
}

pub fn get_purge_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(config_dir) = dirs::config_dir() {
        paths.push(config_dir.join("aish"));
    }
    if let Some(data_dir) = dirs::data_dir() {
        paths.push(data_dir.join("aish"));
    }
    paths
}

fn remove_binary() -> Result<(), AishError> {
    let exe = std::env::current_exe()
        .map_err(|e| AishError::Config(format!("Cannot determine binary path: {e}")))?;
    if exe.exists() {
        std::fs::remove_file(&exe)
            .map_err(|e| AishError::Config(format!("Failed to remove binary: {e}")))?;
    }
    Ok(())
}

fn purge_data() -> Result<(), AishError> {
    for path in get_purge_paths() {
        if path.exists() {
            std::fs::remove_dir_all(&path).map_err(|e| {
                AishError::Config(format!("Failed to remove {}: {e}", path.display()))
            })?;
        }
    }
    Ok(())
}

pub fn run_uninstall(purge: bool, yes: bool) {
    println!("\x1b[1;36mAI Shell Uninstall\x1b[0m\n");

    let method = detect_installation_method();
    println!("\x1b[2mInstallation method: {}\x1b[0m", method);

    if purge {
        println!("\x1b[1;33m--purge: ALL config and data files will be removed.\x1b[0m");
        for p in get_purge_paths() {
            if p.exists() {
                println!("  \x1b[2m{}\x1b[0m", p.display());
            }
        }
    }

    if !yes {
        print!("\n\x1b[33mProceed with uninstall? [y/N] \x1b[0m");
        io::stdout().flush().unwrap();
        let mut answer = String::new();
        io::stdin().read_line(&mut answer).unwrap();
        if answer.trim().to_lowercase() != "y" {
            println!("Cancelled.");
            return;
        }
    }

    println!("Uninstalling...");
    if let Err(e) = remove_binary() {
        eprintln!("\x1b[31mFailed to remove binary: {}\x1b[0m", e);
        return;
    }
    println!("\x1b[32mBinary removed.\x1b[0m");

    if purge {
        match purge_data() {
            Ok(()) => println!("\x1b[32mConfig and data purged.\x1b[0m"),
            Err(e) => eprintln!("\x1b[31mPurge failed: {}\x1b[0m", e),
        }
    }

    println!("\x1b[32mAI Shell has been uninstalled. Goodbye!\x1b[0m");
}
