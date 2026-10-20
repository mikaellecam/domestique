//! What a sync does, shared by the command line and the desktop app.

use crate::config::{self, Config};
use crate::{Result, fit, garmin, mywhoosh, now};
use serde::{Deserialize, Serialize};
use std::{env, fs, path::PathBuf};

/// A Garmin activity starting this long after a ride's start still counts as
/// that ride, e.g. the same ride recorded on a watch.
const SLACK: i64 = 5 * 60;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Status {
    /// Garmin had it before this app uploaded anything.
    OnGarmin,
    Uploaded,
    /// Missing from Garmin: uploads are set to ask first, or not done yet.
    Waiting,
    /// Missing from Garmin, and the user chose not to upload it.
    Skipped,
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Source {
    /// MyWhoosh's copy of the ride, downloaded with the account.
    Cloud { file_id: String },
    /// The file MyWhoosh saved on this computer; it holds the latest ride only.
    Local(PathBuf),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Ride {
    /// Unix time; identifies the ride.
    pub start: i64,
    pub title: String,
    pub source: Source,
    pub status: Status,
}

/// The rides of the last `cfg.days` days, oldest first, from the MyWhoosh
/// account (when there is one) and from this computer, each marked OnGarmin
/// or Waiting.
pub fn check(cfg: &mut Config) -> Result<Vec<Ride>> {
    let since = now() - cfg.days * 86_400;
    let mut rides = Vec::new();
    if !cfg.mywhoosh_email.is_empty() {
        let mw = mywhoosh::Session::login(&cfg.mywhoosh_email, &cfg.mywhoosh_password)?;
        for r in mw.rides_since(since)? {
            let source = Source::Cloud { file_id: r.file_id };
            rides.push(Ride {
                start: r.start,
                title: r.title,
                source,
                status: Status::Waiting,
            });
        }
    }
    if let Some(path) = local_ride_file() {
        let (start, title) = fit::summary(&fs::read(&path)?)?;
        // The local copy is the same file, without a download.
        match rides.iter_mut().find(|r| (r.start - start).abs() <= SLACK) {
            Some(r) => r.source = Source::Local(path),
            None if start >= since => rides.push(Ride {
                start,
                title,
                source: Source::Local(path),
                status: Status::Waiting,
            }),
            None => {}
        }
    }
    rides.sort_by_key(|r| r.start);

    let on_garmin = garmin::activities_since(&garmin_token(cfg)?, since)?;
    for r in &mut rides {
        if is_synced(&on_garmin, r.start) {
            r.status = Status::OnGarmin;
        }
    }
    Ok(rides)
}

/// Uploads one ride and records its new status in status.json, for the
/// other process to show. A failed upload is a status, not an error.
pub fn upload_and_record(cfg: &mut Config, ride: &Ride) -> Result<Status> {
    let status = upload(cfg, ride);
    let mut rides = load_status();
    match rides.iter_mut().find(|r| r.start == ride.start) {
        Some(r) => r.status = status.clone(),
        // Listed by `check`, which saves nothing.
        None => {
            rides.push(Ride {
                status: status.clone(),
                ..ride.clone()
            });
            rides.sort_by_key(|r| r.start);
        }
    }
    save_status(&rides)?;
    Ok(status)
}

fn upload(cfg: &mut Config, ride: &Ride) -> Status {
    match upload_file(cfg, ride) {
        Ok(true) => Status::Uploaded,
        Ok(false) => Status::OnGarmin, // Garmin already had it
        Err(e) => Status::Failed(e.to_string()),
    }
}

/// Reads or downloads the ride, rewrites it for `cfg.device` and uploads it.
/// Returns false when Garmin already had it.
fn upload_file(cfg: &mut Config, ride: &Ride) -> Result<bool> {
    let device = cfg.device.ok_or("no Garmin device chosen")?;
    let raw = match &ride.source {
        Source::Local(path) => {
            let raw = fs::read(path)?;
            if fit::summary(&raw)?.0 != ride.start {
                return Err(
                    "MyWhoosh has since replaced this ride's file with a newer ride".into(),
                );
            }
            raw
        }
        Source::Cloud { file_id } => {
            mywhoosh::Session::login(&cfg.mywhoosh_email, &cfg.mywhoosh_password)?
                .download(file_id)?
        }
    };
    let data = fit::rewrite(&raw, device)?;
    let name = format!("{}.fit", ride.start);
    garmin::upload(&garmin_token(cfg)?, &name, &ride.title, &data)
}

/// One sync: check, then upload what Garmin is missing and the user didn't skip,
/// when uploads are automatic. Saves the result for the other process to show.
pub fn run(cfg: &mut Config) -> Result<Vec<Ride>> {
    let mut rides = check(cfg)?;
    let auto = cfg.auto_upload;
    settle(&mut rides, &load_status(), |r| {
        if auto {
            upload(cfg, r)
        } else {
            Status::Waiting
        }
    });
    save_status(&rides)?;
    Ok(rides)
}

/// Settles what `check` found against the last sync: a ride this app
/// uploaded stays Uploaded, one the user skipped stays Skipped until it
/// shows up on Garmin, and the others missing from Garmin go to `upload`.
fn settle(rides: &mut [Ride], before: &[Ride], mut upload: impl FnMut(&Ride) -> Status) {
    for r in rides {
        let was = before
            .iter()
            .find(|b| b.start == r.start)
            .map(|b| &b.status);
        r.status = match (&r.status, was) {
            (Status::OnGarmin, Some(Status::Uploaded)) => Status::Uploaded,
            (Status::Waiting, Some(Status::Skipped)) => Status::Skipped,
            (Status::Waiting, _) => upload(r),
            (status, _) => status.clone(),
        };
    }
}

/// The rides of the last sync, by whichever process ran it.
pub fn load_status() -> Vec<Ride> {
    fs::read(config::dir().join("status.json"))
        .ok()
        .and_then(|d| serde_json::from_slice(&d).ok())
        .unwrap_or_default()
}

pub fn save_status(rides: &[Ride]) -> Result<()> {
    fs::create_dir_all(config::dir())?;
    fs::write(
        config::dir().join("status.json"),
        serde_json::to_vec_pretty(rides)?,
    )?;
    Ok(())
}

/// Where MyWhoosh saves the latest ride on this computer, if it is installed
/// here. DOMESTIQUE_RIDE_DIR overrides it, to test on any system.
pub fn local_ride_dir() -> Option<PathBuf> {
    if let Some(d) = env::var_os("DOMESTIQUE_RIDE_DIR") {
        return Some(d.into());
    }
    let dir = if cfg!(windows) {
        // The Microsoft Store package: MyWhooshTechnologyService.<publisher id>
        let packages = PathBuf::from(env::var_os("LOCALAPPDATA")?).join("Packages");
        fs::read_dir(packages)
            .ok()?
            .flatten()
            .find(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("MyWhooshTechnologyService.")
            })?
            .path()
            .join(r"LocalCache\Local\MyWhoosh\Content\Data")
    } else if cfg!(target_os = "macos") {
        PathBuf::from(env::var_os("HOME")?)
            .join("Library/Containers/com.whoosh.whooshgame/Data/Library/Application Support/Epic/MyWhoosh/Content/Data")
    } else {
        return None;
    };
    dir.is_dir().then_some(dir)
}

/// MyWhoosh names the file after its version, MyNewActivity-6.2.3.fit, and
/// overwrites it with every ride.
pub fn local_ride_file() -> Option<PathBuf> {
    fs::read_dir(local_ride_dir()?)
        .ok()?
        .flatten()
        .filter(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.starts_with("MyNewActivity-") && name.ends_with(".fit")
        })
        .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
        .map(|e| e.path())
}

