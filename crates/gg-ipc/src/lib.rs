//! Shared wire types for the `gglinuxd` <-> UI unix-socket protocol.
//!
//! Framing is newline-delimited JSON (one JSON value per line). The client sends
//! [`RequestFrame`] values; the server replies with [`ServerMessage`] values —
//! either a `Response` (correlated by `id`) or an unsolicited `Notification`.

use serde::{Deserialize, Serialize};

/// Broad device category, used by the UI to pick the right panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceKind {
    Keyboard,
    Mouse,
    Headset,
    Other,
}

/// One HID interface exposed by a physical device (each device exposes several).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterfaceInfo {
    pub interface_number: i32,
    pub usage_page: u16,
    pub usage: u16,
    /// hidraw path, e.g. `/dev/hidraw3` (or the platform path hidapi reports).
    pub path: String,
}

/// A logical device (all HID interfaces sharing one vendor/product/serial).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInfo {
    /// Stable identifier: `vid:pid` or `vid:pid:serial`.
    pub id: String,
    pub kind: DeviceKind,
    pub vendor_id: u16,
    pub product_id: u16,
    pub name: String,
    pub serial: Option<String>,
    pub connected: bool,
    pub interfaces: Vec<InterfaceInfo>,
}

/// A request method (client -> server). Adjacently tagged: `{"method":"list_devices"}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum Request {
    /// Return the current device list.
    ListDevices,
    /// Liveness check.
    Ping,
}

/// A framed request: `{"id":1,"req":{"method":"list_devices"}}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestFrame {
    pub id: u64,
    pub req: Request,
}

/// The payload of a [`ServerMessage::Response`]. Internally tagged on `type`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseData {
    Devices { devices: Vec<DeviceInfo> },
    Pong,
    Error { message: String },
}

/// An unsolicited server push. Adjacently tagged: `{"event":"device_added","data":{...}}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", content = "data", rename_all = "snake_case")]
pub enum Event {
    DeviceAdded(DeviceInfo),
    DeviceRemoved { id: String },
    /// Full current list — sent on connect and after a lag/resync.
    DevicesChanged { devices: Vec<DeviceInfo> },
}

/// Anything the server writes to a client. Internally tagged on `kind`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ServerMessage {
    Response { id: u64, result: ResponseData },
    Notification { event: Event },
}

impl ServerMessage {
    pub fn response(id: u64, result: ResponseData) -> Self {
        ServerMessage::Response { id, result }
    }
    pub fn notify(event: Event) -> Self {
        ServerMessage::Notification { event }
    }
    /// Serialize to a single newline-terminated JSON line.
    pub fn to_line(&self) -> serde_json::Result<String> {
        let mut s = serde_json::to_string(self)?;
        s.push('\n');
        Ok(s)
    }
}

/// Default unix-socket path: `$XDG_RUNTIME_DIR/gglinux/gglinuxd.sock`.
pub fn socket_path() -> std::path::PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(format!("/run/user/{}", unsafe { geteuid() })));
    base.join("gglinux").join("gglinuxd.sock")
}

// Avoid a libc dependency in this leaf crate just for one call.
extern "C" {
    fn geteuid() -> u32;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_roundtrips() {
        let line = r#"{"id":7,"req":{"method":"list_devices"}}"#;
        let f: RequestFrame = serde_json::from_str(line).unwrap();
        assert_eq!(f.id, 7);
        assert!(matches!(f.req, Request::ListDevices));
    }

    #[test]
    fn server_messages_are_distinguishable() {
        let resp = ServerMessage::response(1, ResponseData::Pong).to_line().unwrap();
        assert!(resp.contains("\"kind\":\"response\""));
        let note = ServerMessage::notify(Event::DeviceRemoved { id: "1038:1872".into() })
            .to_line()
            .unwrap();
        assert!(note.contains("\"kind\":\"notification\""));
        assert!(note.contains("\"event\":\"device_removed\""));
    }
}
