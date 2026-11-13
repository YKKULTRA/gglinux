// SPDX-License-Identifier: GPL-3.0-or-later
//! Shared wire types for the `gglinuxd` <-> UI unix-socket protocol.
//!
//! Framing is newline-delimited JSON (one JSON value per line). The client sends
//! [`RequestFrame`] values; the server replies with [`ServerMessage`] values —
//! either a `Response` (correlated by `id`) or an unsolicited `Notification`.

use serde::{Deserialize, Serialize};

mod actions;
mod bindings;
mod experimental;
mod lighting;
mod macros;
pub mod mouse_keys;
mod onboard;

pub use actions::{validate as validate_actions, Action, ActionBinding, ActionsState};
pub use bindings::{Bindings, BindingsState, KeyAction, KeyBinding, Layer};
pub use experimental::{Experimental, Feature};
pub use lighting::{IdleLighting, ReflectStatus, Zone, ZoneEffect, ZoneKeys, ZonePreset};
pub use macros::{Macro, MacroBinding, MacroEvent, MacroLibrary, MacroMode, MacrosState};
pub use onboard::{check_slot, OnboardSlot, OnboardSource, OnboardState, SLOTS as ONBOARD_SLOTS};

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

/// The DPI stages written to the mouse, and which one is active.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MouseDpi {
    pub stages: Vec<DpiStage>,
    /// 0-based index into `stages`.
    pub active: usize,
}

/// Mouse settings, as written by gglinux.
///
/// Each part is `None` until it has been written. A part never written is unknown, not its
/// factory default; what the mouse itself reports is in [`MouseState`] (`sleep_secs`,
/// `radio`, `reported_dpi`, `reported_polling_hz`). The buttons and the wheel aren't read
/// back: the mouse has no query for them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MouseSettings {
    pub dpi: Option<MouseDpi>,
    pub polling_hz: Option<u16>,
    /// One action name per physical button, in button order, then the wheel's two (up,
    /// down) once they've been written: 6 names, or 8. A write of six buttons keeps the
    /// wheel's two as they were.
    pub buttons: Option<Vec<String>>,
    /// The sleep timer, in minutes ([`MOUSE_SLEEP_MINUTES`]).
    #[serde(default)]
    pub sleep_minutes: Option<u8>,
    #[serde(default)]
    pub radio: Option<MouseRadio>,
}

/// How the mouse talks to its dongle: GG's High-Efficiency Mode and Wireless Stability
/// switches, as the three states GG sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseRadio {
    /// GG's High-Efficiency Mode: longer battery life.
    HighEfficiency,
    /// Neither switch on.
    Normal,
    /// GG's Wireless Stability: for places with much radio traffic.
    Stable,
}

impl MouseRadio {
    pub const ALL: [MouseRadio; 3] = [
        MouseRadio::Normal,
        MouseRadio::Stable,
        MouseRadio::HighEfficiency,
    ];

    /// The name `ggctl mouse radio` takes.
    pub fn key(self) -> &'static str {
        match self {
            MouseRadio::HighEfficiency => "efficient",
            MouseRadio::Normal => "normal",
            MouseRadio::Stable => "stable",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            MouseRadio::HighEfficiency => "High-Efficiency Mode",
            MouseRadio::Normal => "Normal",
            MouseRadio::Stable => "Wireless Stability",
        }
    }
}

/// The sleep timers the mouse takes, in whole minutes: GG's own range, the only values seen
/// on the wire.
pub const MOUSE_SLEEP_MINUTES: std::ops::RangeInclusive<u8> = 1..=20;

/// The answer to a mouse write while the mouse is asleep. An asleep mouse drops what it's
/// sent, so nothing is sent; the daemon decides from a status read made for that request.
pub const MOUSE_ASLEEP: &str = "the mouse is asleep: move it, then try again";

/// What the daemon knows about the mouse.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MouseState {
    pub connected: bool,
    /// `None` while the mouse is asleep or has not reported yet.
    pub battery: Option<BatteryState>,
    /// Settings this daemon has written since it started.
    ///
    /// `None` means we have not written any: the mouse is running whatever is in
    /// its onboard memory. Most settings can't be read back (rivalcfg has no reads),
    /// so this deliberately does not pretend to reflect the hardware's actual state.
    pub applied: Option<MouseSettings>,
    /// The sleep timer as the mouse reports it, in seconds. Read only while
    /// [`Feature::MousePower`] is on; `None` while it's off, or the mouse is asleep (what
    /// it reported before it fell asleep isn't kept: it can't be checked until it wakes).
    #[serde(default)]
    pub sleep_secs: Option<u16>,
    /// The radio mode as the mouse reports it; read and `None` as [`MouseState::sleep_secs`].
    #[serde(default)]
    pub radio: Option<MouseRadio>,
    /// The DPI stages as the mouse reports them; read and `None` as
    /// [`MouseState::sleep_secs`].
    #[serde(default)]
    pub reported_dpi: Option<MouseDpi>,
    /// The polling rate as the mouse reports it; read and `None` as
    /// [`MouseState::sleep_secs`].
    #[serde(default)]
    pub reported_polling_hz: Option<u16>,
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
        ScreenSettings {
            protect: true,
            lit_seconds: 10,
            brightness: None,
        }
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

/// What a Bluetooth call does to the game sound.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BluetoothCall {
    Nothing,
    /// Game sound 12 dB lower during the call.
    LowerGame,
    MuteGame,
}

/// The base station's output gain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeadsetGain {
    Low,
    High,
}

/// What the base station's line out carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineOut {
    /// The headset's sound, for speakers.
    Speakers,
    /// A mix of main, aux and mic at their own levels ([`LineOutMix`]).
    Stream,
}

/// The base station's line-out stream mix, 0..=[`LINE_OUT_LEVEL_MAX`] each. Not
/// Sonar's [`StreamMix`]: this one is mixed in the base station.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineOutMix {
    pub main: u8,
    pub aux: u8,
    pub mic: u8,
}

/// Highest level in a [`LineOutMix`].
pub const LINE_OUT_LEVEL_MAX: u8 = 100;

/// The base station's home screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HomeScreen {
    Detailed,
    Simple,
}

/// The base station screen's dim timeout choices, in minutes; 0 never dims.
pub const SCREEN_DIM_MINUTES: [u16; 7] = [0, 1, 5, 10, 15, 30, 60];

/// Bluetooth's link to a phone, as the base station reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BluetoothLink {
    Off,
    Connected,
    /// On, with nothing connected.
    NotConnected,
}

/// The 2.4 GHz link between the base station and the headset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeadsetPairing {
    NotPaired,
    /// Paired, but the headset is off or out of reach.
    PairedOffline,
    Connected,
}

/// One headset setting to change: `{"setting":"sidetone","value":2}`.
///
/// The base station's own settings at the end need [`Feature::HeadsetExtras`]
/// ([`HeadsetChange::feature`]).
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
    /// Bluetooth on when the headset powers on.
    BluetoothAtPowerOn(bool),
    BluetoothCall(BluetoothCall),
    Gain(HeadsetGain),
    LineOut(LineOut),
    /// All three levels at once, as the base station takes them.
    LineOutMix(LineOutMix),
    HomeScreen(HomeScreen),
    /// One of [`SCREEN_DIM_MINUTES`].
    ScreenDimMinutes(u16),
}

impl HeadsetChange {
    /// The experimental gate this change needs, if any.
    pub fn feature(&self) -> Option<Feature> {
        match self {
            HeadsetChange::BluetoothAtPowerOn(_)
            | HeadsetChange::BluetoothCall(_)
            | HeadsetChange::Gain(_)
            | HeadsetChange::LineOut(_)
            | HeadsetChange::LineOutMix(_)
            | HeadsetChange::HomeScreen(_)
            | HeadsetChange::ScreenDimMinutes(_) => Some(Feature::HeadsetExtras),
            HeadsetChange::NoiseMode(_)
            | HeadsetChange::Transparency(_)
            | HeadsetChange::Sidetone(_)
            | HeadsetChange::MicVolume(_)
            | HeadsetChange::MicLed(_)
            | HeadsetChange::AutoOffMinutes(_)
            | HeadsetChange::RadioMode(_)
            | HeadsetChange::EqPreset(_)
            | HeadsetChange::EqBands(_) => None,
        }
    }
}

/// Headset settings, as the base station reports them where it does (its `b0` and
/// `20` replies each second; the home screen and the dim timeout from its `80`
/// reply, asked only while [`Feature::HeadsetExtras`] is on), else as gglinux wrote
/// them this session. `None` means not known.
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
    #[serde(default)]
    pub bluetooth_at_power_on: Option<bool>,
    #[serde(default)]
    pub bluetooth_call: Option<BluetoothCall>,
    /// Whether Bluetooth is on now. Read only.
    #[serde(default)]
    pub bluetooth_on: Option<bool>,
    /// Read only.
    #[serde(default)]
    pub bluetooth_link: Option<BluetoothLink>,
    /// The 2.4 GHz link to the headset. Read only.
    #[serde(default)]
    pub pairing: Option<HeadsetPairing>,
    #[serde(default)]
    pub gain: Option<HeadsetGain>,
    #[serde(default)]
    pub line_out: Option<LineOut>,
    #[serde(default)]
    pub line_out_mix: Option<LineOutMix>,
    #[serde(default)]
    pub home_screen: Option<HomeScreen>,
    #[serde(default)]
    pub screen_dim_minutes: Option<u16>,
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

/// A physical audio device: an output the channels play into, or an input the mic reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioDevice {
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

/// Processing on the virtual mic.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MicSettings {
    /// RNNoise noise suppression.
    pub denoise: bool,
    /// RNNoise's voice-detection threshold, 0..=95 %: higher cuts more non-voice sound.
    pub denoise_strength: f32,
    /// Noise gate: silence below `gate_threshold_db`.
    pub gate: bool,
    /// -80..=-10 dB.
    pub gate_threshold_db: f32,
    /// Compressor (4:1 above -18 dB) plus makeup gain.
    pub compressor: bool,
    /// 0..=18 dB.
    pub makeup_db: f32,
}

impl Default for MicSettings {
    fn default() -> Self {
        MicSettings {
            denoise: true,
            denoise_strength: 50.0,
            gate: false,
            gate_threshold_db: -45.0,
            compressor: false,
            makeup_db: 6.0,
        }
    }
}

/// The virtual mic as the daemon sees it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MicState {
    /// Whether the mic chain's plugins are installed.
    pub available: bool,
    /// Whether PipeWire has the virtual mic right now.
    pub present: bool,
    /// `node.name` of the physical mic it reads.
    pub input: Option<String>,
    pub inputs: Vec<AudioDevice>,
    /// 0..=1 on the mixer's scale; `None` when absent.
    pub volume: Option<f32>,
    pub settings: MicSettings,
}

/// Streamer mode: a second mix of the channels, on the "gglinux Stream" device.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StreamMix {
    pub enabled: bool,
    /// Whether PipeWire has the stream device now.
    pub present: bool,
    /// Per-channel stream level, 0..=1, in `Channel::ALL` order.
    pub volumes: Vec<f32>,
    /// The stream's master level, 0..=1.
    pub master: f32,
}

/// How loud Smart Volume aims a channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SmartLevel {
    Quiet,
    #[default]
    Balanced,
    Loud,
}

/// Smart Volume on one channel: quiet sounds come up, loud ones are held down.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SmartVolume {
    /// Off by default.
    pub enabled: bool,
    pub level: SmartLevel,
}

/// Smart Volume across the channels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SmartVolumeState {
    /// Whether the channels carry Smart Volume's stage, so it can be switched.
    pub available: bool,
    /// Why not, for the Sonar tab: swh-plugins is missing, PipeWire refused the
    /// stage, or the channels were made before swh-plugins was installed.
    pub reason: Option<String>,
    /// Per channel, in `Channel::ALL` order.
    pub settings: Vec<SmartVolume>,
}

