//! Sonar worker: channel routing on its own thread.
//!
//! Every PipeWire action shells out to PipeWire's tools and can block — enabling
//! restarts PipeWire and waits for it — so it runs off the async runtime, like
//! the device workers.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use gg_audio::sonar::Sonar;
use gg_audio::tools::SystemTools;
use gg_ipc::{Channel, EqBand, Event, HeadphoneEq, MicSettings, SmartVolume, SonarState};
use tokio::sync::{broadcast, oneshot, watch};

use crate::keyboard;

/// How often to look for new app streams while Sonar is on.
const TICK: Duration = Duration::from_secs(1);

/// Reply channel for a command. `Err` carries a message meant for the user.
type Reply<T> = oneshot::Sender<Result<T, String>>;

pub enum Command {
    GetState(Reply<SonarState>),
    SetEnabled {
        enabled: bool,
        reply: Reply<()>,
    },
    SetOutput {
        name: String,
        reply: Reply<()>,
    },
    SetVolume {
        channel: Channel,
        volume: f32,
        reply: Reply<()>,
    },
    SetChatMix {
        balance: f32,
        reply: Reply<()>,
    },
    SetAppChannel {
        binary: String,
        channel: Channel,
        reply: Reply<()>,
    },
    SetChannelEq {
        channel: Channel,
        bands: Vec<EqBand>,
        reply: Reply<()>,
    },
    SetHeadphoneEq {
        eq: HeadphoneEq,
        reply: Reply<()>,
    },
    ImportAutoEq {
        text: String,
        name: Option<String>,
        reply: Reply<()>,
    },
    SetMicInput {
        name: String,
        reply: Reply<()>,
    },
    SetMicSettings {
        settings: MicSettings,
        reply: Reply<()>,
    },
    SetMicVolume {
        volume: f32,
        reply: Reply<()>,
    },
    SetStreamer {
        enabled: bool,
        reply: Reply<()>,
    },
    SetStreamVolume {
        channel: Channel,
        volume: f32,
        reply: Reply<()>,
    },
    SetStreamMaster {
        volume: f32,
        reply: Reply<()>,
    },
    SetSmartVolume {
        channel: Channel,
        settings: SmartVolume,
        reply: Reply<()>,
    },
}

#[derive(Clone)]
pub struct Handle {
    tx: Sender<Command>,
}

impl Handle {
    pub async fn send<T>(&self, make: impl FnOnce(Reply<T>) -> Command) -> Result<T, String> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(make(reply_tx))
            .map_err(|_| "Sonar worker is not running".to_string())?;
        reply_rx
            .await
            .map_err(|_| "Sonar worker dropped the request".to_string())?
    }
}

pub fn spawn(
    event_tx: broadcast::Sender<Event>,
    keyboard: keyboard::Handle,
    mixed_tx: watch::Sender<Option<String>>,
) -> Handle {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("gg-sonar".into())
        .spawn(move || worker(rx, event_tx, keyboard, mixed_tx))
        .expect("spawn gg-sonar thread");
    Handle { tx }
}

fn worker(
    rx: Receiver<Command>,
    event_tx: broadcast::Sender<Event>,
    keyboard: keyboard::Handle,
    mixed_tx: watch::Sender<Option<String>>,
) {
    let mut sonar = Sonar::new(
        SystemTools,
        gg_audio::settings::path(),
        gg_audio::config::dropin_path(),
    );
    let mut published: Option<SonarState> = None;
    // What the keyboard's visualizer and clips were last told to listen to;
    // they start on the default output.
    let mut heard: Option<String> = None;
    let mut last_tick = Instant::now();
    loop {
        match rx.recv_timeout(TICK) {
            Ok(command) => handle(command, &mut sonar),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        // By elapsed time, so a stream of commands cannot starve routing.
        if sonar.is_enabled() && last_tick.elapsed() >= TICK {
            sonar.tick();
            last_tick = Instant::now();
        }
        let now = sonar.snapshot();
        if published.as_ref() != Some(&now) {
            let output = mixed_output(now.enabled, &now.output);
            if output != heard {
                keyboard.listen_to(output.clone());
                mixed_tx.send_replace(output.clone());
                heard = output;
            }
            published = Some(now.clone());
            let _ = event_tx.send(Event::SonarChanged { state: now });
        }
    }
}

/// Where everything playing mixes, for the keyboard's visualizer and clips'
/// desktop audio: with Sonar on, the output its channels play into, since the
/// default output is then the Game channel alone; with it off, none — the
/// default output has it all.
fn mixed_output(enabled: bool, output: &Option<String>) -> Option<String> {
    output.clone().filter(|_| enabled)
}

fn handle(command: Command, sonar: &mut Sonar<SystemTools>) {
    let text = |e: anyhow::Error| format!("{e:#}");
    match command {
        Command::GetState(reply) => {
            let _ = reply.send(Ok(sonar.state()));
        }
        Command::SetEnabled { enabled, reply } => {
            let result = if enabled {
                sonar.enable()
            } else {
                sonar.disable()
            };
            let _ = reply.send(result.map_err(text));
        }
        Command::SetOutput { name, reply } => {
            let _ = reply.send(sonar.set_output(&name).map_err(text));
        }
        Command::SetVolume {
            channel,
            volume,
            reply,
        } => {
            let _ = reply.send(sonar.set_volume(channel, volume).map_err(text));
        }
        Command::SetChatMix { balance, reply } => {
            let _ = reply.send(sonar.set_chatmix(balance).map_err(text));
        }
        Command::SetAppChannel {
            binary,
            channel,
            reply,
        } => {
            let _ = reply.send(sonar.set_app_channel(&binary, channel).map_err(text));
        }
        Command::SetChannelEq {
            channel,
            bands,
            reply,
        } => {
            let _ = reply.send(sonar.set_channel_eq(channel, bands).map_err(text));
        }
        Command::SetHeadphoneEq { eq, reply } => {
            let _ = reply.send(sonar.set_headphone_eq(eq).map_err(text));
        }
        Command::ImportAutoEq {
            text: eq_text,
            name,
            reply,
        } => {
            let _ = reply.send(sonar.import_autoeq(&eq_text, name).map_err(text));
        }
        Command::SetMicInput { name, reply } => {
            let _ = reply.send(sonar.set_mic_input(&name).map_err(text));
        }
        Command::SetMicSettings { settings, reply } => {
            let _ = reply.send(sonar.set_mic_settings(settings).map_err(text));
        }
        Command::SetMicVolume { volume, reply } => {
            let _ = reply.send(sonar.set_mic_volume(volume).map_err(text));
        }
        Command::SetStreamer { enabled, reply } => {
            let _ = reply.send(sonar.set_streamer(enabled).map_err(text));
        }
        Command::SetStreamVolume {
            channel,
            volume,
            reply,
        } => {
            let _ = reply.send(sonar.set_stream_volume(channel, volume).map_err(text));
        }
        Command::SetStreamMaster { volume, reply } => {
            let _ = reply.send(sonar.set_stream_master(volume).map_err(text));
        }
        Command::SetSmartVolume {
            channel,
            settings,
            reply,
        } => {
            let _ = reply.send(sonar.set_smart_volume(channel, settings).map_err(text));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_visualizer_hears_sonars_output_only_while_sonar_is_on() {
        let output = Some("alsa_output.usb-SteelSeries".to_owned());
        assert_eq!(
            mixed_output(true, &output),
            output,
            "the default output is only the Game channel then"
        );
        assert_eq!(
            mixed_output(false, &output),
            None,
            "Sonar off: the default output has it all"
        );
    }
}
