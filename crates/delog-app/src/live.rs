use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::time::{SystemTime, UNIX_EPOCH};

use delog_stream::{Endpoint, EndpointKind};

use crate::settings::{LiveConnectionMode, LiveConnectionSettings};

#[derive(Debug)]
pub struct ConnectionDialog {
    kind: EndpointKind,
    host: String,
    port: String,
    serial_path: String,
    baud: String,
    recording_enabled: bool,
    recording_dir: String,
    folder_picker: Option<Receiver<Option<PathBuf>>>,
    error: Option<String>,
}

impl Default for ConnectionDialog {
    fn default() -> Self {
        Self {
            kind: EndpointKind::UdpServer,
            host: "0.0.0.0".to_owned(),
            port: "14550".to_owned(),
            serial_path: default_serial_path(),
            baud: "115200".to_owned(),
            recording_enabled: false,
            recording_dir: String::new(),
            folder_picker: None,
            error: None,
        }
    }
}

impl ConnectionDialog {
    pub fn from_settings(settings: &LiveConnectionSettings) -> Self {
        Self {
            kind: endpoint_kind(settings.mode),
            host: settings.host.clone(),
            port: settings.port.to_string(),
            serial_path: settings.serial_path.clone(),
            baud: settings.baud.to_string(),
            recording_enabled: settings.recording_enabled,
            recording_dir: settings.recording_dir.clone(),
            folder_picker: None,
            error: None,
        }
    }

    pub fn to_settings(&self) -> LiveConnectionSettings {
        LiveConnectionSettings {
            mode: live_connection_mode(self.kind),
            host: self.host.trim().to_owned(),
            port: self
                .port
                .trim()
                .parse()
                .unwrap_or(default_port_u16(self.kind)),
            serial_path: self.serial_path.trim().to_owned(),
            baud: self.baud.trim().parse().unwrap_or(115_200),
            recording_enabled: self.recording_enabled,
            recording_dir: self.recording_dir.trim().to_owned(),
        }
    }

