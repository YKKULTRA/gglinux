// SPDX-License-Identifier: GPL-3.0-or-later
// Desktop chores the UI process does itself, not through the daemon.

import { invoke } from "@tauri-apps/api/core";

export const getAutostart = () => invoke<boolean>("get_autostart");
export interface SessionInfo {
  /** XDG_CURRENT_DESKTOP, or "". */
  desktop: string;
  /** Whether the desktop runs autostart entries (Hyprland doesn't). */
  reads_autostart: boolean;
  /** Starts this app hidden in the tray, quoted for a shell. */
  tray_command: string;
}
export const sessionInfo = () => invoke<SessionInfo>("session_info");
export const setAutostart = (enabled: boolean) => invoke<void>("set_autostart", { enabled });
/** Writes the backup into ~/Documents (or home) and returns its path. */
export const exportSettings = () => invoke<string>("export_settings");
/** Restores a backup's text, restarts gglinuxd, and says what happened. */
export const importSettings = (text: string) => invoke<string>("import_settings", { text });
/** Show a folder in the file manager. */
export const openFolder = (path: string) => invoke<void>("open_folder", { path });
