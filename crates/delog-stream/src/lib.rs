//! DeLOG live streaming: MAVLink link backends (UDP/TCP/serial), the link
//! state machine, message→field extraction and the raw-frame recorder.
//!
//! Dependency rule (PLAN.md §3.2): like parsers, this crate never sees GPU
//! or UI; live batches feed the same `IngestSink` path as files.

use std::fmt;
use std::net::SocketAddr;

/// Configured live-link endpoint (PLAN.md §7.1, LIV-01).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    /// Listen for UDP datagrams on `bind` (GCS-style).
    UdpServer { bind: SocketAddr },
    /// Send/receive UDP datagrams to/from `remote`.
    UdpClient { remote: SocketAddr },
    /// Connect to a TCP server.
    TcpClient { remote: SocketAddr },
    /// Listen for one TCP client.
    TcpServer { bind: SocketAddr },
    /// Open a serial device at `baud`.
    Serial { path: String, baud: u32 },
}

/// Endpoint transport/mode without its address payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointKind {
    UdpServer,
    UdpClient,
    TcpClient,
    TcpServer,
    Serial,
}

/// Endpoint validation failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndpointError {
    EmptySerialPath,
    InvalidBaud,
}

impl Endpoint {
    pub fn serial(path: impl Into<String>, baud: u32) -> Result<Self, EndpointError> {
        let path = path.into();
        if path.trim().is_empty() {
            return Err(EndpointError::EmptySerialPath);
        }
        if baud == 0 {
            return Err(EndpointError::InvalidBaud);
        }
        Ok(Self::Serial { path, baud })
    }

    pub fn kind(&self) -> EndpointKind {
        match self {
            Self::UdpServer { .. } => EndpointKind::UdpServer,
            Self::UdpClient { .. } => EndpointKind::UdpClient,
            Self::TcpClient { .. } => EndpointKind::TcpClient,
            Self::TcpServer { .. } => EndpointKind::TcpServer,
            Self::Serial { .. } => EndpointKind::Serial,
        }
    }
}

impl EndpointKind {
    pub const ALL: [Self; 5] = [
        Self::UdpServer,
        Self::UdpClient,
        Self::TcpClient,
        Self::TcpServer,
        Self::Serial,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::UdpServer => "UDP server",
            Self::UdpClient => "UDP client",
            Self::TcpClient => "TCP client",
            Self::TcpServer => "TCP server",
            Self::Serial => "Serial",
        }
    }
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UdpServer { bind } => write!(f, "udp-server://{bind}"),
            Self::UdpClient { remote } => write!(f, "udp-client://{remote}"),
            Self::TcpClient { remote } => write!(f, "tcp-client://{remote}"),
            Self::TcpServer { bind } => write!(f, "tcp-server://{bind}"),
            Self::Serial { path, baud } => write!(f, "serial://{path}@{baud}"),
        }
    }
}

impl fmt::Display for EndpointKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

impl fmt::Display for EndpointError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptySerialPath => write!(f, "serial path is required"),
            Self::InvalidBaud => write!(f, "baud must be greater than zero"),
        }
    }
}

impl std::error::Error for EndpointError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_kind_labels_cover_all_modes() {
        let labels: Vec<_> = EndpointKind::ALL.iter().map(|kind| kind.label()).collect();
        assert_eq!(
            labels,
            vec![
                "UDP server",
                "UDP client",
                "TCP client",
                "TCP server",
                "Serial"
            ]
        );
    }

    #[test]
    fn endpoint_display_is_stable_and_compact() {
        let bind = "0.0.0.0:14550".parse().unwrap();
        let remote = "127.0.0.1:14550".parse().unwrap();
        assert_eq!(
            Endpoint::UdpServer { bind }.to_string(),
            "udp-server://0.0.0.0:14550"
        );
        assert_eq!(
            Endpoint::TcpClient { remote }.to_string(),
            "tcp-client://127.0.0.1:14550"
        );
        assert_eq!(
            Endpoint::serial("/dev/ttyACM0", 115_200)
                .unwrap()
                .to_string(),
            "serial:///dev/ttyACM0@115200"
        );
    }

    #[test]
    fn serial_endpoint_validates_required_fields() {
        assert_eq!(
            Endpoint::serial("", 115_200),
            Err(EndpointError::EmptySerialPath)
        );
        assert_eq!(
            Endpoint::serial("/dev/ttyACM0", 0),
            Err(EndpointError::InvalidBaud)
        );
    }
}
