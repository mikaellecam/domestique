//! Accounts, device and how syncing runs. Each change is saved at once.

use super::*;
use crate::autostart;

impl App {
    pub(super) fn settings_page(&mut self, ui: &mut egui::Ui) {
        self.error_label(ui);
        let busy = self.busy();

        ui.heading("MyWhoosh");
        if self.cfg.mywhoosh_email.is_empty() || self.changing_account {
            if !self.local && self.cfg.mywhoosh_email.is_empty() {
                ui.label(
                    "MyWhoosh isn't installed on this computer: rides come from your account.",
                );
            }
            if self.account_form(ui, self.changing_account.then_some("Cancel")) {
                self.changing_account = false;
            }
        } else {
            ui.horizontal_wrapped(|ui| {
                ui.label(format!("Signed in as {}", self.cfg.mywhoosh_email));
                if ui.add_enabled(!busy, egui::Button::new("Change")).clicked() {
                    self.changing_account = true;
                    self.email = self.cfg.mywhoosh_email.clone();
                    self.password.clear();
                }
                if ui.add_enabled(!busy, egui::Button::new("Remove")).clicked() {
                    self.cfg.mywhoosh_email.clear();
                    self.cfg.mywhoosh_password.clear();
                    self.email.clear();
                    self.password.clear();
                    self.save();
                }
            });
        }

        section(ui, "Garmin Connect");
        if self.garmin.is_some() {
            self.garmin_waiting(ui);
        } else {
            ui.horizontal_wrapped(|ui| {
                let signed_in = self.cfg.garmin.is_some();
                ui.label(if signed_in {
                    "Connected"
                } else {
                    "Not connected"
                });
                let label = if signed_in {
                    "Sign in again"
                } else {
                    "Sign in with Garmin"
                };
                if ui.add_enabled(!busy, egui::Button::new(label)).clicked() {
                    self.sign_in_garmin(ui.ctx());
                }
                if signed_in
                    && ui
                        .add_enabled(!busy, egui::Button::new("Sign out"))
                        .clicked()
                {
                    self.set_garmin(None);
                }
            });
        }

        section(ui, "Device");
        let mut changed = self.device_picker(ui);

        section(ui, "Syncing");
        changed |= self.upload_mode(ui);
        if ui
            .checkbox(&mut self.cfg.start_with_os, "Start with the computer")
            .changed()
        {
            match autostart::set(self.cfg.start_with_os) {
                Ok(()) => changed = true,
                Err(e) => {
                    self.error = Some(e.to_string());
                    self.cfg.start_with_os = !self.cfg.start_with_os;
                }
            }
        }
        ui.add_enabled_ui(self.local, |ui| {
            changed |= ui
                .checkbox(&mut self.cfg.sync_after_ride, "Sync after each ride")
                .changed();
            if !self.local {
                ui.weak("MyWhoosh isn't installed on this computer.");
            }
        });
        changed |= (ui.checkbox(&mut self.cfg.periodic_check, "Check every 30 minutes"))
            .on_hover_text("For rides recorded on your other devices")
            .changed();
        ui.horizontal(|ui| {
            let label = ui.label("Look back");
            let days = egui::DragValue::new(&mut self.cfg.days)
                .range(1..=365)
                .suffix(" days");
            changed |= ui.add(days).labelled_by(label.id).changed();
        });
        if changed {
            self.save();
        }
    }
}

fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(16.0);
    ui.heading(title);
}
