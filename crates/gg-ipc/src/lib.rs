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

/// Burn-in protection for the headset base station's screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenSettings {
    /// Keep the screen dark, lighting it briefly after events.
    pub protect: bool,
    /// How long the screen stays lit after an event.
    pub lit_seconds: u16,
    /// OLED brightness, or `None` to leave the device's own level alone — it
    /// cannot be read back, so gglinux never assumes one.
    pub brightness: Option<u8>,
}

impl ScreenSettings {
    pub const LIT_SECONDS: std::ops::RangeInclusive<u16> = 3..=600;
    /// There is no "off" level; see `gg_device_headset::proto::OLED_BRIGHTNESS_MIN`.
    pub const BRIGHTNESS: std::ops::RangeInclusive<u8> = 1..=10;

    /// Check ranges, with a message meant for the user.
    pub fn validate(&self) -> Result<(), String> {
        if !Self::LIT_SECONDS.contains(&self.lit_seconds) {
            return Err(format!(
                "screen lit period {} s out of range ({}..={})",
                self.lit_seconds,
                Self::LIT_SECONDS.start(),
                Self::LIT_SECONDS.end()
            ));
        }
        if let Some(level) = self.brightness {
            if !Self::BRIGHTNESS.contains(&level) {
                return Err(format!(
                    "screen brightness {level} out of range ({}..={}) — the lowest level is \
                     still lit, so no brightness turns the screen off",
                    Self::BRIGHTNESS.start(),
                    Self::BRIGHTNESS.end()
                ));
            }
        }
        Ok(())
    }
}

impl Default for ScreenSettings {
    fn default() -> Self {
        ScreenSettings { protect: true, lit_seconds: 10, brightness: None }
    }
}

/// Where the headset is, as the base station reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeadsetPower {
    Offline,
    Charging,
    Online,
    /// Not reported yet, or a value we have not seen.
    Unknown,
}

/// Headset noise control.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoiseMode {
    Off,
    Transparency,
    Anc,
}

/// 2.4 GHz link profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RadioMode {
    Speed,
    Range,
}

/// One headset setting to change: `{"setting":"sidetone","value":2}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "setting", content = "value", rename_all = "snake_case")]
pub enum HeadsetChange {
    NoiseMode(NoiseMode),
    /// 1..=10.
    Transparency(u8),
    /// 0 off, 1 low, 2 medium, 3 high.
    Sidetone(u8),
    /// 1 (muted)..=10.
    MicVolume(u8),
    /// 1..=10.
    MicLed(u8),
    /// 0 (never), 1, 5, 10, 15, 30 or 60.
    AutoOffMinutes(u16),
    RadioMode(RadioMode),
    /// 0 Flat, 1 Bass Boost, 2 Focus, 3 Smiley, 4 Custom.
    EqPreset(u8),
    /// Ten gains in dB, -10.0..=10.0 in 0.5 steps. Selects the Custom preset.
    EqBands(Vec<f32>),
}

/// Headset settings. The first group is read from the base station every
/// second; the second is what gglinux wrote this session, because the device
/// cannot report it. `None` means not known.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HeadsetControls {
    pub noise_mode: Option<NoiseMode>,
    pub transparency: Option<u8>,
    pub mic_led: Option<u8>,
    pub mic_muted: Option<bool>,
    pub auto_off_minutes: Option<u16>,
    pub radio_mode: Option<RadioMode>,
    pub sidetone: Option<u8>,
    pub mic_volume: Option<u8>,
    pub eq_preset: Option<u8>,
    pub eq_bands: Option<Vec<f32>>,
}

/// What the daemon knows about the headset and its base station.
///
/// Unlike [`MouseState`], these are readings: the base station reports them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeadsetState {
    /// Whether the base station is plugged in and open.
    pub connected: bool,
    pub power: HeadsetPower,
    /// Percent; `None` while the headset is off.
    pub headset_battery: Option<u8>,
    /// Percent; `None` when no spare pack is seated.
    pub spare_battery: Option<u8>,
    pub screen: ScreenSettings,
    /// Whether gglinux has the screen blanked right now.
    pub screen_dark: bool,
    pub controls: HeadsetControls,
}

/// A Sonar output channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    Game,
    Chat,
    Media,
    Aux,
}

impl Channel {
    pub const ALL: [Channel; 4] = [Channel::Game, Channel::Chat, Channel::Media, Channel::Aux];

    /// Lower-case name, as used in node names and settings.
    pub fn key(self) -> &'static str {
        match self {
            Channel::Game => "game",
            Channel::Chat => "chat",
            Channel::Media => "media",
            Channel::Aux => "aux",
        }
    }
}

/// A physical output device the channels can play into.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioOutput {
    /// PipeWire `node.name`.
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChannelState {
    pub channel: Channel,
    /// Whether PipeWire currently has this channel's sink.
    pub present: bool,
    /// 0..=1 on the desktop mixer's scale; `None` when absent.
    pub volume: Option<f32>,
}

