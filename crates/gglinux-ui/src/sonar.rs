// SPDX-License-Identifier: GPL-3.0-or-later
//! Sonar: channels, routing, ChatMix, Smart Volume, the stream mix, the mic and
//! the EQ.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gg_ipc::{
    AudioDevice, BandKind, Channel, EqBand, HeadphoneEq, MicSettings, Part, Request, ResponseData,
    SmartLevel, SmartVolume, SonarState,
};
use slint::{ComponentHandle, SharedString};

use crate::client::{Client, State, SLOW, TIMEOUT};
use crate::eq::{self, Field};
use crate::{sync_rows, AppWindow, EqHandle, SonarApp, SonarChannel, SonarUi};

/// While dragging, a change goes out at most this often, and once more on release.
const SEND_EVERY: Duration = Duration::from_millis(150);
const HEADPHONE: i32 = 4;
const LEVELS: [SmartLevel; 3] = [SmartLevel::Quiet, SmartLevel::Balanced, SmartLevel::Loud];
/// What the output and input boxes say before Sonar has picked one.
const UNCHOSEN: &str = "chosen when Sonar turns on";
/// An AutoEQ file is a few hundred bytes; anything this big isn't one.
const AUTOEQ_MAX: u64 = 64 * 1024;

/// What the Sonar page keeps between the daemon's answers.
#[derive(Default)]
pub struct SonarForm {
    /// The EQ tab: 0..4 the channels, 4 the headphone correction.
    pub tab: i32,
    /// The bands being edited: they follow the daemon's, except mid-drag and
    /// until the answer to the last change of them.
    pub bands: Vec<EqBand>,
    pub dragging: bool,
    last_sent: Option<Instant>,
    /// The tab and number of the last EQ change sent, until it's answered.
    eq_sent: Option<(i32, u64)>,
    eq_count: u64,
    /// The band whose numbers are shown for typing.
    pub selected: Option<usize>,
    /// What the number boxes were last filled with: they're filled again only
    /// when the band changes, not under someone typing.
    boxes: Option<(i32, usize, EqBand)>,
    /// Settings that go out whole, from controls that don't wait for the answer.
    /// A change made before the last one's answer builds on what that one sent:
    /// built from the state, which doesn't have it yet, it would undo it. Till
    /// the answer, the page shows these.
    mic: Option<MicSettings>,
    headphone: Option<HeadphoneEq>,
    smart: [Option<SmartVolume>; 4],
}

impl SonarForm {
    /// Take the daemon's bands for the tab, unless a drag or an unanswered
    /// change holds the ones here: the state may still have older ones.
    pub fn follow(&mut self, s: &SonarState) {
        let held = self.dragging || self.eq_sent.is_some_and(|(tab, _)| tab == self.tab);
        if !held {
            self.bands = tab_bands(s, self.tab);
        }
        if self.selected.is_some_and(|i| i >= self.bands.len()) {
            self.selected = None;
        }
    }

    pub fn pick_tab(&mut self, s: &SonarState, tab: i32) {
        self.tab = tab.clamp(0, HEADPHONE);
        self.dragging = false;
        self.selected = None;
        self.bands = tab_bands(s, self.tab);
    }

    /// Band `i` dragged to (x, y) of the picture. The bands to send, if it's
    /// time to send again.
    pub fn drag(&mut self, i: usize, x: f32, y: f32, now: Instant) -> Option<Vec<EqBand>> {
        let lim = limit(self.tab);
        eq::dragged(self.bands.get_mut(i)?, x, y, lim);
        self.dragging = true;
        self.selected = Some(i);
        if self
            .last_sent
            .is_some_and(|t| now.duration_since(t) < SEND_EVERY)
        {
            return None;
        }
        self.last_sent = Some(now);
        Some(self.bands.clone())
    }

    /// Let go: the bands to send a last time, if anything was dragged.
    pub fn release(&mut self, now: Instant) -> Option<Vec<EqBand>> {
        if !self.dragging {
            return None;
        }
        self.dragging = false;
        self.last_sent = Some(now);
        Some(self.bands.clone())
    }

    pub fn wheel(&mut self, i: usize, up: bool) -> Option<Vec<EqBand>> {
        eq::wheeled(self.bands.get_mut(i)?, up);
        self.selected = Some(i);
        Some(self.bands.clone())
    }

    pub fn reset_gains(&mut self) -> Vec<EqBand> {
        for b in self.bands.iter_mut() {
            b.gain = 0.0;
        }
        self.bands.clone()
    }

    /// A value typed for the selected band: the bands to send if it changed
    /// one, or an error if it isn't a number.
    pub fn enter(&mut self, field: Field, text: &str) -> Result<Option<Vec<EqBand>>, String> {
        let lim = limit(self.tab);
        let Some(b) = self.selected.and_then(|i| self.bands.get_mut(i)) else {
            return Ok(None);
        };
        let before = *b;
        // Shown again as it was, or as it was taken (rounded, in range).
        self.boxes = None;
        eq::typed(b, field, text, lim)
            .ok_or_else(|| format!("\"{}\" isn't a number.", text.trim()))?;
        Ok((*b != before).then(|| self.bands.clone()))
    }

