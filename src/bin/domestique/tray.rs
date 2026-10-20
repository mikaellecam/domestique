//! The tray process: starts with the computer, sleeps until MyWhoosh saves a
//! ride, then syncs.
//!
//! It blocks in the OS event loop and wakes only for a click, a change in
//! MyWhoosh's ride folder, a second launch, a finished sync, or a deadline:
//! the startup sync, the ride file going quiet, a retry, the periodic check,
//! or the daily update check.

mod toast;
mod update;

use crate::autostart;
use domestique::Result;
use domestique::config::{self, Config};
use domestique::sync::{self, Ride, Status};
use std::fs::{self, OpenOptions, TryLockError};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{ChildStdin, Command, Stdio};
use std::time::{Duration, Instant};
use std::{env, thread};
use toast::Button;
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::window::WindowId;

/// After login the network may not be up yet.
const STARTUP_DELAY: Duration = Duration::from_secs(60);
/// MyWhoosh may write the ride file during the ride or only at its end, so
/// the sync waits until the file has been left alone this long.
const QUIET: Duration = Duration::from_secs(2 * 60);
/// A failed sync is tried once more after this.
const RETRY: Duration = Duration::from_secs(15 * 60);
const PERIOD: Duration = Duration::from_secs(30 * 60);
const DAY: Duration = Duration::from_secs(24 * 60 * 60);

pub enum Msg {
    Menu(MenuId),
    /// A click on the icon or a notification, or a second launch.
    Open,
    WindowClosed,
    RideFileChanged,
    /// A job finished: the rides' status before it, and after.
    Done(Job, Vec<Ride>, Result<Vec<Ride>>),
    /// The Upload button of a waiting ride's notification, on Windows.
    #[cfg_attr(not(windows), allow(dead_code))]
    Upload(i64),
    /// An update check found this newer version, or none.
    Checked(Result<Option<update::Release>>),
}

#[derive(Clone, PartialEq)]
pub enum Job {
    Sync,
    /// The one retry of a failed sync.
    Retry,
    Upload(i64),
    /// Installs the new version and starts it; the tray then quits.
    Update(update::Release),
}

pub fn run() -> Result<()> {
    let Some((_lock, listener)) = first_instance()? else {
        return Ok(());
    };
    if let Ok(exe) = env::current_exe() {
        update::clean_up(&exe);
    }
    #[allow(unused_mut)]
    let mut builder = EventLoop::with_user_event();
    #[cfg(target_os = "macos")]
    {
        use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
        builder.with_activation_policy(ActivationPolicy::Accessory); // no Dock icon
    }
    let event_loop = builder.build()?;
    let proxy = event_loop.create_proxy();

    let p = proxy.clone();
    thread::spawn(move || {
        while listener.accept().is_ok() {
            let _ = p.send_event(Msg::Open);
        }
    });
    let p = proxy.clone();
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        let _ = p.send_event(Msg::Menu(e.id));
    }));
    let p = proxy.clone();
    TrayIconEvent::set_event_handler(Some(move |e| {
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = e
        {
            let _ = p.send_event(Msg::Open);
        }
    }));

    let mut app = App {
        proxy,
        cfg: config::load()?,
        tray: None,
        menu: Menu::new(),
        status: MenuItem::new("", false, None),
        sync_now: MenuItem::with_id("sync", "Sync now", true, None),
        update_now: MenuItem::with_id("update", "", true, None),
        window: None,
        watcher: None,
        running: None,
        queue: Vec::new(),
        test_delay: env::var("DOMESTIQUE_DELAY_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .map(Duration::from_secs),
        sync_due: None,
        retry_due: None,
        check_due: None,
        update_due: None,
        update: None,
        rides: sync::load_status(),
        last_sync: None,
        error: None,
    };
    event_loop.run_app(&mut app)?;
    Ok(())
}

