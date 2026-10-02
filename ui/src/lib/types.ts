// Mirrors the wire types in crates/gg-ipc. Keep the two in sync — the daemon is
// the source of truth and these are hand-written because the payloads are small.

export type DeviceKind = "keyboard" | "mouse" | "headset" | "other";

export interface InterfaceInfo {
  interface_number: number;
  usage_page: number;
  usage: number;
  path: string;
}

export interface DeviceInfo {
  id: string;
  kind: DeviceKind;
  vendor_id: number;
  product_id: number;
  name: string;
  serial: string | null;
  connected: boolean;
  interfaces: InterfaceInfo[];
}

/** One DPI stage. The protocol allows independent axes; the UI writes x === y,
 *  matching how SteelSeries GG presents this mouse. */
export interface DpiStage {
  x: number;
  y: number;
}

export interface BatteryState {
  /** Charge percentage, 1..=100. */
  level: number;
  charging: boolean;
}

export interface MouseDpi {
  stages: DpiStage[];
  /** 0-based index into `stages`. */
  active: number;
}

/** Each part is `null` until this daemon has written it: the mouse can't be read
 *  back, so a part never written is unknown, not its factory default. */
export interface MouseSettings {
  dpi: MouseDpi | null;
  polling_hz: number | null;
  /** One action name per physical button, in button order. */
  buttons: string[] | null;
}

export interface MouseState {
  connected: boolean;
  /** `null` while the mouse is asleep or has not reported yet. */
  battery: BatteryState | null;
  /** What this daemon has written since it started; `null` if nothing.
   *  The vendor protocol has no getters, so this is never a read of the device. */
  applied: MouseSettings | null;
}

export interface ScreenSettings {
  /** Keep the base-station screen dark, lighting it briefly after events. */
  protect: boolean;
  /** Seconds the screen stays lit after an event, 3..=600. */
  lit_seconds: number;
  /** 1..=10, or null to leave the device's own level alone (it can't be read back). */
  brightness: number | null;
}

export type HeadsetPower = "offline" | "charging" | "online" | "unknown";

export type NoiseMode = "off" | "transparency" | "anc";
export type RadioMode = "speed" | "range";

/** One setting change; mirrors gg-ipc HeadsetChange. */
export type HeadsetChange =
  | { setting: "noise_mode"; value: NoiseMode }
  | { setting: "transparency"; value: number }
  | { setting: "sidetone"; value: number }
  | { setting: "mic_volume"; value: number }
  | { setting: "mic_led"; value: number }
  | { setting: "auto_off_minutes"; value: number }
  | { setting: "radio_mode"; value: RadioMode }
  | { setting: "eq_preset"; value: number }
  | { setting: "eq_bands"; value: number[] };

/** Read-back values first, then what gglinux wrote (the device can't report those). */
export interface HeadsetControls {
  noise_mode: NoiseMode | null;
  transparency: number | null;
  mic_led: number | null;
  mic_muted: boolean | null;
  auto_off_minutes: number | null;
  radio_mode: RadioMode | null;
  sidetone: number | null;
  mic_volume: number | null;
  eq_preset: number | null;
  eq_bands: number[] | null;
}

/** Readings, unlike MouseState: the base station reports these. */
export interface HeadsetState {
  connected: boolean;
  power: HeadsetPower;
  /** Percent; null while the headset is off. */
  headset_battery: number | null;
  /** Percent; null when no spare pack is seated. */
  spare_battery: number | null;
  screen: ScreenSettings;
  /** Whether gglinux has the screen blanked right now. */
  screen_dark: boolean;
  controls: HeadsetControls;
}

export type Channel = "game" | "chat" | "media" | "aux";

export interface AudioDevice {
  name: string;
  description: string;
}

export interface ChannelState {
  channel: Channel;
  present: boolean;
  /** 0..=1 on the desktop mixer's scale; null when absent. */
  volume: number | null;
}

export interface AppStream {
  id: number;
  name: string;
  /** What routing rules match on; null if the app can't be told apart. */
  binary: string | null;
  channel: Channel;
}

export type BandKind = "low_shelf" | "peaking" | "high_shelf";

export interface EqBand {
  kind: BandKind;
  freq: number;
  q: number;
  gain: number;
}

export interface ChannelEq {
  channel: Channel;
  bands: EqBand[];
}

export interface HeadphoneEq {
  preamp: number;
  bands: EqBand[];
  name: string | null;
}

