// SPDX-License-Identifier: GPL-3.0-or-later
//! What the fake prints for each request: its wire name, and its values.

use gg_ipc::Request;

pub fn method_name(req: &Request) -> &'static str {
    match req {
        Request::ListDevices => "list_devices",
        Request::Ping => "ping",
        Request::GetMouseState => "get_mouse_state",
        Request::SetMouseDpi { .. } => "set_mouse_dpi",
        Request::SetMousePolling { .. } => "set_mouse_polling",
        Request::SetMouseButtons { .. } => "set_mouse_buttons",
        Request::SaveMouse => "save_mouse",
        Request::GetHeadsetState => "get_headset_state",
        Request::SetHeadsetScreen { .. } => "set_headset_screen",
        Request::SetHeadset { .. } => "set_headset",
        Request::GetSonarState => "get_sonar_state",
        Request::SetSonarEnabled { .. } => "set_sonar_enabled",
        Request::SetSonarOutput { .. } => "set_sonar_output",
        Request::SetChannelVolume { .. } => "set_channel_volume",
        Request::SetChatMix { .. } => "set_chat_mix",
        Request::SetAppChannel { .. } => "set_app_channel",
        Request::SetChannelEq { .. } => "set_channel_eq",
        Request::SetHeadphoneEq { .. } => "set_headphone_eq",
        Request::ImportAutoEq { .. } => "import_auto_eq",
        Request::SetMicInput { .. } => "set_mic_input",
        Request::SetMicSettings { .. } => "set_mic_settings",
        Request::SetMicVolume { .. } => "set_mic_volume",
        Request::SetStreamer { .. } => "set_streamer",
        Request::SetStreamVolume { .. } => "set_stream_volume",
        Request::SetStreamMaster { .. } => "set_stream_master",
        Request::SetSmartVolume { .. } => "set_smart_volume",
        Request::GetKeyboardState => "get_keyboard_state",
        Request::SetLighting { .. } => "set_lighting",
        Request::SetSwitches { .. } => "set_switches",
        Request::SetScreen { .. } => "set_screen",
        Request::SetScreenCare { .. } => "set_screen_care",
        Request::GetParts => "get_parts",
        Request::SetParts { .. } => "set_parts",
        Request::GetExperimental => "get_experimental",
        Request::SetExperimental { .. } => "set_experimental",
        Request::GetBindings => "get_bindings",
        Request::ReadBindings => "read_bindings",
        Request::SetBindings { .. } => "set_bindings",
        Request::GetOnboard => "get_onboard",
        Request::ReadOnboard { .. } => "read_onboard",
        Request::LoadOnboard { .. } => "load_onboard",
        Request::WriteOnboard { .. } => "write_onboard",
        Request::GetActions => "get_actions",
        Request::SetActions { .. } => "set_actions",
        Request::GetGamesenseState => "get_gamesense_state",
        Request::SetGamesenseEnabled { .. } => "set_gamesense_enabled",
        Request::SetGameEnabled { .. } => "set_game_enabled",
        Request::RemoveGame { .. } => "remove_game",
        Request::GamesenseTest => "gamesense_test",
        Request::GetProfiles => "get_profiles",
        Request::SaveProfile { .. } => "save_profile",
        Request::ApplyProfile { .. } => "apply_profile",
        Request::DeleteProfile { .. } => "delete_profile",
        Request::SetGameRules { .. } => "set_game_rules",
        Request::GetClipsState => "get_clips_state",
        Request::SetClipsSettings { .. } => "set_clips_settings",
        Request::SaveClip => "save_clip",
        Request::DeleteClip { .. } => "delete_clip",
    }
}

/// One-line rendering of a request, so the log shows values and not just methods.
pub fn describe(req: &Request) -> String {
    match req {
        Request::SetMouseDpi {
            stages,
            active,
            save,
        } => {
            let list: Vec<String> = stages
                .iter()
                .map(|s| {
                    if s.x == s.y {
                        s.x.to_string()
                    } else {
                        format!("{}:{}", s.x, s.y)
                    }
                })
                .collect();
            format!(
                "set_mouse_dpi {} active={active} save={save}",
                list.join(",")
            )
        }
        Request::SetMousePolling { hz, save } => format!("set_mouse_polling {hz}Hz save={save}"),
        Request::SetMouseButtons { actions, save } => {
            format!("set_mouse_buttons {} save={save}", actions.join(","))
        }
        Request::SetHeadsetScreen { settings } => format!(
            "set_headset_screen protect={} lit={}s brightness={:?}",
            settings.protect, settings.lit_seconds, settings.brightness
        ),
        Request::SetHeadset { change } => format!("set_headset {change:?}"),
        // Everything else with its params as they go over the wire.
        other => match serde_json::to_value(other)
            .ok()
            .and_then(|mut v| v.get_mut("params").map(serde_json::Value::take))
        {
            Some(params) => format!("{} {params}", method_name(other)),
            None => method_name(other).to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gg_ipc::{Channel, Effect, Lighting, Rgb};

    #[test]
    fn a_request_with_no_rendering_of_its_own_is_logged_with_its_params() {
        let volume = Request::SetChannelVolume {
            channel: Channel::Chat,
            volume: 0.25,
        };
        assert_eq!(
            describe(&volume),
            r#"set_channel_volume {"channel":"chat","volume":0.25}"#
        );

        let lighting = Lighting {
            enabled: true,
            effect: Effect::Breathing {
                color: Rgb { r: 255, g: 0, b: 0 },
                period_s: 3.0,
            },
            ..Lighting::default()
        };
        let shown = describe(&Request::SetLighting { lighting });
        assert!(shown.starts_with("set_lighting {"), "{shown}");
        assert!(
            shown.contains(r#""kind":"breathing""#) && shown.contains(r#""period_s":3.0"#),
            "{shown}"
        );

        // A request that carries nothing is still just its name.
        assert_eq!(describe(&Request::SaveClip), "save_clip");
    }
}