    /// A change of `tab`'s bands goes out; its number, for [`Self::answered`].
    fn sending(&mut self, tab: i32) -> u64 {
        self.eq_count += 1;
        self.eq_sent = Some((tab, self.eq_count));
        self.eq_count
    }

    fn answered(&mut self, tab: i32, n: u64) {
        if self.eq_sent == Some((tab, n)) {
            self.eq_sent = None;
        }
    }
}

/// A whole setting with `patch` applied, built on the one still in flight if
/// there is one; it's in flight from now on.
fn build_on<T: Clone>(pending: &mut Option<T>, current: &T, patch: impl FnOnce(&mut T)) -> T {
    let mut whole = pending.clone().unwrap_or_else(|| current.clone());
    patch(&mut whole);
    *pending = Some(whole.clone());
    whole
}

/// `sent` was answered: unless a later one went out since, nothing's in flight.
fn settle<T: PartialEq>(pending: &mut Option<T>, sent: &T) {
    if pending.as_ref() == Some(sent) {
        *pending = None;
    }
}

/// One mic setting from the page, in the daemon's steps and ranges.
pub fn patch_mic(m: &mut MicSettings, field: &str, v: f32) {
    match field {
        "denoise" => m.denoise = v != 0.0,
        "strength" => m.denoise_strength = ((v / 5.0).round() * 5.0).clamp(0.0, 95.0),
        "gate" => m.gate = v != 0.0,
        "gate-db" => m.gate_threshold_db = v.round().clamp(-80.0, -10.0),
        "compressor" => m.compressor = v != 0.0,
        "makeup" => m.makeup_db = v.round().clamp(0.0, 18.0),
        _ => {}
    }
}

/// The names for a device box and which is chosen. When the chosen one isn't
/// in the list (none yet, or unplugged) a stand-in for it comes first.
pub fn choices(devices: &[AudioDevice], chosen: Option<&str>) -> (Vec<String>, i32) {
    let names = devices.iter().map(|d| d.description.clone());
    match chosen.and_then(|c| devices.iter().position(|d| d.name == c)) {
        Some(i) => (names.collect(), i as i32),
        None => {
            let stand_in = chosen.map_or(UNCHOSEN.to_string(), |c| format!("{c} (not connected)"));
            (std::iter::once(stand_in).chain(names).collect(), 0)
        }
    }
}

/// The device picked at `index` of [`choices`]' list; `None` for the stand-in.
pub fn picked<'a>(
    devices: &'a [AudioDevice],
    chosen: Option<&str>,
    index: i32,
) -> Option<&'a AudioDevice> {
    let listed = chosen.is_some_and(|c| devices.iter().any(|d| d.name == c));
    let index = if listed { index } else { index - 1 };
    usize::try_from(index).ok().and_then(|i| devices.get(i))
}

/// A headphone name from an AutoEQ file's name ("HD 600 ParametricEQ.txt").
pub fn autoeq_name(file: &str) -> Option<String> {
    let stem = file.strip_suffix(".txt").unwrap_or(file);
    let name = stem.strip_suffix("ParametricEQ").unwrap_or(stem).trim();
    (!name.is_empty()).then(|| name.to_string())
}

fn read_autoeq(path: &Path) -> Result<(String, Option<String>), String> {
    let size = std::fs::metadata(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?
        .len();
    if size > AUTOEQ_MAX {
        return Err("That file is too big to be an AutoEQ ParametricEQ.txt.".into());
    }
    let text = std::fs::read_to_string(path).map_err(|_| "That file isn't text.".to_string())?;
    let name = path
        .file_name()
        .and_then(|n| autoeq_name(&n.to_string_lossy()));
    Ok((text, name))
}

fn channel(i: i32) -> Channel {
    Channel::ALL[(i.clamp(0, 3)) as usize]
}

fn channel_index(c: Channel) -> i32 {
    Channel::ALL.iter().position(|x| *x == c).unwrap_or(0) as i32
}

fn label(c: Channel) -> &'static str {
    match c {
        Channel::Game => "Game",
        Channel::Chat => "Chat",
        Channel::Media => "Media",
        Channel::Aux => "Aux",
    }
}

/// The bands the tab shows, as the daemon has them.
pub fn tab_bands(s: &SonarState, tab: i32) -> Vec<EqBand> {
    if tab == HEADPHONE {
        s.headphone.bands.clone()
    } else {
        s.eq.iter()
            .find(|c| c.channel == channel(tab))
            .map(|c| c.bands.clone())
            .unwrap_or_default()
    }
}

fn limit(tab: i32) -> f32 {
    if tab == HEADPHONE {
        20.0
    } else {
        12.0
    }
}

fn strings(v: Vec<String>) -> Vec<SharedString> {
    v.into_iter().map(SharedString::from).collect()
}

