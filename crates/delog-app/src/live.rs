use std::net::{IpAddr, SocketAddr};

use delog_stream::{Endpoint, EndpointKind};

use crate::settings::{LiveConnectionMode, LiveConnectionSettings};

#[derive(Debug, Clone)]
pub struct ConnectionDialog {
    kind: EndpointKind,
    host: String,
    port: String,
    serial_path: String,
    baud: String,
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
        }
    }

    pub fn ui(&mut self, ctx: &egui::Context, open: &mut bool) -> Option<Endpoint> {
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

                if let Some(error) = &self.error {
                    ui.add_space(6.0);
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Connect").clicked() {
                        match self.endpoint() {
                            Ok(endpoint) => {
                                out = Some(endpoint);
                                self.error = None;
                                close = true;
                            }
                            Err(err) => self.error = Some(err),
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
        };

        let dialog = ConnectionDialog::from_settings(&settings);
        assert_eq!(
            dialog.endpoint().unwrap(),
            Endpoint::TcpClient {
                remote: "192.168.1.20:5760".parse().unwrap()
            }
        );
        assert_eq!(dialog.to_settings(), settings);
    }
}