/// Sonar as the daemon sees it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SonarState {
    /// Whether PipeWire's tools answered.
    pub available: bool,
    pub enabled: bool,
    /// `node.name` of the physical output the channels play into.
    pub output: Option<String>,
    pub outputs: Vec<AudioDevice>,
    pub channels: Vec<ChannelState>,
    /// -1.0 all Game … 0.0 both full … 1.0 all Chat.
    pub chatmix: f32,
    pub apps: Vec<AppStream>,
    /// Per-channel EQ, in `Channel::ALL` order.
    pub eq: Vec<ChannelEq>,
    pub headphone: HeadphoneEq,
    pub mic: MicState,
    pub stream: StreamMix,
    pub smart_volume: SmartVolumeState,
}

/// An sRGB colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

/// What the keyboard lighting does.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Effect {
    /// Each key its own colour, from [`Lighting::keys`].
    Static,
    /// Every key pulses one colour, one breath per `period_s`.
    Breathing { color: Rgb, period_s: f32 },
    /// A rainbow moving across the keyboard, one pass per `period_s`.
    Wave { period_s: f32 },
    /// Every key cycles the rainbow together, once per `period_s`.
    Spectrum { period_s: f32 },
    /// The keys as a spectrum analyser of whatever is playing.
    Visualizer { sensitivity: f32 },
}

/// One key's static colour, by HID usage code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyColor {
    pub code: u8,
    pub color: Rgb,
}

/// How a key press shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReactiveMode {
    /// The pressed key lights and fades back.
    Fade,
    /// A ring spreads out from the pressed key.
    Ripple,
}

/// Key presses drawn over the base effect.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Reactive {
    pub enabled: bool,
    pub mode: ReactiveMode,
    pub color: Rgb,
    /// How long one press shows, in seconds.
    pub duration_s: f32,
}

impl Reactive {
    pub const DURATION_S: std::ops::RangeInclusive<f32> = 0.1..=3.0;
}

impl Default for Reactive {
    fn default() -> Self {
        Reactive {
            enabled: false,
            mode: ReactiveMode::Fade,
            color: Rgb {
                r: 255,
                g: 255,
                b: 255,
            },
            duration_s: 0.6,
        }
    }
}

/// A key lit while the default microphone is muted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MicMuteLight {
    pub enabled: bool,
    /// HID code of the key; must be on the keyboard.
    pub key: u8,
    pub color: Rgb,
}

impl Default for MicMuteLight {
    fn default() -> Self {
        MicMuteLight {
            enabled: false,
            key: 0xFB,
            color: Rgb { r: 255, g: 0, b: 0 },
        }
    }
}

/// Keyboard lighting as gglinux runs it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Lighting {
    /// `false` leaves the lighting to the keyboard's own onboard profile.
    pub enabled: bool,
    pub effect: Effect,
    /// Static colours; keys not listed are dark.
    pub keys: Vec<KeyColor>,
    /// 0..=100 %, applied to every frame.
    pub brightness: u8,
    /// Presses drawn over the effect; absent in files saved before it existed.
    #[serde(default)]
    pub reactive: Reactive,
    /// Absent in files saved before it existed.
    #[serde(default)]
    pub mic_mute: MicMuteLight,
    /// The zone stack, bottom first. `None` in lighting saved before zones existed, which
    /// [`Lighting::stack`] reads as `effect` over every key; `effect` counts only then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zones: Option<Vec<Zone>>,
    /// The scene drawn while nobody is at the computer. Absent in files saved before it
    /// existed.
    #[serde(default)]
    pub idle: IdleLighting,
}

impl Lighting {
    pub const PERIOD_S: std::ops::RangeInclusive<f32> = 0.5..=60.0;
    pub const SENSITIVITY: std::ops::RangeInclusive<f32> = 0.25..=4.0;

    /// Check ranges, with a message meant for the user.
    pub fn validate(&self) -> Result<(), String> {
        if self.brightness > 100 {
            return Err(format!(
                "brightness {}% out of range (0..=100)",
                self.brightness
            ));
        }
        let period = match self.effect {
            Effect::Static => None,
            Effect::Breathing { period_s, .. }
            | Effect::Wave { period_s }
            | Effect::Spectrum { period_s } => Some(period_s),
            Effect::Visualizer { .. } => None,
        };
        if let Some(p) = period {
            if !(p.is_finite() && Self::PERIOD_S.contains(&p)) {
                return Err(format!("effect period {p} s out of range (0.5..=60)"));
            }
        }
        if let Effect::Visualizer { sensitivity } = self.effect {
            if !(sensitivity.is_finite() && Self::SENSITIVITY.contains(&sensitivity)) {
                return Err(format!(
                    "visualizer sensitivity {sensitivity} out of range (0.25..=4)"
                ));
            }
        }
        let d = self.reactive.duration_s;
        if !(d.is_finite() && Reactive::DURATION_S.contains(&d)) {
            return Err(format!("reactive duration {d} s out of range (0.1..=3)"));
        }
        self.validate_zones()
    }
}

impl Default for Lighting {
    fn default() -> Self {
        Lighting {
            enabled: false,
            effect: Effect::Wave { period_s: 4.0 },
            keys: Vec::new(),
            brightness: 100,
            reactive: Reactive::default(),
            mic_mute: MicMuteLight::default(),
            zones: None,
            idle: IdleLighting::default(),
        }
    }
}

/// What the keyboard's screen shows.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Screen {
    /// The keyboard's own menus: gglinux leaves the screen alone.
    #[default]
    Keyboard,
    /// One to five lines of text, drawn as big as they fit.
    Text { text: String },
    /// The time, with seconds, a 24-hour clock and the date under it as chosen.
    Clock {
        seconds: bool,
        h24: bool,
        date: bool,
    },
    /// A 128 × 40 picture: 640 bytes, 16 a row from the top, the leftmost pixel in each
    /// byte's high bit.
    Image { pixels: Vec<u8> },
    /// An animation: pictures laid out as [`Screen::Image`]'s, each shown for its delay, looping.
    Animation {
        frames: Vec<Vec<u8>>,
        delays_ms: Vec<u16>,
    },
    /// Live readings of the computer, one to four of them.
    Monitor { items: Vec<MonitorItem> },
}

/// One reading on the System Monitor screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MonitorItem {
    /// CPU use, %.
    Cpu,
    /// CPU package temperature.
    CpuTemp,
    /// Memory in use.
    Ram,
    /// Graphics card use, % (shown as idle while the card sleeps: it is never woken to ask).
    Gpu,
    /// Graphics card temperature, under the same rule.
    GpuTemp,
    /// The time.
    Clock,
}

impl Screen {
    pub const MAX_LINES: usize = 5;
    pub const MAX_COLUMNS: usize = 16;
    pub const IMAGE_BYTES: usize = 640;
    pub const MAX_FRAMES: usize = 256;
    pub const FRAME_DELAY_MS: std::ops::RangeInclusive<u16> = 100..=10_000;
    pub const MAX_MONITOR_ITEMS: usize = 4;

    /// Check what the screen can show, with a message meant for the user.
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Screen::Text { text } => {
                let lines: Vec<&str> = text.lines().collect();
                if lines.len() > Self::MAX_LINES {
                    return Err(format!(
                        "the screen holds {} lines of text, not {}",
                        Self::MAX_LINES,
                        lines.len()
                    ));
                }
                if let Some(long) = lines.iter().find(|l| l.chars().count() > Self::MAX_COLUMNS) {
                    return Err(format!(
                        "\"{long}\" is longer than the {} characters a line holds",
                        Self::MAX_COLUMNS
                    ));
                }
                if let Some(c) = text
                    .chars()
                    .find(|c| !(c.is_ascii_graphic() || *c == ' ' || *c == '\n'))
                {
                    return Err(format!(
                        "the screen's font has no '{c}': use plain ASCII letters, digits and punctuation"
                    ));
                }
                Ok(())
            }
            Screen::Image { pixels } if pixels.len() != Self::IMAGE_BYTES => Err(format!(
                "a screen picture is {} bytes, not {}",
                Self::IMAGE_BYTES,
                pixels.len()
            )),
            Screen::Animation { frames, delays_ms } => {
                if frames.is_empty() || frames.len() > Self::MAX_FRAMES {
                    return Err(format!(
                        "an animation has 1 to {} pictures, not {}",
                        Self::MAX_FRAMES,
                        frames.len()
                    ));
                }
                if delays_ms.len() != frames.len() {
                    return Err("every picture of an animation needs its own delay".into());
                }
                if let Some(f) = frames.iter().find(|f| f.len() != Self::IMAGE_BYTES) {
                    return Err(format!(
                        "a screen picture is {} bytes, not {}",
                        Self::IMAGE_BYTES,
                        f.len()
                    ));
                }
                if let Some(d) = delays_ms.iter().find(|d| !Self::FRAME_DELAY_MS.contains(d)) {
                    return Err(format!(
                        "a picture shows for 0.1 to 10 seconds, not {} ms",
                        d
                    ));
                }
                Ok(())
            }
            Screen::Monitor { items } => {
                if items.is_empty() || items.len() > Self::MAX_MONITOR_ITEMS {
                    return Err(format!(
                        "the monitor shows 1 to {} readings, not {}",
                        Self::MAX_MONITOR_ITEMS,
                        items.len()
                    ));
                }
                let mut seen = Vec::new();
                for i in items {
                    if seen.contains(i) {
                        return Err(format!("{i:?} is on the monitor twice"));
                    }
                    seen.push(*i);
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

/// Burn-in care for the keyboard's screen. An OLED pixel wears while it's lit, so one
/// picture held for hours leaves its ghost.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScreenCare {
    /// Move gglinux's picture a pixel at a time, within two pixels of where it's drawn,
    /// every couple of minutes, so no pixel stays lit for hours.
    pub shift: bool,
    /// Turn gglinux's picture dark while nobody is at the computer: the session locked,
    /// another one switched to, the computer going to sleep, or nobody logged in.
    pub dark_when_away: bool,
    /// Turn the keyboard's own screen dark then too. This draws on a screen gglinux
    /// otherwise leaves alone, and hands it back when someone's there again.
    pub own_screen_too: bool,
}

impl Default for ScreenCare {
    fn default() -> Self {
        ScreenCare {
            shift: true,
            dark_when_away: true,
            own_screen_too: false,
        }
    }
}

/// How a Rapid Tap pair settles both keys held at once. What each does is as documented
/// by others; it hasn't been checked on hardware.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TapMode {
    /// The key pressed last counts.
    LastWins,
    /// Neither counts while both are held.
    Neither,
    /// The pair's first key always wins.
    FirstWins,
    /// The pair's second key always wins.
    SecondWins,
}

/// Two opposing keys for Rapid Tap (SOCD), such as A and D, by HID code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TapPair {
    pub a: u8,
    pub b: u8,
    pub mode: TapMode,
    /// Send both keys rather than holding the losing one back.
    #[serde(default)]
    pub report_both: bool,
}

/// One key's own actuation point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyActuation {
    pub code: u8,
    /// Tenths of a millimetre (1..=40).
    pub actuation: u8,
}

/// Rapid Trigger: a key resets as soon as it rises a little, instead of at its actuation
/// point, and fires again as soon as it goes down that much.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RapidTrigger {
    pub enabled: bool,
    /// The keys it's on for, by HID code; empty means every adjustable key.
    #[serde(default)]
    pub keys: Vec<u8>,
    /// How far a key moves to reset or fire again, in tenths of a millimetre (1..=20).
    pub sensitivity: u8,
}