fn show_eq(ui: &AppWindow, s: &SonarState, f: &mut SonarForm) {
    let g = ui.global::<SonarUi>();
    let lim = limit(f.tab);
    let preamp = if f.tab == HEADPHONE {
        f.headphone.as_ref().unwrap_or(&s.headphone).preamp
    } else {
        0.0
    };
    g.set_eq_tab(f.tab);
    g.set_eq_limit(lim);
    g.set_eq_path(eq::path(&f.bands, preamp, lim).into());
    let handles: Vec<EqHandle> = f
        .bands
        .iter()
        .map(|b| EqHandle {
            x: eq::x(b.freq.clamp(eq::F_MIN, eq::F_MAX)),
            y: eq::y(b.gain.clamp(-lim, lim), lim),
            shelf: b.kind != BandKind::Peaking,
            tip: eq::tip(b).into(),
        })
        .collect();
    sync_rows(g.get_eq_handles(), handles, |m| g.set_eq_handles(m));
    match f.selected.and_then(|i| f.bands.get(i).map(|b| (i, *b))) {
        Some((i, b)) => {
            g.set_eq_band(i as i32);
            g.set_eq_band_name(eq::band_name(i, b.kind).into());
            if f.boxes != Some((f.tab, i, b)) {
                let [freq, gain, q] = eq::boxes(&b);
                g.set_eq_freq(freq.into());
                g.set_eq_gain(gain.into());
                g.set_eq_q(q.into());
                f.boxes = Some((f.tab, i, b));
            }
        }
        None => {
            g.set_eq_band(-1);
            f.boxes = None;
        }
    }
}

pub fn show(ui: &AppWindow, st: &State, form: &RefCell<SonarForm>) {
    let g = ui.global::<SonarUi>();
    let Some(s) = st.sonar.as_ref() else {
        g.set_known(false);
        return;
    };
    if !g.get_known() {
        // Whatever failed while it was loading is moot now.
        g.set_error(SharedString::new());
    }
    let mut f = form.borrow_mut();
    g.set_known(true);
    g.set_available(s.available);
    g.set_enabled(s.enabled);
    let (outputs, output) = choices(&s.outputs, s.output.as_deref());
    sync_rows(g.get_outputs(), strings(outputs), |m| g.set_outputs(m));
    g.set_output(output);
    let rows: Vec<SonarChannel> = s
        .channels
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let sv = f
                .smart
                .get(i)
                .copied()
                .flatten()
                .or_else(|| s.smart_volume.settings.get(i).copied())
                .unwrap_or_default();
            SonarChannel {
                name: label(c.channel).into(),
                present: c.present,
                volume: c.volume.unwrap_or(-1.0),
                smart: sv.enabled,
                level: LEVELS.iter().position(|l| *l == sv.level).unwrap_or(1) as i32,
                stream: s.stream.volumes.get(i).copied().unwrap_or(1.0),
            }
        })
        .collect();
    sync_rows(g.get_channels(), rows, |m| g.set_channels(m));
    g.set_smart_available(s.smart_volume.available);
    g.set_smart_reason(s.smart_volume.reason.clone().unwrap_or_default().into());
    g.set_stream(s.stream.enabled);
    g.set_stream_present(s.stream.present);
    g.set_stream_master(s.stream.master);
    g.set_chatmix(s.chatmix);
    let apps: Vec<SonarApp> = s
        .apps
        .iter()
        .map(|a| SonarApp {
            name: a.name.clone().into(),
            channel: channel_index(a.channel),
            routable: a.binary.is_some(),
        })
        .collect();
    sync_rows(g.get_apps(), apps, |m| g.set_apps(m));
    let m = &s.mic;
    g.set_mic_available(m.available);
    g.set_mic_present(m.present);
    let (inputs, input) = choices(&m.inputs, m.input.as_deref());
    sync_rows(g.get_mic_inputs(), strings(inputs), |m| g.set_mic_inputs(m));
    g.set_mic_input(input);
    g.set_mic_volume(m.volume.unwrap_or(-1.0));
    let mic = f.mic.unwrap_or(m.settings);
    g.set_denoise(mic.denoise);
    g.set_strength(mic.denoise_strength);
    g.set_gate(mic.gate);
    g.set_gate_db(mic.gate_threshold_db);
    g.set_compressor(mic.compressor);
    g.set_makeup(mic.makeup_db);
    let headphone = f.headphone.as_ref().unwrap_or(&s.headphone);
    g.set_preamp(headphone.preamp);
    g.set_preamp_text(format!("{} dB", headphone.preamp).into());
    g.set_headphone(s.headphone.name.clone().unwrap_or_default().into());
    f.follow(s);
    show_eq(ui, s, &mut f);
}

/// Show Sonar again from the last state, after a change of the page's own.
fn redraw(ui: &AppWindow, client: &Client) {
    crate::with_forms(|forms| show(ui, &client.state(), &forms.sonar));
}

/// Marks the page busy with `tag` until the answer; after it, shows what's true
/// either way (a pick that failed goes back to what the daemon has).
fn mark(
    tag: &'static str,
    client: &Client,
) -> impl Fn(&AppWindow, bool, SharedString) + Send + Clone + 'static {
    let client = client.clone();
    move |ui: &AppWindow, busy: bool, error: SharedString| {
        let g = ui.global::<SonarUi>();
        g.set_busy(if busy {
            tag.into()
        } else {
            SharedString::new()
        });
        g.set_error(error);
        if !busy {
            redraw(ui, &client);
        }
    }
}

