//! Notifications. On Windows a click opens the window and a waiting ride's
//! notification has an Upload button; elsewhere they only inform.

use super::Msg;
use winit::event_loop::EventLoopProxy;

/// `upload`: the start of a ride waiting for the user's go-ahead.
#[cfg(windows)]
pub fn show(proxy: &EventLoopProxy<Msg>, title: &str, body: &str, upload: Option<i64>) {
    use tauri_winrt_notification::Toast;
    // Windows shows a notification only for a registered app id, and names it
    // after it. Store apps and installers register theirs; this exe registers
    // one under HKCU, the way the Windows App SDK does for unpackaged apps.
    const APP_ID: &str = "whoosh2garmin";
    let registered = windows_registry::CURRENT_USER
        .create(format!(r"Software\Classes\AppUserModelId\{APP_ID}"))
        .and_then(|key| key.set_string("DisplayName", "whoosh2garmin"));
    if let Err(e) = registered {
        eprintln!("can't register for notifications: {e}");
    }

    let proxy = proxy.clone();
    let mut toast = Toast::new(APP_ID).title(title).on_activated(move |button| {
        let _ = proxy.send_event(match (button, upload) {
            (Some(_), Some(start)) => Msg::Upload(start),
            _ => Msg::Open,
        });
        Ok(())
    });
    if !body.is_empty() {
        toast = toast.text1(body);
    }
    if upload.is_some() {
        toast = toast.add_button("Upload", "upload");
    }
    if let Err(e) = toast.show() {
        eprintln!("can't notify: {e}");
    }
}

/// Through the freedesktop notification service on D-Bus.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn show(_: &EventLoopProxy<Msg>, title: &str, body: &str, _: Option<i64>) {
    let notify = || -> zbus::Result<()> {
        let hints = std::collections::HashMap::<&str, zbus::zvariant::Value>::new();
        let actions = Vec::<&str>::new();
        let args = (
            "whoosh2garmin",
            0u32,
            "",
            title,
            body,
            actions,
            hints,
            -1i32,
        );
        zbus::blocking::Connection::session()?.call_method(
            Some("org.freedesktop.Notifications"),
            "/org/freedesktop/Notifications",
            Some("org.freedesktop.Notifications"),
            "Notify",
            &args,
        )?;
        Ok(())
    };
    if let Err(e) = notify() {
        eprintln!("can't notify: {e}");
    }
}

/// Through AppleScript, so they appear under Script Editor's name.
#[cfg(target_os = "macos")]
pub fn show(_: &EventLoopProxy<Msg>, title: &str, body: &str, _: Option<i64>) {
    let quote = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    let script = format!(
        "display notification {} with title \"whoosh2garmin\" subtitle {}",
        quote(body),
        quote(title)
    );
    if let Err(e) = std::process::Command::new("osascript")
        .args(["-e", &script])
        .status()
    {
        eprintln!("can't notify: {e}");
    }
}
