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

/** Request bodies — the `req` field of a {@link RequestFrame}. */
export type Request =
  | { method: "list_devices" }
  | { method: "ping" }
  | { method: "get_mouse_state" }
  | { method: "set_mouse_dpi"; params: { stages: DpiStage[]; active: number; save: boolean } }
  | { method: "set_mouse_polling"; params: { hz: number; save: boolean } }
  | { method: "set_mouse_buttons"; params: { actions: string[]; save: boolean } }
  | { method: "save_mouse" };

export interface RequestFrame {
  id: number;
  req: Request;
}

export type ResponseData =
  | { type: "devices"; devices: DeviceInfo[] }
  | { type: "pong" }
  | { type: "mouse"; state: MouseState }
  | { type: "success" }
  | { type: "error"; message: string };

export type DaemonEvent =
  | { event: "device_added"; data: DeviceInfo }
  | { event: "device_removed"; data: { id: string } }
  | { event: "devices_changed"; data: { devices: DeviceInfo[] } }
  | { event: "mouse_battery"; data: { battery: BatteryState | null } };

export type ServerMessage =
  | { kind: "response"; id: number; result: ResponseData }
  | { kind: "notification"; event: DaemonEvent };