/// Send without marking the page busy (a drag shouldn't be cut short by its own
/// request); only an error shows. `after` runs on the working copy once the
/// answer is in, before the page is shown again.
fn quietly(
    ui: &AppWindow,
    client: &Client,
    req: Request,
    after: impl FnOnce(&mut SonarForm) + Send + 'static,
) {
    ui.global::<SonarUi>().set_error(SharedString::new());
    let weak = ui.as_weak();
    let c = client.clone();
    client.command(req, TIMEOUT, move |result| {
        let _ = weak.upgrade_in_event_loop(move |ui| {
            crate::with_forms(|forms| after(&mut forms.sonar.borrow_mut()));
            if let Err(e) = result {
                ui.global::<SonarUi>().set_error(e.into());
            }
            redraw(&ui, &c);
        });
    });
}

fn sonar(client: &Client) -> Option<SonarState> {
    client.state().sonar
}

/// A whole setting sent built on the last one in flight, or on the state's;
/// the page shows it at once.
fn send_whole<T: Clone + PartialEq + Send + 'static>(
    ui: &AppWindow,
    client: &Client,
    slot: fn(&mut SonarForm) -> &mut Option<T>,
    current: T,
    patch: impl FnOnce(&mut T),
    make: fn(T) -> Request,
    after: impl FnOnce(&mut SonarForm) + Send + 'static,
) {
    let whole =
        crate::with_forms(|forms| build_on(slot(&mut forms.sonar.borrow_mut()), &current, patch));
    let sent = whole.clone();
    redraw(ui, client);
    quietly(ui, client, make(whole), move |f| {
        settle(slot(f), &sent);
        after(f);
    });
}

/// The edited bands, sent to the tab they belong to. The bands here stand until
/// the answer to the last of these.
fn send_bands(ui: &AppWindow, client: &Client, tab: i32, bands: Vec<EqBand>) {
    let Some(s) = sonar(client) else { return };
    let n = crate::with_forms(|forms| forms.sonar.borrow_mut().sending(tab));
    let answered = move |f: &mut SonarForm| f.answered(tab, n);
    if tab == HEADPHONE {
        send_whole(
            ui,
            client,
            |f| &mut f.headphone,
            s.headphone,
            move |h| h.bands = bands,
            |eq| Request::SetHeadphoneEq { eq },
            answered,
        );
    } else {
        let req = Request::SetChannelEq {
            channel: channel(tab),
            bands,
        };
        quietly(ui, client, req, answered);
    }
}

type SmartSlot = fn(&mut SonarForm) -> &mut Option<SmartVolume>;

/// Smart Volume goes out whole per channel, with an in-flight slot each.
const SMART_SLOTS: [SmartSlot; 4] = [
    |f| &mut f.smart[0],
    |f| &mut f.smart[1],
    |f| &mut f.smart[2],
    |f| &mut f.smart[3],
];
const SMART_REQUESTS: [fn(SmartVolume) -> Request; 4] = [
    |settings| Request::SetSmartVolume {
        channel: Channel::Game,
        settings,
    },
    |settings| Request::SetSmartVolume {
        channel: Channel::Chat,
        settings,
    },
    |settings| Request::SetSmartVolume {
        channel: Channel::Media,
        settings,
    },
    |settings| Request::SetSmartVolume {
        channel: Channel::Aux,
        settings,
    },
];

/// Live: the channel's own controls, never a PipeWire restart.
fn smart(ui: &AppWindow, client: &Client, i: i32, patch: impl FnOnce(&mut SmartVolume)) {
    let Some(s) = sonar(client) else { return };
    let idx = (i.clamp(0, 3)) as usize;
    let current = s
        .smart_volume
        .settings
        .get(idx)
        .copied()
        .unwrap_or_default();
    send_whole(
        ui,
        client,
        SMART_SLOTS[idx],
        current,
        patch,
        SMART_REQUESTS[idx],
        |_| {},
    );
}

