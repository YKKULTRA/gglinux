// SPDX-License-Identifier: GPL-3.0-or-later
//! The fake Sonar: a plausible one, changed as the client asks.

use anyhow::{bail, Result};
use gg_ipc::{
    AppStream, AudioDevice, Channel, ChannelEq, ChannelState, MicSettings, MicState, Request,
    ResponseData, SmartVolume, SmartVolumeState, SonarState, StreamMix,
};

use crate::log::method_name;
use crate::{error, Fake};

pub fn handle(req: Request, s: &mut Fake) -> ResponseData {
    match req {
        Request::GetSonarState => ResponseData::Sonar {
            state: s.sonar.clone(),
        },
        Request::SetSonarEnabled { enabled } => {
            s.sonar.enabled = enabled;
            // The daemon picks the default output when it has none yet.
            if enabled && s.sonar.output.is_none() {
                s.sonar.output = s.sonar.outputs.first().map(|o| o.name.clone());
            }
            ResponseData::Success
        }
        Request::SetSonarOutput { name } => {
            s.sonar.output = Some(name);
            ResponseData::Success
        }
        Request::SetChannelVolume { channel, volume } => {
            if let Some(c) = s.sonar.channels.iter_mut().find(|c| c.channel == channel) {
                c.volume = Some(volume);
            }
            ResponseData::Success
        }
        Request::SetChatMix { balance } => {
            s.sonar.chatmix = balance;
            ResponseData::Success
        }
        Request::SetAppChannel { binary, channel } => {
            for app in s
                .sonar
                .apps
                .iter_mut()
                .filter(|a| a.binary.as_deref() == Some(binary.as_str()))
            {
                app.channel = channel;
            }
            ResponseData::Success
        }
        Request::SetChannelEq { channel, bands } => {
            if let Some(c) = s.sonar.eq.iter_mut().find(|c| c.channel == channel) {
                c.bands = bands;
            }
            ResponseData::Success
        }
        Request::SetHeadphoneEq { eq } => {
            s.sonar.headphone = eq;
            ResponseData::Success
        }
        // Parsed as the daemon parses it, so a bad paste fails the same way.
        Request::ImportAutoEq { text, name } => match gg_audio::eq::parse_autoeq(&text) {
            Ok(mut eq) => {
                eq.name = name.map(|n| n.trim().to_owned()).filter(|n| !n.is_empty());
                s.sonar.headphone = eq;
                ResponseData::Success
            }
            Err(e) => error(format!("{e:#}")),
        },
        Request::SetMicInput { name } => {
            s.sonar.mic.input = Some(name);
            ResponseData::Success
        }
        Request::SetMicSettings { settings } => {
            s.sonar.mic.settings = settings;
            ResponseData::Success
        }
        Request::SetMicVolume { volume } => {
            s.sonar.mic.volume = Some(volume);
            ResponseData::Success
        }
        Request::SetStreamer { enabled } => {
            s.sonar.stream.enabled = enabled;
            s.sonar.stream.present = enabled;
            ResponseData::Success
        }
        Request::SetStreamVolume { channel, volume } => {
            if let Some(i) = Channel::ALL.iter().position(|&c| c == channel) {
                s.sonar.stream.volumes[i] = volume;
            }
            ResponseData::Success
        }
        Request::SetStreamMaster { volume } => {
            s.sonar.stream.master = volume;
            ResponseData::Success
        }
        Request::SetSmartVolume { channel, settings } => {
            if let Some(i) = Channel::ALL.iter().position(|&c| c == channel) {
                s.sonar.smart_volume.settings[i] = settings;
            }
            ResponseData::Success
        }
        other => error(format!("{} is not a Sonar request", method_name(&other))),
    }
}

/// A plausible Sonar: on, two outputs, two apps.
pub fn fake_sonar() -> SonarState {
    SonarState {
        available: true,
        enabled: true,
        output: Some("alsa_output.fake-headset".into()),
        outputs: vec![
            AudioDevice {
                name: "alsa_output.fake-headset".into(),
                description: "Fake Headset".into(),
            },
            AudioDevice {
                name: "alsa_output.fake-speakers".into(),
                description: "Fake Speakers".into(),
            },
        ],
        channels: Channel::ALL
            .iter()
            .map(|&channel| ChannelState {
                channel,
                present: true,
                volume: Some(1.0),
            })
            .collect(),
        chatmix: 0.0,
        apps: vec![
            AppStream {
                id: 90,
                name: "Discord".into(),
                binary: Some("discord".into()),
                channel: Channel::Chat,
            },
            AppStream {
                id: 91,
                name: "Firefox".into(),
                binary: Some("firefox".into()),
                channel: Channel::Media,
            },
        ],
        eq: Channel::ALL
            .iter()
            .map(|&channel| ChannelEq {
                channel,
                bands: gg_audio::eq::flat_channel(),
            })
            .collect(),
        headphone: gg_audio::eq::flat_headphone(),
        mic: MicState {
            available: true,
            present: true,
            input: Some("alsa_input.fake-headset".into()),
            inputs: vec![AudioDevice {
                name: "alsa_input.fake-headset".into(),
                description: "Fake Headset Mic".into(),
            }],
            volume: Some(1.0),
            settings: MicSettings::default(),
        },
        stream: StreamMix {
            enabled: false,
            present: false,
            volumes: vec![1.0; 4],
            master: 1.0,
        },
        smart_volume: SmartVolumeState {
            available: true,
            reason: None,
            settings: vec![SmartVolume::default(); 4],
        },
    }
}

/// Sonar in one of the states the Sonar page has to show (`--sonar`).
pub fn sonar_variant(s: &mut SonarState, variant: &str) -> Result<()> {
    match variant {
        "off" => {
            s.enabled = false;
            s.output = None;
        }
        "unavailable" => s.available = false,
        // Sonar on, but without the optional plugins, any app playing, or the Aux sink.
        "bare" => {
            s.apps.clear();
            s.channels[3].present = false;
            s.channels[3].volume = None;
            s.mic.available = false;
            s.mic.present = false;
            s.smart_volume.available = false;
            s.smart_volume.reason = Some(
                "Smart Volume needs the SC4 compressor from swh-plugins (sc4m_1916.so), \
                 which isn't installed. Install swh-plugins, then turn Sonar off and on."
                    .into(),
            );
            s.stream.enabled = true;
            s.stream.present = false;
        }
        other => bail!("--sonar {other}: expected off, unavailable or bare"),
    }
    Ok(())
}