    pub fn ui(&mut self, ctx: &egui::Context, open: &mut bool) -> Option<Endpoint> {
        self.poll_folder_picker();
        let mut out = None;
        let mut close = false;
        egui::Window::new("MAVLink Connection")
            .open(open)
            .collapsible(false)
            .default_pos(ctx.content_rect().center())
            .pivot(egui::Align2::CENTER_CENTER)
            .resizable(false)
            .show(ctx, |ui| {
                egui::Grid::new("live_endpoint_grid")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("Mode");
                        let prev_kind = self.kind;
                        egui::ComboBox::from_id_salt("live_endpoint_kind")
                            .selected_text(self.kind.label())
                            .show_ui(ui, |ui| {
                                for kind in EndpointKind::ALL {
                                    ui.selectable_value(&mut self.kind, kind, kind.label());
                                }
                            });
                        // Swap to the new mode's default port only if the user
                        // hadn't customized the old one.
                        if self.kind != prev_kind && self.port.trim() == default_port(prev_kind) {
                            self.port = default_port(self.kind).to_owned();
                        }
                        ui.end_row();

                        if self.kind == EndpointKind::Serial {
                            ui.label("Serial");
                            ui.text_edit_singleline(&mut self.serial_path);
                            ui.end_row();
                            ui.label("Baud");
                            ui.text_edit_singleline(&mut self.baud);
                            ui.end_row();
                        } else {
                            ui.label(match self.kind {
                                EndpointKind::UdpServer => "Bind",
                                EndpointKind::TcpClient => "Remote",
                                EndpointKind::Serial => unreachable!(),
                            });
                            ui.text_edit_singleline(&mut self.host);
                            ui.end_row();
                            ui.label("Port");
                            ui.text_edit_singleline(&mut self.port);
                            ui.end_row();
                        }
                    });

                ui.add_space(6.0);
                ui.checkbox(&mut self.recording_enabled, "Record .tlog");
                if self.recording_enabled {
                    egui::Grid::new("live_recording_grid")
                        .num_columns(2)
                        .spacing([12.0, 8.0])
                        .show(ui, |ui| {
                            ui.label("Folder");
                            ui.horizontal(|ui| {
                                if self.recording_dir.trim().is_empty() {
                                    ui.weak("No folder selected");
                                } else {
                                    ui.monospace(self.recording_dir.trim());
                                }
                                let picking = self.folder_picker.is_some();
                                if ui
                                    .add_enabled(!picking, egui::Button::new("Choose Folder..."))
                                    .clicked()
                                {
                                    self.choose_recording_folder(ui.ctx());
                                }
                            });
                            ui.end_row();
                        });
                }

                if let Some(error) = &self.error {
                    ui.add_space(6.0);
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Connect").clicked() {
                        match (self.endpoint(), self.recording_path()) {
                            (Ok(endpoint), Ok(_recording)) => {
                                out = Some(endpoint);
                                self.error = None;
                                close = true;
                            }
                            (Err(err), _) | (_, Err(err)) => self.error = Some(err),
                        }
                    }
                    if ui.button("Cancel").clicked() {
                        self.error = None;
                        close = true;
                    }
                });
            });
        if close {
            *open = false;
        }
        out
    }

    pub fn endpoint(&self) -> Result<Endpoint, String> {
        match self.kind {
            EndpointKind::UdpServer => Ok(Endpoint::UdpServer {
                bind: self.socket_addr()?,
            }),
            EndpointKind::TcpClient => Ok(Endpoint::TcpClient {
                remote: self.socket_addr()?,
            }),
            EndpointKind::Serial => {
                let baud = self
                    .baud
                    .trim()
                    .parse::<u32>()
                    .map_err(|_| "baud must be a positive integer".to_owned())?;
                Endpoint::serial(self.serial_path.trim(), baud).map_err(|err| err.to_string())
            }
        }
    }

    pub fn recording_path(&self) -> Result<Option<PathBuf>, String> {
        self.recording_path_at(now_unix_us())
    }

    pub fn recording_path_at(&self, unix_us: i64) -> Result<Option<PathBuf>, String> {
        if !self.recording_enabled {
            return Ok(None);
        }
        let dir = self.recording_dir.trim();
        if dir.is_empty() {
            return Err("recording folder is required".to_owned());
        }
        Ok(Some(PathBuf::from(dir).join(recording_filename(unix_us))))
    }

    fn choose_recording_folder(&mut self, ctx: &egui::Context) {
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        let start_dir = self.recording_dir.trim().to_owned();
        match std::thread::Builder::new()
            .name("delog-recording-folder".into())
            .spawn(move || {
                let mut dialog = rfd::FileDialog::new().set_title("Choose recording folder");
                if !start_dir.is_empty() {
                    dialog = dialog.set_directory(start_dir);
                }
                let _ = tx.send(dialog.pick_folder());
                ctx.request_repaint();
            }) {
            Ok(_) => self.folder_picker = Some(rx),
            Err(err) => self.error = Some(format!("open folder picker: {err}")),
        }
    }

    fn poll_folder_picker(&mut self) {
        let Some(rx) = self.folder_picker.take() else {
            return;
        };
        match rx.try_recv() {
            Ok(Some(path)) => self.recording_dir = path.display().to_string(),
            Ok(None) | Err(mpsc::TryRecvError::Disconnected) => {}
            Err(mpsc::TryRecvError::Empty) => {
                self.folder_picker = Some(rx);
            }
        }
    }

    fn socket_addr(&self) -> Result<SocketAddr, String> {
        let ip = self
            .host
            .trim()
            .parse::<IpAddr>()
            .map_err(|_| "address must be an IP literal".to_owned())?;
        let port = self
            .port
            .trim()
            .parse::<u16>()
            .map_err(|_| "port must be 0-65535".to_owned())?;
        Ok(SocketAddr::new(ip, port))
    }
}

// 14550 is the MAVLink/GCS UDP port; 5760 is ArduPilot SITL's TCP port.
fn default_port(kind: EndpointKind) -> &'static str {
    match kind {
        EndpointKind::UdpServer => "14550",
        EndpointKind::TcpClient => "5760",
        EndpointKind::Serial => "",
    }
}

fn default_port_u16(kind: EndpointKind) -> u16 {
    match kind {
        EndpointKind::UdpServer => 14550,
        EndpointKind::TcpClient => 5760,
        EndpointKind::Serial => 0,
    }
}

fn endpoint_kind(mode: LiveConnectionMode) -> EndpointKind {
    match mode {
        LiveConnectionMode::UdpServer => EndpointKind::UdpServer,
        LiveConnectionMode::TcpClient => EndpointKind::TcpClient,
        LiveConnectionMode::Serial => EndpointKind::Serial,
    }
}

