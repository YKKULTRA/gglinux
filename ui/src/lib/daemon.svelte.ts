// Typed client for gglinuxd.
//
// The webview cannot open a unix socket, so the Rust side (src-tauri) owns the
// connection and relays it: every NDJSON line the daemon sends is re-emitted as a
// `daemon://message` Tauri event, and `send` pushes a request frame back down.
// Responses arrive asynchronously and are correlated by request id.

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type {
  DeviceInfo,
  DpiStage,
  HeadsetState,
  MouseState,
  Request,
  ResponseData,
  ScreenSettings,
  ServerMessage,
} from "./types";

/** Reject a pending request if the daemon never answers. */
const REQUEST_TIMEOUT_MS = 5000;

interface Pending {
  resolve: (r: ResponseData) => void;
  reject: (e: Error) => void;
  timer: ReturnType<typeof setTimeout>;
}

class Daemon {
  /** True while the bridge holds a live socket to gglinuxd. */
  connected = $state(false);
  /** Latest device snapshot, kept current by push notifications. */
  devices = $state<DeviceInfo[]>([]);
  /** Mouse battery + applied settings; null until first fetched. */
  mouse = $state<MouseState | null>(null);
  /** Headset battery, power, and screen settings; null until first fetched. */
  headset = $state<HeadsetState | null>(null);
  /** Last transport/protocol error, for surfacing in the UI. */
  lastError = $state<string | null>(null);

  #pending = new Map<number, Pending>();
  #nextId = 1;
  #started = false;

  /** Subscribe to bridge events. Safe to call more than once. */
  async start(): Promise<void> {
    if (this.#started) return;
    this.#started = true;

    await listen<ServerMessage>("daemon://message", (e) => this.#onMessage(e.payload));
    await listen<{ connected: boolean }>("daemon://status", (e) => {
      this.connected = e.payload.connected;
      if (!this.connected) {
        this.devices = [];
        // `applied` describes what *that* daemon process wrote. If it restarted it
        // has written nothing, so keeping the old value would be a lie.
        this.mouse = null;
        this.headset = null;
        this.#failAllPending("daemon disconnected");
      }
    });

    // The bridge may have connected before we subscribed.
    this.connected = await invoke<boolean>("daemon_status");
    if (this.connected) {
      void this.listDevices();
    }
  }

  /** Send a request and resolve with the daemon's response. */
  request(req: Request): Promise<ResponseData> {
    const id = this.#nextId++;
    return new Promise<ResponseData>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.#pending.delete(id);
        reject(new Error(`request ${req.method} timed out`));
      }, REQUEST_TIMEOUT_MS);
      this.#pending.set(id, { resolve, reject, timer });

      invoke("daemon_send", { frame: { id, req } }).catch((e: unknown) => {
        clearTimeout(timer);
        this.#pending.delete(id);
        reject(e instanceof Error ? e : new Error(String(e)));
      });
    });
  }

  async listDevices(): Promise<DeviceInfo[]> {
    const res = await this.request({ method: "list_devices" });
    if (res.type !== "devices") throw new Error(`unexpected response: ${res.type}`);
    this.devices = res.devices;
    return res.devices;
  }

  /** Refresh {@link mouse}. The daemon rate-limits the underlying battery read,
   *  so calling this on a timer is cheap. */
  async getMouseState(): Promise<MouseState> {
    const res = await this.request({ method: "get_mouse_state" });
    if (res.type !== "mouse") throw new Error(`unexpected response: ${res.type}`);
    this.mouse = res.state;
    return res.state;
  }

  // Each mutation refreshes `mouse` afterwards: the daemon owns the record of what
  // has been applied, and only it knows whether the write actually landed.

  async setMouseDpi(stages: DpiStage[], active: number, save: boolean): Promise<void> {
    await this.#command({ method: "set_mouse_dpi", params: { stages, active, save } });
  }

  async setMousePolling(hz: number, save: boolean): Promise<void> {
    await this.#command({ method: "set_mouse_polling", params: { hz, save } });
  }

  async setMouseButtons(actions: string[], save: boolean): Promise<void> {
    await this.#command({ method: "set_mouse_buttons", params: { actions, save } });
  }

  /** Commit whatever is currently applied to the mouse's onboard memory. */
  async saveMouse(): Promise<void> {
    await this.#command({ method: "save_mouse" });
  }

  async getHeadsetState(): Promise<HeadsetState> {
    const res = await this.request({ method: "get_headset_state" });
    if (res.type !== "headset") throw new Error(`unexpected response: ${res.type}`);
    this.headset = res.state;
    return res.state;
  }

  async setHeadsetScreen(settings: ScreenSettings): Promise<void> {
    const res = await this.request({ method: "set_headset_screen", params: { settings } });
    if (res.type !== "success") throw new Error(`unexpected response: ${res.type}`);
    await this.getHeadsetState();
  }

  async #command(req: Request): Promise<void> {
    const res = await this.request(req);
    if (res.type !== "success") throw new Error(`unexpected response: ${res.type}`);
    await this.getMouseState();
  }

  #onMessage(msg: ServerMessage): void {
    if (msg.kind === "response") {
      const p = this.#pending.get(msg.id);
      if (!p) return; // late response to a timed-out request
      this.#pending.delete(msg.id);
      clearTimeout(p.timer);
      if (msg.result.type === "error") {
        this.lastError = msg.result.message;
        p.reject(new Error(msg.result.message));
      } else {
        p.resolve(msg.result);
      }
      return;
    }

    const ev = msg.event;
    switch (ev.event) {
      case "devices_changed":
        this.devices = ev.data.devices;
        break;
      case "device_added":
        // Replace-or-append so a re-announce doesn't duplicate the entry.
        this.devices = [...this.devices.filter((d) => d.id !== ev.data.id), ev.data].sort((a, b) =>
          a.id.localeCompare(b.id),
        );
        break;
      case "device_removed":
        this.devices = this.devices.filter((d) => d.id !== ev.data.id);
        break;
      case "mouse_battery":
        // Dropped if we have no state yet; the panel fetches one when it mounts.
        if (this.mouse) this.mouse = { ...this.mouse, battery: ev.data.battery };
        break;
      case "headset_changed":
        this.headset = ev.data.state;
        break;
    }
  }

  #failAllPending(reason: string): void {
    for (const [, p] of this.#pending) {
      clearTimeout(p.timer);
      p.reject(new Error(reason));
    }
    this.#pending.clear();
  }
}

export const daemon = new Daemon();
