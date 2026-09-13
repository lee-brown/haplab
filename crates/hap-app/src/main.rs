//! # HAP Video Studio
//!
//! A pure Rust desktop GUI and CLI tool for encoding, decoding, and inspecting HAP video streams.
//! Built by Lee Brown. Licensed under the MIT License.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] // Hide console window in release GUI mode on Windows

mod cli;
mod gui;
mod worker;

use clap::Parser;
use eframe::egui;
use gui::HapStudioApp;
use std::env;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init_from_env(env_logger::Env::default().default_filter_or("info"));

    let args: Vec<String> = env::args().collect();

    // If CLI arguments were provided, execute in headless command-line mode
    if args.len() > 1 {
        let cli_args = cli::Cli::parse();
        cli::run_cli(cli_args)?;
        return Ok(());
    }

    // Otherwise, launch the full interactive graphical user interface
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([960.0, 720.0])
            .with_min_inner_size([640.0, 480.0])
            .with_title("HAP Video Studio")
            .with_drag_and_drop(true),
        ..Default::default()
    };

    eframe::run_native(
        "HAP Video Studio",
        native_options,
        Box::new(|_cc| Ok(Box::new(HapStudioApp::default()))),
    )
    .map_err(|e| format!("GUI launch failed: {}", e))?;

    Ok(())
}