/// One tray at a time: the first holds a lock on tray.lock and listens on a
/// loopback port saved in tray.port. A second launch finds the lock taken,
/// connects to the port so that the running tray opens the window, and
/// exits. The OS drops the lock when the process ends, even on a crash.
///
/// A tray that an update started waits for the old one to quit instead.
fn first_instance() -> Result<Option<(fs::File, TcpListener)>> {
    let dir = config::dir();
    fs::create_dir_all(&dir)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("tray.lock"))?;
    let mut locked = lock.try_lock();
    let updated = env::args().any(|a| a == "--updated");
    let deadline = Instant::now() + Duration::from_secs(10);
    while updated && matches!(locked, Err(TryLockError::WouldBlock)) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(100));
        locked = lock.try_lock();
    }
    match locked {
        Ok(()) => {
            let listener = TcpListener::bind("127.0.0.1:0")?;
            fs::write(
                dir.join("tray.port"),
                listener.local_addr()?.port().to_string(),
            )?;
            Ok(Some((lock, listener)))
        }
        Err(TryLockError::WouldBlock) => {
            if let Some(port) = fs::read_to_string(dir.join("tray.port"))
                .ok()
                .and_then(|p| p.parse::<u16>().ok())
            {
                let _ = TcpStream::connect(("127.0.0.1", port));
            }
            Ok(None)
        }
        Err(TryLockError::Error(e)) => Err(e.into()),
    }
}

struct App {
    proxy: EventLoopProxy<Msg>,
    cfg: Config,
    /// Created once the event loop runs, as macOS requires.
    tray: Option<TrayIcon>,
    menu: Menu,
    status: MenuItem,
    sync_now: MenuItem,
    /// In the menu once there is a new version.
    update_now: MenuItem,
    /// The open window's stdin. The window exits when it closes, so it
    /// quits along with the tray.
    window: Option<ChildStdin>,
    watcher: Option<notify::RecommendedWatcher>,
    /// The sync, upload or update running; others wait in `queue`.
    running: Option<Job>,
    queue: Vec<Job>,
    /// DOMESTIQUE_DELAY_SECS, for tests: replaces STARTUP_DELAY, QUIET and RETRY.
    test_delay: Option<Duration>,
    /// For the startup sync, then the ride file going quiet.
    sync_due: Option<Instant>,
    retry_due: Option<Instant>,
    check_due: Option<Instant>,
    update_due: Option<Instant>,
    /// A newer version, found by an update check.
    update: Option<update::Release>,
    /// As of the last sync, by this process or the window.
    rides: Vec<Ride>,
    /// Local time of the last sync, and how many rides it uploaded.
    last_sync: Option<(String, usize)>,
    error: Option<String>,
}

impl ApplicationHandler<Msg> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.tray.is_some() {
            return;
        }
        match self.build_tray() {
            Ok(tray) => self.tray = Some(tray),
            Err(e) => {
                eprintln!("can't show the tray icon: {e}");
                return event_loop.exit();
            }
        }
        self.apply_settings();
        if self.cfg.setup_done {
            self.sync_due = Some(Instant::now() + self.delay(STARTUP_DELAY));
        } else {
            self.open_window();
        }
        self.refresh();
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, msg: Msg) {
        match msg {
            Msg::Menu(id) => match id.as_ref() {
                "sync" if self.running.is_none() => {
                    self.error = None; // asked for: report a failure even if it repeats
                    self.request(Job::Sync);
                }
                "update" => {
                    if let Some(release) = self.update.clone() {
                        self.request(Job::Update(release));
                    }
                }
                "open" => self.open_window(),
                "quit" => event_loop.exit(),
                _ => {}
            },
            Msg::Open => self.open_window(),
            Msg::WindowClosed => {
                self.window = None;
                let was_setup = self.cfg.setup_done;
                match config::load() {
                    Ok(cfg) => self.cfg = cfg,
                    Err(e) => eprintln!("can't read the settings: {e}"),
                }
                self.rides = sync::load_status();
                self.apply_settings();
                if self.cfg.setup_done && !was_setup {
                    self.request(Job::Sync);
                }
            }
            Msg::RideFileChanged => {
                // May come often during a ride: only move the deadline.
                self.sync_due = Some(Instant::now() + self.delay(QUIET));
                return;
            }
            Msg::Done(job, before, result) => {
                self.running = None;
                match (job, result) {
                    (Job::Update(_), Ok(_)) => return event_loop.exit(), // for the new version
                    (Job::Update(release), Err(e)) => {
                        let title = format!("Couldn't update to {}", release.version());
                        toast::show(&self.proxy, &title, &e.to_string(), None);
                    }
                    (job, result) => self.done(job, &before, result),
                }
                if !self.queue.is_empty() {
                    let job = self.queue.remove(0);
                    self.request(job);
                }
            }
            Msg::Upload(start) => self.request(Job::Upload(start)),
            Msg::Checked(Ok(Some(release))) => self.offer(release),
            Msg::Checked(Ok(None)) => {}
            Msg::Checked(Err(e)) => eprintln!("can't check for updates: {e}"),
        }
        self.refresh();
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        if self.sync_due.is_some_and(|t| t <= now) {
            self.sync_due = None;
            self.error = None; // after a ride the user waits: report a failure even if it repeats
            self.request(Job::Sync);
            self.refresh();
        }
        if self.retry_due.is_some_and(|t| t <= now) {
            self.retry_due = None;
            self.request(Job::Retry);
            self.refresh();
        }
        if self.check_due.is_some_and(|t| t <= now) {
            self.check_due = Some(now + PERIOD);
            self.request(Job::Sync);
            self.refresh();
        }
        if self.update_due.is_some_and(|t| t <= now) {
            self.update_due = Some(now + DAY);
            let proxy = self.proxy.clone();
            thread::spawn(move || {
                let _ = proxy.send_event(Msg::Checked(update::check()));
            });
        }
        let next = [
            self.sync_due,
            self.retry_due,
            self.check_due,
            self.update_due,
        ];
        event_loop.set_control_flow(match next.into_iter().flatten().min() {
            Some(t) => ControlFlow::WaitUntil(t),
            None => ControlFlow::Wait,
        });
    }

    fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}
}

