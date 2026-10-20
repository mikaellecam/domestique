//! The tray process: starts with the computer, sleeps until MyWhoosh saves a
//! ride, then syncs.
//!
//! It blocks in the OS event loop and wakes only for a click, a change in
//! MyWhoosh's ride folder, a second launch, a finished sync, or a deadline:
//! the startup sync, the ride file going quiet, a retry, or the periodic check.

mod toast;

use crate::autostart;
use std::fs::{self, OpenOptions, TryLockError};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};
use std::{env, thread};
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use whoosh2garmin::Result;
use whoosh2garmin::config::{self, Config};
use whoosh2garmin::sync::{self, Ride, Status};
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
}

#[derive(PartialEq)]
pub enum Job {
    Sync,
    /// The one retry of a failed sync.
    Retry,
    Upload(i64),
}

pub fn run() -> Result<()> {
    let Some((_lock, listener)) = first_instance()? else {
        return Ok(());
    };
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
        status: MenuItem::new("", false, None),
        sync_now: MenuItem::with_id("sync", "Sync now", true, None),
        window: false,
        watcher: None,
        busy: false,
        queue: Vec::new(),
        test_delay: env::var("WHOOSH2GARMIN_DELAY_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .map(Duration::from_secs),
        sync_due: None,
        retry_due: None,
        check_due: None,
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
fn first_instance() -> Result<Option<(fs::File, TcpListener)>> {
    let dir = config::dir();
    fs::create_dir_all(&dir)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("tray.lock"))?;
    match lock.try_lock() {
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
    status: MenuItem,
    sync_now: MenuItem,
    /// A window process is running.
    window: bool,
    watcher: Option<notify::RecommendedWatcher>,
    /// A sync or upload is running; others wait in `queue`.
    busy: bool,
    queue: Vec<Job>,
    /// WHOOSH2GARMIN_DELAY_SECS, for tests: replaces STARTUP_DELAY, QUIET and RETRY.
    test_delay: Option<Duration>,
    /// For the startup sync, then the ride file going quiet.
    sync_due: Option<Instant>,
    retry_due: Option<Instant>,
    check_due: Option<Instant>,
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
                "sync" if !self.busy => {
                    self.error = None; // asked for: report a failure even if it repeats
                    self.request(Job::Sync);
                }
                "open" => self.open_window(),
                "quit" => event_loop.exit(),
                _ => {}
            },
            Msg::Open => self.open_window(),
            Msg::WindowClosed => {
                self.window = false;
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
                self.busy = false;
                self.done(job, &before, result);
                if !self.queue.is_empty() {
                    let job = self.queue.remove(0);
                    self.request(job);
                }
            }
            Msg::Upload(start) => self.request(Job::Upload(start)),
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
        let next = [self.sync_due, self.retry_due, self.check_due];
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
        let menu = Menu::with_items(&[
            &self.status,
            &PredefinedMenuItem::separator(),
            &self.sync_now,
            &MenuItem::with_id("open", "Open", true, None),
            &PredefinedMenuItem::separator(),
            &MenuItem::with_id("quit", "Quit", true, None),
        ])?;
        Ok(TrayIconBuilder::new()
            .with_id("whoosh2garmin")
            .with_menu(Box::new(menu))
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
        if on && let Err(e) = autostart::set(self.cfg.start_with_os) {
            eprintln!("can't change starting with the computer: {e}");
        }
    }

    /// Runs `job` in a worker thread, or after the running one.
    fn request(&mut self, job: Job) {
        if !self.cfg.setup_done {
            return self.open_window();
        }
        if self.busy {
            if !self.queue.contains(&job) {
                self.queue.push(job);
            }
            return;
        }
        self.busy = true;
        let proxy = self.proxy.clone();
        thread::spawn(move || {
            let before = sync::load_status();
            let result = match job {
                Job::Sync | Job::Retry => config::load().and_then(|mut cfg| sync::run(&mut cfg)),
                Job::Upload(start) => upload(start),
            };
            let _ = proxy.send_event(Msg::Done(job, before, result));
        });
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
                        let body = "Open whoosh2garmin to sign in again.";
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
                    toast::show(&self.proxy, &title, "Not on Garmin yet.", Some(r.start));
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
        if self.window {
            return;
        }
        match env::current_exe().and_then(|exe| Command::new(exe).arg("--window").spawn()) {
            Ok(mut child) => {
                self.window = true;
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
        let text = if self.busy {
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
        self.sync_now.set_enabled(!self.busy);
        let alert = !self.cfg.setup_done
            || self.error.is_some()
            || self
                .rides
                .iter()
                .any(|r| matches!(r.status, Status::Waiting | Status::Failed(_)));
        if let Some(tray) = &self.tray {
            let _ = tray.set_tooltip(Some(format!("whoosh2garmin\n{text}")));
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

/// A dot: blue, or orange when something needs the user.
fn icon(alert: bool) -> Icon {
    const SIZE: u32 = 32;
    let [r, g, b] = if alert {
        [0xf2, 0x8c, 0x28]
    } else {
        [0x1a, 0x8c, 0xd8]
    };
    let rgba = (0..SIZE * SIZE)
        .flat_map(|i| {
            let c = SIZE as f32 / 2.0;
            let (x, y) = ((i % SIZE) as f32 + 0.5 - c, (i / SIZE) as f32 + 0.5 - c);
            // Coverage of the pixel, for a smooth edge.
            let a = (c - 1.0 - x.hypot(y)).clamp(0.0, 1.0);
            [r, g, b, (a * 255.0) as u8]
        })
        .collect();
    Icon::from_rgba(rgba, SIZE, SIZE).expect("a valid icon size")
}

#[cfg(test)]
mod tests {
    use super::*;
    use whoosh2garmin::sync::Source;

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
