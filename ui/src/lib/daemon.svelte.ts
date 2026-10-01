// Typed client for gglinuxd.
//
// The webview cannot open a unix socket, so the Rust side (src-tauri) owns the
// connection and relays it: every NDJSON line the daemon sends is re-emitted as a
// `daemon://message` Tauri event, and `send` pushes a request frame back down.
// Responses arrive asynchronously and are correlated by request id.

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type {
  Channel,
  ClipsSettings,
  ClipsState,
  DeviceInfo,
  DpiStage,
  EqBand,
  HeadphoneEq,
  HeadsetChange,
  HeadsetState,
  KeyboardState,
  GameSenseState,
  GameRule,
  ProfilesState,
  Lighting,
  MicSettings,
  MouseState,
  Request,
  ResponseData,
  ScreenSettings,
  ServerMessage,
  SonarState,
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
  /** Sonar channels, routing and ChatMix; null until first fetched. */
  sonar = $state<SonarState | null>(null);
  /** Keyboard layout and lighting; null until first fetched. */
  keyboard = $state<KeyboardState | null>(null);
  /** GameSense server and games; null until first fetched. */
  gamesense = $state<GameSenseState | null>(null);
  /** Profiles and game rules; null until first fetched. */
  profiles = $state<ProfilesState | null>(null);
  /** The replay recorder, its settings and the clip library; null until first fetched. */
  clips = $state<ClipsState | null>(null);
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
        this.sonar = null;
        this.keyboard = null;
        this.gamesense = null;
        this.profiles = null;
        this.clips = null;
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
  request(req: Request, timeoutMs = REQUEST_TIMEOUT_MS): Promise<ResponseData> {
    const id = this.#nextId++;
    return new Promise<ResponseData>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.#pending.delete(id);
        reject(new Error(`request ${req.method} timed out`));
      }, timeoutMs);
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

  async setHeadset(change: HeadsetChange): Promise<void> {
    const res = await this.request({ method: "set_headset", params: { change } });
    if (res.type !== "success") throw new Error(`unexpected response: ${res.type}`);
    await this.getHeadsetState();
  }

  async getSonarState(): Promise<SonarState> {
    const res = await this.request({ method: "get_sonar_state" });
    if (res.type !== "sonar") throw new Error(`unexpected response: ${res.type}`);
    this.sonar = res.state;
    return res.state;
  }

  /** Every Sonar change answers `success`, then the fresh state is fetched. */
  async #sonarCommand(req: Request, timeoutMs?: number): Promise<void> {
    const res = await this.request(req, timeoutMs);
    if (res.type !== "success") throw new Error(`unexpected response: ${res.type}`);
    await this.getSonarState();
  }

  /** Restarts PipeWire and waits for it, so it gets a longer timeout. */
  setSonarEnabled(enabled: boolean): Promise<void> {
    return this.#sonarCommand({ method: "set_sonar_enabled", params: { enabled } }, 15000);
  }

  setSonarOutput(name: string): Promise<void> {
    return this.#sonarCommand({ method: "set_sonar_output", params: { name } });
  }

  setChannelVolume(channel: Channel, volume: number): Promise<void> {
    return this.#sonarCommand({ method: "set_channel_volume", params: { channel, volume } });
  }

  setChatMix(balance: number): Promise<void> {
    return this.#sonarCommand({ method: "set_chat_mix", params: { balance } });
  }

  setAppChannel(binary: string, channel: Channel): Promise<void> {
    return this.#sonarCommand({ method: "set_app_channel", params: { binary, channel } });
  }

  setChannelEq(channel: Channel, bands: EqBand[]): Promise<void> {
    return this.#sonarCommand({ method: "set_channel_eq", params: { channel, bands } });
  }

  setHeadphoneEq(eq: HeadphoneEq): Promise<void> {
    return this.#sonarCommand({ method: "set_headphone_eq", params: { eq } });
  }

  importAutoEq(text: string, name: string | null): Promise<void> {
    return this.#sonarCommand({ method: "import_auto_eq", params: { text, name } });
  }

  setMicInput(name: string): Promise<void> {
    return this.#sonarCommand({ method: "set_mic_input", params: { name } });
  }

  setMicSettings(settings: MicSettings): Promise<void> {
    return this.#sonarCommand({ method: "set_mic_settings", params: { settings } });
  }

  setMicVolume(volume: number): Promise<void> {
    return this.#sonarCommand({ method: "set_mic_volume", params: { volume } });
  }

  /** Restarts PipeWire while Sonar is on, so it gets the same longer timeout. */
  setStreamer(enabled: boolean): Promise<void> {
    return this.#sonarCommand({ method: "set_streamer", params: { enabled } }, 15000);
  }

  setStreamVolume(channel: Channel, volume: number): Promise<void> {
    return this.#sonarCommand({ method: "set_stream_volume", params: { channel, volume } });
  }

  setStreamMaster(volume: number): Promise<void> {
    return this.#sonarCommand({ method: "set_stream_master", params: { volume } });
  }

  async getKeyboardState(): Promise<KeyboardState> {
    const res = await this.request({ method: "get_keyboard_state" });
    if (res.type !== "keyboard") throw new Error(`unexpected response: ${res.type}`);
    this.keyboard = res.state;
    return res.state;
  }

  async setLighting(lighting: Lighting): Promise<void> {
    const res = await this.request({ method: "set_lighting", params: { lighting } });
    if (res.type !== "success") throw new Error(`unexpected response: ${res.type}`);
    await this.getKeyboardState();
  }

  async getGamesenseState(): Promise<GameSenseState> {
    const res = await this.request({ method: "get_gamesense_state" });
    if (res.type !== "gamesense") throw new Error(`unexpected response: ${res.type}`);
    this.gamesense = res.state;
    return res.state;
  }

  setGamesenseEnabled(enabled: boolean): Promise<void> {
    return this.#gamesenseCommand({ method: "set_gamesense_enabled", params: { enabled } });
  }

  setGameEnabled(game: string, enabled: boolean): Promise<void> {
    return this.#gamesenseCommand({ method: "set_game_enabled", params: { game, enabled } });
  }

  removeGame(game: string): Promise<void> {
    return this.#gamesenseCommand({ method: "remove_game", params: { game } });
  }

  gamesenseTest(): Promise<void> {
    return this.#gamesenseCommand({ method: "gamesense_test" });
  }

  async getProfiles(): Promise<ProfilesState> {
    const res = await this.request({ method: "get_profiles" });
    if (res.type !== "profiles") throw new Error(`unexpected response: ${res.type}`);
    this.profiles = res.state;
    return res.state;
  }

  saveProfile(name: string): Promise<void> {
    return this.#profilesCommand({ method: "save_profile", params: { name } });
  }

  applyProfile(name: string): Promise<void> {
    return this.#profilesCommand({ method: "apply_profile", params: { name } });
  }

  deleteProfile(name: string): Promise<void> {
    return this.#profilesCommand({ method: "delete_profile", params: { name } });
  }

  setGameRules(rules: GameRule[]): Promise<void> {
    return this.#profilesCommand({ method: "set_game_rules", params: { rules } });
  }

  async getClipsState(): Promise<ClipsState> {
    const res = await this.request({ method: "get_clips_state" });
    if (res.type !== "clips") throw new Error(`unexpected response: ${res.type}`);
    this.clips = res.state;
    return res.state;
  }

  /** Restarts the recorder, which can take it 4 s to stop, so it gets a longer timeout. */
  setClipsSettings(settings: ClipsSettings): Promise<void> {
    return this.#clipsCommand({ method: "set_clips_settings", params: { settings } }, 10000);
  }

  /** The new file shows up a moment later, in a `clips_changed` event. */
  saveClip(): Promise<void> {
    return this.#clipsCommand({ method: "save_clip" });
  }

  deleteClip(name: string): Promise<void> {
    return this.#clipsCommand({ method: "delete_clip", params: { name } });
  }

  async #clipsCommand(req: Request, timeoutMs?: number): Promise<void> {
    const res = await this.request(req, timeoutMs);
    if (res.type !== "success") throw new Error(`unexpected response: ${res.type}`);
    await this.getClipsState();
  }

  async #profilesCommand(req: Request): Promise<void> {
    const res = await this.request(req);
    if (res.type !== "success") throw new Error(`unexpected response: ${res.type}`);
    await this.getProfiles();
  }

  async #gamesenseCommand(req: Request): Promise<void> {
    const res = await this.request(req);
    if (res.type !== "success") throw new Error(`unexpected response: ${res.type}`);
    await this.getGamesenseState();
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
      case "sonar_changed":
        this.sonar = ev.data.state;
        break;
      case "keyboard_changed":
        this.keyboard = ev.data.state;
        break;
      case "gamesense_changed":
        this.gamesense = ev.data.state;
        break;
      case "profiles_changed":
        // Events carry no running-program list; keep the last one fetched.
        this.profiles = { ...ev.data.state, running: this.profiles?.running ?? [] };
        break;
      case "clips_changed":
        this.clips = ev.data.state;
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
