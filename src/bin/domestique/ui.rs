//! The window: first-run setup, the ride list and settings. It runs in its
//! own process, started by the tray with --window, and exits when closed.

mod rides;
mod settings;
mod setup;
#[cfg(test)]
mod tests;

use domestique::config::{self, Config};
use domestique::sync::{self, Ride, Status};
use domestique::{Result, fit, garmin, mywhoosh};
use eframe::egui;
use jiff::{Timestamp, tz::TimeZone};
use std::sync::{LazyLock, mpsc};
use std::{fs, thread};

pub fn run() -> Result<()> {
    let app = App::new(config::load()?, sync::local_ride_dir().is_some());
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Domestique")
            .with_app_id("domestique")
            .with_inner_size([560.0, 640.0])
            .with_min_inner_size([400.0, 360.0]),
        ..Default::default()
    };
    eframe::run_native("domestique", options, Box::new(|_| Ok(Box::new(app))))
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Setup,
    Rides,
    Settings,
}

/// A worker thread's messages; None once it has finished.
type Job<T> = Option<mpsc::Receiver<T>>;

enum GarminMsg {
    Url(String),
    Done(Result<garmin::Tokens>),
}

struct App {
    cfg: Config,
    /// MyWhoosh is installed on this computer.
    local: bool,
    page: Page,
    /// Setup step, 0 to 3.
    step: usize,
    /// Oldest first, as `sync` returns them.
    rides: Vec<Ride>,
    /// The `last_check` that `rides` were loaded at.
    loaded: Option<Timestamp>,
    /// Rides to upload, by index in `rides`.
    ticked: Vec<bool>,
    /// The MyWhoosh account form, kept apart from `cfg` until MyWhoosh accepts it.
    email: String,
    password: String,
    /// Settings: the account form is open over a connected account.
    changing_account: bool,
    garmin_url: Option<String>,
    device_search: String,
    error: Option<String>,
    login: Job<Result<()>>,
    garmin: Job<GarminMsg>,
    check: Job<Result<Vec<Ride>>>,
    /// (index in `rides`, new status) per ride, then None when all are done.
    upload: Job<Option<(usize, Status)>>,
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.show(ui);
    }
}

impl App {
    fn new(cfg: Config, local: bool) -> Self {
        App {
            page: if cfg.setup_done {
                Page::Rides
            } else {
                Page::Setup
            },
            email: cfg.mywhoosh_email.clone(),
            password: cfg.mywhoosh_password.clone(),
            cfg,
            local,
            step: 0,
            rides: Vec::new(),
            loaded: None,
            ticked: Vec::new(),
            changing_account: false,
            garmin_url: None,
            device_search: String::new(),
            error: None,
            login: None,
            garmin: None,
            check: None,
            upload: None,
        }
    }

