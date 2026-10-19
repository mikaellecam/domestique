//! Desktop app: a tray process that syncs after each ride, and a window
//! process for setup, the ride list and settings, started from the tray.
#![windows_subsystem = "windows"]

mod autostart;
mod tray;
mod ui;

fn main() -> whoosh2garmin::Result<()> {
    if std::env::args().any(|a| a == "--window") {
        ui::run()
    } else {
        tray::run()
    }
}
