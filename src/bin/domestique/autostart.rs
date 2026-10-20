//! Starting the tray process when the user logs in.

use domestique::Result;
use std::env;

const NAME: &str = "Domestique";

/// Turns starting the app at login on or off. It starts this exe where it
/// is now, so after moving the exe the setting needs saving again.
pub fn set(enabled: bool) -> Result<()> {
    let exe = env::current_exe()?.display().to_string();
    #[cfg(windows)]
    {
        let run = windows_registry::CURRENT_USER
            .create(r"Software\Microsoft\Windows\CurrentVersion\Run")?;
        if enabled {
            run.set_string(NAME, format!("\"{exe}\""))?;
        } else if run.get_value(NAME).is_ok() {
            run.remove_value(NAME)?;
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let (path, entry) = if cfg!(target_os = "macos") {
            let home = env::var_os("HOME").ok_or("HOME is not set")?;
            let exe = exe.replace('&', "&amp;").replace('<', "&lt;");
            let plist = format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>{NAME}</string>
    <key>ProgramArguments</key><array><string>{exe}</string></array>
    <key>RunAtLoad</key><true/>
</dict>
</plist>
"#
            );
            let path = std::path::Path::new(&home).join("Library/LaunchAgents");
            (path.join(format!("{NAME}.plist")), plist)
        } else {
            // XDG autostart, in $XDG_CONFIG_HOME or ~/.config like our own folder.
            let path = domestique::config::dir().with_file_name("autostart");
            let desktop =
                format!("[Desktop Entry]\nType=Application\nName={NAME}\nExec=\"{exe}\"\n");
            (path.join(format!("{NAME}.desktop")), desktop)
        };
        if enabled {
            std::fs::create_dir_all(path.parent().unwrap())?;
            std::fs::write(path, entry)?;
        } else if path.exists() {
            std::fs::remove_file(path)?;
        }
        Ok(())
    }
}