    fn show(&mut self, ui: &mut egui::Ui) {
        self.poll();
        if self.page != Page::Setup {
            egui::Panel::top("tabs").show(ui, |ui| {
                ui.horizontal(|ui| {
                    let page = self.page;
                    ui.selectable_value(&mut self.page, Page::Rides, "Rides");
                    ui.selectable_value(&mut self.page, Page::Settings, "Settings");
                    if self.page != page {
                        self.error = None;
                    }
                });
            });
        }
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| match self.page {
                Page::Setup => self.setup_page(ui),
                Page::Rides => self.rides_page(ui),
                Page::Settings => self.settings_page(ui),
            });
        });
    }

    /// Takes in what the workers sent since the last frame.
    fn poll(&mut self) {
        if let Some(r) = recv(&mut self.login) {
            match r {
                Ok(()) => {
                    self.cfg.mywhoosh_email = self.email.trim().to_owned();
                    self.cfg.mywhoosh_password = self.password.clone();
                    self.changing_account = false;
                    match self.page {
                        Page::Setup => self.step = 1,
                        _ => self.save(),
                    }
                }
                Err(e) => self.error = Some(e.to_string()),
            }
        }
        while let Some(msg) = recv(&mut self.garmin) {
            match msg {
                GarminMsg::Url(url) => self.garmin_url = Some(url),
                GarminMsg::Done(Ok(tokens)) => self.set_garmin(Some(tokens)),
                GarminMsg::Done(Err(e)) => self.error = Some(e.to_string()),
            }
        }
        if let Some(r) = recv(&mut self.check) {
            match r {
                Ok(rides) => self.set_rides(rides),
                Err(e) => self.error = Some(e.to_string()),
            }
        }
        while let Some(msg) = recv(&mut self.upload) {
            match msg {
                Some((i, status)) => self.rides[i].status = status,
                None => self.uploads_done(),
            }
        }
    }

    fn busy(&self) -> bool {
        self.login.is_some()
            || self.garmin.is_some()
            || self.check.is_some()
            || self.upload.is_some()
    }

    fn set_rides(&mut self, rides: Vec<Ride>) {
        self.ticked = rides.iter().map(|r| r.status == Status::Waiting).collect();
        self.rides = rides;
    }

    /// Saves the settings. Keeps the Garmin tokens on disk: the tray, or a
    /// sync, may have renewed them since this window loaded them.
    fn save(&mut self) {
        let r = config::load().and_then(|disk| {
            self.cfg.garmin = disk.garmin;
            config::save(&self.cfg)
        });
        self.report(r);
    }

    /// New Garmin tokens, or None to sign out, go to disk at once, as `save`
    /// takes them from there.
    fn set_garmin(&mut self, tokens: Option<garmin::Tokens>) {
        self.cfg.garmin = tokens;
        let r = config::load().and_then(|mut disk| {
            disk.garmin = self.cfg.garmin.clone();
            config::save(&disk)
        });
        self.report(r);
    }

    fn report(&mut self, r: Result<()>) {
        if let Err(e) = r {
            self.error = Some(e.to_string());
        }
    }

    fn connect_mywhoosh(&mut self, ctx: &egui::Context) {
        let (email, password) = (self.email.trim().to_owned(), self.password.clone());
        self.error = None;
        self.login = spawn(ctx, move |send| {
            send(mywhoosh::Session::login(&email, &password).map(|_| ()))
        });
    }

    fn sign_in_garmin(&mut self, ctx: &egui::Context) {
        self.error = None;
        self.garmin_url = None;
        self.garmin = spawn(ctx, |send| {
            let tokens = garmin::browser_login(|url| send(GarminMsg::Url(url.to_owned())));
            send(GarminMsg::Done(tokens));
        });
    }

    /// Uploads the ticked rides that are missing from Garmin, one by one.
    fn upload_ticked(&mut self, ctx: &egui::Context) {
        let todo: Vec<(usize, Ride)> = (self.rides.iter().cloned().enumerate())
            .filter(|(i, r)| self.ticked[*i] && r.status == Status::Waiting)
            .collect();
        let mut cfg = self.cfg.clone();
        self.error = None;
        self.upload = spawn(ctx, move |send| {
            for (i, ride) in todo {
                let status = sync::upload_and_record(&mut cfg, &ride)
                    .unwrap_or_else(|e| Status::Failed(e.to_string()));
                send(Some((i, status)));
            }
            send(None);
        });
    }

    fn uploads_done(&mut self) {
        if self.page == Page::Setup {
            // The review stands for the last sync, so the tray doesn't
            // announce the rides it showed as new, nor upload the unticked ones.
            for (r, &ticked) in self.rides.iter_mut().zip(&self.ticked) {
                if r.status == Status::Waiting && !ticked {
                    r.status = Status::Skipped;
                }
            }
            let r = sync::save_status(&self.rides);
            self.report(r);
            self.finish_setup();
        }
    }

    /// The MyWhoosh email and password fields, Connect, and `other`, a button
    /// beside it. Returns true when `other` was clicked.
    fn account_form(&mut self, ui: &mut egui::Ui, other: Option<&str>) -> bool {
        let mut submit = false;
        egui::Grid::new("account").num_columns(2).show(ui, |ui| {
            let label = ui.label("Email");
            ui.text_edit_singleline(&mut self.email)
                .labelled_by(label.id);
            ui.end_row();
            let label = ui.label("Password");
            let field = ui.add(egui::TextEdit::singleline(&mut self.password).password(true));
            submit = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            field.labelled_by(label.id);
            ui.end_row();
        });
        let busy = self.busy();
        let ready = !busy && !self.email.trim().is_empty() && !self.password.is_empty();
        ui.add_space(4.0);
        let (connect, other) = ui
            .horizontal(|ui| {
                let connect = ui.add_enabled(ready, egui::Button::new("Connect"));
                let other = other.map(|o| ui.add_enabled(!busy, egui::Button::new(o)));
                (connect.clicked(), other.is_some_and(|o| o.clicked()))
            })
            .inner;
        if ready && (submit || connect) {
            self.connect_mywhoosh(ui.ctx());
        }
        if self.login.is_some() {
            ui.label("Connecting to MyWhoosh…");
        }
        other
    }

    /// Runs `sync::check` or `sync::run` on a worker.
    fn check_rides(&mut self, ctx: &egui::Context, f: fn(&mut Config) -> Result<Vec<Ride>>) {
        let mut cfg = self.cfg.clone();
        self.error = None;
        self.check = spawn(ctx, move |send| send(f(&mut cfg)));
    }

    /// While uploads run, the ride being uploaded: they go in order.
    fn uploading(&self) -> Option<usize> {
        self.upload.as_ref()?;
        (0..self.rides.len()).find(|&i| self.ticked[i] && self.rides[i].status == Status::Waiting)
    }

    /// Returns true when the choice changed.
    fn upload_mode(&mut self, ui: &mut egui::Ui) -> bool {
        ui.horizontal_wrapped(|ui| {
            ui.label("Upload new rides:");
            let auto = &mut self.cfg.auto_upload;
            ui.radio_value(auto, true, "Automatically").changed()
                | ui.radio_value(auto, false, "Ask me first").changed()
        })
        .inner
    }

    /// While the browser sign-in runs: what to do, and a way out.
    fn garmin_waiting(&mut self, ui: &mut egui::Ui) {
        ui.label("Waiting for you to sign in in your browser…");
        if let Some(url) = &self.garmin_url {
            ui.horizontal_wrapped(|ui| {
                ui.label("If no browser opened,");
                ui.hyperlink_to("open Garmin's sign-in page", url);
            });
        }
        if ui.button("Cancel").clicked() {
            // The worker stays blocked on its port until the window closes; that is harmless.
            self.garmin = None;
        }
    }

    /// A searchable list of Garmin models. Returns true when the choice changed.
    fn device_picker(&mut self, ui: &mut egui::Ui) -> bool {
        ui.horizontal(|ui| {
            let label = ui.label("Search");
            ui.add(egui::TextEdit::singleline(&mut self.device_search).hint_text("e.g. edge 840"))
                .labelled_by(label.id);
        });
        let query = squash(&self.device_search);
        let matches: Vec<&(u16, String)> = (DEVICES.iter())
            .filter(|(_, name)| squash(name).contains(&query))
            .collect();
        let mut changed = false;
        egui::Frame::group(ui.style()).show(ui, |ui| {
            let row = ui.spacing().interact_size.y;
            egui::ScrollArea::vertical()
                .max_height(180.0)
                .auto_shrink([false, true])
                .show_rows(ui, row, matches.len(), |ui, range| {
                    for &&(id, ref name) in &matches[range] {
                        if ui
                            .selectable_label(self.cfg.device == Some(id), name)
                            .clicked()
                        {
                            changed = self.cfg.device != Some(id);
                            self.cfg.device = Some(id);
                        }
                    }
                });
        });
        match self.cfg.device {
            Some(id) => ui.label(format!("Chosen: {}", device_name(id))),
            None => ui.weak("No model chosen yet."),
        };
        changed
    }

    fn error_label(&self, ui: &mut egui::Ui) {
        if let Some(e) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, e);
        }
    }
}