pub fn bind(ui: &AppWindow, client: &Client, form: Rc<RefCell<SonarForm>>) {
    let g = ui.global::<SonarUi>();
    let weak = ui.as_weak();
    let c = client.clone();
    g.on_load(move || {
        // Only when it's stuck: connected, Sonar running, and no state read.
        let st = c.state();
        if !st.connected || st.sonar.is_some() || !st.running().sonar {
            return;
        }
        let weak = weak.clone();
        let again = c.clone();
        c.request(
            Request::GetSonarState,
            TIMEOUT,
            move |answer| match answer {
                Ok(ResponseData::Sonar { .. }) => again.refresh_part(Part::Sonar),
                Ok(_) => {}
                Err(e) => {
                    let _ = weak.upgrade_in_event_loop(move |ui| {
                        ui.global::<SonarUi>().set_error(e.into())
                    });
                }
            },
        );
    });
    let weak = ui.as_weak();
    let c = client.clone();
    g.on_toggle(move || {
        let (Some(ui), Some(s)) = (weak.upgrade(), sonar(&c)) else {
            return;
        };
        // Restarts PipeWire and waits for it.
        crate::act(
            &ui,
            &c,
            Request::SetSonarEnabled {
                enabled: !s.enabled,
            },
            SLOW,
            mark("enable", &c),
        );
    });
    let weak = ui.as_weak();
    let c = client.clone();
    g.on_pick_output(move |i| {
        let (Some(ui), Some(s)) = (weak.upgrade(), sonar(&c)) else {
            return;
        };
        match picked(&s.outputs, s.output.as_deref(), i) {
            Some(o) if s.output.as_deref() != Some(o.name.as_str()) => crate::act(
                &ui,
                &c,
                Request::SetSonarOutput {
                    name: o.name.clone(),
                },
                TIMEOUT,
                mark("output", &c),
            ),
            // The stand-in, or the one already chosen.
            _ => redraw(&ui, &c),
        }
    });
    let weak = ui.as_weak();
    let c = client.clone();
    g.on_set_volume(move |i, v| {
        let Some(ui) = weak.upgrade() else { return };
        crate::act(
            &ui,
            &c,
            Request::SetChannelVolume {
                channel: channel(i),
                volume: v.clamp(0.0, 1.0),
            },
            TIMEOUT,
            mark("volume", &c),
        );
    });
    let weak = ui.as_weak();
    let c = client.clone();
    g.on_set_smart_on(move |i, on| {
        let Some(ui) = weak.upgrade() else { return };
        smart(&ui, &c, i, |sv| sv.enabled = on);
    });
    let weak = ui.as_weak();
    let c = client.clone();
    g.on_set_smart_level(move |i, level| {
        let Some(ui) = weak.upgrade() else { return };
        smart(&ui, &c, i, |sv| {
            sv.level = LEVELS[(level.clamp(0, 2)) as usize]
        });
    });
    let weak = ui.as_weak();
    let c = client.clone();
    g.on_set_streamer(move |on| {
        let Some(ui) = weak.upgrade() else { return };
        // Restarts PipeWire while Sonar is on.
        crate::act(
            &ui,
            &c,
            Request::SetStreamer { enabled: on },
            SLOW,
            mark("streamer", &c),
        );
    });
    let weak = ui.as_weak();
    let c = client.clone();
    g.on_set_stream_volume(move |i, v| {
        let Some(ui) = weak.upgrade() else { return };
        let req = Request::SetStreamVolume {
            channel: channel(i),
            volume: v.clamp(0.0, 1.0),
        };
        quietly(&ui, &c, req, |_| {});
    });
    let weak = ui.as_weak();
    let c = client.clone();
    g.on_set_stream_master(move |v| {
        let Some(ui) = weak.upgrade() else { return };
        let req = Request::SetStreamMaster {
            volume: v.clamp(0.0, 1.0),
        };
        quietly(&ui, &c, req, |_| {});
    });
    let weak = ui.as_weak();
    let c = client.clone();
    g.on_set_chatmix(move |v| {
        let Some(ui) = weak.upgrade() else { return };
        crate::act(
            &ui,
            &c,
            Request::SetChatMix {
                balance: v.clamp(-1.0, 1.0),
            },
            TIMEOUT,
            mark("chatmix", &c),
        );
    });
    let weak = ui.as_weak();
    let c = client.clone();
    g.on_set_app(move |i, ch| {
        let (Some(ui), Some(s)) = (weak.upgrade(), sonar(&c)) else {
            return;
        };
        if let Some(binary) = s.apps.get(i as usize).and_then(|a| a.binary.clone()) {
            crate::act(
                &ui,
                &c,
                Request::SetAppChannel {
                    binary,
                    channel: channel(ch),
                },
                TIMEOUT,
                mark("route", &c),
            );
        }
    });
    let weak = ui.as_weak();
    let c = client.clone();
    g.on_pick_mic_input(move |i| {
        let (Some(ui), Some(s)) = (weak.upgrade(), sonar(&c)) else {
            return;
        };
        match picked(&s.mic.inputs, s.mic.input.as_deref(), i) {
            Some(input) if s.mic.input.as_deref() != Some(input.name.as_str()) => crate::act(
                &ui,
                &c,
                Request::SetMicInput {
                    name: input.name.clone(),
                },
                TIMEOUT,
                mark("mic-input", &c),
            ),
            _ => redraw(&ui, &c),
        }
    });
    let weak = ui.as_weak();
    let c = client.clone();
    g.on_set_mic_volume(move |v| {
        let Some(ui) = weak.upgrade() else { return };
        let req = Request::SetMicVolume {
            volume: v.clamp(0.0, 1.0),
        };
        quietly(&ui, &c, req, |_| {});
    });
    let weak = ui.as_weak();
    let c = client.clone();
    g.on_set_mic(move |field, v| {
        let (Some(ui), Some(s)) = (weak.upgrade(), sonar(&c)) else {
            return;
        };
        send_whole(
            &ui,
            &c,
            |f| &mut f.mic,
            s.mic.settings,
            |m| patch_mic(m, &field, v),
            |settings| Request::SetMicSettings { settings },
            |_| {},
        );
    });
    let weak = ui.as_weak();
    let c = client.clone();
    g.on_set_preamp(move |v| {
        let (Some(ui), Some(s)) = (weak.upgrade(), sonar(&c)) else {
            return;
        };
        send_whole(
            &ui,
            &c,
            |f| &mut f.headphone,
            s.headphone,
            |h| h.preamp = ((v * 2.0).round() / 2.0).clamp(-20.0, 20.0),
            |eq| Request::SetHeadphoneEq { eq },
            |_| {},
        );
    });
    let weak = ui.as_weak();
    g.on_import_edited(move || {
        let Some(ui) = weak.upgrade() else { return };
        let g = ui.global::<SonarUi>();
        g.set_import_ready(!g.get_import_text().trim().is_empty());
    });
    let weak = ui.as_weak();
    let c = client.clone();
    g.on_import_autoeq(move || {
        let Some(ui) = weak.upgrade() else { return };
        let g = ui.global::<SonarUi>();
        let text = g.get_import_text().to_string();
        if text.trim().is_empty() {
            return;
        }
        let name = Some(g.get_import_name().trim().to_string()).filter(|n| !n.is_empty());
        crate::act_then(
            &ui,
            &c,
            Request::ImportAutoEq { text, name },
            TIMEOUT,
            mark("import", &c),
            |ui, _| {
                let g = ui.global::<SonarUi>();
                g.set_import_text(SharedString::new());
                g.set_import_ready(false);
            },
        );
    });
    let weak = ui.as_weak();
    let c = client.clone();
    g.on_load_autoeq(move || {
        let weak = weak.clone();
        c.spawn(async move {
            let loaded = match crate::system::pick_file(
                "An AutoEQ ParametricEQ.txt",
                ("Text files", &["text/plain"]),
            )
            .await
            {
                Ok(Some(path)) => tokio::task::spawn_blocking(move || read_autoeq(&path))
                    .await
                    .unwrap_or_else(|e| Err(e.to_string())),
                Ok(None) => return,
                Err(e) => Err(e),
            };
            let _ = weak.upgrade_in_event_loop(move |ui| {
                let g = ui.global::<SonarUi>();
                match loaded {
                    Ok((text, name)) => {
                        g.set_import_ready(!text.trim().is_empty());
                        g.set_import_text(text.into());
                        if let Some(name) = name.filter(|_| g.get_import_name().trim().is_empty()) {
                            g.set_import_name(name.into());
                        }
                        g.set_error(SharedString::new());
                    }
                    Err(e) => g.set_error(e.into()),
                }
            });
        });
    });

    // The EQ editor.
    let weak = ui.as_weak();
    let c = client.clone();
    let f = form.clone();
    g.on_pick_eq_tab(move |tab| {
        let (Some(ui), Some(s)) = (weak.upgrade(), sonar(&c)) else {
            return;
        };
        f.borrow_mut().pick_tab(&s, tab);
        redraw(&ui, &c);
    });
    let weak = ui.as_weak();
    let c = client.clone();
    let f = form.clone();
    g.on_eq_select(move |i| {
        let Some(ui) = weak.upgrade() else { return };
        f.borrow_mut().selected = usize::try_from(i).ok();
        redraw(&ui, &c);
    });
    let weak = ui.as_weak();
    let c = client.clone();
    let f = form.clone();
    g.on_eq_drag(move |i, x, y| {
        let (Some(ui), Ok(i)) = (weak.upgrade(), usize::try_from(i)) else {
            return;
        };
        let (tab, send) = {
            let mut form = f.borrow_mut();
            (form.tab, form.drag(i, x, y, Instant::now()))
        };
        if let Some(bands) = send {
            send_bands(&ui, &c, tab, bands);
        }
        redraw(&ui, &c);
    });
    let weak = ui.as_weak();
    let c = client.clone();
    let f = form.clone();
    g.on_eq_release(move || {
        let Some(ui) = weak.upgrade() else { return };
        let (tab, send) = {
            let mut form = f.borrow_mut();
            (form.tab, form.release(Instant::now()))
        };
        if let Some(bands) = send {
            send_bands(&ui, &c, tab, bands);
        }
    });
    let weak = ui.as_weak();
    let c = client.clone();
    let f = form.clone();
    g.on_eq_wheel(move |i, up| {
        let (Some(ui), Ok(i)) = (weak.upgrade(), usize::try_from(i)) else {
            return;
        };
        let (tab, send) = {
            let mut form = f.borrow_mut();
            (form.tab, form.wheel(i, up))
        };
        if let Some(bands) = send {
            send_bands(&ui, &c, tab, bands);
        }
        redraw(&ui, &c);
    });
    let weak = ui.as_weak();
    let c = client.clone();
    let f = form.clone();
    g.on_eq_enter(move |field, text| {
        let Some(ui) = weak.upgrade() else { return };
        let field = match field {
            0 => Field::Freq,
            1 => Field::Gain,
            _ => Field::Q,
        };
        let (tab, entered) = {
            let mut form = f.borrow_mut();
            (form.tab, form.enter(field, &text))
        };
        match entered {
            Ok(Some(bands)) => send_bands(&ui, &c, tab, bands),
            Ok(None) => {}
            Err(e) => ui.global::<SonarUi>().set_error(e.into()),
        }
        redraw(&ui, &c);
    });
    let weak = ui.as_weak();
    let c = client.clone();
    g.on_eq_reset(move || {
        let Some(ui) = weak.upgrade() else { return };
        let (tab, bands) = {
            let mut form = form.borrow_mut();
            (form.tab, form.reset_gains())
        };
        send_bands(&ui, &c, tab, bands);
        redraw(&ui, &c);
    });
}