export interface MicSettings {
  denoise: boolean;
  /** 0..=95 % */
  denoise_strength: number;
  gate: boolean;
  /** -80..=-10 dB */
  gate_threshold_db: number;
  compressor: boolean;
  /** 0..=18 dB */
  makeup_db: number;
}

export interface MicState {
  available: boolean;
  present: boolean;
  input: string | null;
  inputs: AudioDevice[];
  volume: number | null;
  settings: MicSettings;
}

/** Streamer mode: a second mix of the channels, for OBS. */
export interface StreamMix {
  enabled: boolean;
  /** Whether PipeWire has the "gglinux Stream" device now. */
  present: boolean;
  /** Per-channel stream level, 0..=1, in Game, Chat, Media, Aux order. */
  volumes: number[];
  master: number;
}

export type SmartLevel = "quiet" | "balanced" | "loud";

/** Smart Volume on one channel: quiet sounds come up, loud ones are held down. */
export interface SmartVolume {
  enabled: boolean;
  level: SmartLevel;
}

export interface SmartVolumeState {
  /** Whether the channels carry Smart Volume's stage, so it can be switched. */
  available: boolean;
  /** Why not: swh-plugins is missing, or PipeWire refused the stage. */
  reason: string | null;
  /** Per channel, in Game, Chat, Media, Aux order. */
  settings: SmartVolume[];
}

export interface SonarState {
  available: boolean;
  enabled: boolean;
  output: string | null;
  outputs: AudioDevice[];
  channels: ChannelState[];
  /** -1 all Game … 0 both full … 1 all Chat. */
  chatmix: number;
  apps: AppStream[];
  /** Per-channel EQ, in Game, Chat, Media, Aux order. */
  eq: ChannelEq[];
  headphone: HeadphoneEq;
  mic: MicState;
  stream: StreamMix;
  smart_volume: SmartVolumeState;
}

export interface Rgb {
  r: number;
  g: number;
  b: number;
}

export type Effect =
  | { kind: "static" }
  | { kind: "breathing"; color: Rgb; period_s: number }
  | { kind: "wave"; period_s: number }
  | { kind: "spectrum"; period_s: number }
  | { kind: "visualizer"; sensitivity: number };

export interface KeyColor {
  code: number;
  color: Rgb;
}

export interface MicMuteLight {
  enabled: boolean;
  /** HID code of the key */
  key: number;
  color: Rgb;
}

export type ReactiveMode = "fade" | "ripple";

export interface Reactive {
  enabled: boolean;
  mode: ReactiveMode;
  color: Rgb;
  /** seconds one press shows, 0.1..=3 */
  duration_s: number;
}

export interface Lighting {
  /** false leaves the lighting to the keyboard's own profile. */
  enabled: boolean;
  effect: Effect;
  keys: KeyColor[];
  /** 0..=100 % */
  brightness: number;
  reactive: Reactive;
  mic_mute: MicMuteLight;
}

export interface KeyInfo {
  code: number;
  label: string;
  x: number;
  y: number;
  w: number;
}

export interface KeyboardState {
  connected: boolean;
  layout: KeyInfo[];
  lighting: Lighting;
  /** Whether the daemon is reading key presses for reactive lighting. */
  reactive: "off" | "reading" | "no_access";
}

export interface GameInfo {
  name: string;
  display_name: string | null;
  developer: string | null;
  enabled: boolean;
  active: boolean;
  events: string[];
}

export interface GameSenseState {
  enabled: boolean;
  address: string | null;
  prefixes: number;
  games: GameInfo[];
}

export interface GameRule {
  process: string;
  profile: string;
}

export interface ProfilesState {
  profiles: string[];
  rules: GameRule[];
  active: GameRule | null;
  running: string[];
}

export type ClipQuality = "medium" | "high" | "very_high" | "ultra";

export interface ClipsSettings {
  /** Off by default: while on, the screen is kept in a rolling buffer. */
  enabled: boolean;
  /** How much a saved clip holds, 5..=300 s. */
  seconds: number;
  /** 15..=240 */
  fps: number;
  quality: ClipQuality;
  desktop_audio: boolean;
  mic: boolean;
  /** Absolute folder; null means `~/Videos/gglinux`. */
  dir: string | null;
  /** Record only while a program from the game rules runs. */
  games_only: boolean;
}

export interface ClipInfo {
  name: string;
  bytes: number;
  /** Seconds since the Unix epoch. */
  modified: number;
}