/// A valid Garmin access token, renewed and saved when about to expire.
fn garmin_token(cfg: &mut Config) -> Result<String> {
    // Another process may have renewed the tokens since `cfg` was loaded.
    if let Some(t) = config::load()?.garmin {
        cfg.garmin = Some(t);
    }
    let t = cfg.garmin.as_ref().ok_or("not signed in to Garmin")?;
    if now() + 120 >= t.expires_at {
        let fresh = garmin::refresh(t)?;
        // Save only the tokens: `cfg` may hold overrides, e.g. from the command line.
        let mut disk = config::load()?;
        disk.garmin = Some(fresh.clone());
        config::save(&disk)?;
        cfg.garmin = Some(fresh);
    }
    Ok(cfg.garmin.as_ref().unwrap().access_token.clone())
}

fn is_synced(on_garmin: &[(i64, i64)], start: i64) -> bool {
    on_garmin
        .iter()
        .any(|&(s, e)| s - SLACK <= start && start <= e)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching() {
        let start = 1_792_176_323;
        assert!(is_synced(&[(start, start + 2911)], start));
        assert!(is_synced(&[(start + 60, start + 2900)], start)); // watch started a minute late
        assert!(!is_synced(&[(start + 3600, start + 7200)], start));
    }

    fn ride(start: i64, status: Status) -> Ride {
        let source = Source::Cloud {
            file_id: String::new(),
        };
        Ride {
            start,
            title: String::new(),
            source,
            status,
        }
    }

    #[test]
    fn skipped_is_kept_and_not_uploaded() {
        use Status::*;
        let before = [ride(1, Skipped), ride(2, Skipped), ride(3, Uploaded)];
        // As `check` finds them: 2 has since shown up on Garmin, 4 is new.
        let mut rides = [
            ride(1, Waiting),
            ride(2, OnGarmin),
            ride(3, OnGarmin),
            ride(4, Waiting),
        ];
        let mut uploads = Vec::new();
        settle(&mut rides, &before, |r| {
            uploads.push(r.start);
            Uploaded
        });
        assert_eq!(uploads, [4]);
        let statuses: Vec<_> = rides.into_iter().map(|r| r.status).collect();
        assert_eq!(statuses, [Skipped, OnGarmin, Uploaded, Uploaded]);
    }

    #[test]
    fn upload_records_the_status() {
        let dir = env::temp_dir().join("domestique-sync-tests");
        let _ = fs::remove_dir_all(&dir);
        // SAFETY: no other test of this crate reads the environment.
        unsafe {
            env::set_var("XDG_CONFIG_HOME", &dir);
            env::set_var("APPDATA", &dir);
            env::set_var("HOME", &dir);
        }
        save_status(&[ride(1, Status::OnGarmin), ride(3, Status::Waiting)]).unwrap();
        // With no device chosen, it fails before going online.
        let mut cfg = Config::default();
        let failed = Status::Failed("no Garmin device chosen".into());
        assert_eq!(
            upload_and_record(&mut cfg, &ride(2, Status::Waiting)).unwrap(),
            failed
        );
        upload_and_record(&mut cfg, &ride(3, Status::Waiting)).unwrap();
        let saved: Vec<_> = load_status()
            .into_iter()
            .map(|r| (r.start, r.status))
            .collect();
        assert_eq!(
            saved,
            [(1, Status::OnGarmin), (2, failed.clone()), (3, failed)]
        );
    }
}