/// An AutoEQ ParametricEQ.txt, for the `shot` example's paste step.
const SAMPLE_AUTOEQ: &str = "Preamp: -6.2 dB
Filter 1: ON LSC Fc 105 Hz Gain 7.0 dB Q 0.70
Filter 2: ON PK Fc 180 Hz Gain -3.4 dB Q 0.53
Filter 3: ON PK Fc 1200 Hz Gain 2.1 dB Q 1.40
Filter 4: ON HSC Fc 10000 Hz Gain -2.0 dB Q 0.70
";

/// The `shot` example's scripted steps for this page (see [`crate::poke`]).
pub(crate) fn poke(ui: &AppWindow, name: &str, n: i32) -> bool {
    let g = ui.global::<SonarUi>();
    match name {
        "sonar.toggle" => g.invoke_toggle(),
        "sonar.set-streamer" => g.invoke_set_streamer(n != 0),
        "sonar.output" => {
            g.set_output(n);
            g.invoke_pick_output(n);
        }
        "sonar.volume" => g.invoke_set_volume(0, n as f32 / 100.0),
        "sonar.chatmix" => g.invoke_set_chatmix(n as f32 / 100.0),
        "sonar.app" => g.invoke_set_app(0, n),
        "sonar.smart" => g.invoke_set_smart_on(n, true),
        "sonar.smart-level" => g.invoke_set_smart_level(0, n),
        "sonar.gate" => g.invoke_set_mic("gate".into(), n as f32),
        "sonar.eq-tab" => g.invoke_pick_eq_tab(n),
        "sonar.eq-select" => g.invoke_eq_select(n),
        // Band n dragged up and right, and let go.
        "sonar.eq-drag" => {
            g.invoke_eq_drag(n, 300.0, 60.0);
            g.invoke_eq_release();
        }
        // ... or held there.
        "sonar.eq-hold" => g.invoke_eq_drag(n, 300.0, 60.0),
        "sonar.eq-freq" => g.invoke_eq_enter(0, n.to_string().into()),
        "sonar.eq-gain" => g.invoke_eq_enter(1, format!("{}", n as f32 / 10.0).into()),
        "sonar.eq-bad" => g.invoke_eq_enter(1, "loud".into()),
        "sonar.paste" => {
            g.set_import_text(SAMPLE_AUTOEQ.into());
            g.set_import_name("Sennheiser HD 600".into());
            g.invoke_import_edited();
        }
        "sonar.import" => g.invoke_import_autoeq(),
        "sonar.down" | "sonar.move" | "sonar.up" => pointer(ui, name, n),
        _ => return false,
    }
    true
}

