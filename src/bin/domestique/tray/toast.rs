//! Notifications. On Windows a click opens the window, and a waiting ride's
//! or a new version's notification has a button; elsewhere they only inform.

use super::Msg;
use winit::event_loop::EventLoopProxy;

/// A notification's button, on Windows.
#[derive(Clone, Copy)]
#[cfg_attr(not(windows), allow(dead_code))]
pub enum Button {
    /// Uploads the ride that starts then, waiting for the user's go-ahead.
    Upload(i64),
    /// Installs the new version, as the menu's item does.
    Update,
}

#[cfg(windows)]
pub fn show(proxy: &EventLoopProxy<Msg>, title: &str, body: &str, button: Option<Button>) {
    use tauri_winrt_notification::Toast;
    // Windows shows a notification only for a registered app id, and names it
    // after it. Store apps and installers register theirs; this exe registers
    // one under HKCU, the way the Windows App SDK does for unpackaged apps.
    const APP_ID: &str = "domestique";
    let registered = windows_registry::CURRENT_USER
        .create(format!(r"Software\Classes\AppUserModelId\{APP_ID}"))
        .and_then(|key| key.set_string("DisplayName", "Domestique"));
    if let Err(e) = registered {
        eprintln!("can't register for notifications: {e}");
    }

    let proxy = proxy.clone();
    let mut toast = Toast::new(APP_ID).title(title).on_activated(move |action| {
        let _ = proxy.send_event(match (action, button) {
            (Some(_), Some(Button::Upload(start))) => Msg::Upload(start),
            (Some(_), Some(Button::Update)) => Msg::Menu("update".into()),
            _ => Msg::Open,
        });
        Ok(())
    });
    if !body.is_empty() {
        toast = toast.text1(body);
    }
    match button {
        Some(Button::Upload(_)) => toast = toast.add_button("Upload", "upload"),
        Some(Button::Update) => toast = toast.add_button("Update", "update"),
        None => {}
    }
    if let Err(e) = toast.show() {
        eprintln!("can't notify: {e}");
    }
}

/// Through the freedesktop notification service on D-Bus.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn show(_: &EventLoopProxy<Msg>, title: &str, body: &str, _: Option<Button>) {
    let notify = || -> zbus::Result<()> {
        let hints = std::collections::HashMap::<&str, zbus::zvariant::Value>::new();
        let actions = Vec::<&str>::new();
        let args = ("Domestique", 0u32, "", title, body, actions, hints, -1i32);
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
pub fn show(_: &EventLoopProxy<Msg>, title: &str, body: &str, _: Option<Button>) {
    let quote = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    let script = format!(
        "display notification {} with title \"Domestique\" subtitle {}",
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
