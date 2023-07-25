use std::fs;

mod controller;
pub mod controller_type;
mod cpu;
mod cpuset;
pub mod manager;
mod memory;
mod pids;
mod unified;
mod zbus;

/// Checks if the system was booted with systemd
pub fn booted() -> bool {
    fs::symlink_metadata("/run/systemd/system")
        .map(|p| p.is_dir())
        .unwrap_or_default()
}