/// Runs `f` on a worker thread, so the window never waits on the network.
/// Each message `f` sends, and its end, wake the window.
fn spawn<T: Send + 'static>(
    ctx: &egui::Context,
    f: impl FnOnce(&dyn Fn(T)) + Send + 'static,
) -> Job<T> {
    let (tx, rx) = mpsc::channel();
    let ctx = ctx.clone();
    thread::spawn(move || {
        f(&|msg| {
            let _ = tx.send(msg);
            ctx.request_repaint();
        });
        drop(tx);
        ctx.request_repaint();
    });
    Some(rx)
}

/// The job's next message, if any. Clears the job once its worker is gone.
fn recv<T>(job: &mut Job<T>) -> Option<T> {
    match job.as_ref()?.try_recv() {
        Ok(msg) => Some(msg),
        Err(mpsc::TryRecvError::Empty) => None,
        Err(mpsc::TryRecvError::Disconnected) => {
            *job = None;
            None
        }
    }
}

/// Garmin models, (product id, readable name), by name.
static DEVICES: LazyLock<Vec<(u16, String)>> = LazyLock::new(|| {
    let mut all: Vec<_> = (0..=u16::MAX)
        .filter_map(|id| Some((id, readable(&fit::product_name(id)?))))
        .collect();
    all.sort_by(|a, b| a.1.cmp(&b.1));
    all
});

