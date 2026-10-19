//! Command line: sync from a terminal or cron, with the desktop app's settings.

use std::io::Write;
use std::{env, fs};
use whoosh2garmin::sync::{self, Status};
use whoosh2garmin::{Result, config, fit, garmin, mywhoosh, utc_string};

const USAGE: &str = "\
usage:
  whoosh2garmin login                          sign in to MyWhoosh (optional) and Garmin
  whoosh2garmin sync [--device NAME] [--days N] [--dry-run]
                                               upload the rides Garmin does not have yet;
                                               defaults come from the app's settings
  whoosh2garmin rewrite IN.fit OUT.fit --device NAME
                                               rewrite one file without uploading it
  whoosh2garmin devices                        list device names, e.g. edge_explore2";

fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    let flag = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
    };
    let device = || -> Result<Option<u16>> {
        let Some(name) = flag("--device") else {
            return Ok(None);
        };
        Ok(Some(fit::product_id(name).ok_or(format!(
            "unknown device {name:?}, see `whoosh2garmin devices`"
        ))?))
    };
    match args.first().map(String::as_str) {
        Some("login") => login(),
        Some("sync") => {
            let dry_run = args.iter().any(|a| a == "--dry-run");
            let mut cfg = config::load()?;
            cfg.device = device()?.or(cfg.device);
            if let Some(d) = flag("--days") {
                cfg.days = d.parse()?;
            }
            cfg.auto_upload = true; // the command line never asks
            if cfg.device.is_none() && !dry_run {
                return Err("missing --device NAME, see `whoosh2garmin devices`".into());
            }
            let rides = if dry_run {
                sync::check(&mut cfg)?
            } else {
                sync::run(&mut cfg)?
            };
            for r in rides {
                let status = match r.status {
                    Status::OnGarmin => "on Garmin".to_owned(),
                    Status::Uploaded => "uploaded".to_owned(),
                    Status::Waiting => "to upload".to_owned(),
                    Status::Failed(e) => format!("failed: {e}"),
                };
                println!("{}  {}  {status}", utc_string(r.start), r.title);
            }
            Ok(())
        }
        Some("rewrite") if args.len() >= 3 => {
            let device = device()?.ok_or("missing --device NAME, see `whoosh2garmin devices`")?;
            Ok(fs::write(
                &args[2],
                fit::rewrite(&fs::read(&args[1])?, device)?,
            )?)
        }
        Some("devices") => {
            fit::product_names().for_each(|n| println!("{n}"));
            Ok(())
        }
        _ => Err(USAGE.into()),
    }
}

fn login() -> Result<()> {
    let mut cfg = config::load()?;
    print!("MyWhoosh email (leave empty if you only ride on this computer): ");
    std::io::stdout().flush()?;
    let mut email = String::new();
    std::io::stdin().read_line(&mut email)?;
    let email = email.trim().to_owned();
    if !email.is_empty() {
        let password = rpassword::prompt_password("MyWhoosh password: ")?;
        mywhoosh::Session::login(&email, &password)?; // fail on a typo now, not in cron
        (cfg.mywhoosh_email, cfg.mywhoosh_password) = (email, password);
    }
    cfg.garmin = Some(garmin::browser_login()?);
    config::save(&cfg)?;
    println!("Saved to {}", config::path().display());
    Ok(())
}
