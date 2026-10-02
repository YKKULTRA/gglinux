//! Minimal CLI for poking the Rival 3 Wireless Gen 2 directly, without the daemon.
//!
//! ```text
//! mousectl                                  # read-only: battery + status
//! mousectl dpi 800,1600 [--select 0] [--save]
//! mousectl dpi 400:800,1600:1600 --save     # per-axis X:Y
//! mousectl polling 1000 [--save]
//! mousectl buttons button1,button2,button3,button4,button5,dpi [--save]
//! ```
//!
//! Settings take effect immediately but are lost when the mouse sleeps unless you
//! pass `--save`, which commits them to onboard memory.

use anyhow::{bail, Context, Result};
use gg_device_mouse::{proto, ButtonAction, DpiStage, Mouse, PollingRate, BUTTON_COUNT};
use hidapi::HidApi;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let save = args.iter().any(|a| a == "--save");
    let positional = positional(&args);

    let api = HidApi::new().context("initialising hidapi")?;
    let mouse = Mouse::open(&api)?;

    match positional.split_first() {
        None => {
            report_battery(&mouse)?;
            return Ok(());
        }
        Some((&"dpi", rest)) => {
            let spec = rest.first().context("usage: mousectl dpi 800,1600")?;
            let stages = parse_stages(spec)?;
            let selected = flag_value(&args, "--select")
                .map(|v| v.parse::<usize>().context("--select expects a number"))
                .transpose()?
                .unwrap_or(0);
            mouse.set_dpi(&stages, selected)?;
            println!("set {} DPI stage(s), stage {} active", stages.len(), selected + 1);
        }
        Some((&"polling", rest)) => {
            let hz: u16 = rest
                .first()
                .context("usage: mousectl polling 1000")?
                .parse()
                .context("polling rate must be a number")?;
            let rate = PollingRate::from_hz(hz)?;
            mouse.set_polling_rate(rate)?;
            println!("set polling rate to {} Hz", rate.hz());
        }
        Some((&"buttons", rest)) => {
            let spec = rest
                .first()
                .context("usage: mousectl buttons button1,button2,...,dpi")?;
            let actions = parse_buttons(spec)?;
            mouse.set_buttons(&actions)?;
            println!("remapped {BUTTON_COUNT} buttons");
        }
        Some((other, _)) => bail!("unknown command '{other}' (try: dpi, polling, buttons)"),
    }

    if save {
        mouse.save()?;
        println!("committed to onboard memory");
    } else {
        println!("note: not saved — pass --save to keep this across a power cycle");
    }
    Ok(())
}

fn report_battery(mouse: &Mouse) -> Result<()> {
    match mouse.battery()? {
        Some(b) => println!(
            "battery: {}%{}",
            b.level,
            if b.charging { " (charging)" } else { "" }
        ),
        None => println!("battery: unknown (mouse asleep — move it and retry)"),
    }
    Ok(())
}

/// The arguments that aren't `--flags`, or the value that follows `--select`.
fn positional(args: &[String]) -> Vec<&str> {
    let mut words = Vec::new();
    let mut args = args.iter().map(String::as_str);
    while let Some(a) = args.next() {
        if a == "--select" {
            // Its number, which would otherwise be taken for the DPI list.
            args.next();
        } else if !a.starts_with("--") {
            words.push(a);
        }
    }
    words
}

fn flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    let i = args.iter().position(|a| a == flag)?;
    args.get(i + 1).map(String::as_str)
}

/// Parse `800,1600` or `400:800,1600:1600` into DPI stages.
fn parse_stages(spec: &str) -> Result<Vec<DpiStage>> {
    spec.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|part| {
            Ok(match part.split_once(':') {
                Some((x, y)) => DpiStage {
                    x: x.trim().parse().context("invalid X DPI")?,
                    y: y.trim().parse().context("invalid Y DPI")?,
                },
                None => DpiStage::uniform(part.parse().context("invalid DPI")?),
            })
        })
        .collect()
}

fn parse_buttons(spec: &str) -> Result<[ButtonAction; BUTTON_COUNT]> {
    let parsed: Vec<ButtonAction> = spec
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ButtonAction::parse)
        .collect::<Result<_>>()?;

    if parsed.len() != BUTTON_COUNT {
        bail!(
            "expected {BUTTON_COUNT} comma-separated actions, got {}. Default is: {}",
            parsed.len(),
            "button1,button2,button3,button4,button5,dpi"
        );
    }
    let mut actions = proto::default_buttons();
    actions.copy_from_slice(&parsed);
    Ok(actions)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(line: &str) -> Vec<String> {
        line.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn the_select_value_is_not_read_as_the_dpi_list() {
        for line in ["dpi --select 0 800,1600 --save", "dpi 800,1600 --select 0 --save"] {
            assert_eq!(positional(&args(line)), ["dpi", "800,1600"], "{line}");
        }
    }
}