impl App {
    fn delay(&self, normal: Duration) -> Duration {
        self.test_delay.unwrap_or(normal)
    }

    fn build_tray(&self) -> Result<TrayIcon> {
        self.menu.append_items(&[
            &self.status,
            &PredefinedMenuItem::separator(),
            &self.sync_now,
            &MenuItem::with_id("open", "Open", true, None),
            &PredefinedMenuItem::separator(),
            &MenuItem::with_id("quit", "Quit", true, None),
        ])?;
        Ok(TrayIconBuilder::new()
            .with_id("domestique")
            .with_menu(Box::new(self.menu.clone()))
            .with_menu_on_left_click(false)
            .with_icon(icon(false))
            .build()?)
    }

    /// Follows the settings, at startup and whenever the window closes.
    fn apply_settings(&mut self) {
        let on = self.cfg.setup_done;
        self.watcher = None;
        if on
            && self.cfg.sync_after_ride
            && let Some(dir) = sync::local_ride_dir()
        {
            match watch(&dir, self.proxy.clone()) {
                Ok(w) => self.watcher = Some(w),
                Err(e) => eprintln!("can't watch {}: {e}", dir.display()),
            }
        }
        self.check_due = (on && self.cfg.periodic_check).then(|| Instant::now() + PERIOD);
        // Kept when already set: the window closing doesn't move the daily check.
        if !self.cfg.check_updates {
            self.update_due = None;
        } else if self.update_due.is_none() {
            self.update_due = Some(Instant::now() + self.delay(STARTUP_DELAY));
        }
        if on && let Err(e) = autostart::set(self.cfg.start_with_os) {
            eprintln!("can't change starting with the computer: {e}");
        }
    }

    /// Runs `job` in a worker thread, or after the running one.
    fn request(&mut self, job: Job) {
        if !self.cfg.setup_done && !matches!(job, Job::Update(_)) {
            return self.open_window();
        }
        if self.running.is_some() {
            if !self.queue.contains(&job) {
                self.queue.push(job);
            }
            return;
        }
        self.running = Some(job.clone());
        let proxy = self.proxy.clone();
        thread::spawn(move || {
            let before = sync::load_status();
            let result = match &job {
                Job::Sync | Job::Retry => config::load().and_then(|mut cfg| sync::run(&mut cfg)),
                Job::Upload(start) => upload(*start),
                Job::Update(release) => update::install(release).map(|()| Vec::new()),
            };
            let _ = proxy.send_event(Msg::Done(job, before, result));
        });
    }

