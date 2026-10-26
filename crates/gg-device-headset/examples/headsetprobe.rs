//! Read-only probe for the Nova Pro Wireless base station.
//!
//! ```text
//! headsetprobe            # one status read, decoded and raw
//! headsetprobe events     # stream interrupt reports (turn the ChatMix dial)
//! ```
//!
//! Sends nothing but `06 b0` (a status request) and reads. It changes no setting,
//! so it is safe to run against a headset in use — which is the point: it confirms
//! the transport before any setter is trusted.

use std::time::{Duration, Instant};

use anyhow::Result;
use gg_device_headset::{proto, Headset};

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let api = hidapi::HidApi::new()?;
    let headset = Headset::open(&api)?;

    if std::env::args().nth(1).as_deref() == Some("events") {
        return stream_events(&headset);
    }

    println!("status request: 06 {:02x}", proto::status_packet()[0]);
    match headset.status()? {
        Some(s) => {
            println!("\ndecoded:");
            println!("  power            {:?}", s.power);
            match s.headset_battery {
                Some(p) => println!("  headset battery  {p}%"),
                None => println!("  headset battery  n/a (headset offline)"),
            }
            match s.spare_battery {
                Some(p) => println!("  spare pack       {p}%"),
                None => println!("  spare pack       none seated"),
            }
            println!("  auto-off         {:?}", s.auto_off.map(|t| t.minutes()));
            println!("  wireless mode    {:?}", s.wireless_mode);
            println!("  mic LED          {}", s.mic_led_brightness);
            println!("  mic muted        {:?}", s.mic_muted());
            println!("  noise mode       {:?}", s.noise_mode());
            println!("  transparency     {}", s.transparency_raw);
        }
        None => println!("\nno status reply — base station did not answer"),
    }
    Ok(())
}

/// Print interrupt reports as they arrive. The dial is the easiest one to trigger.
fn stream_events(headset: &Headset) -> Result<()> {
    println!("streaming interrupts for 30s — turn the ChatMix dial or the volume knob\n");
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if let Some(report) = headset.read_event()? {
            let tail: Vec<String> = report.iter().take(8).map(|b| format!("{b:02x}")).collect();
            let label = match report.get(1) {
                Some(0x45) => "chatmix dial",
                Some(0x25) => "volume",
                Some(0x31) => "eq band",
                Some(0xbd) => "anc",
                Some(0xb9) => "transparency",
                Some(0xb0) => "status",
                _ => "unknown",
            };
            println!("{:<14} {}", label, tail.join(" "));
        }
    }
    Ok(())
}
