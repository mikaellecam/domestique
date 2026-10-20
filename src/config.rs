//! Settings and credentials, in one JSON file readable only by the user:
//! the app runs unattended, so it can't ask for a keyring password.

use crate::{Result, garmin};
use serde::{Deserialize, Serialize};
use std::{env, fs, path::PathBuf};

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Empty when the user only rides on this computer.
    pub mywhoosh_email: String,
    pub mywhoosh_password: String,
    pub garmin: Option<garmin::Tokens>,
    /// Garmin product id written into the files, see `fit::product_names`.
    pub device: Option<u16>,
    /// False: ask before each upload.
    pub auto_upload: bool,
    pub start_with_os: bool,
    /// Sync as soon as MyWhoosh saves a ride on this computer.
    pub sync_after_ride: bool,
    /// Check every 30 minutes, for rides recorded on other devices.
    pub periodic_check: bool,
    /// How far back a sync looks.
    pub days: i64,
    pub setup_done: bool,
    /// Look for a new version on GitHub once a day.
    pub check_updates: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            mywhoosh_email: String::new(),
            mywhoosh_password: String::new(),
            garmin: None,
            device: None,
            auto_upload: true,
            start_with_os: true,
            sync_after_ride: true,
            periodic_check: false,
            days: 7,
            setup_done: false,
            check_updates: true,
        }
    }
}

/// Holds config.json and status.json.
pub fn dir() -> PathBuf {
    let home = || env::var_os("HOME").map(PathBuf::from);
    let base = if cfg!(windows) {
        env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        home().map(|h| h.join("Library/Application Support"))
    } else {
        env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| home().map(|h| h.join(".config")))
    };
    base.unwrap_or_default().join("domestique")
}

pub fn path() -> PathBuf {
    dir().join("config.json")
}

/// The defaults when nothing is saved yet.
pub fn load() -> Result<Config> {
    match fs::read(path()) {
        Ok(data) => Ok(serde_json::from_slice(&data)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(e.into()),
    }
}

pub fn save(c: &Config) -> Result<()> {
    fs::create_dir_all(dir())?;
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    serde_json::to_writer_pretty(opts.open(path())?, c)?;
    Ok(())
}
