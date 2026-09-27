//! Dependency rule: like parsers, this crate never sees GPU or UI; live
//! batches feed the same `IngestSink` path as files.

use std::fmt;
use std::net::SocketAddr;

pub mod live;
pub mod reader;
pub mod recorder;

pub use live::{LiveIngestStats, LiveLink, LiveLinkStatus, LiveStats};
pub use reader::{LinkCounters, LinkReader, LinkState, LinkStats};
pub use recorder::TlogRecorder;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    UdpServer { bind: SocketAddr },
    TcpClient { remote: SocketAddr },
    Serial { path: String, baud: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointKind {
    UdpServer,
    TcpClient,
    Serial,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EndpointError {
    #[error("serial path is required")]
    EmptySerialPath,
    #[error("baud must be greater than zero")]
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
            Self::TcpClient { .. } => EndpointKind::TcpClient,
            Self::Serial { .. } => EndpointKind::Serial,
        }
    }
}

impl EndpointKind {
    pub const ALL: [Self; 3] = [Self::UdpServer, Self::TcpClient, Self::Serial];

    pub const fn label(self) -> &'static str {
        match self {
            Self::UdpServer => "UDP server",
            Self::TcpClient => "TCP client",
            Self::Serial => "Serial",
        }
    }
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UdpServer { bind } => write!(f, "UDP {bind}"),
            Self::TcpClient { remote } => write!(f, "TCP {remote}"),
            Self::Serial { path, baud } => write!(f, "Serial {path}@{baud}"),
        }
    }
}

impl fmt::Display for EndpointKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_errors_keep_existing_messages() {
        let empty = EndpointError::EmptySerialPath;
        assert_eq!(empty.clone(), empty);
        assert_eq!(empty.to_string(), "serial path is required");
        assert!(std::error::Error::source(&empty).is_none());

        let baud = EndpointError::InvalidBaud;
        assert_eq!(baud.to_string(), "baud must be greater than zero");
        assert!(std::error::Error::source(&baud).is_none());
    }

    #[test]
    fn endpoint_kind_labels_cover_all_modes() {
        let labels: Vec<_> = EndpointKind::ALL.iter().map(|kind| kind.label()).collect();
        assert_eq!(labels, vec!["UDP server", "TCP client", "Serial"]);
    }

    #[test]
    fn endpoint_display_is_stable_and_compact() {
        let bind = "0.0.0.0:14550".parse().unwrap();
        let remote = "127.0.0.1:14550".parse().unwrap();
        assert_eq!(
            Endpoint::UdpServer { bind }.to_string(),
            "UDP 0.0.0.0:14550"
        );
        assert_eq!(
            Endpoint::TcpClient { remote }.to_string(),
            "TCP 127.0.0.1:14550"
        );
        assert_eq!(
            Endpoint::serial("/dev/ttyACM0", 115_200)
                .unwrap()
                .to_string(),
            "Serial /dev/ttyACM0@115200"
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
