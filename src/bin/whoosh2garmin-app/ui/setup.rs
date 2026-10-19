//! First run: connect the accounts, choose a device, review recent rides.

use super::*;
use crate::autostart;

const STEPS: [&str; 4] = ["MyWhoosh", "Garmin Connect", "Device", "Your rides"];

impl App {
    pub(super) fn setup_page(&mut self, ui: &mut egui::Ui) {
        ui.heading("Set up whoosh2garmin");
        ui.weak(format!("Step {} of 4: {}", self.step + 1, STEPS[self.step]));
        ui.add_space(12.0);
        match self.step {
            0 => self.setup_mywhoosh(ui),
            1 => self.setup_garmin(ui),
            2 => self.setup_device(ui),
            _ => self.setup_review(ui),
        }
        ui.add_space(8.0);
        self.error_label(ui);
    }

    fn setup_mywhoosh(&mut self, ui: &mut egui::Ui) {
        ui.label("Sign in with your MyWhoosh account.");
        ui.add_space(8.0);
        if self.account_form(ui, self.local.then_some("Skip")) {
            self.cfg.mywhoosh_email.clear();
            self.cfg.mywhoosh_password.clear();
            self.go(1, ui.ctx());
        }
        if self.local {
            ui.add_space(8.0);
            ui.weak(
                "Without an account, rides done on this computer are read from MyWhoosh's \
                 file here. An account adds rides from your other devices, and the review \
                 of past rides.",
            );
        }
    }

    fn setup_garmin(&mut self, ui: &mut egui::Ui) {
        ui.label(
            "Sign in to Garmin Connect in your browser. Your password stays on Garmin's \
             page: this app only receives a session.",
        );
        ui.add_space(8.0);
        if self.garmin.is_some() {
            self.garmin_waiting(ui);
        } else if self.cfg.garmin.is_some() {
            ui.label("Signed in to Garmin Connect");
        } else if ui.button("Sign in with Garmin").clicked() {
            self.sign_in_garmin(ui.ctx());
        }
        self.back_next(ui, self.cfg.garmin.is_some());
    }

    fn setup_device(&mut self, ui: &mut egui::Ui) {
        ui.label(
            "Garmin computes Training Effect and Load only for its own devices, so rides \
             are uploaded as if recorded by the model you choose. Pick one with Garmin's \
             training metrics, such as the one you own.",
        );
        ui.add_space(8.0);
        self.device_picker(ui);
        self.back_next(ui, self.cfg.device.is_some());
    }

    fn setup_review(&mut self, ui: &mut egui::Ui) {
        let days = self.cfg.days;
        if self.check.is_some() {
            ui.label(format!("Looking for your rides of the last {days} days…"));
        } else if self.error.is_some() {
            if ui.button("Try again").clicked() {
                self.check_rides(ui.ctx(), sync::check);
            }
        } else if self.rides.is_empty() {
            ui.label(format!("No rides in the last {days} days."));
        } else {
            ui.label(format!(
                "Your rides of the last {days} days. The ticked ones will be uploaded."
            ));
            ui.add_space(8.0);
            let busy = self.busy();
            let uploading = self.uploading();
            for (i, ride) in self.rides.iter().enumerate().rev() {
                let ticked = &mut self.ticked[i];
                ride_row(ui, ride, |ui| match &ride.status {
                    Status::Waiting if uploading == Some(i) => ui.label("Uploading…"),
                    Status::Waiting if self.upload.is_some() && *ticked => ui.weak("Queued"),
                    Status::Waiting => ui.add_enabled(!busy, egui::Checkbox::new(ticked, "Upload")),
                    _ => status_label(ui, &ride.status),
                });
            }
        }
        ui.add_space(12.0);
        self.upload_mode(ui);
        ui.add_space(12.0);

        let n = (self.rides.iter().zip(&self.ticked))
            .filter(|(r, t)| **t && r.status == Status::Waiting)
            .count();
        let start = match n {
            0 => "Start".to_owned(),
            1 => "Upload 1 ride and start".to_owned(),
            n => format!("Upload {n} rides and start"),
        };
        let busy = self.busy();
        ui.horizontal(|ui| {
            if ui.add_enabled(!busy, egui::Button::new("Back")).clicked() {
                self.go(2, ui.ctx());
            }
            if ui.add_enabled(!busy, egui::Button::new(start)).clicked() {
                match n {
                    0 => self.uploads_done(),
                    _ => self.upload_ticked(ui.ctx()),
                }
            }
        });
    }

    fn back_next(&mut self, ui: &mut egui::Ui, can_go_on: bool) {
        ui.add_space(12.0);
        let busy = self.busy();
        ui.horizontal(|ui| {
            if ui.add_enabled(!busy, egui::Button::new("Back")).clicked() {
                self.go(self.step - 1, ui.ctx());
            }
            if ui
                .add_enabled(!busy && can_go_on, egui::Button::new("Next"))
                .clicked()
            {
                self.go(self.step + 1, ui.ctx());
            }
        });
    }

    fn go(&mut self, step: usize, ctx: &egui::Context) {
        self.step = step;
        self.error = None;
        if step == 3 {
            self.set_rides(Vec::new());
            self.check_rides(ctx, sync::check);
        }
    }

    pub(super) fn finish_setup(&mut self) {
        self.cfg.setup_done = true;
        // Right after a ride is the moment to sync, when this computer sees it.
        self.cfg.sync_after_ride = self.local;
        self.cfg.periodic_check = !self.local;
        let r = autostart::set(self.cfg.start_with_os);
        self.report(r);
        self.save();
        self.page = Page::Rides;
    }
}
