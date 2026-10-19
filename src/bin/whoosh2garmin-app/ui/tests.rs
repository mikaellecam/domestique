//! The pages, driven with fake data and without the network. Snapshots are
//! in tests/snapshots; `UPDATE_SNAPSHOTS=1 cargo test` renews them.

use super::*;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use std::sync::Once;

fn harness(app: App) -> Harness<'static, App> {
    env();
    Harness::builder()
        .with_size([560.0, 640.0])
        .build_ui_state(|ui, app: &mut App| app.show(ui), app)
}

/// Keeps the tests off the real settings, and their times the same anywhere.
fn env() {
    static ENV: Once = Once::new();
    ENV.call_once(|| {
        let dir = std::env::temp_dir().join("whoosh2garmin-ui-tests");
        let _ = fs::remove_dir_all(&dir);
        // SAFETY: every test passes here before anything reads the environment.
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", &dir);
            std::env::set_var("APPDATA", &dir);
            std::env::set_var("TZ", "UTC");
        }
    });
}

fn disabled(h: &Harness<'_, App>, label: &str) -> bool {
    h.get_by_label(label).accesskit_node().is_disabled()
}

fn tokens() -> garmin::Tokens {
    serde_json::from_str(
        r#"{"access_token": "a", "refresh_token": "r", "expires_at": 0, "client_id": "c"}"#,
    )
    .unwrap()
}

fn ride(days_ago: i64, title: &str, status: Status) -> Ride {
    Ride {
        start: 1_792_176_323 - days_ago * 86_400, // 2026-10-16 18:45 UTC
        title: title.to_owned(),
        source: sync::Source::Cloud {
            file_id: "1".to_owned(),
        },
        status,
    }
}

fn configured() -> Config {
    Config {
        mywhoosh_email: "rider@example.com".to_owned(),
        garmin: Some(tokens()),
        device: fit::product_id("edge_explore2"),
        setup_done: true,
        ..Config::default()
    }
}

#[test]
fn setup_steps() {
    let mut h = harness(App::new(Config::default(), true));
    h.run();
    h.snapshot("setup_1_mywhoosh");

    h.get_by_label("Skip").click();
    h.run();
    assert!(disabled(&h, "Next"));
    h.snapshot("setup_2_garmin");

    // What the sign-in worker sends, without a browser.
    let (tx, rx) = mpsc::channel();
    h.state_mut().garmin = Some(rx);
    let url = "https://sso.garmin.com/sso/signin?service=http%3A%2F%2F127.0.0.1%3A40000%2F";
    tx.send(GarminMsg::Url(url.to_owned())).unwrap();
    h.run();
    h.get_by_label("Waiting for you to sign in in your browser…");
    h.get_by_label("open Garmin's sign-in page");
    h.snapshot("setup_2_garmin_waiting");
    tx.send(GarminMsg::Done(Ok(tokens()))).unwrap();
    drop(tx);
    h.run();
    h.get_by_label("Signed in to Garmin Connect");
    assert!(
        config::load().unwrap().garmin.is_some(),
        "tokens are saved at once"
    );
    h.get_by_label("Next").click();
    h.run();
    assert!(disabled(&h, "Next"), "no device is preselected");
    h.get_by_label("Search").focus();
    h.run();
    h.get_by_label("Search").type_text("explore 2");
    h.run();
    assert!(h.query_by_label("Edge 840").is_none());
    h.get_by_label("Edge Explore2").click();
    h.run();
    assert_eq!(h.state().cfg.device, fit::product_id("edge_explore2"));
    assert!(!disabled(&h, "Next"));
    h.snapshot("setup_3_device");
}

#[test]
fn setup_needs_an_account_without_mywhoosh_here() {
    let mut h = harness(App::new(Config::default(), false));
    h.run();
    assert!(h.query_by_label("Skip").is_none());
    assert!(disabled(&h, "Connect"));
    for (field, text) in [("Email", "rider@example.com"), ("Password", "secret")] {
        h.get_by_label(field).focus();
        h.run();
        h.get_by_label(field).type_text(text);
        h.run();
    }
    assert!(!disabled(&h, "Connect"));
}

#[test]
fn review_ticks_the_rides_missing_from_garmin() {
    let mut app = App::new(
        Config {
            setup_done: false,
            ..configured()
        },
        true,
    );
    app.step = 3;
    app.set_rides(vec![
        ride(3, "Tempus Fugit", Status::OnGarmin),
        ride(2, "Downtown Dolomites", Status::Waiting),
        ride(0, "Champs-Élysées", Status::Waiting),
    ]);
    let mut h = harness(app);
    h.run();
    h.get_by_label("Upload 2 rides and start");
    h.snapshot("setup_4_review");

    h.get_all_by_label("Upload").next().unwrap().click();
    h.run();
    h.get_by_label("Upload 1 ride and start");
}

#[test]
fn rides_page_shows_each_status() {
    let reason = "Garmin upload: http status: 500";
    env();
    sync::save_status(&[
        ride(3, "Tempus Fugit", Status::OnGarmin),
        ride(2, "Downtown Dolomites", Status::Uploaded),
        ride(1, "Alula Loop", Status::Failed(reason.to_owned())),
        ride(0, "Champs-Élysées", Status::Waiting),
    ])
    .unwrap();
    // The last check an hour after the last ride, so the snapshot doesn't change.
    let checked = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_792_179_923);
    let status = fs::File::options()
        .write(true)
        .open(config::dir().join("status.json"));
    status.unwrap().set_modified(checked).unwrap();

    let mut h = harness(App::new(configured(), true));
    h.run();
    h.get_by_label("Last check Fri 16 Oct, 19:45");
    for label in ["Upload", "Retry", reason, "Uploaded", "Already on Garmin"] {
        h.get_by_label(label);
    }
    h.snapshot("rides_dark");
    h.ctx.set_theme(egui::Theme::Light);
    h.run();
    h.snapshot("rides_light");
}

#[test]
fn settings_page() {
    let mut app = App::new(configured(), false);
    app.page = Page::Settings;
    let mut h = harness(app);
    h.set_size(egui::vec2(560.0, 860.0)); // the whole page, which scrolls in the window
    h.run();
    h.get_by_label("Signed in as rider@example.com");
    assert!(
        disabled(&h, "Remove"),
        "the account is the only source of rides"
    );
    assert!(disabled(&h, "Sync after each ride"));
    h.snapshot("settings");
}

#[test]
fn names() {
    env();
    assert_eq!(readable("edge_explore2"), "Edge Explore2");
    assert_eq!(squash("Edge Explore 2"), "edgeexplore2");
    assert_eq!(local_time(1_792_176_323), "Fri 16 Oct, 18:45");
}