fn device_name(id: u16) -> String {
    fit::product_name(id).map_or_else(|| format!("Garmin model {id}"), |n| readable(&n))
}

/// `edge_explore2` -> "Edge Explore2".
fn readable(name: &str) -> String {
    let words: Vec<String> = (name.split('_'))
        .map(|w| {
            let mut c = w.chars();
            c.next()
                .map(|first| first.to_uppercase().chain(c).collect())
                .unwrap_or_default()
        })
        .collect();
    words.join(" ")
}

/// Lowercase letters and digits only, so "Edge Explore 2" finds `edge_explore2`.
fn squash(s: &str) -> String {
    (s.chars().filter(char::is_ascii_alphanumeric))
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// "Fri 16 Oct, 20:45" in this computer's time zone.
fn local_time(unix: i64) -> String {
    static TZ: LazyLock<TimeZone> = LazyLock::new(TimeZone::system);
    Timestamp::from_second(unix).map_or_else(
        |_| "?".to_owned(),
        |t| {
            t.to_zoned(TZ.clone())
                .strftime("%a %-d %b, %H:%M")
                .to_string()
        },
    )
}

/// When a sync last saved its result, by either process.
fn last_check() -> Option<Timestamp> {
    let modified = fs::metadata(config::dir().join("status.json"))
        .and_then(|m| m.modified())
        .ok()?;
    Timestamp::try_from(modified).ok()
}

fn status_label(ui: &mut egui::Ui, status: &Status) -> egui::Response {
    match status {
        Status::OnGarmin => ui.weak("Already on Garmin"),
        Status::Uploaded => ui.label("Uploaded"),
        Status::Waiting => ui.label("Not on Garmin"),
        Status::Skipped => ui.weak("Skipped"),
        Status::Failed(_) => ui.colored_label(ui.visuals().error_fg_color, "Failed"),
    }
}

/// A ride's title and start time, `right` (its status or what to do about it)
/// on the right, and why it failed below.
fn ride_row<R>(ui: &mut egui::Ui, ride: &Ride, right: impl FnOnce(&mut egui::Ui) -> R) {
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.strong(&ride.title);
            ui.weak(local_time(ride.start));
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), right);
    });
    if let Status::Failed(reason) = &ride.status {
        ui.colored_label(ui.visuals().error_fg_color, reason);
    }
    ui.separator();
}
