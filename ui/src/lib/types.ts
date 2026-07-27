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

/** Request bodies — the `req` field of a {@link RequestFrame}. */
export type Request = { method: "list_devices" } | { method: "ping" };

export interface RequestFrame {
  id: number;
  req: Request;
}

export type ResponseData =
  | { type: "devices"; devices: DeviceInfo[] }
  | { type: "pong" }
  | { type: "error"; message: string };

export type DaemonEvent =
  | { event: "device_added"; data: DeviceInfo }
  | { event: "device_removed"; data: { id: string } }
  | { event: "devices_changed"; data: { devices: DeviceInfo[] } };

export type ServerMessage =
  | { kind: "response"; id: number; result: ResponseData }
  | { kind: "notification"; event: DaemonEvent };
