#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod chart;
mod error_log;
mod export;
mod file_ops;
mod format;
mod hash;
mod icons;
mod item;
mod list;
mod platform;
mod query;
mod scan;
mod settings;
mod sidebar;

mod ui {
    slint::include_modules!();
}

use std::path::PathBuf;

const SCAN_STACK_BYTES: usize = 16 << 20;

#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() -> Result<(), slint::PlatformError> {
    error_log::install();
    rayon::ThreadPoolBuilder::new()
        .stack_size(SCAN_STACK_BYTES)
        .build_global()
        .expect("the scan thread pool starts once");
    app::run(std::env::args_os().nth(1).map(PathBuf::from))
}
