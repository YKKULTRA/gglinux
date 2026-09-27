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

export interface MouseSettings {
  dpi_stages: DpiStage[];
  /** 0-based index into `dpi_stages`. */
  active_stage: number;
  polling_hz: number;
  /** One action name per physical button, in button order. */
  buttons: string[];
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
  | { method: "set_headset"; params: { change: HeadsetChange } };

export interface RequestFrame {
  id: number;
  req: Request;
}

export type ResponseData =
  | { type: "devices"; devices: DeviceInfo[] }
  | { type: "pong" }
  | { type: "mouse"; state: MouseState }
  | { type: "headset"; state: HeadsetState }
  | { type: "success" }
  | { type: "error"; message: string };

export type DaemonEvent =
  | { event: "device_added"; data: DeviceInfo }
  | { event: "device_removed"; data: { id: string } }
  | { event: "devices_changed"; data: { devices: DeviceInfo[] } }
  | { event: "mouse_battery"; data: { battery: BatteryState | null } }
  | { event: "headset_changed"; data: { state: HeadsetState } };

export type ServerMessage =
  | { kind: "response"; id: number; result: ResponseData }
  | { kind: "notification"; event: DaemonEvent };