/// GG's Protection Mode: a protected key is guarded against being set off by accident
/// while a key next to it is pressed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Protection {
    pub enabled: bool,
    /// The keys it's on for, by HID code; empty means every adjustable key.
    #[serde(default)]
    pub keys: Vec<u8>,
}

impl Default for RapidTrigger {
    fn default() -> Self {
        RapidTrigger {
            enabled: false,
            keys: Vec::new(),
            sensitivity: 2,
        }
    }
}

/// Rapid Tap (SOCD): pairs of opposing keys the keyboard settles itself.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RapidTap {
    pub enabled: bool,
    #[serde(default)]
    pub pairs: Vec<TapPair>,
}

/// The Apex Pro TKL Gen 3's adjustable switches as gglinux sets them. The keyboard keeps
/// them in RAM only, so the daemon sends them again every time it connects. With `enabled`
/// off it sends nothing, and the keyboard keeps what it has until it loses power, when its
/// onboard profile comes back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Switches {
    pub enabled: bool,
    /// The actuation point of every key not in `keys`, in tenths of a millimetre (1..=40).
    pub actuation: u8,
    /// Keys with an actuation point of their own.
    #[serde(default)]
    pub keys: Vec<KeyActuation>,
    #[serde(default)]
    pub rapid_trigger: RapidTrigger,
    #[serde(default)]
    pub rapid_tap: RapidTap,
    /// Protection Mode. Needs [`Feature::KeyboardProtection`].
    #[serde(default)]
    pub protection: Protection,
    /// Keys with a second (deep-press) actuation point, deeper than their first; what they do
    /// there is their binding on [`Layer::Second`]. Needs [`Feature::KeyboardBindings`].
    #[serde(default)]
    pub second: Vec<KeyActuation>,
}

impl Switches {
    pub const ACTUATION: std::ops::RangeInclusive<u8> = 1..=40;
    pub const SENSITIVITY: std::ops::RangeInclusive<u8> = 1..=20;
    pub const MAX_TAP_PAIRS: usize = 10;

    /// Check ranges and pairs, with a message meant for the user. Whether each key has an
    /// adjustable switch is the daemon's to check: it knows the keyboard.
    pub fn validate(&self) -> Result<(), String> {
        let mm = |tenths: u8| format!("{:.1} mm", f32::from(tenths) / 10.0);
        if !Self::ACTUATION.contains(&self.actuation) {
            return Err(format!(
                "actuation point {} out of range (0.1 to 4.0 mm)",
                mm(self.actuation)
            ));
        }
        let mut seen = Vec::new();
        for k in &self.keys {
            if !Self::ACTUATION.contains(&k.actuation) {
                return Err(format!(
                    "actuation point {} of key {:#04x} out of range (0.1 to 4.0 mm)",
                    mm(k.actuation),
                    k.code
                ));
            }
            if seen.contains(&k.code) {
                return Err(format!("key {:#04x} has two actuation points", k.code));
            }
            seen.push(k.code);
        }
        let s = self.rapid_trigger.sensitivity;
        if !Self::SENSITIVITY.contains(&s) {
            return Err(format!(
                "Rapid Trigger distance {} out of range (0.1 to 2.0 mm)",
                mm(s)
            ));
        }
        let pairs = &self.rapid_tap.pairs;
        if pairs.len() > Self::MAX_TAP_PAIRS {
            return Err(format!(
                "at most {} Rapid Tap pairs, not {}",
                Self::MAX_TAP_PAIRS,
                pairs.len()
            ));
        }
        let mut in_pairs = Vec::new();
        for p in pairs {
            if p.a == p.b {
                return Err("a Rapid Tap pair needs two different keys".into());
            }
            for code in [p.a, p.b] {
                if in_pairs.contains(&code) {
                    return Err(format!("key {code:#04x} is in two Rapid Tap pairs"));
                }
                in_pairs.push(code);
            }
        }
        let mut seen = Vec::new();
        for k in &self.second {
            if !Self::ACTUATION.contains(&k.actuation) {
                return Err(format!(
                    "second actuation point {} of key {:#04x} out of range (0.1 to 4.0 mm)",
                    mm(k.actuation),
                    k.code
                ));
            }
            if k.actuation <= self.actuation_of(k.code) {
                return Err(format!(
                    "the second actuation point of key {:#04x} ({}) must be deeper than its first ({})",
                    k.code,
                    mm(k.actuation),
                    mm(self.actuation_of(k.code))
                ));
            }
            if seen.contains(&k.code) {
                return Err(format!(
                    "key {:#04x} has two second actuation points",
                    k.code
                ));
            }
            seen.push(k.code);
        }
        Ok(())
    }

    /// The actuation point of `code`, in tenths of a millimetre.
    pub fn actuation_of(&self, code: u8) -> u8 {
        self.keys
            .iter()
            .find(|k| k.code == code)
            .map_or(self.actuation, |k| k.actuation)
    }

    /// Whether `code` has Rapid Trigger.
    pub fn rapid_trigger_on(&self, code: u8) -> bool {
        let rt = &self.rapid_trigger;
        rt.enabled && (rt.keys.is_empty() || rt.keys.contains(&code))
    }

    /// Whether `code` has Protection Mode.
    pub fn protection_on(&self, code: u8) -> bool {
        let p = &self.protection;
        p.enabled && (p.keys.is_empty() || p.keys.contains(&code))
    }
}

impl Default for Switches {
    fn default() -> Self {
        Switches {
            enabled: false,
            actuation: 15,
            keys: Vec::new(),
            rapid_trigger: RapidTrigger::default(),
            rapid_tap: RapidTap::default(),
            protection: Protection::default(),
            second: Vec::new(),
        }
    }
}

/// A key on the keyboard picture, in key units (a standard key is 1 × 1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeyInfo {
    pub code: u8,
    pub label: String,
    pub x: f32,
    pub y: f32,
    pub w: f32,
}

/// Whether the daemon is reading key presses for reactive lighting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReactiveStatus {
    /// Not reading: reactive or lighting is off, or the keyboard is away.
    #[default]
    Off,
    Reading,
    /// The typing interfaces can't be opened: the optional udev rule isn't installed.
    NoAccess,
}

/// The keyboard as the daemon sees it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeyboardState {
    pub connected: bool,
    /// Every key and where it is, for drawing.
    pub layout: Vec<KeyInfo>,
    pub lighting: Lighting,
    pub reactive: ReactiveStatus,
    /// The idle scene is drawn now, as nobody has been at the computer for a while.
    #[serde(default)]
    pub lighting_idle: bool,
    /// Whether Reflect is looking at the screen.
    #[serde(default)]
    pub reflect: ReflectStatus,
    /// The keys with adjustable switches, by HID code.
    #[serde(default)]
    pub analog_keys: Vec<u8>,
    #[serde(default)]
    pub switches: Switches,
    /// Why the keyboard didn't take the switch settings, if it didn't.
    #[serde(default)]
    pub switches_error: Option<String>,
    #[serde(default)]
    pub screen: Screen,
    /// Why the keyboard didn't take the screen, if it didn't.
    #[serde(default)]
    pub screen_error: Option<String>,
    #[serde(default)]
    pub screen_care: ScreenCare,
    /// The screen is dark right now because nobody is at the computer.
    #[serde(default)]
    pub screen_resting: bool,
}

/// A game that has talked to the GameSense server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameInfo {
    pub name: String,
    pub display_name: Option<String>,
    pub developer: Option<String>,
    /// `false` keeps the game's events off the keyboard.
    pub enabled: bool,
    /// Heard from within its timeout, so drawing now.
    pub active: bool,
    pub events: Vec<String>,
}

/// The GameSense server as the daemon runs it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct GameSenseState {
    /// Off by default: turning it on writes into Steam's Proton prefixes.
    pub enabled: bool,
    /// `127.0.0.1:<port>` while the server runs.
    pub address: Option<String>,
    /// How many Proton prefixes hold gglinux's coreProps.json.
    pub prefixes: u32,
    pub games: Vec<GameInfo>,
}

/// A saved keyboard look.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    pub lighting: Lighting,
}

/// While a process with this name runs, use this profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameRule {
    pub process: String,
    pub profile: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ProfilesState {
    pub profiles: Vec<String>,
    pub rules: Vec<GameRule>,
    /// The rule applied right now, if a game from the list is running.
    pub active: Option<GameRule>,
    /// Process names running now, for picking rules. Only in answers, not in events.
    pub running: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipQuality {
    Medium,
    High,
    VeryHigh,
    Ultra,
}

/// The instant-replay recorder's settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClipsSettings {
    /// Off by default: while on, the screen is kept in a rolling buffer.
    pub enabled: bool,
    /// How much a saved clip holds.
    pub seconds: u32,
    pub fps: u32,
    pub quality: ClipQuality,
    pub desktop_audio: bool,
    pub mic: bool,
    /// Absolute folder; `None` means `~/Videos/gglinux`.
    pub dir: Option<String>,
    /// Record only while a program from the profiles' game rules runs.
    pub games_only: bool,
}

impl Default for ClipsSettings {
    fn default() -> Self {
        ClipsSettings {
            enabled: false,
            seconds: 30,
            fps: 60,
            quality: ClipQuality::High,
            desktop_audio: true,
            mic: false,
            dir: None,
            games_only: false,
        }
    }
}

impl ClipsSettings {
    pub const SECONDS: std::ops::RangeInclusive<u32> = 5..=300;
    pub const FPS: std::ops::RangeInclusive<u32> = 15..=240;

