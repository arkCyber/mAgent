//! mAgent-Man: Tauri Application Entry Point

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    magent_man::run();
}