/// An app playing audio, and the channel its rule sends it to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppStream {
    /// PipeWire node id; changes when the app reopens its stream.
    pub id: u32,
    pub name: String,
    /// `application.process.binary`, the key routing rules match on.
    pub binary: Option<String>,
    pub channel: Channel,
}

/// The shape of an EQ band.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BandKind {
    LowShelf,
    Peaking,
    HighShelf,
}

/// One parametric EQ band.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct EqBand {
    pub kind: BandKind,
    /// Centre (peaking) or corner (shelf) frequency, Hz.
    pub freq: f32,
    pub q: f32,
    /// dB.
    pub gain: f32,
}

/// One channel's EQ.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChannelEq {
    pub channel: Channel,
    pub bands: Vec<EqBand>,
}

/// Headphone correction, shared by every channel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeadphoneEq {
    /// dB applied before the bands (AutoEQ's "Preamp").
    pub preamp: f32,
    /// Low shelf, ten peaking bands, high shelf.
    pub bands: Vec<EqBand>,
    /// What it corrects, e.g. an AutoEQ headphone name.
    pub name: Option<String>,
}

/// Sonar as the daemon sees it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SonarState {
    /// Whether PipeWire's tools answered.
    pub available: bool,
    pub enabled: bool,
    /// `node.name` of the physical output the channels play into.
    pub output: Option<String>,
    pub outputs: Vec<AudioOutput>,
    pub channels: Vec<ChannelState>,
    /// -1.0 all Game … 0.0 both full … 1.0 all Chat.
    pub chatmix: f32,
    pub apps: Vec<AppStream>,
    /// Per-channel EQ, in `Channel::ALL` order.
    pub eq: Vec<ChannelEq>,
    pub headphone: HeadphoneEq,
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
    /// Headset battery, power, and screen settings.
    GetHeadsetState,
    /// Replace the base-station screen settings.
    SetHeadsetScreen { settings: ScreenSettings },
    /// Change one headset setting and save it on the base station.
    SetHeadset { change: HeadsetChange },
    /// Sonar channels, routing, and ChatMix.
    GetSonarState,
    /// Turn Sonar on or off. Restarts PipeWire.
    SetSonarEnabled { enabled: bool },
    /// The physical output the channels play into (a `node.name`).
    SetSonarOutput { name: String },
    /// 0..=1.
    SetChannelVolume { channel: Channel, volume: f32 },
    /// -1.0 all Game … 1.0 all Chat.
    SetChatMix { balance: f32 },
    /// Route an app (by `application.process.binary`) to a channel, and remember it.
    SetAppChannel { binary: String, channel: Channel },
    /// Replace a channel's EQ bands.
    SetChannelEq { channel: Channel, bands: Vec<EqBand> },
    /// Replace the headphone correction.
    SetHeadphoneEq { eq: HeadphoneEq },
    /// Parse an AutoEQ ParametricEQ.txt and make it the headphone correction.
    ImportAutoEq { text: String, name: Option<String> },
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
    Headset { state: HeadsetState },
    Sonar { state: SonarState },
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
    /// Headset state changed: battery, power, connection, or screen.
    HeadsetChanged { state: HeadsetState },
    /// Sonar changed: channels, apps, volumes, or on/off.
    SonarChanged { state: SonarState },
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

    #[test]
    fn headset_requests_parse_from_their_documented_json() {
        let set = r#"{"id":5,"req":{"method":"set_headset_screen","params":
            {"settings":{"protect":true,"lit_seconds":10,"brightness":null}}}}"#;
        match serde_json::from_str::<RequestFrame>(set).unwrap().req {
            Request::SetHeadsetScreen { settings } => assert_eq!(settings, ScreenSettings::default()),
            other => panic!("parsed as {other:?}"),
        }
        serde_json::from_str::<RequestFrame>(r#"{"id":6,"req":{"method":"get_headset_state"}}"#)
            .expect("get_headset_state should parse");
    }

    #[test]
    fn headset_state_serializes_for_the_ui() {
        let state = HeadsetState {
            connected: true,
            power: HeadsetPower::Online,
            headset_battery: Some(12),
            spare_battery: None,
            screen: ScreenSettings::default(),
            screen_dark: true,
            controls: HeadsetControls::default(),
        };
        let json = serde_json::to_string(&ServerMessage::notify(Event::HeadsetChanged { state })).unwrap();
        assert!(json.contains(r#""event":"headset_changed""#), "{json}");
        assert!(json.contains(r#""power":"online""#), "{json}");
        assert!(json.contains(r#""spare_battery":null"#), "{json}");
        assert!(json.contains(r#""screen_dark":true"#), "{json}");
        assert!(json.contains(r#""lit_seconds":10"#), "{json}");
    }

    #[test]
    fn screen_settings_reject_out_of_range_values() {
        let ok = ScreenSettings::default();
        assert!(ok.validate().is_ok());
        assert!(ScreenSettings { lit_seconds: 2, ..ok }.validate().is_err());
        assert!(ScreenSettings { lit_seconds: 601, ..ok }.validate().is_err());
        assert!(ScreenSettings { brightness: Some(0), ..ok }.validate().is_err(), "0 is not off");
        assert!(ScreenSettings { brightness: Some(11), ..ok }.validate().is_err());
        assert!(ScreenSettings { brightness: Some(1), lit_seconds: 600, ..ok }.validate().is_ok());
    }

    #[test]
    fn headset_changes_parse_from_their_documented_json() {
        let cases = [
            (r#"{"setting":"noise_mode","value":"anc"}"#, HeadsetChange::NoiseMode(NoiseMode::Anc)),
            (r#"{"setting":"sidetone","value":2}"#, HeadsetChange::Sidetone(2)),
            (r#"{"setting":"auto_off_minutes","value":0}"#, HeadsetChange::AutoOffMinutes(0)),
            (r#"{"setting":"radio_mode","value":"range"}"#, HeadsetChange::RadioMode(RadioMode::Range)),
            (
                r#"{"setting":"eq_bands","value":[0,0,0,0,0,0,0,0,0,1.5]}"#,
                HeadsetChange::EqBands(vec![0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.5]),
            ),
        ];
        for (json, expected) in cases {
            let line = format!(r#"{{"id":1,"req":{{"method":"set_headset","params":{{"change":{json}}}}}}}"#);
            match serde_json::from_str::<RequestFrame>(&line).unwrap().req {
                Request::SetHeadset { change } => assert_eq!(change, expected, "{json}"),
                other => panic!("{json} parsed as {other:?}"),
            }
        }
    }

    #[test]
    fn unknown_controls_serialize_as_null() {
        let json = serde_json::to_string(&HeadsetControls::default()).unwrap();
        assert!(json.contains(r#""sidetone":null"#), "{json}");
        assert!(json.contains(r#""noise_mode":null"#), "{json}");
        assert!(json.contains(r#""eq_bands":null"#), "{json}");
    }

    #[test]
    fn sonar_requests_parse_from_their_documented_json() {
        for (line, label) in [
            (r#"{"id":1,"req":{"method":"get_sonar_state"}}"#, "get"),
            (r#"{"id":2,"req":{"method":"set_sonar_enabled","params":{"enabled":true}}}"#, "enable"),
            (r#"{"id":4,"req":{"method":"set_chat_mix","params":{"balance":-0.25}}}"#, "chatmix"),
            (r#"{"id":5,"req":{"method":"set_app_channel","params":{"binary":"discord","channel":"chat"}}}"#, "app"),
            (r#"{"id":6,"req":{"method":"set_sonar_output","params":{"name":"alsa_output.x"}}}"#, "output"),
        ] {
            serde_json::from_str::<RequestFrame>(line).unwrap_or_else(|e| panic!("{label}: {e}"));
        }
        let volume = r#"{"id":3,"req":{"method":"set_channel_volume","params":{"channel":"chat","volume":0.5}}}"#;
        match serde_json::from_str::<RequestFrame>(volume).unwrap().req {
            Request::SetChannelVolume { channel, volume } => {
                assert_eq!(channel, Channel::Chat);
                assert_eq!(volume, 0.5);
            }
            other => panic!("parsed as {other:?}"),
        }
    }

    #[test]
    fn channels_have_stable_keys() {
        let keys: Vec<_> = Channel::ALL.iter().map(|c| c.key()).collect();
        assert_eq!(keys, ["game", "chat", "media", "aux"]);
        assert_eq!(serde_json::to_string(&Channel::Media).unwrap(), r#""media""#);
    }

    #[test]
    fn eq_requests_parse_from_their_documented_json() {
        let band = r#"{"kind":"peaking","freq":250.0,"q":1.0,"gain":-3.5}"#;
        let set = format!(
            r#"{{"id":1,"req":{{"method":"set_channel_eq","params":{{"channel":"game","bands":[{band}]}}}}}}"#
        );
        match serde_json::from_str::<RequestFrame>(&set).unwrap().req {
            Request::SetChannelEq { channel, bands } => {
                assert_eq!(channel, Channel::Game);
                assert_eq!(bands, vec![EqBand { kind: BandKind::Peaking, freq: 250.0, q: 1.0, gain: -3.5 }]);
            }
            other => panic!("parsed as {other:?}"),
        }
        for line in [
            r#"{"id":2,"req":{"method":"import_auto_eq","params":{"text":"Preamp: -1 dB","name":"HD 650"}}}"#,
            r#"{"id":3,"req":{"method":"set_headphone_eq","params":{"eq":{"preamp":0.0,"bands":[],"name":null}}}}"#,
        ] {
            serde_json::from_str::<RequestFrame>(line).unwrap_or_else(|e| panic!("{line}: {e}"));
        }
    }

    #[test]
    fn band_kinds_are_snake_case() {
        assert_eq!(serde_json::to_string(&BandKind::LowShelf).unwrap(), r#""low_shelf""#);
        assert_eq!(serde_json::to_string(&BandKind::HighShelf).unwrap(), r#""high_shelf""#);
    }
}
