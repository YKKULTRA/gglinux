// SPDX-License-Identifier: GPL-3.0-or-later
//! Prove a setter reaches the hardware, then put it back.
//!
//! ```text
//! writecheck            # mic-LED brightness: read, change, confirm, restore
//! writecheck --hold 5   # linger 5s at the changed value so it can be eyeballed
//! ```
//!
//! Mic-LED brightness is the one setting on this device that closes the loop
//! entirely in software: the status block reports it back, so the current value is
//! known before the write, the change is confirmable after it, and the original can
//! be restored exactly. Nothing else here has all three — sidetone and EQ are
//! audible but absent from the readback, so confirming them means asking a human
//! and restoring them means trusting a remembered value.
//!
//! Deliberately never sends `save` (`06 09`). Without it the change lives in the
//! base station's RAM, so a power cycle undoes anything this leaves behind.

use std::time::Duration;

use anyhow::{bail, Context, Result};
use gg_device_headset::{Headset, Status};

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let hold: u64 = match std::env::args().collect::<Vec<_>>().as_slice() {
        [_, flag, secs] if flag == "--hold" => secs.parse().context("--hold")?,
        [_] => 0,
        _ => bail!("usage: writecheck [--hold <seconds>]"),
    };

    let api = hidapi::HidApi::new()?;
    let headset = Headset::open(&api)?;

    let before = read(&headset, "before")?;
    let original = before.mic_led_brightness;
    // Any value that is not the current one; the contrast just has to be visible.
    let probe = if original > 5 { 2 } else { 9 };
    println!("\nmic LED brightness is {original}; setting it to {probe}");

    headset.set_mic_led_brightness(probe)?;
    let after = read(&headset, "after write")?;

    if hold > 0 {
        println!("holding {hold}s — look at the mic-mute LED");
        std::thread::sleep(Duration::from_secs(hold));
    }

    // Restore before reporting the verdict, so a failed assertion still leaves the
    // device as it was found.
    println!("\nrestoring {original}");
    headset.set_mic_led_brightness(original)?;
    let restored = read(&headset, "after restore")?;

    println!();
    if after.mic_led_brightness != probe {
        bail!(
            "write did NOT land: asked for {probe}, device still reports {}",
            after.mic_led_brightness
        );
    }
    println!("✓ write landed: {original} -> {probe}, confirmed by readback");

    if restored.mic_led_brightness != original {
        bail!(
            "restore failed: wanted {original}, device reports {}",
            restored.mic_led_brightness
        );
    }
    println!("✓ restored to {original}, confirmed by readback");
    println!("\nno save was sent — nothing was written to the base station's flash");
    Ok(())
}

fn read(headset: &Headset, label: &str) -> Result<Status> {
    let status = headset
        .status()?
        .with_context(|| format!("no status reply ({label})"))?;
    println!("{label:<14} mic LED {}", status.mic_led_brightness);
    Ok(status)
}
