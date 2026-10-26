// SPDX-License-Identifier: GPL-3.0-or-later
// Stroke-only 24x24 icon paths, rendered by <Icon>. Kept as data so nav/device
// tables can reference icons by name without a component per glyph.

export const ICONS = {
  engine: [
    "M3 6.5h18v11H3z",
    "M7 10h.01 M10.5 10h.01 M14 10h.01 M17 10h.01 M8.5 14h7",
  ],
  sonar: ["M11 5 6.5 9H3v6h3.5L11 19z", "M15.5 9.2a4 4 0 0 1 0 5.6", "M18 6.7a7.5 7.5 0 0 1 0 10.6"],
  moments: ["M3 7.5h11.5v9H3z", "M14.5 12l6.5-3.5v7L14.5 12z"],
  games: ["M7 8.5h10a4.5 4.5 0 0 1 4.5 4.5v.5a3.5 3.5 0 0 1-6.4 2l-.9-1.3h-4.4l-.9 1.3a3.5 3.5 0 0 1-6.4-2V13A4.5 4.5 0 0 1 7 8.5z", "M8 11v3 M6.5 12.5h3", "M15.5 12h.01 M17.5 13.5h.01"],
  settings: [
    "M12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6z",
    "M19.1 14.4a1.5 1.5 0 0 0 .3 1.7l.1.1a1.9 1.9 0 1 1-2.7 2.7l-.1-.1a1.5 1.5 0 0 0-2.5 1v.2a1.9 1.9 0 1 1-3.8 0v-.1a1.5 1.5 0 0 0-2.5-1l-.1.1a1.9 1.9 0 1 1-2.7-2.7l.1-.1a1.5 1.5 0 0 0-1-2.5H4a1.9 1.9 0 1 1 0-3.8h.1a1.5 1.5 0 0 0 1-2.5l-.1-.1a1.9 1.9 0 1 1 2.7-2.7l.1.1a1.5 1.5 0 0 0 2.5-1V4a1.9 1.9 0 1 1 3.8 0v.1a1.5 1.5 0 0 0 2.5 1l.1-.1a1.9 1.9 0 1 1 2.7 2.7l-.1.1a1.5 1.5 0 0 0 1 2.5h.2a1.9 1.9 0 1 1 0 3.8h-.1a1.5 1.5 0 0 0-1.4 1z",
  ],
  keyboard: ["M2.5 6.5h19v11h-19z", "M6 10h.01 M9.5 10h.01 M13 10h.01 M16.5 10h.01 M8 14h8"],
  mouse: ["M12 2.5a6 6 0 0 1 6 6v7a6 6 0 0 1-12 0v-7a6 6 0 0 1 6-6z", "M12 6.5v4"],
  headset: [
    "M4 14v-2a8 8 0 0 1 16 0v2",
    "M4 13.5h2.5a1 1 0 0 1 1 1v3a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1z",
    "M20 13.5h-2.5a1 1 0 0 0-1 1v3a1 1 0 0 0 1 1H19a1 1 0 0 0 1-1z",
  ],
  other: ["M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18z", "M12 8h.01 M12 11.5v5"],
} as const;

export type IconName = keyof typeof ICONS;
