//! Starting the tray process when the user logs in.

/// Turns starting the app at login on or off.
pub fn set(enabled: bool) -> whoosh2garmin::Result<()> {
    let _ = enabled;
    Ok(())
}
