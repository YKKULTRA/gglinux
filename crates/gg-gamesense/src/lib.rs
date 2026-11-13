// SPDX-License-Identifier: GPL-3.0-or-later
//! SteelSeries **GameSense**: what games send, and what it paints on the
//! Apex Pro TKL Gen 3: its keys and its screen. Pure — the HTTP server and the
//! clock live in the daemon.

pub mod bitmap;
pub mod paint;
pub mod prefixes;
pub mod proto;
pub mod registry;
pub mod screen;
pub mod zones;

pub use registry::{Draws, GameSummary, Layer, Registry, SavedEvent, SavedGame, ScreenNow};
