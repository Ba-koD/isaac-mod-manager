#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] // Hide console window on Windows in release

mod fs_utils;
mod gui;
mod patcher;
mod steam_api;
mod steam_workshop;

use anyhow::Result;

fn main() -> Result<()> {
    gui::run().map_err(|e| anyhow::anyhow!("GUI Error: {}", e))
}