export interface ClipsState {
  /** `gpu-screen-recorder` is installed. */
  available: boolean;
  enabled: boolean;
  /** The recorder is running now. */
  recording: boolean;
  settings: ClipsSettings;
  /** The folder clips go to. */
  dir: string;
  /** Newest first. */
  clips: ClipInfo[];
  /** The last thing that went wrong. */
  error: string | null;
  /** The hotkey KDE bound, if any. */
  hotkey: string | null;
  /** Why there's no hotkey, once binding it has failed. */
  hotkey_error: string | null;
  /** The game from the rules that's running now. */
  game: string | null;
  /** On and games-only, with no game from the rules running. */
  waiting: boolean;
}

/** Request bodies — the `req` field of a {@link RequestFrame}. */
export type Request =
  | { method: "list_devices" }
  | { method: "ping" }
  | { method: "get_mouse_state" }
  | { method: "set_mouse_dpi"; params: { stages: DpiStage[]; active: number; save: boolean } }
  | { method: "set_mouse_polling"; params: { hz: number; save: boolean } }
  | { method: "set_mouse_buttons"; params: { actions: string[]; save: boolean } }
  | { method: "save_mouse" }
  | { method: "get_headset_state" }
  | { method: "set_headset_screen"; params: { settings: ScreenSettings } }
  | { method: "set_headset"; params: { change: HeadsetChange } }
  | { method: "get_sonar_state" }
  | { method: "set_sonar_enabled"; params: { enabled: boolean } }
  | { method: "set_sonar_output"; params: { name: string } }
  | { method: "set_channel_volume"; params: { channel: Channel; volume: number } }
  | { method: "set_chat_mix"; params: { balance: number } }
  | { method: "set_app_channel"; params: { binary: string; channel: Channel } }
  | { method: "set_channel_eq"; params: { channel: Channel; bands: EqBand[] } }
  | { method: "set_headphone_eq"; params: { eq: HeadphoneEq } }
  | { method: "import_auto_eq"; params: { text: string; name: string | null } }
  | { method: "set_mic_input"; params: { name: string } }
  | { method: "set_mic_settings"; params: { settings: MicSettings } }
  | { method: "set_mic_volume"; params: { volume: number } }
  | { method: "set_streamer"; params: { enabled: boolean } }
  | { method: "set_stream_volume"; params: { channel: Channel; volume: number } }
  | { method: "set_stream_master"; params: { volume: number } }
  | { method: "set_smart_volume"; params: { channel: Channel; settings: SmartVolume } }
  | { method: "get_keyboard_state" }
  | { method: "set_lighting"; params: { lighting: Lighting } }
  | { method: "get_gamesense_state" }
  | { method: "set_gamesense_enabled"; params: { enabled: boolean } }
  | { method: "set_game_enabled"; params: { game: string; enabled: boolean } }
  | { method: "remove_game"; params: { game: string } }
  | { method: "gamesense_test" }
  | { method: "get_profiles" }
  | { method: "save_profile"; params: { name: string } }
  | { method: "apply_profile"; params: { name: string } }
  | { method: "delete_profile"; params: { name: string } }
  | { method: "set_game_rules"; params: { rules: GameRule[] } }
  | { method: "get_clips_state" }
  | { method: "set_clips_settings"; params: { settings: ClipsSettings } }
  | { method: "save_clip" }
  | { method: "delete_clip"; params: { name: string } };

export interface RequestFrame {
  id: number;
  req: Request;
}

export type ResponseData =
  | { type: "devices"; devices: DeviceInfo[] }
  | { type: "pong" }
  | { type: "mouse"; state: MouseState }
  | { type: "headset"; state: HeadsetState }
  | { type: "sonar"; state: SonarState }
  | { type: "keyboard"; state: KeyboardState }
  | { type: "gamesense"; state: GameSenseState }
  | { type: "profiles"; state: ProfilesState }
  | { type: "clips"; state: ClipsState }
  | { type: "success" }
  | { type: "error"; message: string };

export type DaemonEvent =
  | { event: "device_added"; data: DeviceInfo }
  | { event: "device_removed"; data: { id: string } }
  | { event: "devices_changed"; data: { devices: DeviceInfo[] } }
  | { event: "mouse_battery"; data: { battery: BatteryState | null } }
  | { event: "headset_changed"; data: { state: HeadsetState } }
  | { event: "sonar_changed"; data: { state: SonarState } }
  | { event: "keyboard_changed"; data: { state: KeyboardState } }
  | { event: "gamesense_changed"; data: { state: GameSenseState } }
  | { event: "profiles_changed"; data: { state: ProfilesState } }
  | { event: "clips_changed"; data: { state: ClipsState } };

export type ServerMessage =
  | { kind: "response"; id: number; result: ResponseData }
  | { kind: "notification"; event: DaemonEvent };
