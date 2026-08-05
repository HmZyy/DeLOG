// Release builds: hide the Windows console window that a console-subsystem
// binary would otherwise spawn alongside the GUI. Debug keeps it for `tracing`.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> eframe::Result {
    delog_app::run()
}
