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

/// One DPI stage. X and Y sensitivity can be set independently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DpiStage {
    pub x: u16,
    pub y: u16,
}

/// Battery reading from a wireless device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatteryState {
    /// Charge percentage, 1..=100.
    pub level: u8,
    pub charging: bool,
}

/// Mouse settings, as written by gglinux.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MouseSettings {
    pub dpi_stages: Vec<DpiStage>,
    /// 0-based index into `dpi_stages`.
    pub active_stage: usize,
    pub polling_hz: u16,
    /// One action name per physical button, in button order.
    pub buttons: Vec<String>,
}

/// What the daemon knows about the mouse.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MouseState {
    pub connected: bool,
    /// `None` while the mouse is asleep or has not reported yet.
    pub battery: Option<BatteryState>,
    /// Settings this daemon has written since it started.
    ///
    /// `None` means we have not written any: the mouse is running whatever is in
    /// its onboard memory. The vendor protocol is write-only — there is no command
    /// to read settings back — so this is the only honest answer available, and it
    /// deliberately does not pretend to reflect the hardware's actual state.
    pub applied: Option<MouseSettings>,
}

/// A request method (client -> server). Adjacently tagged: `{"method":"list_devices"}`.
///
/// The `set_mouse_*` methods address "the connected Rival 3 Wireless Gen 2" without
/// a device id — only one mouse model is supported so far. Addressing gains a device
/// id when a second controllable device lands.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum Request {
    /// Return the current device list.
    ListDevices,
    /// Liveness check.
    Ping,
    /// Battery + whatever settings we have applied.
    GetMouseState,
    /// Set the DPI stages and which one is active.
    SetMouseDpi {
        stages: Vec<DpiStage>,
        /// 0-based.
        active: usize,
        #[serde(default)]
        save: bool,
    },
    SetMousePolling {
        hz: u16,
        #[serde(default)]
        save: bool,
    },
    /// One action name per button, e.g. `["button1", ..., "dpi"]`.
    SetMouseButtons {
        actions: Vec<String>,
        #[serde(default)]
        save: bool,
    },
    /// Commit current settings to the mouse's onboard memory.
    SaveMouse,
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
    Mouse { state: MouseState },
    /// A command was accepted and applied.
    Success,
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
    /// Mouse battery changed (or became unknown when it went to sleep).
    MouseBattery { battery: Option<BatteryState> },
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

    /// Pins the exact JSON a UI has to send for each mouse method.
    #[test]
    fn mouse_requests_parse_from_their_documented_json() {
        let dpi = r#"{"id":1,"req":{"method":"set_mouse_dpi","params":
            {"stages":[{"x":800,"y":800},{"x":1600,"y":1600}],"active":1,"save":true}}}"#;
        match serde_json::from_str::<RequestFrame>(dpi).unwrap().req {
            Request::SetMouseDpi { stages, active, save } => {
                assert_eq!(stages, vec![DpiStage { x: 800, y: 800 }, DpiStage { x: 1600, y: 1600 }]);
                assert_eq!(active, 1);
                assert!(save);
            }
            other => panic!("parsed as {other:?}"),
        }

        let buttons = r#"{"id":2,"req":{"method":"set_mouse_buttons","params":
            {"actions":["button1","button2","button3","button4","button5","dpi"]}}}"#;
        match serde_json::from_str::<RequestFrame>(buttons).unwrap().req {
            Request::SetMouseButtons { actions, save } => {
                assert_eq!(actions.len(), 6);
                // Omitted `save` must default to "do not persist", never to a write
                // into the mouse's onboard memory.
                assert!(!save);
            }
            other => panic!("parsed as {other:?}"),
        }

        for (line, label) in [
            (r#"{"id":3,"req":{"method":"get_mouse_state"}}"#, "get_mouse_state"),
            (r#"{"id":4,"req":{"method":"save_mouse"}}"#, "save_mouse"),
        ] {
            serde_json::from_str::<RequestFrame>(line)
                .unwrap_or_else(|e| panic!("{label} should parse: {e}"));
        }
    }

    #[test]
    fn mouse_state_reports_unwritten_settings_as_null() {
        let state = MouseState {
            connected: true,
            battery: Some(BatteryState { level: 12, charging: false }),
            applied: None,
        };
        let json = serde_json::to_string(&ResponseData::Mouse { state }).unwrap();
        assert!(json.contains(r#""applied":null"#), "{json}");
        assert!(json.contains(r#""level":12"#), "{json}");
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