    /// Offers a new version in the menu, and tells about it once.
    fn offer(&mut self, release: update::Release) {
        let version = release.version();
        if self.update.as_ref().is_some_and(|u| u.version() == version) {
            return;
        }
        self.update_now.set_text(format!("Update to {version}"));
        if self.update.is_none() {
            let _ = self.menu.insert(&self.update_now, 2);
        }
        let title = format!("Domestique {version} is available");
        toast::show(&self.proxy, &title, "", Some(Button::Update));
        self.update = Some(release);
    }

    fn done(&mut self, job: Job, before: &[Ride], result: Result<Vec<Ride>>) {
        // One retry per sync, then the next trigger.
        if job == Job::Sync {
            let failed = match &result {
                Ok(rides) => rides.iter().any(|r| matches!(r.status, Status::Failed(_))),
                Err(_) => true,
            };
            self.retry_due = failed.then(|| Instant::now() + self.delay(RETRY));
        }
        let rides = match result {
            Ok(rides) => rides,
            Err(e) => {
                let e = e.to_string();
                if self.error.as_ref() != Some(&e) {
                    if sign_in_needed(&e) {
                        let body = "Open Domestique to sign in again.";
                        toast::show(&self.proxy, "Garmin sign-in needed", body, None);
                    } else {
                        toast::show(&self.proxy, "Sync failed", &e, None);
                    }
                }
                self.error = Some(e);
                return;
            }
        };
        let mut uploaded = 0;
        for r in changed(before, &rides) {
            match &r.status {
                Status::Uploaded => {
                    uploaded += 1;
                    let title = format!("Uploaded {} to Garmin", r.title);
                    toast::show(&self.proxy, &title, "", None);
                }
                Status::Waiting => {
                    let title = format!("New ride: {}", r.title);
                    let upload = Some(Button::Upload(r.start));
                    toast::show(&self.proxy, &title, "Not on Garmin yet.", upload);
                }
                Status::Failed(e) => {
                    let title = format!("Couldn't upload {}", r.title);
                    toast::show(&self.proxy, &title, e, None);
                }
                Status::OnGarmin | Status::Skipped => {}
            }
        }
        let time = jiff::Zoned::now().strftime("%H:%M").to_string();
        self.last_sync = Some((time, uploaded));
        self.rides = rides;
        self.error = None;
    }

    fn open_window(&mut self) {
        if self.window.is_some() {
            return;
        }
        let spawned = env::current_exe().and_then(|exe| {
            let mut window = Command::new(exe);
            window.arg("--window").stdin(Stdio::piped()).spawn()
        });
        match spawned {
            Ok(mut child) => {
                self.window = child.stdin.take();
                let proxy = self.proxy.clone();
                thread::spawn(move || {
                    let _ = child.wait();
                    let _ = proxy.send_event(Msg::WindowClosed);
                });
            }
            Err(e) => eprintln!("can't open the window: {e}"),
        }
    }

    /// Shows the state in the menu's first line, the tooltip and the icon.
    fn refresh(&self) {
        let updating = matches!(self.running, Some(Job::Update(_)));
        let text = if updating {
            "Updating…".to_owned()
        } else if self.running.is_some() {
            "Syncing…".to_owned()
        } else if !self.cfg.setup_done {
            "Setup needed".to_owned()
        } else if let Some(e) = &self.error {
            if sign_in_needed(e) {
                "Garmin sign-in needed"
            } else {
                "Sync failed"
            }
            .to_owned()
        } else if let Some((time, uploaded)) = &self.last_sync {
            format!("Last sync {time} · {}", summary(*uploaded, &self.rides))
        } else {
            "Not synced yet".to_owned()
        };
        self.status.set_text(&text);
        self.sync_now.set_enabled(self.running.is_none());
        self.update_now.set_enabled(!updating);
        let alert = !self.cfg.setup_done
            || self.error.is_some()
            || self
                .rides
                .iter()
                .any(|r| matches!(r.status, Status::Waiting | Status::Failed(_)));
        if let Some(tray) = &self.tray {
            let _ = tray.set_tooltip(Some(format!("Domestique\n{text}")));
            let _ = tray.set_icon(Some(icon(alert)));
        }
    }
}

