// SPDX-License-Identifier: GPL-3.0-or-later
//! The fake replay recorder, pretending gpu-screen-recorder is installed.

use gg_ipc::{ClipInfo, Request, ResponseData};

use crate::log::method_name;
use crate::{error, Fake};

pub fn handle(req: Request, s: &mut Fake) -> ResponseData {
    match req {
        Request::GetClipsState => ResponseData::Clips {
            state: s.clips.clone(),
        },
        Request::SetClipsSettings { settings } => {
            s.clips.enabled = settings.enabled;
            s.clips.recording = settings.enabled && !settings.games_only;
            s.clips.waiting = settings.enabled && settings.games_only;
            s.clips.settings = settings;
            ResponseData::Success
        }
        Request::SaveClip => {
            if s.clips.enabled {
                let n = s.clips.clips.len() + 1;
                let modified = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs());
                s.clips.clips.insert(
                    0,
                    ClipInfo {
                        name: format!("Replay_{n}.mp4"),
                        bytes: 1_000_000,
                        modified,
                    },
                );
                ResponseData::Success
            } else {
                ResponseData::Error {
                    message: "clips are off".into(),
                }
            }
        }
        Request::DeleteClip { name } => {
            s.clips.clips.retain(|c| c.name != name);
            ResponseData::Success
        }
        other => error(format!("{} is not a clips request", method_name(&other))),
    }
}
