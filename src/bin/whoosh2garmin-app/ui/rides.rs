//! Home: the accounts at a glance, Sync now, and the rides of the last sync.

use super::*;

impl App {
    pub(super) fn rides_page(&mut self, ui: &mut egui::Ui) {
        // The tray may have synced since; its result is the newer one.
        let checked = last_check();
        if checked != self.loaded && !self.busy() {
            self.set_rides(sync::load_status());
            self.loaded = checked;
        }

        egui::Grid::new("accounts").num_columns(2).show(ui, |ui| {
            ui.label("MyWhoosh");
            match self.cfg.mywhoosh_email.as_str() {
                "" if self.local => ui.weak("No account: rides from this computer only"),
                "" => ui.colored_label(ui.visuals().error_fg_color, "Not connected"),
                email => ui.label(email),
            };
            ui.end_row();
            ui.label("Garmin Connect");
            match self.cfg.garmin {
                Some(_) => ui.label("Connected"),
                None => ui.colored_label(ui.visuals().error_fg_color, "Not connected"),
            };
            ui.end_row();
            ui.label("Device");
            ui.label(
                self.cfg
                    .device
                    .map_or("None chosen".to_owned(), device_name),
            );
            ui.end_row();
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!self.busy(), egui::Button::new("Sync now"))
                .clicked()
            {
                self.check_rides(ui.ctx(), sync::run);
            }
            if self.check.is_some() {
                ui.label("Syncing…");
            } else if let Some(t) = checked {
                ui.weak(format!("Last check {}", local_time(t.as_second())));
            }
        });
        self.error_label(ui);
        ui.add_space(8.0);
        ui.separator();

        if self.rides.is_empty() {
            ui.weak(match checked {
                Some(_) => format!("No rides in the last {} days.", self.cfg.days),
                None => "No check yet.".to_owned(),
            });
        }
        let busy = self.busy();
        let uploading = self.uploading();
        let mut upload = None;
        for (i, ride) in self.rides.iter().enumerate().rev() {
            ride_row(ui, ride, |ui| match &ride.status {
                Status::Waiting if uploading == Some(i) => {
                    ui.label("Uploading…");
                }
                Status::Waiting | Status::Skipped | Status::Failed(_) => {
                    let label = match ride.status {
                        Status::Failed(_) => "Retry",
                        _ => "Upload",
                    };
                    if ui.add_enabled(!busy, egui::Button::new(label)).clicked() {
                        upload = Some(i);
                    }
                    if ride.status == Status::Skipped {
                        status_label(ui, &ride.status);
                    }
                }
                _ => {
                    status_label(ui, &ride.status);
                }
            });
        }
        if let Some(i) = upload {
            self.rides[i].status = Status::Waiting;
            self.ticked = (0..self.rides.len()).map(|j| j == i).collect();
            self.upload_ticked(ui.ctx());
        }
    }
}
