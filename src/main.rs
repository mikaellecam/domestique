//! Syncs MyWhoosh rides to Garmin Connect, rewritten as if a Garmin device
//! had recorded them.

mod fit;
mod garmin;
mod mywhoosh;

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::{env, fs, path::PathBuf};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const USAGE: &str = "\
usage:
  whoosh2garmin login                          save MyWhoosh credentials, sign in to Garmin
  whoosh2garmin sync --device NAME [--days N] [--dry-run]
                                               upload the rides of the last N days (default 7)
                                               that Garmin does not have yet
  whoosh2garmin rewrite IN.fit OUT.fit --device NAME
                                               rewrite one file without uploading it
  whoosh2garmin devices                        list device names, e.g. edge_explore2";

/// A Garmin activity starting this long after a ride's start still counts as
/// that ride, e.g. the same ride recorded on a watch.
const SLACK: i64 = 5 * 60;

#[derive(Serialize, Deserialize)]
struct Credentials {
    mywhoosh_email: String,
    mywhoosh_password: String,
    garmin: garmin::Tokens,
}

fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    let flag = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1));
    let device = || -> Result<u16> {
        let name = flag("--device").ok_or("missing --device NAME, see `whoosh2garmin devices`")?;
        Ok(fit::product_id(name).ok_or(format!("unknown device {name:?}, see `whoosh2garmin devices`"))?)
    };
    match args.first().map(String::as_str) {
        Some("login") => login(),
        Some("sync") => {
            let days = flag("--days").map_or(Ok(7), |d| d.parse())?;
            sync(device()?, days, args.iter().any(|a| a == "--dry-run"))
        }
        Some("rewrite") if args.len() >= 3 => Ok(fs::write(&args[2], fit::rewrite(&fs::read(&args[1])?, device()?)?)?),
        Some("devices") => {
            fit::product_names().for_each(|n| println!("{n}"));
            Ok(())
        }
        _ => Err(USAGE.into()),
    }
}

fn login() -> Result<()> {
    print!("MyWhoosh email: ");
    std::io::stdout().flush()?;
    let mut email = String::new();
    std::io::stdin().read_line(&mut email)?;
    let email = email.trim().to_owned();
    let password = rpassword::prompt_password("MyWhoosh password: ")?;
    mywhoosh::Session::login(&email, &password)?; // fail on a typo now, not in cron
    let garmin = garmin::browser_login()?;
    save(&Credentials { mywhoosh_email: email, mywhoosh_password: password, garmin })?;
    println!("Saved to {}", path().display());
    Ok(())
}

fn sync(product: u16, days: i64, dry_run: bool) -> Result<()> {
    let mut creds = load()?;
    if now() + 120 >= creds.garmin.expires_at {
        creds.garmin = garmin::refresh(&creds.garmin)?;
        save(&creds)?; // Garmin may rotate the refresh token
    }
    let since = now() - days * 86_400;
    let mw = mywhoosh::Session::login(&creds.mywhoosh_email, &creds.mywhoosh_password)?;
    let rides = mw.rides_since(since)?;
    let mut on_garmin = garmin::activities_since(&creds.garmin.access_token, since)?;

    for ride in rides.iter().rev() {
        if is_synced(&on_garmin, ride.start) {
            continue;
        }
        println!("{} {} {}", if dry_run { "would upload" } else { "uploading" }, ride.when, ride.title);
        if dry_run {
            continue;
        }
        let data = fit::rewrite(&mw.download(ride)?, product)?;
        if !garmin::upload(&creds.garmin.access_token, &format!("{}.fit", ride.start), &data)? {
            println!("  Garmin already has it");
        }
        on_garmin.push((ride.start, ride.start));
    }
    Ok(())
}

fn is_synced(on_garmin: &[(i64, i64)], start: i64) -> bool {
    on_garmin.iter().any(|&(s, e)| s - SLACK <= start && start <= e)
}

fn path() -> PathBuf {
    let home = || env::var_os("HOME").map(PathBuf::from);
    let base = if cfg!(windows) {
        env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        home().map(|h| h.join("Library/Application Support"))
    } else {
        env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| home().map(|h| h.join(".config")))
    };
    base.unwrap_or_default().join("whoosh2garmin").join("credentials.json")
}

/// Plain text, readable only by the user on Unix: cron can't unlock a keyring.
fn save(c: &Credentials) -> Result<()> {
    let p = path();
    fs::create_dir_all(p.parent().unwrap())?;
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    serde_json::to_writer_pretty(opts.open(&p)?, c)?;
    Ok(())
}

fn load() -> Result<Credentials> {
    let p = path();
    let data = fs::read(&p).map_err(|e| format!("{}: {e}; run `whoosh2garmin login` first", p.display()))?;
    Ok(serde_json::from_slice(&data)?)
}

pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
}

/// Unix time of a UTC "YYYY-MM-DDTHH:MM:SS..." or "YYYY-MM-DD HH:MM:SS".
pub fn parse_utc(s: &str) -> Option<i64> {
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, m, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
    // Days since 1970-01-01, Howard Hinnant's days_from_civil.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
    Some(days * 86_400 + n(11..13)? * 3600 + n(14..16)? * 60 + n(17..19)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_and_matching() {
        // The ride's FIT start_time, 1161110723 s after the FIT epoch (Unix 631065600).
        let start = 1_161_110_723 + 631_065_600;
        assert_eq!(parse_utc("2026-10-16T18:45:23.000Z"), Some(start)); // MyWhoosh
        assert_eq!(parse_utc("2026-10-16 18:45:23"), Some(start)); // Garmin
        assert_eq!(parse_utc("2024-02-29 00:00:00"), Some(1_709_164_800));

        assert!(is_synced(&[(start, start + 2911)], start));
        assert!(is_synced(&[(start + 60, start + 2900)], start)); // watch started a minute late
        assert!(!is_synced(&[(start + 3600, start + 7200)], start));
    }
}