/// The pointer pressed, moved or let go at `n` = x * 10000 + y (logical px), as
/// a hand would: for drags in scripted shots. The window gets the shot's usual
/// size first (1180 × 3400), so the layout is the one drawn.
fn pointer(ui: &AppWindow, what: &str, n: i32) {
    use slint::platform::{PointerEventButton, WindowEvent};
    let w = ui.window();
    if w.size().width == 0 {
        w.set_size(slint::PhysicalSize::new(1180, 3400));
        let _ = ui.show();
    }
    let position = slint::LogicalPosition::new((n / 10_000) as f32, (n % 10_000) as f32);
    let button = PointerEventButton::Left;
    w.dispatch_event(match what {
        "sonar.down" => WindowEvent::PointerPressed { position, button },
        "sonar.up" => WindowEvent::PointerReleased { position, button },
        _ => WindowEvent::PointerMoved { position },
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use gg_ipc::{ChannelEq, ChannelState, MicState, SmartVolumeState, StreamMix};

    fn device(name: &str) -> AudioDevice {
        AudioDevice {
            name: name.into(),
            description: name.to_uppercase(),
        }
    }

    fn flat(n: usize) -> Vec<EqBand> {
        (0..n)
            .map(|i| EqBand {
                kind: BandKind::Peaking,
                freq: 100.0 * (i + 1) as f32,
                q: 1.0,
                gain: 0.0,
            })
            .collect()
    }

    fn state() -> SonarState {
        SonarState {
            available: true,
            enabled: true,
            output: None,
            outputs: vec![],
            channels: Channel::ALL
                .iter()
                .map(|&channel| ChannelState {
                    channel,
                    present: true,
                    volume: Some(1.0),
                })
                .collect(),
            chatmix: 0.0,
            apps: vec![],
            eq: Channel::ALL
                .iter()
                .map(|&channel| ChannelEq {
                    channel,
                    bands: flat(10),
                })
                .collect(),
            headphone: HeadphoneEq {
                preamp: 0.0,
                bands: flat(12),
                name: None,
            },
            mic: MicState {
                available: true,
                present: true,
                input: None,
                inputs: vec![],
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

    fn form(s: &SonarState) -> SonarForm {
        let mut f = SonarForm::default();
        f.follow(s);
        f
    }

    #[test]
    fn the_device_box_puts_a_stand_in_first_when_the_chosen_one_is_missing() {
        let devices = [device("a"), device("b")];
        assert_eq!(
            choices(&devices, Some("b")),
            (vec!["A".into(), "B".into()], 1)
        );
        assert_eq!(
            choices(&devices, None),
            (vec![UNCHOSEN.into(), "A".into(), "B".into()], 0),
            "every device can still be picked"
        );
        assert_eq!(choices(&devices, Some("gone")).0[0], "gone (not connected)");
        assert_eq!(picked(&devices, Some("b"), 0).unwrap().name, "a");
        assert_eq!(picked(&devices, None, 0), None, "the stand-in");
        assert_eq!(picked(&devices, None, 2).unwrap().name, "b");
        assert_eq!(picked(&devices, Some("gone"), 1).unwrap().name, "a");
        assert_eq!(picked(&devices, None, 3), None);
    }

    #[test]
    fn mic_settings_are_patched_one_at_a_time_in_the_daemons_steps() {
        let mut m = MicSettings::default();
        patch_mic(&mut m, "gate", 1.0);
        patch_mic(&mut m, "strength", 62.0);
        patch_mic(&mut m, "gate-db", -99.0);
        patch_mic(&mut m, "makeup", 7.6);
        assert!(m.gate && m.denoise, "the rest stays");
        assert_eq!(m.denoise_strength, 60.0);
        assert_eq!(m.gate_threshold_db, -80.0);
        assert_eq!(m.makeup_db, 8.0);
        patch_mic(&mut m, "denoise", 0.0);
        assert!(!m.denoise && m.gate);
    }

    #[test]
    fn a_change_before_the_last_ones_answer_builds_on_it() {
        let state = MicSettings::default();
        let mut pending = None;
        let first = build_on(&mut pending, &state, |m| patch_mic(m, "gate", 1.0));
        // The state hasn't caught up: built from it, this would turn the gate off again.
        let second = build_on(&mut pending, &state, |m| patch_mic(m, "compressor", 1.0));
        assert!(second.gate && second.compressor);
        settle(&mut pending, &first);
        assert_eq!(pending, Some(second), "the later one is still in flight");
        settle(&mut pending, &second);
        assert_eq!(pending, None);
    }

    #[test]
    fn the_bands_follow_the_daemon_except_while_dragged_or_unanswered() {
        let mut s = state();
        let mut f = form(&s);
        let t0 = Instant::now();
        let sent = f.drag(2, eq::x(1000.0), eq::y(6.0, 12.0), t0).unwrap();
        assert_eq!((sent[2].freq, sent[2].gain), (1000.0, 6.0));
        // A state event mid-drag (the daemon's tick) leaves the drag alone.
        s.chatmix = 0.5;
        f.follow(&s);
        assert_eq!(f.bands[2].gain, 6.0);
        // Let go: the last send is out, the state still has the old bands.
        let n = f.sending(0);
        assert!(f.release(t0).is_some());
        let n2 = f.sending(0);
        f.follow(&s);
        assert_eq!(f.bands[2].gain, 6.0, "no snap back before the answer");
        f.answered(0, n);
        f.follow(&s);
        assert_eq!(f.bands[2].gain, 6.0, "an earlier answer doesn't free it");
        f.answered(0, n2);
        f.follow(&s);
        assert_eq!(
            f.bands[2].gain, 0.0,
            "the daemon's again (here, it refused)"
        );
    }

    #[test]
    fn a_drag_sends_at_most_every_150_ms_and_once_more_on_release() {
        let s = state();
        let mut f = form(&s);
        let t0 = Instant::now();
        assert!(f.drag(0, 100.0, 50.0, t0).is_some());
        assert!(f
            .drag(0, 110.0, 50.0, t0 + Duration::from_millis(100))
            .is_none());
        assert!(f
            .drag(0, 120.0, 50.0, t0 + Duration::from_millis(160))
            .is_some());
        let last = f.release(t0 + Duration::from_millis(170)).unwrap();
        assert_eq!(last[0].freq, eq::freq_at(120.0).round());
        assert!(
            f.release(t0).is_none(),
            "a click without a drag sends nothing"
        );
        assert_eq!(f.selected, Some(0), "the dragged band's numbers show");
    }

    #[test]
    fn typed_numbers_change_only_the_selected_band() {
        let s = state();
        let mut f = form(&s);
        assert_eq!(f.enter(Field::Gain, "3"), Ok(None), "no band selected");
        f.selected = Some(4);
        let bands = f.enter(Field::Gain, "+3,5 dB").unwrap().unwrap();
        assert_eq!(bands[4].gain, 3.5);
        assert!(bands
            .iter()
            .enumerate()
            .all(|(i, b)| i == 4 || b.gain == 0.0));
        assert_eq!(f.enter(Field::Gain, "3.5"), Ok(None), "unchanged");
        assert!(f.enter(Field::Freq, "abc").unwrap_err().contains("abc"));
        assert_eq!(f.bands[4].freq, 500.0);
    }

    #[test]
    fn switching_tabs_takes_that_tabs_bands_and_drops_the_selection() {
        let s = state();
        let mut f = form(&s);
        f.selected = Some(11);
        f.follow(&s);
        assert_eq!(f.selected, None, "channel EQs have ten bands");
        f.pick_tab(&s, HEADPHONE);
        assert_eq!(f.bands.len(), 12);
        f.selected = Some(11);
        f.pick_tab(&s, 9);
        assert_eq!((f.tab, f.selected), (HEADPHONE, None), "clamped");
    }

    #[test]
    fn autoeq_files_name_their_headphones() {
        assert_eq!(
            autoeq_name("Sennheiser HD 600 ParametricEQ.txt").as_deref(),
            Some("Sennheiser HD 600")
        );
        assert_eq!(autoeq_name("mine.txt").as_deref(), Some("mine"));
        assert_eq!(autoeq_name("ParametricEQ.txt"), None);
    }
}