/// Wakes the loop when MyWhoosh writes its ride file. Reading it doesn't
/// count, or each sync would schedule the next.
fn watch(dir: &Path, proxy: EventLoopProxy<Msg>) -> notify::Result<notify::RecommendedWatcher> {
    use notify::event::{EventKind, ModifyKind};
    let mut watcher = notify::recommended_watcher(move |e: notify::Result<notify::Event>| {
        let Ok(e) = e else { return };
        let written = matches!(
            e.kind,
            EventKind::Create(_)
                | EventKind::Modify(ModifyKind::Data(_) | ModifyKind::Name(_) | ModifyKind::Any)
        );
        // The name sync::local_ride_file looks for.
        let ride_file = |p: &Path| {
            p.file_name().is_some_and(|n| {
                let n = n.to_string_lossy();
                n.starts_with("MyNewActivity-") && n.ends_with(".fit")
            })
        };
        if written && e.paths.iter().any(|p| ride_file(p)) {
            let _ = proxy.send_event(Msg::RideFileChanged);
        }
    })?;
    notify::Watcher::watch(&mut watcher, dir, notify::RecursiveMode::NonRecursive)?;
    Ok(watcher)
}

/// Uploads a ride the last sync listed. Returns the rides' new status.
fn upload(start: i64) -> Result<Vec<Ride>> {
    let ride = (sync::load_status().into_iter())
        .find(|r| r.start == start)
        .ok_or("the ride is no longer in the list")?;
    sync::upload_and_record(&mut config::load()?, &ride)?;
    Ok(sync::load_status())
}

/// The rides that are new or whose status differs from `before`.
fn changed<'a>(before: &[Ride], after: &'a [Ride]) -> impl Iterator<Item = &'a Ride> {
    after.iter().filter(|r| {
        !before
            .iter()
            .any(|b| b.start == r.start && b.status == r.status)
    })
}

/// The core's errors are worded for the command line.
fn sign_in_needed(e: &str) -> bool {
    e.contains("session expired") || e.contains("not signed in")
}

/// "1 ride uploaded · 2 rides to upload", or "up to date".
fn summary(uploaded: usize, rides: &[Ride]) -> String {
    let count = |f: fn(&Status) -> bool| rides.iter().filter(|r| f(&r.status)).count();
    let parts: Vec<String> = [
        (uploaded, "uploaded"),
        (count(|s| *s == Status::Waiting), "to upload"),
        (count(|s| matches!(s, Status::Failed(_))), "failed"),
    ]
    .into_iter()
    .filter(|&(n, _)| n > 0)
    .map(|(n, what)| format!("{n} ride{} {what}", if n == 1 { "" } else { "s" }))
    .collect();
    if parts.is_empty() {
        "up to date".to_owned()
    } else {
        parts.join(" · ")
    }
}

/// The app's icon, with an orange badge when something needs the user.
fn icon(alert: bool) -> Icon {
    let png: &[u8] = if alert {
        include_bytes!("../../../assets/icon-alert-32.png")
    } else {
        include_bytes!("../../../assets/icon-32.png")
    };
    let icon = eframe::icon_data::from_png_bytes(png).expect("a valid PNG");
    Icon::from_rgba(icon.rgba, icon.width, icon.height).expect("a valid icon size")
}

#[cfg(test)]
mod tests {
    use super::*;
    use domestique::sync::Source;

    fn ride(start: i64, status: Status) -> Ride {
        let source = Source::Cloud {
            file_id: String::new(),
        };
        Ride {
            start,
            title: format!("ride {start}"),
            source,
            status,
        }
    }

    #[test]
    fn changes() {
        let before = [ride(1, Status::OnGarmin), ride(2, Status::Waiting)];
        let after = [
            ride(1, Status::OnGarmin),
            ride(2, Status::Uploaded),
            ride(3, Status::Failed("no device".into())),
        ];
        let starts: Vec<i64> = changed(&before, &after).map(|r| r.start).collect();
        assert_eq!(starts, [2, 3]);
        assert_eq!(changed(&after, &after).count(), 0);
    }

    #[test]
    fn icons() {
        icon(false);
        icon(true);
    }

    #[test]
    fn summaries() {
        assert_eq!(summary(0, &[ride(1, Status::OnGarmin)]), "up to date");
        let rides = [
            ride(1, Status::Uploaded),
            ride(2, Status::Waiting),
            ride(3, Status::Waiting),
        ];
        assert_eq!(summary(1, &rides), "1 ride uploaded · 2 rides to upload");
    }
}
