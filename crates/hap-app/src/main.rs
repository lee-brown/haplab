//! # HapLab
//!
//! A pure Rust desktop GUI and CLI tool for encoding, decoding, and inspecting HAP video streams.
//! Built by Lee Brown. Licensed under the MIT License.

mod benchmark;
mod cli;
mod gui;
mod platform;
mod worker;

use clap::Parser;
use eframe::egui;
use gui::HapLabApp;
use std::env;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init_from_env(env_logger::Env::default().default_filter_or("info"));

    let args: Vec<String> = env::args().collect();

    // If CLI arguments were provided, execute headless CLI mode
    if args.len() > 1 {
        let cli_args = cli::Cli::parse();
        cli::run_cli(cli_args)?;
        return Ok(());
    }

    // If launched without CLI arguments (e.g. double-clicked from File Explorer),
    // detach from the Windows console immediately before launching the graphical window.
    #[cfg(windows)]
    unsafe {
        extern "system" {
            fn FreeConsole() -> i32;
        }
        let _ = FreeConsole();
    }

    // Otherwise, launch the full interactive graphical user interface
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1040.0, 780.0])
            .with_min_inner_size([720.0, 520.0])
            .with_title("HapLab")
            .with_drag_and_drop(true),
        ..Default::default()
    };

    eframe::run_native(
        "HapLab",
        native_options,
        Box::new(|_cc| Ok(Box::new(HapLabApp::default()))),
    )
    .map_err(|e| format!("GUI launch failed: {}", e))?;

    Ok(())
}