    pub fn validate(&self) -> Result<(), String> {
        if !Self::SECONDS.contains(&self.seconds) {
            return Err(format!(
                "clip length {} s out of range (5..=300)",
                self.seconds
            ));
        }
        if !Self::FPS.contains(&self.fps) {
            return Err(format!("{} fps out of range (15..=240)", self.fps));
        }
        if let Some(dir) = &self.dir {
            if !std::path::Path::new(dir).is_absolute() {
                return Err(format!(
                    "the clips folder must be an absolute path, not {dir:?}"
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipInfo {
    pub name: String,
    pub bytes: u64,
    /// Seconds since the Unix epoch.
    pub modified: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ClipsState {
    /// `gpu-screen-recorder` is installed.
    pub available: bool,
    pub enabled: bool,
    /// The recorder is running now.
    pub recording: bool,
    pub settings: ClipsSettings,
    /// The folder clips go to.
    pub dir: String,
    pub clips: Vec<ClipInfo>,
    /// The last thing that went wrong, for the UI.
    pub error: Option<String>,
    /// The hotkey KDE bound, if any.
    pub hotkey: Option<String>,
    /// Why there's no hotkey, once binding it has failed.
    pub hotkey_error: Option<String>,
    /// The game from the rules that's running now, if any.
    pub game: Option<String>,
    /// On and games-only, with no game from the rules running: the recorder waits.
    pub waiting: bool,
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
    /// One action name per button, e.g. `["button1", ..., "dpi"]`: six, or eight with the
    /// wheel's two after them (`[..., "dpi", "button4", "scrolldown"]`). With six the wheel
    /// is left alone. The wheel takes only the actions a capture shows
    /// ([`mouse_keys::WHEEL`]) and anything but its factory pair needs
    /// [`Feature::MouseWheel`]. Neither buttons nor wheel can be read back.
    SetMouseButtons {
        actions: Vec<String>,
        #[serde(default)]
        save: bool,
    },
    /// Commit current settings to the mouse's onboard memory.
    SaveMouse,
    /// Set the sleep timer ([`MOUSE_SLEEP_MINUTES`]) and save it, as GG does: unsaved, the
    /// mouse would forget it when it next sleeps. The save also commits the other settings
    /// the mouse is running to its onboard memory. Needs [`Feature::MousePower`].
    SetMouseSleep {
        minutes: u8,
    },
    /// Set how the mouse talks to its dongle, at once as GG does. Needs
    /// [`Feature::MousePower`].
    SetMouseRadio {
        radio: MouseRadio,
        #[serde(default)]
        save: bool,
    },
    /// Headset battery, power, and screen settings.
    GetHeadsetState,
    /// Replace the base-station screen settings.
    SetHeadsetScreen {
        settings: ScreenSettings,
    },
    /// Change one headset setting and save it on the base station.
    SetHeadset {
        change: HeadsetChange,
    },
    /// Sonar channels, routing, and ChatMix.
    GetSonarState,
    /// Turn Sonar on or off. Restarts PipeWire.
    SetSonarEnabled {
        enabled: bool,
    },
    /// The physical output the channels play into (a `node.name`).
    SetSonarOutput {
        name: String,
    },
    /// 0..=1.
    SetChannelVolume {
        channel: Channel,
        volume: f32,
    },
    /// -1.0 all Game … 1.0 all Chat.
    SetChatMix {
        balance: f32,
    },
    /// Route an app (by `application.process.binary`) to a channel, and remember it.
    SetAppChannel {
        binary: String,
        channel: Channel,
    },
    /// Replace a channel's EQ bands.
    SetChannelEq {
        channel: Channel,
        bands: Vec<EqBand>,
    },
    /// Replace the headphone correction.
    SetHeadphoneEq {
        eq: HeadphoneEq,
    },
    /// Parse an AutoEQ ParametricEQ.txt and make it the headphone correction.
    ImportAutoEq {
        text: String,
        name: Option<String>,
    },
    /// The physical mic the virtual mic reads (a source `node.name`).
    SetMicInput {
        name: String,
    },
    SetMicSettings {
        settings: MicSettings,
    },
    /// 0..=1.
    SetMicVolume {
        volume: f32,
    },
    /// Turn streamer mode on or off (restarts PipeWire while Sonar is on).
    SetStreamer {
        enabled: bool,
    },
    /// A channel's level in the stream mix, 0..=1.
    SetStreamVolume {
        channel: Channel,
        volume: f32,
    },
    /// The stream mix's master level, 0..=1.
    SetStreamMaster {
        volume: f32,
    },
    /// Turn Smart Volume on or off for a channel, or change its level. Live: no restart.
    SetSmartVolume {
        channel: Channel,
        settings: SmartVolume,
    },
    /// Keyboard connection, layout and lighting.
    GetKeyboardState,
    /// Replace the keyboard lighting.
    SetLighting {
        lighting: Lighting,
    },
    /// Replace the keyboard's switch settings (actuation, Rapid Trigger, Rapid Tap).
    SetSwitches {
        switches: Switches,
    },
    /// Replace what the keyboard's screen shows.
    SetScreen {
        screen: Screen,
    },
    /// Replace the keyboard screen's burn-in care.
    SetScreenCare {
        care: ScreenCare,
    },
    GetGamesenseState,
    /// Start or stop the GameSense server (and its files in Proton prefixes).
    SetGamesenseEnabled {
        enabled: bool,
    },
    /// Let one game drive the keyboard, or not.
    SetGameEnabled {
        game: String,
        enabled: bool,
    },
    RemoveGame {
        game: String,
    },
    /// Play the built-in test game: an F-key health bar.
    GamesenseTest,
    GetProfiles,
    /// Save the current lighting under this name.
    SaveProfile {
        name: String,
    },
    ApplyProfile {
        name: String,
    },
    DeleteProfile {
        name: String,
    },
    SetGameRules {
        rules: Vec<GameRule>,
    },
    GetClipsState,
    SetClipsSettings {
        settings: ClipsSettings,
    },
    /// Save the replay buffer as a clip.
    SaveClip,
    DeleteClip {
        name: String,
    },
    /// Which parts of gglinux run.
    GetParts,
    /// Save which parts run. A change restarts the daemon, which answers first.
    SetParts {
        parts: Parts,
    },
    /// Which experimental (not yet hardware-tried) features are on.
    GetExperimental,
    /// Turn experimental features on or off.
    SetExperimental {
        experimental: Experimental,
    },
    /// The keyboard bindings gglinux applies, and the keyboard's own as last read.
    GetBindings,
    /// Read the keyboard's bindings now, all three layers. Needs [`Feature::KeyboardBindings`].
    ReadBindings,
    /// Replace the bindings gglinux applies; `enabled` false sends nothing more (what was sent
    /// stays until the keyboard is unplugged).
    SetBindings {
        bindings: Bindings,
        enabled: bool,
    },
    /// The keyboard's onboard slots as last read.
    GetOnboard,
    /// Read one slot (`None`: all five) and back it up to a file. Needs
    /// [`Feature::KeyboardOnboard`].
    ReadOnboard {
        slot: Option<u8>,
    },
    /// Make the keyboard run one of its slots.
    LoadOnboard {
        slot: u8,
    },
    /// Write a slot, as one checked transaction that restores the backup on any mismatch.
    WriteOnboard {
        slot: u8,
        from: OnboardSource,
    },
    /// The actions on the spare keys F13–F24.
    GetActions,
    SetActions {
        bindings: Vec<ActionBinding>,
    },
    /// The keyboard macros and the keys that play them.
    GetMacros,
    /// Replace the macros and their keys. Saved; nothing is sent to the keyboard. Needs
    /// [`Feature::KeyboardMacros`].
    SetMacros {
        library: MacroLibrary,
    },
    /// Write the macros and their keys into onboard slot `slot` (0..=4), as one checked
    /// transaction like [`Request::WriteOnboard`]. Needs [`Feature::KeyboardMacros`] and
    /// [`Feature::KeyboardOnboard`].
    WriteMacros {
        slot: u8,
    },
}

impl Request {
    /// The part of gglinux that answers this request; `None` for the daemon itself.
    pub fn part(&self) -> Option<Part> {
        use Request as R;
        Some(match self {
            R::ListDevices
            | R::Ping
            | R::GetParts
            | R::SetParts { .. }
            | R::GetExperimental
            | R::SetExperimental { .. }
            | R::GetActions
            | R::SetActions { .. } => return None,
            R::GetMouseState
            | R::SetMouseDpi { .. }
            | R::SetMousePolling { .. }
            | R::SetMouseButtons { .. }
            | R::SaveMouse
            | R::SetMouseSleep { .. }
            | R::SetMouseRadio { .. } => Part::Mouse,
            R::GetHeadsetState | R::SetHeadsetScreen { .. } | R::SetHeadset { .. } => Part::Headset,
            R::GetSonarState
            | R::SetSonarEnabled { .. }
            | R::SetSonarOutput { .. }
            | R::SetChannelVolume { .. }
            | R::SetChatMix { .. }
            | R::SetAppChannel { .. }
            | R::SetChannelEq { .. }
            | R::SetHeadphoneEq { .. }
            | R::ImportAutoEq { .. }
            | R::SetMicInput { .. }
            | R::SetMicSettings { .. }
            | R::SetMicVolume { .. }
            | R::SetStreamer { .. }
            | R::SetStreamVolume { .. }
            | R::SetStreamMaster { .. }
            | R::SetSmartVolume { .. } => Part::Sonar,
            R::GetKeyboardState
            | R::SetLighting { .. }
            | R::SetSwitches { .. }
            | R::SetScreen { .. }
            | R::SetScreenCare { .. }
            | R::GetBindings
            | R::ReadBindings
            | R::SetBindings { .. }
            | R::GetOnboard
            | R::ReadOnboard { .. }
            | R::LoadOnboard { .. }
            | R::WriteOnboard { .. }
            | R::GetMacros
            | R::SetMacros { .. }
            | R::WriteMacros { .. } => Part::Keyboard,
            R::GetGamesenseState
            | R::SetGamesenseEnabled { .. }
            | R::SetGameEnabled { .. }
            | R::RemoveGame { .. }
            | R::GamesenseTest => Part::Gamesense,
            R::GetProfiles
            | R::SaveProfile { .. }
            | R::ApplyProfile { .. }
            | R::DeleteProfile { .. }
            | R::SetGameRules { .. } => Part::Profiles,
            R::GetClipsState | R::SetClipsSettings { .. } | R::SaveClip | R::DeleteClip { .. } => {
                Part::Clips
            }
        })
    }
}

/// One part of gglinux that can be turned off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Part {
    Keyboard,
    Headset,
    Mouse,
    Sonar,
    Gamesense,
    Profiles,
    Clips,
}

impl Part {
    pub const ALL: [Part; 7] = [
        Part::Keyboard,
        Part::Headset,
        Part::Mouse,
        Part::Sonar,
        Part::Gamesense,
        Part::Profiles,
        Part::Clips,
    ];

    /// Its name, as Settings shows it.
    pub fn label(self) -> &'static str {
        match self {
            Part::Keyboard => "Keyboard",
            Part::Headset => "Headset",
            Part::Mouse => "Mouse",
            Part::Sonar => "Sonar",
            Part::Gamesense => "GameSense",
            Part::Profiles => "Profiles",
            Part::Clips => "Clips",
        }
    }
}

/// Which parts of gglinux run. A part that's off isn't started at all: its device is
/// left alone, nothing of it runs in the background, and its page is hidden. Its settings
/// are kept for when it's turned back on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Parts {
    pub keyboard: bool,
    pub headset: bool,
    pub mouse: bool,
    pub sonar: bool,
    pub gamesense: bool,
    pub profiles: bool,
    pub clips: bool,
}

impl Default for Parts {
    fn default() -> Self {
        Parts {
            keyboard: true,
            headset: true,
            mouse: true,
            sonar: true,
            gamesense: true,
            profiles: true,
            clips: true,
        }
    }
}

impl Parts {
    pub fn on(&self, part: Part) -> bool {
        match part {
            Part::Keyboard => self.keyboard,
            Part::Headset => self.headset,
            Part::Mouse => self.mouse,
            Part::Sonar => self.sonar,
            Part::Gamesense => self.gamesense,
            Part::Profiles => self.profiles,
            Part::Clips => self.clips,
        }
    }

    pub fn set(&mut self, part: Part, on: bool) {
        match part {
            Part::Keyboard => self.keyboard = on,
            Part::Headset => self.headset = on,
            Part::Mouse => self.mouse = on,
            Part::Sonar => self.sonar = on,
            Part::Gamesense => self.gamesense = on,
            Part::Profiles => self.profiles = on,
            Part::Clips => self.clips = on,
        }
    }

    /// What actually runs: GameSense draws only on the keyboard, so it needs that too.
    pub fn running(&self) -> Parts {
        Parts {
            gamesense: self.gamesense && self.keyboard,
            ..*self
        }
    }

    /// Why a request for `part` can't be answered, or `None` if it runs.
    pub fn refusal(&self, part: Part) -> Option<String> {
        if self.running().on(part) {
            None
        } else if part == Part::Gamesense && self.gamesense {
            Some("GameSense draws on the keyboard, which is turned off in Settings → Parts".into())
        } else {
            Some(format!(
                "{} is turned off in Settings → Parts",
                part.label()
            ))
        }
    }
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
    Devices {
        devices: Vec<DeviceInfo>,
    },
    Pong,
    Mouse {
        state: MouseState,
    },
    Headset {
        state: HeadsetState,
    },
    Sonar {
        state: SonarState,
    },
    Keyboard {
        state: KeyboardState,
    },
    Gamesense {
        state: GameSenseState,
    },
    Profiles {
        state: ProfilesState,
    },
    Clips {
        state: ClipsState,
    },
    Parts {
        parts: Parts,
    },
    Experimental {
        experimental: Experimental,
    },
    Bindings {
        state: BindingsState,
    },
    Onboard {
        state: OnboardState,
    },
    Actions {
        state: ActionsState,
    },
    Macros {
        state: MacrosState,
    },
    /// A command was accepted and applied.
    Success,
    Error {
        message: String,
    },
}

/// An unsolicited server push. Adjacently tagged: `{"event":"device_added","data":{...}}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", content = "data", rename_all = "snake_case")]
pub enum Event {
    DeviceAdded(DeviceInfo),
    DeviceRemoved {
        id: String,
    },
    /// Full current list — sent on connect and after a lag/resync.
    DevicesChanged {
        devices: Vec<DeviceInfo>,
    },
    /// Mouse battery changed (or became unknown when it went to sleep).
    MouseBattery {
        battery: Option<BatteryState>,
    },
    /// Headset state changed: battery, power, connection, or screen.
    HeadsetChanged {
        state: HeadsetState,
    },
    /// Sonar changed: channels, apps, volumes, or on/off.
    SonarChanged {
        state: SonarState,
    },
    /// Keyboard connection or lighting changed.
    KeyboardChanged {
        state: KeyboardState,
    },
    /// GameSense switched on/off, or a game appeared, changed, or went quiet.
    GamesenseChanged {
        state: GameSenseState,
    },
    /// Profiles, rules, or the active rule changed.
    ProfilesChanged {
        state: ProfilesState,
    },
    /// The recorder started or stopped, a clip was saved or deleted, or settings changed.
    ClipsChanged {
        state: ClipsState,
    },
    /// An experimental feature was turned on or off.
    ExperimentalChanged {
        experimental: Experimental,
    },
    /// The keyboard bindings changed, or were read.
    BindingsChanged {
        state: BindingsState,
    },
    /// An onboard slot was read, loaded or written, or an operation started or ended.
    OnboardChanged {
        state: OnboardState,
    },
    /// The actions changed, or one ran or failed.
    ActionsChanged {
        state: ActionsState,
    },
    /// The macros changed, or a write of them started or ended.
    MacrosChanged {
        state: MacrosState,
    },
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

/// gglinux's config directory: `$XDG_CONFIG_HOME/gglinux`, else `~/.config/gglinux`.
pub fn config_dir() -> std::path::PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or_default()
                .join(".config")
        });
    base.join("gglinux")
}

/// Move a settings file that doesn't parse out of the way, to
/// `<name>.unreadable-<unix seconds>` beside it, so the defaults saved in its
/// place later don't destroy the only copy. Says where it went, for the log;
/// call it before the log macro, whose arguments only run when it's enabled.
pub fn set_aside(path: &std::path::Path) -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".unreadable-{secs}"));
    let aside = path.with_file_name(name);
    match std::fs::rename(path, &aside) {
        Ok(()) => aside.display().to_string(),
        Err(e) => format!("nowhere ({e})"),
    }
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
            Request::SetMouseDpi {
                stages,
                active,
                save,
            } => {
                assert_eq!(
                    stages,
                    vec![DpiStage { x: 800, y: 800 }, DpiStage { x: 1600, y: 1600 }]
                );
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
            (
                r#"{"id":3,"req":{"method":"get_mouse_state"}}"#,
                "get_mouse_state",
            ),
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
            battery: Some(BatteryState {
                level: 12,
                charging: false,
            }),
            applied: None,
            ..MouseState::default()
        };
        let json = serde_json::to_string(&ResponseData::Mouse { state }).unwrap();
        assert!(json.contains(r#""applied":null"#), "{json}");
        assert!(json.contains(r#""level":12"#), "{json}");
    }

    #[test]
    fn mouse_settings_report_each_unwritten_part_as_null() {
        let applied = MouseSettings {
            polling_hz: Some(500),
            ..Default::default()
        };
        let json = serde_json::to_string(&applied).unwrap();
        assert_eq!(
            json,
            r#"{"dpi":null,"polling_hz":500,"buttons":null,"sleep_minutes":null,"radio":null}"#
        );
    }

    /// The sleep timer's request, and a state from an older daemon without the reading.
    #[test]
    fn the_sleep_timer_has_its_request_and_an_older_state_still_reads() {
        let line = r#"{"id":5,"req":{"method":"set_mouse_sleep","params":{"minutes":20}}}"#;
        let req = serde_json::from_str::<RequestFrame>(line).unwrap().req;
        assert!(
            matches!(req, Request::SetMouseSleep { minutes: 20 }),
            "{req:?}"
        );
        assert_eq!(req.part(), Some(Part::Mouse));
        let old = r#"{"connected":true,"battery":null,"applied":{"dpi":null,"polling_hz":null,"buttons":null}}"#;
        let state: MouseState = serde_json::from_str(old).unwrap();
        assert_eq!(
            (state.sleep_secs, state.applied.unwrap().sleep_minutes),
            (None, None)
        );
    }

    /// Eight button actions, the wheel's two last, as the app and ggctl send them.
    #[test]
    fn the_buttons_request_takes_the_wheel_after_the_six_buttons() {
        let line = r#"{"id":6,"req":{"method":"set_mouse_buttons","params":{"actions":
            ["button1","button2","button3","button4","button5","dpi","button4","scrolldown"],
            "save":false}}}"#;
        match serde_json::from_str::<RequestFrame>(line).unwrap().req {
            Request::SetMouseButtons { actions, save } => {
                assert_eq!((actions.len(), save), (8, false));
                assert_eq!(actions[6..], ["button4", "scrolldown"]);
            }
            other => panic!("parsed as {other:?}"),
        }
        assert!(MOUSE_ASLEEP.contains("asleep") && MOUSE_ASLEEP.contains("move it"));
    }

    #[test]
    fn server_messages_are_distinguishable() {
        let resp = ServerMessage::response(1, ResponseData::Pong)
            .to_line()
            .unwrap();
        assert!(resp.contains("\"kind\":\"response\""));
        let note = ServerMessage::notify(Event::DeviceRemoved {
            id: "1038:1872".into(),
        })
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
            Request::SetHeadsetScreen { settings } => {
                assert_eq!(settings, ScreenSettings::default())
            }
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
        let json =
            serde_json::to_string(&ServerMessage::notify(Event::HeadsetChanged { state })).unwrap();
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
        assert!(ScreenSettings {
            lit_seconds: 2,
            ..ok
        }
        .validate()
        .is_err());
        assert!(ScreenSettings {
            lit_seconds: 601,
            ..ok
        }
        .validate()
        .is_err());
        assert!(
            ScreenSettings {
                brightness: Some(0),
                ..ok
            }
            .validate()
            .is_err(),
            "0 is not off"
        );
        assert!(ScreenSettings {
            brightness: Some(11),
            ..ok
        }
        .validate()
        .is_err());
        assert!(ScreenSettings {
            brightness: Some(1),
            lit_seconds: 600,
            ..ok
        }
        .validate()
        .is_ok());
    }

    #[test]
    fn headset_changes_parse_from_their_documented_json() {
        let cases = [
            (
                r#"{"setting":"noise_mode","value":"anc"}"#,
                HeadsetChange::NoiseMode(NoiseMode::Anc),
            ),
            (
                r#"{"setting":"sidetone","value":2}"#,
                HeadsetChange::Sidetone(2),
            ),
            (
                r#"{"setting":"auto_off_minutes","value":0}"#,
                HeadsetChange::AutoOffMinutes(0),
            ),
            (
                r#"{"setting":"radio_mode","value":"range"}"#,
                HeadsetChange::RadioMode(RadioMode::Range),
            ),
            (
                r#"{"setting":"eq_bands","value":[0,0,0,0,0,0,0,0,0,1.5]}"#,
                HeadsetChange::EqBands(vec![0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.5]),
            ),
        ];
        for (json, expected) in cases {
            let line = format!(
                r#"{{"id":1,"req":{{"method":"set_headset","params":{{"change":{json}}}}}}}"#
            );
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
    fn base_station_changes_parse_from_their_documented_json() {
        let cases = [
            (
                r#"{"setting":"bluetooth_at_power_on","value":true}"#,
                HeadsetChange::BluetoothAtPowerOn(true),
            ),
            (
                r#"{"setting":"bluetooth_call","value":"lower_game"}"#,
                HeadsetChange::BluetoothCall(BluetoothCall::LowerGame),
            ),
            (
                r#"{"setting":"gain","value":"high"}"#,
                HeadsetChange::Gain(HeadsetGain::High),
            ),
            (
                r#"{"setting":"line_out","value":"stream"}"#,
                HeadsetChange::LineOut(LineOut::Stream),
            ),
            (
                r#"{"setting":"line_out_mix","value":{"main":50,"aux":100,"mic":0}}"#,
                HeadsetChange::LineOutMix(LineOutMix {
                    main: 50,
                    aux: 100,
                    mic: 0,
                }),
            ),
            (
                r#"{"setting":"home_screen","value":"simple"}"#,
                HeadsetChange::HomeScreen(HomeScreen::Simple),
            ),
            (
                r#"{"setting":"screen_dim_minutes","value":5}"#,
                HeadsetChange::ScreenDimMinutes(5),
            ),
        ];
        for (json, expected) in cases {
            assert_eq!(
                serde_json::from_str::<HeadsetChange>(json).unwrap(),
                expected,
                "{json}"
            );
            assert_eq!(serde_json::to_string(&expected).unwrap(), json);
        }
    }

    #[test]
    fn only_the_base_station_settings_need_headset_extras() {
        let gated = [
            HeadsetChange::BluetoothAtPowerOn(false),
            HeadsetChange::BluetoothCall(BluetoothCall::Nothing),
            HeadsetChange::Gain(HeadsetGain::Low),
            HeadsetChange::LineOut(LineOut::Speakers),
            HeadsetChange::LineOutMix(LineOutMix {
                main: 100,
                aux: 100,
                mic: 100,
            }),
            HeadsetChange::HomeScreen(HomeScreen::Detailed),
            HeadsetChange::ScreenDimMinutes(1),
        ];
        for change in gated {
            assert_eq!(change.feature(), Some(Feature::HeadsetExtras), "{change:?}");
        }
        for change in [
            HeadsetChange::NoiseMode(NoiseMode::Off),
            HeadsetChange::Sidetone(0),
            HeadsetChange::AutoOffMinutes(30),
            HeadsetChange::EqBands(vec![0.0; 10]),
        ] {
            assert_eq!(change.feature(), None, "{change:?}");
        }
        assert!(
            SCREEN_DIM_MINUTES.contains(&60),
            "captured in arctis-5 (83 06)"
        );
    }

    /// A daemon from before the base-station settings sends none of their fields.
    #[test]
    fn controls_without_the_base_station_fields_read_as_unknown() {
        let old = r#"{"noise_mode":"anc","transparency":5,"mic_led":7,"mic_muted":false,
            "auto_off_minutes":30,"radio_mode":"speed","sidetone":null,"mic_volume":null,
            "eq_preset":null,"eq_bands":null}"#;
        let c: HeadsetControls = serde_json::from_str(old).unwrap();
        assert_eq!(c.noise_mode, Some(NoiseMode::Anc));
        assert_eq!((c.gain, c.line_out, c.line_out_mix), (None, None, None));
        assert_eq!(
            (c.pairing, c.bluetooth_on, c.screen_dim_minutes),
            (None, None, None)
        );
    }

    #[test]
    fn sonar_requests_parse_from_their_documented_json() {
        for (line, label) in [
            (r#"{"id":1,"req":{"method":"get_sonar_state"}}"#, "get"),
            (
                r#"{"id":2,"req":{"method":"set_sonar_enabled","params":{"enabled":true}}}"#,
                "enable",
            ),
            (
                r#"{"id":4,"req":{"method":"set_chat_mix","params":{"balance":-0.25}}}"#,
                "chatmix",
            ),
            (
                r#"{"id":5,"req":{"method":"set_app_channel","params":{"binary":"discord","channel":"chat"}}}"#,
                "app",
            ),
            (
                r#"{"id":6,"req":{"method":"set_sonar_output","params":{"name":"alsa_output.x"}}}"#,
                "output",
            ),
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
        assert_eq!(
            serde_json::to_string(&Channel::Media).unwrap(),
            r#""media""#
        );
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
                assert_eq!(
                    bands,
                    vec![EqBand {
                        kind: BandKind::Peaking,
                        freq: 250.0,
                        q: 1.0,
                        gain: -3.5
                    }]
                );
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
        assert_eq!(
            serde_json::to_string(&BandKind::LowShelf).unwrap(),
            r#""low_shelf""#
        );
        assert_eq!(
            serde_json::to_string(&BandKind::HighShelf).unwrap(),
            r#""high_shelf""#
        );
    }

    #[test]
    fn mic_requests_parse_from_their_documented_json() {
        let line = r#"{"id":1,"req":{"method":"set_mic_settings","params":{"settings":
            {"denoise":true,"denoise_strength":60.0,"gate":true,"gate_threshold_db":-40.0,"compressor":false,"makeup_db":6.0}}}}"#;
        match serde_json::from_str::<RequestFrame>(line).unwrap().req {
            Request::SetMicSettings { settings } => {
                assert!(settings.gate);
                assert_eq!(settings.denoise_strength, 60.0);
            }
            other => panic!("parsed as {other:?}"),
        }
        for line in [
            r#"{"id":2,"req":{"method":"set_mic_input","params":{"name":"alsa_input.x"}}}"#,
            r#"{"id":3,"req":{"method":"set_mic_volume","params":{"volume":0.8}}}"#,
        ] {
            serde_json::from_str::<RequestFrame>(line).unwrap_or_else(|e| panic!("{line}: {e}"));
        }
    }

    #[test]
    fn mic_defaults_suppress_noise_and_leave_the_rest_off() {
        let d = MicSettings::default();
        assert!(d.denoise && !d.gate && !d.compressor);
        assert_eq!(
            (d.denoise_strength, d.gate_threshold_db, d.makeup_db),
            (50.0, -45.0, 6.0)
        );
    }

    #[test]
    fn lighting_requests_parse_from_their_documented_json() {
        let line = r#"{"id":1,"req":{"method":"set_lighting","params":{"lighting":{"enabled":true,
            "effect":{"kind":"breathing","color":{"r":255,"g":0,"b":0},"period_s":3.0},
            "keys":[{"code":10,"color":{"r":0,"g":255,"b":0}}],"brightness":80}}}}"#;
        match serde_json::from_str::<RequestFrame>(line).unwrap().req {
            Request::SetLighting { lighting } => {
                assert!(lighting.enabled);
                assert_eq!(
                    lighting.effect,
                    Effect::Breathing {
                        color: Rgb { r: 255, g: 0, b: 0 },
                        period_s: 3.0
                    }
                );
                assert_eq!(
                    lighting.keys,
                    vec![KeyColor {
                        code: 10,
                        color: Rgb { r: 0, g: 255, b: 0 }
                    }]
                );
                assert_eq!(lighting.brightness, 80);
            }
            other => panic!("parsed as {other:?}"),
        }
        serde_json::from_str::<RequestFrame>(r#"{"id":2,"req":{"method":"get_keyboard_state"}}"#)
            .expect("get_keyboard_state should parse");
        assert_eq!(
            serde_json::to_string(&Effect::Static).unwrap(),
            r#"{"kind":"static"}"#
        );
    }

    #[test]
    fn lighting_is_off_by_default_and_validated() {
        let d = Lighting::default();
        assert!(!d.enabled, "gglinux never takes the lighting until asked");
        assert!(d.validate().is_ok());
        assert!(Lighting {
            brightness: 101,
            ..d.clone()
        }
        .validate()
        .is_err());
        assert!(Lighting {
            effect: Effect::Wave { period_s: 0.1 },
            ..d.clone()
        }
        .validate()
        .is_err());
        assert!(Lighting {
            effect: Effect::Spectrum { period_s: f32::NAN },
            ..d.clone()
        }
        .validate()
        .is_err());
        assert!(Lighting {
            effect: Effect::Static,
            ..d
        }
        .validate()
        .is_ok());
    }

    #[test]
    fn lighting_saved_before_reactive_still_loads_with_reactive_off() {
        let old = r#"{"enabled":true,"effect":{"kind":"static"},"keys":[],"brightness":70}"#;
        let l: Lighting = serde_json::from_str(old).unwrap();
        assert!(l.enabled && l.brightness == 70);
        assert_eq!(l.reactive, Reactive::default());
        assert!(!l.reactive.enabled);
    }

    #[test]
    fn reactive_defaults_json_and_bounds() {
        let d = Reactive::default();
        assert_eq!(
            (d.mode, d.color, d.duration_s),
            (
                ReactiveMode::Fade,
                Rgb {
                    r: 255,
                    g: 255,
                    b: 255
                },
                0.6
            )
        );
        let json = serde_json::to_value(Reactive {
            mode: ReactiveMode::Ripple,
            ..d
        })
        .unwrap();
        assert_eq!(json["mode"], "ripple");
        assert_eq!(
            serde_json::to_value(ReactiveStatus::NoAccess).unwrap(),
            "no_access"
        );
        assert_eq!(ReactiveStatus::default(), ReactiveStatus::Off);

        let with = |duration_s| Lighting {
            reactive: Reactive { duration_s, ..d },
            ..Lighting::default()
        };
        assert!(with(0.1).validate().is_ok());
        assert!(with(3.0).validate().is_ok());
        assert!(with(0.05).validate().is_err());
        assert!(with(3.5).validate().is_err());
        assert!(with(f32::NAN).validate().is_err());
    }

    #[test]
    fn gamesense_requests_and_events_use_their_documented_json() {
        for (line, want) in [
            (
                r#"{"id":1,"req":{"method":"get_gamesense_state"}}"#,
                "GetGamesenseState",
            ),
            (
                r#"{"id":2,"req":{"method":"set_gamesense_enabled","params":{"enabled":true}}}"#,
                "SetGamesenseEnabled",
            ),
            (
                r#"{"id":3,"req":{"method":"set_game_enabled","params":{"game":"G","enabled":false}}}"#,
                "SetGameEnabled",
            ),
            (
                r#"{"id":4,"req":{"method":"remove_game","params":{"game":"G"}}}"#,
                "RemoveGame",
            ),
            (
                r#"{"id":5,"req":{"method":"gamesense_test"}}"#,
                "GamesenseTest",
            ),
        ] {
            let req = serde_json::from_str::<RequestFrame>(line).unwrap().req;
            assert!(
                format!("{req:?}").starts_with(want),
                "{line} parsed as {req:?}"
            );
        }
        let state = GameSenseState {
            enabled: true,
            address: Some("127.0.0.1:5000".into()),
            prefixes: 17,
            games: vec![GameInfo {
                name: "G".into(),
                display_name: None,
                developer: None,
                enabled: true,
                active: false,
                events: vec!["E".into()],
            }],
        };
        let v = serde_json::to_value(Event::GamesenseChanged {
            state: state.clone(),
        })
        .unwrap();
        assert_eq!(v["event"], "gamesense_changed");
        assert_eq!(v["data"]["state"]["prefixes"], 17);
        let r = serde_json::to_value(ResponseData::Gamesense { state }).unwrap();
        assert_eq!(r["type"], "gamesense");
        assert_eq!(
            GameSenseState::default(),
            GameSenseState {
                enabled: false,
                address: None,
                prefixes: 0,
                games: vec![]
            }
        );
    }

    #[test]
    fn the_visualizer_effect_and_its_sensitivity_bounds() {
        let v: Effect = serde_json::from_str(r#"{"kind":"visualizer","sensitivity":1.5}"#).unwrap();
        assert_eq!(v, Effect::Visualizer { sensitivity: 1.5 });
        let with = |sensitivity| Lighting {
            effect: Effect::Visualizer { sensitivity },
            ..Lighting::default()
        };
        assert!(with(0.25).validate().is_ok() && with(4.0).validate().is_ok());
        assert!(
            with(0.2).validate().is_err()
                && with(4.5).validate().is_err()
                && with(f32::NAN).validate().is_err()
        );
    }

    #[test]
    fn the_mic_mute_light_defaults_off_on_the_screen_button_in_red_and_old_files_still_load() {
        let d = MicMuteLight::default();
        assert_eq!(
            (d.enabled, d.key, d.color),
            (false, 0xFB, Rgb { r: 255, g: 0, b: 0 })
        );
        let old = r#"{"enabled":true,"effect":{"kind":"static"},"keys":[],"brightness":70}"#;
        assert_eq!(serde_json::from_str::<Lighting>(old).unwrap().mic_mute, d);
    }

    /// `lighting.json` as the daemon saved it before zones: `serde_json::to_string_pretty`
    /// of a `Lighting` with every field it had then.
    const SAVED_BEFORE_ZONES: &str = r#"{
  "enabled": true,
  "effect": {
    "kind": "static"
  },
  "keys": [
    {
      "code": 26,
      "color": {
        "r": 255,
        "g": 0,
        "b": 0
      }
    }
  ],
  "brightness": 80,
  "reactive": {
    "enabled": true,
    "mode": "ripple",
    "color": {
      "r": 0,
      "g": 0,
      "b": 255
    },
    "duration_s": 0.6
  },
  "mic_mute": {
    "enabled": false,
    "key": 251,
    "color": {
      "r": 255,
      "g": 0,
      "b": 0
    }
  }
}"#;

    #[test]
    fn lighting_saved_before_zones_reads_as_one_zone_over_every_key_and_saves_back_the_same() {
        let l: Lighting = serde_json::from_str(SAVED_BEFORE_ZONES).unwrap();
        assert_eq!(l.zones, None);
        assert_eq!(
            l.stack().into_owned(),
            vec![Zone::new("all", ZoneKeys::All, ZoneEffect::PerKey)],
            "the static colours, from the key list as before"
        );
        assert_eq!(l.keys.len(), 1);
        assert_eq!(l.idle, IdleLighting::default());
        assert!(!l.idle.enabled, "no idle scene until asked");
        let again: serde_json::Value =
            serde_json::from_str(&serde_json::to_string_pretty(&l).unwrap()).unwrap();
        let mut was: serde_json::Value = serde_json::from_str(SAVED_BEFORE_ZONES).unwrap();
        was["idle"] = serde_json::to_value(IdleLighting::default()).unwrap();
        assert_eq!(
            again, was,
            "nothing lost, nothing but the idle defaults added"
        );

        let wave =
            SAVED_BEFORE_ZONES.replace(r#""kind": "static""#, r#""kind": "wave", "period_s": 3.0"#);
        let l: Lighting = serde_json::from_str(&wave).unwrap();
        assert_eq!(
            l.stack()[0].effect,
            ZoneEffect::Wave { period_s: 3.0 },
            "every effect comes across"
        );
        for (old, new) in [
            (
                Effect::Breathing {
                    color: Rgb { r: 1, g: 2, b: 3 },
                    period_s: 2.0,
                },
                ZoneEffect::Breathing {
                    color: Rgb { r: 1, g: 2, b: 3 },
                    period_s: 2.0,
                },
            ),
            (
                Effect::Spectrum { period_s: 6.0 },
                ZoneEffect::Spectrum { period_s: 6.0 },
            ),
            (
                Effect::Visualizer { sensitivity: 1.5 },
                ZoneEffect::Visualizer { sensitivity: 1.5 },
            ),
        ] {
            assert_eq!(ZoneEffect::from(old), new);
        }
    }

    #[test]
    fn editing_the_zones_writes_the_old_effect_out_first() {
        let mut l: Lighting = serde_json::from_str(SAVED_BEFORE_ZONES).unwrap();
        l.zones_mut().push(Zone::new(
            "WASD",
            ZonePreset::Wasd.keys(),
            ZoneEffect::Static {
                color: Rgb { r: 0, g: 255, b: 0 },
            },
        ));
        let zones = l.zones.as_ref().unwrap();
        assert_eq!(zones.len(), 2);
        assert_eq!(
            zones[0].effect,
            ZoneEffect::PerKey,
            "the base stays at the bottom"
        );
        assert!(l.validate().is_ok());
        let json = serde_json::to_value(&l).unwrap();
        assert_eq!(json["zones"][1]["keys"]["kind"], "keys");
        assert_eq!(json["zones"][1]["keys"]["codes"][0], 0x1A);
        assert_eq!(json["zones"][1]["effect"]["kind"], "static");
        assert_eq!(json["zones"][0]["keys"]["kind"], "all");
        let back: Lighting = serde_json::from_value(json).unwrap();
        assert_eq!(back, l);
        let mut dark = Lighting::default();
        dark.zones_mut().clear();
        assert!(
            dark.stack().is_empty(),
            "an empty stack stays empty: every key dark"
        );
    }

    #[test]
    fn zones_are_validated_with_messages_for_the_user() {
        let zone = |name: &str, effect| Zone::new(name, ZonePreset::Arrows.keys(), effect);
        let with = |zones: Vec<Zone>| Lighting {
            zones: Some(zones),
            ..Lighting::default()
        };
        let red = Rgb { r: 255, g: 0, b: 0 };
        let ok = ZoneEffect::Static { color: red };
        assert!(with(vec![zone("a", ok), zone("b", ok)]).validate().is_ok());
        let two = with(vec![zone("a", ok), zone("a", ok)]).validate();
        assert_eq!(two.unwrap_err(), "two zones are called 'a'");
        assert!(with(vec![zone("", ok)]).validate().is_err());
        assert!(with(vec![zone(" a", ok)]).validate().is_err());
        assert!(with(vec![zone(&"x".repeat(33), ok)]).validate().is_err());
        let no_keys = Zone::new("a", ZoneKeys::Keys { codes: vec![] }, ok);
        assert_eq!(
            with(vec![no_keys]).validate().unwrap_err(),
            "zone 'a' has no keys"
        );
        let slow = zone("a", ZoneEffect::Wave { period_s: 0.1 });
        assert!(with(vec![slow])
            .validate()
            .unwrap_err()
            .starts_with("zone 'a': effect period"));
        let quick = ZoneEffect::Reactive {
            mode: ReactiveMode::Fade,
            color: red,
            duration_s: 5.0,
        };
        assert!(with(vec![zone("a", quick)]).validate().is_err());
        let loud = ZoneEffect::Visualizer { sensitivity: 9.0 };
        assert!(with(vec![zone("a", loud)]).validate().is_err());
        let many = (0..17).map(|i| zone(&i.to_string(), ok)).collect();
        assert!(with(many).validate().is_err(), "at most 16");

        let mut l = Lighting::default();
        l.idle.after_minutes = 121;
        assert!(l.validate().unwrap_err().contains("idle time"));
        l.idle.after_minutes = 0;
        l.idle.zones = vec![zone("a", ok), zone("a", ok)];
        assert!(l.validate().is_err(), "the idle scene's zones too");
    }

    #[test]
    fn the_presets_name_their_keys_and_every_key_is_all() {
        assert_eq!(
            ZonePreset::Wasd.keys(),
            ZoneKeys::Keys {
                codes: vec![0x1A, 0x04, 0x16, 0x07]
            }
        );
        assert_eq!(ZonePreset::All.keys(), ZoneKeys::All);
        let f = ZonePreset::FRow.keys();
        assert!(f.contains(0x3A) && f.contains(0x45) && !f.contains(0x46));
        let n = ZonePreset::NumberRow.keys();
        assert!(n.contains(0x1E) && n.contains(0x27) && !n.contains(0x35));
        assert!(ZonePreset::Arrows.keys().contains(0x52));
        assert!(ZoneKeys::All.contains(0xFB));
        for p in ZonePreset::ALL {
            assert_eq!(ZonePreset::from_key(p.key()), Some(p));
        }
    }

    #[test]
    fn reflect_goes_over_the_last_every_key_zone_and_comes_out_again() {
        let mut l = Lighting::default();
        l.zones_mut().push(Zone::new(
            "WASD",
            ZonePreset::Wasd.keys(),
            ZoneEffect::Static {
                color: Rgb { r: 0, g: 255, b: 0 },
            },
        ));
        assert!(!l.uses_reflect());
        l.reflect_on();
        let names: Vec<String> = l.stack().iter().map(|z| z.name.clone()).collect();
        assert_eq!(
            names,
            ["all", "Screen", "WASD"],
            "WASD still shows over the screen"
        );
        assert!(l.uses_reflect());
        l.reflect_on();
        assert_eq!(l.stack().len(), 3, "once");
        assert_eq!(
            serde_json::to_value(l.stack()[1].effect).unwrap()["kind"],
            "reflect"
        );
        l.reflect_off();
        assert_eq!(l.stack().len(), 2);
        assert!(!l.uses_reflect());
        l.idle.zones = vec![Zone::new("s", ZoneKeys::All, ZoneEffect::Reflect)];
        assert!(l.uses_reflect(), "the idle scene's count too");

        let mut fresh = Lighting {
            zones: Some(vec![]),
            ..Lighting::default()
        };
        fresh.reflect_on();
        assert_eq!(
            fresh.stack()[0].name,
            "Screen",
            "an empty stack: at the bottom"
        );
    }

    #[test]
    fn the_keyboard_state_tells_the_idle_scene_and_reflect() {
        let json = r#"{"connected":true,"layout":[],"lighting":{"enabled":false,"effect":{"kind":"static"},"keys":[],"brightness":100},"reactive":"off"}"#;
        let state: KeyboardState = serde_json::from_str(json).unwrap();
        assert!(!state.lighting_idle);
        assert_eq!(state.reflect, ReflectStatus::Off);
        let stopped = ReflectStatus::Stopped {
            why: ReflectStatus::NOT_BUILT.into(),
        };
        let v = serde_json::to_value(&stopped).unwrap();
        assert_eq!(v["state"], "stopped");
        assert!(v["why"].as_str().unwrap().contains("isn't built"));
        assert_eq!(
            serde_json::to_value(ReflectStatus::Running).unwrap(),
            serde_json::json!({"state": "running"})
        );
    }

    #[test]
    fn profile_requests_and_events_use_their_documented_json() {
        for line in [
            r#"{"id":1,"req":{"method":"get_profiles"}}"#,
            r#"{"id":2,"req":{"method":"save_profile","params":{"name":"Night"}}}"#,
            r#"{"id":3,"req":{"method":"apply_profile","params":{"name":"Night"}}}"#,
            r#"{"id":4,"req":{"method":"delete_profile","params":{"name":"Night"}}}"#,
            r#"{"id":5,"req":{"method":"set_game_rules","params":{"rules":[{"process":"RDR2.exe","profile":"Night"}]}}}"#,
        ] {
            serde_json::from_str::<RequestFrame>(line).unwrap_or_else(|e| panic!("{line}: {e}"));
        }
        let state = ProfilesState {
            profiles: vec!["Night".into()],
            rules: vec![GameRule {
                process: "RDR2.exe".into(),
                profile: "Night".into(),
            }],
            active: None,
            running: vec![],
        };
        let v = serde_json::to_value(Event::ProfilesChanged { state }).unwrap();
        assert_eq!(v["event"], "profiles_changed");
        assert_eq!(v["data"]["state"]["rules"][0]["process"], "RDR2.exe");
    }

    #[test]
    fn clips_settings_default_off_validate_and_parse() {
        let d = ClipsSettings::default();
        assert_eq!(
            (
                d.enabled,
                d.seconds,
                d.fps,
                d.quality,
                d.desktop_audio,
                d.mic,
                d.dir.as_deref()
            ),
            (false, 30, 60, ClipQuality::High, true, false, None)
        );
        assert!(d.validate().is_ok());
        assert!(ClipsSettings {
            seconds: 4,
            ..d.clone()
        }
        .validate()
        .is_err());
        assert!(ClipsSettings {
            fps: 241,
            ..d.clone()
        }
        .validate()
        .is_err());
        assert!(ClipsSettings {
            dir: Some("relative/dir".into()),
            ..d.clone()
        }
        .validate()
        .is_err());
        let partial: ClipsSettings =
            serde_json::from_str(r#"{"enabled":true,"quality":"very_high"}"#).unwrap();
        assert_eq!(
            (partial.enabled, partial.quality, partial.seconds),
            (true, ClipQuality::VeryHigh, 30)
        );
        for line in [
            r#"{"id":1,"req":{"method":"get_clips_state"}}"#,
            r#"{"id":2,"req":{"method":"save_clip"}}"#,
            r#"{"id":3,"req":{"method":"delete_clip","params":{"name":"Replay.mp4"}}}"#,
            r#"{"id":4,"req":{"method":"set_clips_settings","params":{"settings":{"enabled":true}}}}"#,
        ] {
            serde_json::from_str::<RequestFrame>(line).unwrap_or_else(|e| panic!("{line}: {e}"));
        }
        assert_eq!(
            serde_json::to_value(Event::ClipsChanged {
                state: ClipsState::default()
            })
            .unwrap()["event"],
            "clips_changed"
        );
    }

    #[test]
    fn smart_volume_defaults_off_and_parses() {
        let d = SmartVolume::default();
        assert_eq!((d.enabled, d.level), (false, SmartLevel::Balanced));
        let line = r#"{"id":1,"req":{"method":"set_smart_volume","params":{"channel":"game","settings":{"enabled":true,"level":"loud"}}}}"#;
        match serde_json::from_str::<RequestFrame>(line).unwrap().req {
            Request::SetSmartVolume { channel, settings } => {
                assert_eq!(channel, Channel::Game);
                assert_eq!(
                    settings,
                    SmartVolume {
                        enabled: true,
                        level: SmartLevel::Loud
                    }
                );
            }
            other => panic!("parsed as {other:?}"),
        }
        let partial: SmartVolume = serde_json::from_str(r#"{"enabled":true}"#).unwrap();
        assert_eq!(partial.level, SmartLevel::Balanced);
        assert_eq!(
            serde_json::to_string(&SmartLevel::Quiet).unwrap(),
            r#""quiet""#
        );
    }

    #[test]
    fn games_only_clips_default_off_and_old_files_still_load() {
        assert!(!ClipsSettings::default().games_only);
        let old: ClipsSettings = serde_json::from_str(r#"{"enabled":true,"seconds":45}"#).unwrap();
        assert!(!old.games_only);
        let on: ClipsSettings =
            serde_json::from_str(r#"{"enabled":true,"games_only":true}"#).unwrap();
        assert!(on.games_only);
        let s = serde_json::to_value(ClipsState::default()).unwrap();
        assert_eq!(
            (s["waiting"].as_bool(), s["game"].is_null()),
            (Some(false), true)
        );
    }

    #[test]
    fn an_unreadable_settings_file_is_set_aside_beside_itself() {
        let dir = std::env::temp_dir().join(format!("gglinux-aside-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("x.json");
        std::fs::write(&path, "{ nope").unwrap();
        let moved = set_aside(&path);
        assert!(
            moved.starts_with(&*dir.join("x.json.unreadable-").to_string_lossy()),
            "{moved}"
        );
        assert_eq!(std::fs::read_to_string(&moved).unwrap(), "{ nope");
        assert!(!path.exists());
        assert!(set_aside(&path).starts_with("nowhere"), "nothing to move");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn switches_check_their_ranges_and_pairs() {
        assert!(Switches::default().validate().is_ok());
        let bad = |f: &dyn Fn(&mut Switches)| {
            let mut s = Switches::default();
            f(&mut s);
            s.validate().unwrap_err()
        };
        assert!(bad(&|s| s.actuation = 0).contains("0.0 mm"));
        assert!(bad(&|s| s.actuation = 41).contains("4.1 mm"));
        assert!(bad(&|s| s.rapid_trigger.sensitivity = 21).contains("2.1 mm"));
        let key = |code, actuation| KeyActuation { code, actuation };
        assert!(bad(&|s| s.keys = vec![key(0x04, 10), key(0x04, 12)]).contains("two actuation"));
        let pair = |a, b| TapPair {
            a,
            b,
            mode: TapMode::LastWins,
            report_both: false,
        };
        assert!(bad(&|s| s.rapid_tap.pairs = vec![pair(0x04, 0x04)]).contains("two different"));
        assert!(
            bad(&|s| s.rapid_tap.pairs = vec![pair(0x04, 0x07), pair(0x07, 0x16)])
                .contains("two Rapid Tap pairs")
        );
    }

    #[test]
    fn a_key_uses_its_own_actuation_point_or_the_default() {
        let s = Switches {
            keys: vec![KeyActuation {
                code: 0x1A,
                actuation: 4,
            }],
            ..Switches::default()
        };
        assert_eq!(s.actuation_of(0x1A), 4);
        assert_eq!(s.actuation_of(0x04), 15);
    }

    #[test]
    fn rapid_trigger_covers_every_key_when_none_are_listed() {
        let mut s = Switches::default();
        assert!(!s.rapid_trigger_on(0x04), "off");
        s.rapid_trigger.enabled = true;
        assert!(s.rapid_trigger_on(0x04) && s.rapid_trigger_on(0x1A));
        s.rapid_trigger.keys = vec![0x1A];
        assert!(s.rapid_trigger_on(0x1A) && !s.rapid_trigger_on(0x04));
    }

    #[test]
    fn switches_saved_before_rapid_tap_existed_still_load() {
        let s: Switches = serde_json::from_str(r#"{"enabled":true,"actuation":20}"#).unwrap();
        assert_eq!(s.actuation, 20);
        assert_eq!(s.rapid_trigger, RapidTrigger::default());
        assert!(s.rapid_tap.pairs.is_empty());
    }

    #[test]
    fn the_screen_takes_five_lines_of_sixteen_ascii_characters_or_a_640_byte_picture() {
        let text = |t: &str| Screen::Text { text: t.into() };
        assert!(text("Hello\nworld").validate().is_ok());
        assert!(text("1\n2\n3\n4\n5\n6").validate().is_err());
        assert!(text("seventeen chars!!").validate().is_err());
        assert!(text("Yağız").validate().unwrap_err().contains("'ğ'"));
        assert!(Screen::Image {
            pixels: vec![0; 640]
        }
        .validate()
        .is_ok());
        assert!(Screen::Image {
            pixels: vec![0; 639]
        }
        .validate()
        .is_err());
        let json = serde_json::to_string(&Screen::Clock {
            seconds: true,
            h24: true,
            date: false,
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"kind":"clock","seconds":true,"h24":true,"date":false}"#
        );
    }

    #[test]
    fn keyboard_state_from_an_older_daemon_still_parses() {
        let json = r#"{"connected":true,"layout":[],"lighting":{"enabled":false,"effect":{"kind":"static"},"keys":[],"brightness":100},"reactive":"off"}"#;
        let state: KeyboardState = serde_json::from_str(json).unwrap();
        assert_eq!(state.screen, Screen::Keyboard);
        assert!(!state.switches.enabled);
        assert_eq!(state.screen_care, ScreenCare::default());
        assert!(!state.screen_resting);
    }

    #[test]
    fn screen_care_moves_and_rests_gglinuxs_picture_but_leaves_the_keyboards_own_alone() {
        let care = ScreenCare::default();
        assert!(care.shift && care.dark_when_away && !care.own_screen_too);
        let parsed: Request = serde_json::from_str(
            r#"{"method":"set_screen_care","params":{"care":{"shift":false}}}"#,
        )
        .unwrap();
        match parsed {
            Request::SetScreenCare { care } => assert_eq!(
                care,
                ScreenCare {
                    shift: false,
                    ..ScreenCare::default()
                },
                "a missing field keeps its default"
            ),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn every_part_is_on_until_turned_off_and_old_files_still_load() {
        let parts = Parts::default();
        assert!(Part::ALL.iter().all(|&p| parts.on(p)));
        let saved: Parts = serde_json::from_str(r#"{"sonar":false,"clips":false}"#).unwrap();
        assert!(!saved.sonar && !saved.clips && saved.keyboard && saved.headset);
        let mut toggled = Parts::default();
        for part in Part::ALL {
            toggled.set(part, false);
            assert!(!toggled.on(part), "{part:?}");
        }
        assert_eq!(
            serde_json::to_string(&Request::SetParts { parts: saved }).unwrap(),
            r#"{"method":"set_parts","params":{"parts":{"keyboard":true,"headset":true,"mouse":true,"sonar":false,"gamesense":true,"profiles":true,"clips":false}}}"#
        );
        let r = serde_json::to_value(ResponseData::Parts { parts }).unwrap();
        assert_eq!(r["type"], "parts");
    }

    #[test]
    fn gamesense_needs_the_keyboard_and_a_refusal_says_where_to_turn_it_on() {
        let parts = Parts {
            keyboard: false,
            ..Parts::default()
        };
        assert!(!parts.running().gamesense && parts.gamesense);
        assert!(parts
            .refusal(Part::Gamesense)
            .unwrap()
            .contains("draws on the keyboard"));
        assert!(parts
            .refusal(Part::Keyboard)
            .unwrap()
            .contains("Keyboard is turned off"));
        assert_eq!(parts.refusal(Part::Mouse), None);
    }

    #[test]
    fn requests_name_the_part_that_answers_them() {
        assert_eq!(Request::GetSonarState.part(), Some(Part::Sonar));
        assert_eq!(Request::SaveClip.part(), Some(Part::Clips));
        assert_eq!(
            Request::SetScreenCare {
                care: ScreenCare::default()
            }
            .part(),
            Some(Part::Keyboard)
        );
        assert_eq!(Request::GamesenseTest.part(), Some(Part::Gamesense));
        assert_eq!(Request::GetHeadsetState.part(), Some(Part::Headset));
        assert_eq!(Request::SaveMouse.part(), Some(Part::Mouse));
        assert_eq!(Request::GetProfiles.part(), Some(Part::Profiles));
        // The daemon's own requests work whatever is off, or a part could never come back.
        assert_eq!(Request::GetParts.part(), None);
        assert_eq!(
            Request::SetParts {
                parts: Parts::default()
            }
            .part(),
            None
        );
        assert_eq!(Request::ListDevices.part(), None);
    }
}

/// The M11 additions: what the daemon, the app and `ggctl` agree on.
#[cfg(test)]
mod contract_tests {
    use super::*;

    fn req(json: &str) -> Request {
        serde_json::from_str::<RequestFrame>(&format!(r#"{{"id":1,"req":{json}}}"#))
            .unwrap()
            .req
    }

    #[test]
    fn new_requests_have_their_documented_json_and_part() {
        assert!(matches!(
            req(r#"{"method":"get_experimental"}"#),
            Request::GetExperimental
        ));
        assert!(matches!(
            req(r#"{"method":"set_experimental","params":{"experimental":{"keyboard_onboard":true}}}"#),
            Request::SetExperimental { experimental } if experimental.on(Feature::KeyboardOnboard)
        ));
        assert!(matches!(
            req(r#"{"method":"read_bindings"}"#),
            Request::ReadBindings
        ));
        assert!(matches!(
            req(r#"{"method":"set_bindings","params":{"bindings":{"normal":[{"code":57,"action":{"kind":"keys","codes":[224,4]}}]},"enabled":true}}"#),
            Request::SetBindings { bindings, enabled: true } if bindings.normal.len() == 1
        ));
        assert!(matches!(
            req(r#"{"method":"read_onboard","params":{"slot":null}}"#),
            Request::ReadOnboard { slot: None }
        ));
        assert!(matches!(
            req(r#"{"method":"write_onboard","params":{"slot":4,"from":{"kind":"current"}}}"#),
            Request::WriteOnboard {
                slot: 4,
                from: OnboardSource::Current
            }
        ));
        assert!(matches!(
            req(r#"{"method":"set_actions","params":{"bindings":[]}}"#),
            Request::SetActions { .. }
        ));
        for (r, part) in [
            (Request::GetExperimental, None),
            (Request::GetActions, None),
            (Request::GetBindings, Some(Part::Keyboard)),
            (Request::ReadBindings, Some(Part::Keyboard)),
            (Request::GetOnboard, Some(Part::Keyboard)),
            (Request::LoadOnboard { slot: 0 }, Some(Part::Keyboard)),
        ] {
            assert_eq!(r.part(), part, "{r:?}");
        }
    }

    #[test]
    fn new_responses_and_events_round_trip() {
        let msgs = [
            ResponseData::Experimental {
                experimental: Experimental::default(),
            },
            ResponseData::Bindings {
                state: BindingsState::default(),
            },
            ResponseData::Onboard {
                state: OnboardState::default(),
            },
            ResponseData::Actions {
                state: ActionsState::default(),
            },
        ];
        for m in msgs {
            let json = serde_json::to_string(&m).unwrap();
            let back: ResponseData = serde_json::from_str(&json).unwrap();
            assert_eq!(serde_json::to_string(&back).unwrap(), json);
        }
        let e = Event::OnboardChanged {
            state: OnboardState::default(),
        };
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.starts_with(r#"{"event":"onboard_changed""#), "{json}");
    }

    #[test]
    fn animations_and_the_monitor_are_checked() {
        let frame = vec![0u8; Screen::IMAGE_BYTES];
        let ok = Screen::Animation {
            frames: vec![frame.clone(), frame.clone()],
            delays_ms: vec![100, 10_000],
        };
        ok.validate().unwrap();
        for bad in [
            Screen::Animation {
                frames: vec![],
                delays_ms: vec![],
            },
            Screen::Animation {
                frames: vec![frame.clone()],
                delays_ms: vec![],
            },
            Screen::Animation {
                frames: vec![vec![0; 639]],
                delays_ms: vec![100],
            },
            Screen::Animation {
                frames: vec![frame.clone()],
                delays_ms: vec![99],
            },
            Screen::Monitor { items: vec![] },
            Screen::Monitor {
                items: vec![MonitorItem::Cpu, MonitorItem::Cpu],
            },
            Screen::Monitor {
                items: vec![
                    MonitorItem::Cpu,
                    MonitorItem::Ram,
                    MonitorItem::Gpu,
                    MonitorItem::Clock,
                    MonitorItem::CpuTemp,
                ],
            },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
        Screen::Monitor {
            items: vec![MonitorItem::Cpu, MonitorItem::Clock],
        }
        .validate()
        .unwrap();
    }

    #[test]
    fn a_second_actuation_point_must_be_deeper_than_the_first() {
        let mut s = Switches {
            actuation: 15,
            ..Switches::default()
        };
        s.second = vec![KeyActuation {
            code: 0x1A,
            actuation: 30,
        }];
        s.validate().unwrap();
        s.second[0].actuation = 15;
        assert!(s.validate().unwrap_err().contains("deeper"));
        s.second = vec![
            KeyActuation {
                code: 0x1A,
                actuation: 30,
            },
            KeyActuation {
                code: 0x1A,
                actuation: 31,
            },
        ];
        assert!(s.validate().is_err());
        // Old settings files without the field still load.
        let old: Switches = serde_json::from_str(r#"{"enabled":true,"actuation":15}"#).unwrap();
        assert!(old.second.is_empty());
    }
}