fn live_connection_mode(kind: EndpointKind) -> LiveConnectionMode {
    match kind {
        EndpointKind::UdpServer => LiveConnectionMode::UdpServer,
        EndpointKind::TcpClient => LiveConnectionMode::TcpClient,
        EndpointKind::Serial => LiveConnectionMode::Serial,
    }
}

fn recording_filename(unix_us: i64) -> String {
    let secs = unix_us.div_euclid(1_000_000);
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let (y, mo, d) = civil_from_days(days);
    format!(
        "delog-live-{y:04}-{mo:02}-{d:02}_{:02}-{:02}-{:02}.tlog",
        sod / 3_600,
        (sod / 60) % 60,
        sod % 60
    )
}

fn now_unix_us() -> i64 {
    let Ok(duration) = SystemTime::now().duration_since(UNIX_EPOCH) else {
        return 0;
    };
    duration.as_micros().min(i64::MAX as u128) as i64
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

fn default_serial_path() -> String {
    #[cfg(windows)]
    {
        "COM3".to_owned()
    }
    #[cfg(not(windows))]
    {
        "/dev/ttyACM0".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn dialog(kind: EndpointKind) -> ConnectionDialog {
        ConnectionDialog {
            kind,
            host: "127.0.0.1".to_owned(),
            port: "14550".to_owned(),
            serial_path: "/dev/ttyUSB0".to_owned(),
            baud: "57600".to_owned(),
            recording_enabled: false,
            recording_dir: String::new(),
            folder_picker: None,
            error: None,
        }
    }

    #[test]
    fn builds_network_endpoint_for_each_mode() {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 14550);
        assert_eq!(
            dialog(EndpointKind::UdpServer).endpoint().unwrap(),
            Endpoint::UdpServer { bind: addr }
        );
        assert_eq!(
            dialog(EndpointKind::TcpClient).endpoint().unwrap(),
            Endpoint::TcpClient { remote: addr }
        );
    }

    #[test]
    fn builds_serial_endpoint() {
        assert_eq!(
            dialog(EndpointKind::Serial).endpoint().unwrap(),
            Endpoint::Serial {
                path: "/dev/ttyUSB0".to_owned(),
                baud: 57_600
            }
        );
    }

    #[test]
    fn reports_invalid_address_and_baud() {
        let mut d = dialog(EndpointKind::TcpClient);
        d.host = "localhost".to_owned();
        assert_eq!(d.endpoint().unwrap_err(), "address must be an IP literal");

        let mut d = dialog(EndpointKind::Serial);
        d.baud = "fast".to_owned();
        assert_eq!(d.endpoint().unwrap_err(), "baud must be a positive integer");
    }

    #[test]
    fn restores_endpoint_from_last_live_connection_settings() {
        let settings = crate::settings::LiveConnectionSettings {
            mode: crate::settings::LiveConnectionMode::TcpClient,
            host: "192.168.1.20".to_owned(),
            port: 5760,
            serial_path: "/dev/ttyUSB0".to_owned(),
            baud: 921_600,
            recording_enabled: true,
            recording_dir: "/tmp/logs".to_owned(),
        };

        let dialog = ConnectionDialog::from_settings(&settings);
        assert_eq!(
            dialog.endpoint().unwrap(),
            Endpoint::TcpClient {
                remote: "192.168.1.20:5760".parse().unwrap()
            }
        );
        assert_eq!(dialog.to_settings(), settings);
        assert_eq!(
            dialog.recording_path_at(1_781_369_696_000_000).unwrap(),
            Some(std::path::PathBuf::from(
                "/tmp/logs/delog-live-2026-06-13_16-54-56.tlog"
            ))
        );
    }

    #[test]
    fn recording_folder_is_optional_until_recording_is_enabled() {
        let mut dialog = dialog(EndpointKind::UdpServer);
        assert_eq!(dialog.recording_path_at(0).unwrap(), None);

        dialog.recording_enabled = true;
        assert_eq!(
            dialog.recording_path_at(0).unwrap_err(),
            "recording folder is required"
        );

        dialog.recording_dir = "/tmp/logs".to_owned();
        assert_eq!(
            dialog.recording_path_at(0).unwrap(),
            Some(std::path::PathBuf::from(
                "/tmp/logs/delog-live-1970-01-01_00-00-00.tlog"
            ))
        );
    }
}
