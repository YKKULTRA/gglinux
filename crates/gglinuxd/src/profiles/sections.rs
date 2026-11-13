// SPDX-License-Identifier: GPL-3.0-or-later
//! A profile's sections, read from and applied through the daemon's own request handlers
//! ([`Daemon`]), so each one is checked and gated exactly as the user's own request for it.
//!
//! A section whose gate is off is skipped with a note and never sent. The headset's
//! changes and the mouse's sleep timer are saved on the device each time they're sent, so
//! those are sent only where they differ from what the device has; everything else is sent
//! whole. What the daemon answers about a device may be a reading that comes a second or
//! more after a change, so what profiles sent in the last few seconds counts over what's
//! read ([`Sent`]).

use std::future::Future;
use std::pin::Pin;
use std::time::{Duration, Instant};

use gg_ipc::{
    Experimental, Feature, HeadsetChange, HeadsetControls, MouseSettings, Profile,
    ProfileBindings, Request, ResponseData, Section,
};

/// How profiles reach the devices: the daemon's own request handlers.
pub trait Daemon: Send + Sync + 'static {
    fn ask(&self, req: Request) -> Pin<Box<dyn Future<Output = ResponseData> + Send + '_>>;
}

/// How long what profiles sent counts over what the daemon reads back.
pub const GRACE: Duration = Duration::from_secs(5);

async fn ask_ok(d: &dyn Daemon, req: Request) -> Result<(), String> {
    match d.ask(req).await {
        ResponseData::Success => Ok(()),
        ResponseData::Error { message } => Err(message),
        _ => Err("the daemon answered something else".into()),
    }
}

/// The experimental gates as they are now; all off if the daemon doesn't say.
pub async fn gates(d: &dyn Daemon) -> Experimental {
    match d.ask(Request::GetExperimental).await {
        ResponseData::Experimental { experimental } => experimental,
        _ => Experimental::default(),
    }
}

/// What profiles sent lately, section by section, with when.
#[derive(Default)]
pub struct Sent {
    profile: Option<Profile>,
    at: Vec<(Section, Instant)>,
}

impl Sent {
    fn note(&mut self, section: Section, sent: &Profile, now: Instant) {
        let p = self.profile.get_or_insert_with(|| Profile::named(""));
        overlay_section(p, sent, section);
        self.at.retain(|(s, _)| *s != section);
        self.at.push((section, now));
    }

    /// Put what was sent within `grace` over what was read.
    pub fn over(&mut self, read: &mut Profile, now: Instant, grace: Duration) {
        self.at
            .retain(|(_, t)| now.saturating_duration_since(*t) < grace);
        let Some(sent) = &self.profile else { return };
        for (s, _) in &self.at {
            if read.has(*s) {
                overlay_section(read, sent, *s);
            }
        }
    }
}

/// Put `top`'s part of `section` over `base`'s: a whole keyboard section; each mouse part
/// and each headset change on its own.
fn overlay_section(base: &mut Profile, top: &Profile, section: Section) {
    match section {
        Section::Mouse => {
            let (Some(t), b) = (&top.mouse, &mut base.mouse) else {
                return;
            };
            let b = b.get_or_insert_with(MouseSettings::default);
            if t.dpi.is_some() {
                b.dpi = t.dpi.clone();
            }
            if t.polling_hz.is_some() {
                b.polling_hz = t.polling_hz;
            }
            if t.buttons.is_some() {
                b.buttons = t.buttons.clone();
            }
            if t.sleep_minutes.is_some() {
                b.sleep_minutes = t.sleep_minutes;
            }
            if t.radio.is_some() {
                b.radio = t.radio;
            }
        }
        Section::Headset => {
            let (Some(t), b) = (&top.headset, &mut base.headset) else {
                return;
            };
            let b = b.get_or_insert_with(Vec::new);
            for c in t {
                let kind = std::mem::discriminant(c);
                b.retain(|x| std::mem::discriminant(x) != kind);
                b.push(c.clone());
            }
        }
        s => base.take_section(top, s),
    }
}

/// The headset settings a profile can hold, from what the base station reports; those that
/// need a gate only while it's on.
pub fn headset_changes(c: &HeadsetControls, gates: &Experimental) -> Vec<HeadsetChange> {
    use HeadsetChange as H;
    let eq = match (c.eq_preset, &c.eq_bands) {
        (Some(4), Some(bands)) => Some(H::EqBands(bands.clone())),
        (Some(p), _) => Some(H::EqPreset(p)),
        _ => None,
    };
    let all = [
        c.noise_mode.map(H::NoiseMode),
        c.transparency.map(H::Transparency),
        c.sidetone.map(H::Sidetone),
        c.mic_volume.map(H::MicVolume),
        c.mic_led.map(H::MicLed),
        c.auto_off_minutes.map(H::AutoOffMinutes),
        c.radio_mode.map(H::RadioMode),
        eq,
        c.bluetooth_at_power_on.map(H::BluetoothAtPowerOn),
        c.bluetooth_call.map(H::BluetoothCall),
        c.gain.map(H::Gain),
        c.line_out.map(H::LineOut),
        c.line_out_mix.map(H::LineOutMix),
        c.home_screen.map(H::HomeScreen),
        c.screen_dim_minutes.map(H::ScreenDimMinutes),
    ];
    all.into_iter()
        .flatten()
        .filter(|ch| ch.feature().is_none_or(|f| gates.on(f)))
        .collect()
}

/// The mouse settings a profile can hold: what gglinux set, else what the mouse reports; the
/// sleep timer and the radio only while `mouse_power` is on.
fn mouse_settings(m: &gg_ipc::MouseState, gates: &Experimental) -> MouseSettings {
    let mut s = m.applied.clone().unwrap_or_default();
    if s.dpi.is_none() {
        s.dpi = m.reported_dpi.clone();
    }
    if s.polling_hz.is_none() {
        s.polling_hz = m.reported_polling_hz;
    }
    if gates.on(Feature::MousePower) {
        if let Some(secs) = m.sleep_secs.filter(|s| s % 60 == 0) {
            s.sleep_minutes = u8::try_from(secs / 60)
                .ok()
                .filter(|m| gg_ipc::MOUSE_SLEEP_MINUTES.contains(m));
        }
        s.radio = m.radio.or(s.radio);
    } else {
        s.sleep_minutes = None;
        s.radio = None;
    }
    s
}

/// The current settings of `sections`, and why each one that couldn't be read wasn't.
pub async fn read(
    d: &dyn Daemon,
    sections: &[Section],
    gates: &Experimental,
) -> (Profile, Vec<(Section, String)>) {
    let mut p = Profile::named("");
    let mut missing = Vec::new();
    let wants = |s: Section| sections.contains(&s);
    if wants(Section::Lighting) || wants(Section::Switches) || wants(Section::Screen) {
        match d.ask(Request::GetKeyboardState).await {
            ResponseData::Keyboard { state } => {
                p.lighting = wants(Section::Lighting).then_some(state.lighting);
                p.switches = wants(Section::Switches).then_some(state.switches);
                p.screen = wants(Section::Screen).then_some(state.screen);
            }
            other => {
                let why = why_not(other);
                for s in [Section::Lighting, Section::Switches, Section::Screen] {
                    if wants(s) {
                        missing.push((s, why.clone()));
                    }
                }
            }
        }
    }
    if wants(Section::Bindings) {
        match d.ask(Request::GetBindings).await {
            ResponseData::Bindings { state } => {
                p.bindings = Some(ProfileBindings {
                    enabled: state.enabled,
                    bindings: state.bindings,
                })
            }
            other => missing.push((Section::Bindings, why_not(other))),
        }
    }
    if wants(Section::Mouse) {
        match d.ask(Request::GetMouseState).await {
            ResponseData::Mouse { state } => {
                let s = mouse_settings(&state, gates);
                if s == MouseSettings::default() {
                    missing.push((
                        Section::Mouse,
                        "gglinux hasn't set the mouse since it started, and the mouse can't tell \
                         its settings: set them on the Engine page first"
                            .into(),
                    ));
                } else {
                    p.mouse = Some(s);
                }
            }
            other => missing.push((Section::Mouse, why_not(other))),
        }
    }
    if wants(Section::Headset) {
        match d.ask(Request::GetHeadsetState).await {
            ResponseData::Headset { state } if !state.connected => missing.push((
                Section::Headset,
                "the headset's base station isn't connected".into(),
            )),
            ResponseData::Headset { state } => {
                let changes = headset_changes(&state.controls, gates);
                if changes.is_empty() {
                    missing.push((
                        Section::Headset,
                        "the headset hasn't reported its settings yet".into(),
                    ));
                } else {
                    p.headset = Some(changes);
                }
            }
            other => missing.push((Section::Headset, why_not(other))),
        }
    }
    (p, missing)
}

fn why_not(answer: ResponseData) -> String {
    match answer {
        ResponseData::Error { message } => message,
        _ => "the daemon answered something else".into(),
    }
}

/// One section's note: its name, then what happened.
fn note(section: Section, what: impl std::fmt::Display) -> String {
    format!("{}: {what}", section.label())
}

/// What applying a profile did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// A line for each section, or part of one, that was skipped or didn't go through.
    pub notes: Vec<String>,
    /// How many sections went through, in full or in part.
    pub applied: usize,
    /// How many sections failed.
    pub failed: usize,
}

/// Apply `p`'s sections in [`Section::ALL`]'s order. `now` is the current settings of the
/// headset and the mouse as read (with [`Sent`] over them), for sending only what differs
/// where the device saves each change.
pub async fn apply(
    d: &dyn Daemon,
    p: &Profile,
    now: &Profile,
    gates: &Experimental,
    sent: &mut Sent,
    at: Instant,
) -> Outcome {
    let mut out = Outcome::default();
    for section in p.sections() {
        // What of the section went out, for `sent`.
        let mut done = Profile::named("");
        let result = match section {
            Section::Headset => headset(d, p, now, gates, &mut out.notes, &mut done).await,
            Section::Mouse => mouse(d, p, now, gates, &mut out.notes, &mut done).await,
            Section::Switches => switches(d, p, gates).await,
            Section::Bindings => match gates.refusal(Feature::KeyboardBindings) {
                Some(why) => Err(Skip(why)),
                None => {
                    let b = p.bindings.clone().unwrap_or_default();
                    send(d, Request::SetBindings {
                        bindings: b.bindings,
                        enabled: b.enabled,
                    })
                    .await
                }
            },
            Section::Screen => {
                send(d, Request::SetScreen {
                    screen: p.screen.clone().unwrap_or_default(),
                })
                .await
            }
            Section::Lighting => match &p.lighting {
                Some(lighting) => {
                    send(d, Request::SetLighting {
                        lighting: lighting.clone(),
                    })
                    .await
                }
                None => Ok(()),
            },
        };
        if !matches!(section, Section::Headset | Section::Mouse) && result.is_ok() {
            done.take_section(p, section);
        }
        if done.has(section) {
            sent.note(section, &done, at);
        }
        match result {
            Ok(()) => out.applied += 1,
            Err(Skip(why)) => out.notes.push(note(section, format!("skipped. {why}"))),
            Err(Failed(why)) => {
                out.failed += 1;
                out.notes.push(note(section, why));
            }
        }
    }
    out
}

/// Why a section didn't go: skipped by a gate (never sent), or sent and refused.
enum NotApplied {
    Skip(String),
    Failed(String),
}
use NotApplied::{Failed, Skip};

async fn send(d: &dyn Daemon, req: Request) -> Result<(), NotApplied> {
    ask_ok(d, req).await.map_err(Failed)
}

async fn switches(d: &dyn Daemon, p: &Profile, gates: &Experimental) -> Result<(), NotApplied> {
    let Some(switches) = p.switches.clone() else {
        return Ok(());
    };
    // The user's own request would be refused: never sent.
    if switches.protection.enabled {
        if let Some(why) = gates.refusal(Feature::KeyboardProtection) {
            return Err(Skip(why));
        }
    }
    send(d, Request::SetSwitches { switches }).await
}

/// Each headset change that differs from what the base station has; those whose gate is off
/// are skipped with a note. The first refusal ends the section (the base station is away,
/// most likely).
async fn headset(
    d: &dyn Daemon,
    p: &Profile,
    now: &Profile,
    gates: &Experimental,
    notes: &mut Vec<String>,
    done: &mut Profile,
) -> Result<(), NotApplied> {
    let have = now.headset.clone().unwrap_or_default();
    let mut skipped = Vec::new();
    for change in p.headset.iter().flatten() {
        if let Some(why) = change.feature().and_then(|f| gates.refusal(f)) {
            skipped.push(why);
            continue;
        }
        if have.contains(change) {
            continue;
        }
        send(d, Request::SetHeadset {
            change: change.clone(),
        })
        .await?;
        done.headset.get_or_insert_with(Vec::new).push(change.clone());
    }
    if let Some(why) = skipped.first() {
        notes.push(note(
            Section::Headset,
            format!("{} setting(s) skipped. {why}", skipped.len()),
        ));
    }
    Ok(())
}

/// The mouse's parts in the order GG's own page sends them: DPI, polling rate, buttons, sleep
/// timer, radio. None is saved to the mouse's memory, except the sleep timer, which the
/// mouse forgets unsaved; that one goes only when it differs from what the mouse has.
async fn mouse(
    d: &dyn Daemon,
    p: &Profile,
    now: &Profile,
    gates: &Experimental,
    notes: &mut Vec<String>,
    done: &mut Profile,
) -> Result<(), NotApplied> {
    let Some(m) = &p.mouse else { return Ok(()) };
    let out = done.mouse.get_or_insert_with(MouseSettings::default);
    if let Some(dpi) = &m.dpi {
        send(d, Request::SetMouseDpi {
            stages: dpi.stages.clone(),
            active: dpi.active,
            save: false,
        })
        .await?;
        out.dpi = Some(dpi.clone());
    }
    if let Some(hz) = m.polling_hz {
        send(d, Request::SetMousePolling { hz, save: false }).await?;
        out.polling_hz = Some(hz);
    }
    if let Some(buttons) = &m.buttons {
        let mut actions = buttons.clone();
        let factory = gg_ipc::mouse_keys::WHEEL_FACTORY;
        if actions.len() > 6 && actions[6..] != factory {
            if let Some(why) = gates.refusal(Feature::MouseWheel) {
                actions.truncate(6);
                notes.push(note(Section::Mouse, format!("the wheel's actions skipped. {why}")));
            }
        }
        send(d, Request::SetMouseButtons {
            actions: actions.clone(),
            save: false,
        })
        .await?;
        out.buttons = Some(actions);
    }
    let power = gates.refusal(Feature::MousePower);
    let has_sleep = now.mouse.as_ref().and_then(|n| n.sleep_minutes);
    match (m.sleep_minutes, &power) {
        (Some(_), Some(why)) => notes.push(note(Section::Mouse, format!("the sleep timer skipped. {why}"))),
        (Some(minutes), None) if has_sleep != Some(minutes) => {
            send(d, Request::SetMouseSleep { minutes }).await?;
            out.sleep_minutes = Some(minutes);
        }
        _ => {}
    }
    match (m.radio, &power) {
        (Some(_), Some(why)) => notes.push(note(Section::Mouse, format!("the radio mode skipped. {why}"))),
        (Some(radio), None) => {
            send(d, Request::SetMouseRadio { radio, save: false }).await?;
            out.radio = Some(radio);
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
pub mod testing {
    //! A daemon in memory, lagging like a real one: the first read after a change still
    //! answers the settings from before it.

    use std::sync::{Arc, Mutex};

    use gg_ipc::{
        BindingsState, Experimental, HeadsetControls, HeadsetPower, HeadsetState, KeyboardState,
        MouseState, Request, ResponseData, ScreenSettings,
    };

    use super::Daemon;

    #[derive(Debug, Clone, PartialEq)]
    pub struct Devices {
        pub keyboard: KeyboardState,
        pub bindings: BindingsState,
        pub mouse: MouseState,
        pub headset: HeadsetState,
        pub gates: Experimental,
    }

    pub struct Fake {
        pub now: Mutex<Devices>,
        /// What a read answers while the last change hasn't shown yet.
        stale: Mutex<Option<Devices>>,
        /// Every request that changed something, by its wire name, in order.
        pub log: Mutex<Vec<String>>,
        /// A request (by wire name) that fails.
        pub fail: Mutex<Option<String>>,
    }

    /// A connected keyboard with every setting at its default (read from JSON, so fields
    /// added later with a default don't need naming here).
    pub fn keyboard() -> KeyboardState {
        let lighting = serde_json::to_value(gg_ipc::Lighting::default()).unwrap();
        serde_json::from_value(serde_json::json!({
            "connected": true,
            "layout": [],
            "lighting": lighting,
            "reactive": "off",
        }))
        .expect("a keyboard state")
    }

    impl Fake {
        pub fn new() -> Arc<Fake> {
            Arc::new(Fake {
                now: Mutex::new(Devices {
                    keyboard: keyboard(),
                    bindings: BindingsState::default(),
                    mouse: MouseState {
                        connected: true,
                        ..MouseState::default()
                    },
                    headset: HeadsetState {
                        connected: true,
                        power: HeadsetPower::Online,
                        headset_battery: Some(80),
                        spare_battery: None,
                        screen: ScreenSettings::default(),
                        screen_dark: false,
                        controls: HeadsetControls::default(),
                    },
                    gates: Experimental::default(),
                }),
                stale: Mutex::new(None),
                log: Mutex::new(Vec::new()),
                fail: Mutex::new(None),
            })
        }

        pub fn sent(&self) -> Vec<String> {
            self.log.lock().unwrap().clone()
        }

        fn read(&self) -> Devices {
            self.stale
                .lock()
                .unwrap()
                .take()
                .unwrap_or_else(|| self.now.lock().unwrap().clone())
        }

        fn change(&self, name: &str, f: impl FnOnce(&mut Devices)) -> ResponseData {
            if self.fail.lock().unwrap().as_deref() == Some(name) {
                return ResponseData::Error {
                    message: format!("{name} failed"),
                };
            }
            self.log.lock().unwrap().push(name.to_string());
            let mut now = self.now.lock().unwrap();
            self.stale.lock().unwrap().get_or_insert_with(|| now.clone());
            f(&mut now);
            ResponseData::Success
        }

        fn answer(&self, req: Request) -> ResponseData {
            use gg_ipc::HeadsetChange as H;
            match req {
                Request::GetKeyboardState => ResponseData::Keyboard {
                    state: self.read().keyboard,
                },
                Request::GetBindings => ResponseData::Bindings {
                    state: self.read().bindings,
                },
                Request::GetMouseState => ResponseData::Mouse {
                    state: self.read().mouse,
                },
                Request::GetHeadsetState => ResponseData::Headset {
                    state: self.read().headset,
                },
                Request::GetExperimental => ResponseData::Experimental {
                    experimental: self.now.lock().unwrap().gates,
                },
                Request::SetLighting { lighting } => {
                    self.change("set_lighting", |d| d.keyboard.lighting = lighting)
                }
                Request::SetSwitches { switches } => {
                    self.change("set_switches", |d| d.keyboard.switches = switches)
                }
                Request::SetScreen { screen } => {
                    self.change("set_screen", |d| d.keyboard.screen = screen)
                }
                Request::SetBindings { bindings, enabled } => self.change("set_bindings", |d| {
                    d.bindings.bindings = bindings;
                    d.bindings.enabled = enabled;
                }),
                Request::SetMouseDpi { stages, active, .. } => self.change("set_mouse_dpi", |d| {
                    let a = d.mouse.applied.get_or_insert_with(Default::default);
                    a.dpi = Some(gg_ipc::MouseDpi { stages, active });
                }),
                Request::SetMousePolling { hz, .. } => self.change("set_mouse_polling", |d| {
                    d.mouse.applied.get_or_insert_with(Default::default).polling_hz = Some(hz);
                }),
                Request::SetMouseButtons { actions, .. } => {
                    self.change(&format!("set_mouse_buttons {}", actions.len()), |d| {
                        d.mouse.applied.get_or_insert_with(Default::default).buttons =
                            Some(actions);
                    })
                }
                Request::SetMouseSleep { minutes } => self.change("set_mouse_sleep", |d| {
                    d.mouse.sleep_secs = Some(u16::from(minutes) * 60);
                }),
                Request::SetMouseRadio { radio, .. } => {
                    self.change("set_mouse_radio", |d| d.mouse.radio = Some(radio))
                }
                Request::SetHeadset { change } => {
                    let name = format!("set_headset {change:?}");
                    self.change(&name, |d| {
                        let c = &mut d.headset.controls;
                        match change {
                            H::Sidetone(v) => c.sidetone = Some(v),
                            H::NoiseMode(v) => c.noise_mode = Some(v),
                            H::Gain(v) => c.gain = Some(v),
                            H::EqPreset(v) => c.eq_preset = Some(v),
                            H::MicVolume(v) => c.mic_volume = Some(v),
                            _ => {}
                        }
                    })
                }
                other => ResponseData::Error {
                    message: format!("the fake daemon doesn't answer {other:?}"),
                },
            }
        }
    }

    /// The fake behind an `Arc`, as profiles hold their daemon.
    pub struct Shared(pub Arc<Fake>);

    impl Daemon for Shared {
        fn ask(
            &self,
            req: Request,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ResponseData> + Send + '_>>
        {
            let answer = self.0.answer(req);
            Box::pin(async move { answer })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{Fake, Shared};
    use super::*;
    use gg_ipc::{
        Effect, HeadsetGain, Lighting, MouseDpi, NoiseMode, Protection, Switches, DpiStage,
    };

    fn lit(period: f32) -> Lighting {
        Lighting {
            enabled: true,
            effect: Effect::Wave { period_s: period },
            ..Lighting::default()
        }
    }

    fn everything() -> Profile {
        Profile {
            headset: Some(vec![
                HeadsetChange::Sidetone(2),
                HeadsetChange::Gain(HeadsetGain::High),
            ]),
            mouse: Some(MouseSettings {
                dpi: Some(MouseDpi {
                    stages: vec![DpiStage { x: 800, y: 800 }],
                    active: 0,
                }),
                polling_hz: Some(1000),
                buttons: Some(
                    ["button1", "button2", "button3", "button4", "button5", "dpi", "button4", "scrolldown"]
                        .map(String::from)
                        .to_vec(),
                ),
                sleep_minutes: Some(5),
                radio: Some(gg_ipc::MouseRadio::Stable),
            }),
            switches: Some(Switches {
                enabled: true,
                actuation: 10,
                ..Switches::default()
            }),
            bindings: Some(ProfileBindings::default()),
            screen: Some(gg_ipc::Screen::Text { text: "GG".into() }),
            lighting: Some(lit(2.0)),
            ..Profile::named("All")
        }
    }

    async fn apply_now(fake: &Arc<Fake>, p: &Profile) -> Outcome {
        let d = Shared(Arc::clone(fake));
        let g = gates(&d).await;
        apply(&d, p, &Profile::named(""), &g, &mut Sent::default(), Instant::now()).await
    }

    use std::sync::Arc;

    #[tokio::test]
    async fn sections_go_in_order_and_gated_parts_are_skipped_with_a_note_never_sent() {
        let fake = Fake::new();
        let out = apply_now(&fake, &everything()).await;
        assert_eq!(
            fake.sent(),
            [
                "set_headset Sidetone(2)",
                "set_mouse_dpi",
                "set_mouse_polling",
                "set_mouse_buttons 6",
                "set_switches",
                "set_screen",
                "set_lighting",
            ],
            "headset gain, the wheel, sleep, radio and the bindings wait for their gates"
        );
        let notes = out.notes.join("\n");
        for part in ["Headset: 1 setting(s) skipped", "wheel's actions skipped", "sleep timer skipped", "radio mode skipped", "Key bindings: skipped"] {
            assert!(notes.contains(part), "{part}\n{notes}");
        }
        assert!(notes.contains("Settings → Experimental"), "{notes}");
        assert_eq!((out.applied, out.failed), (5, 0));
    }

    #[tokio::test]
    async fn with_the_gates_on_every_part_is_sent() {
        let fake = Fake::new();
        {
            let mut d = fake.now.lock().unwrap();
            for f in [Feature::HeadsetExtras, Feature::MouseWheel, Feature::MousePower, Feature::KeyboardBindings] {
                d.gates.set(f, true);
            }
        }
        let out = apply_now(&fake, &everything()).await;
        assert_eq!(out.notes, Vec::<String>::new());
        assert_eq!(
            fake.sent(),
            [
                "set_headset Sidetone(2)",
                "set_headset Gain(High)",
                "set_mouse_dpi",
                "set_mouse_polling",
                "set_mouse_buttons 8",
                "set_mouse_sleep",
                "set_mouse_radio",
                "set_switches",
                "set_bindings",
                "set_screen",
                "set_lighting",
            ]
        );
    }

    #[tokio::test]
    async fn protection_without_its_gate_skips_the_switches_and_a_refusal_is_a_note() {
        let fake = Fake::new();
        let p = Profile {
            switches: Some(Switches {
                protection: Protection {
                    enabled: true,
                    keys: vec![],
                },
                ..Switches::default()
            }),
            lighting: Some(lit(3.0)),
            ..Profile::named("P")
        };
        *fake.fail.lock().unwrap() = Some("set_lighting".into());
        let out = apply_now(&fake, &p).await;
        assert!(fake.sent().is_empty());
        assert_eq!(out.failed, 1);
        assert!(out.notes[0].starts_with("Keyboard switches: skipped. "), "{:?}", out.notes);
        assert_eq!(out.notes[1], "Lighting: set_lighting failed");
    }

    #[tokio::test]
    async fn headset_changes_and_the_sleep_timer_go_only_where_they_differ() {
        let fake = Fake::new();
        fake.now.lock().unwrap().gates.set(Feature::MousePower, true);
        let d = Shared(Arc::clone(&fake));
        let g = gates(&d).await;
        let p = Profile {
            headset: Some(vec![
                HeadsetChange::Sidetone(2),
                HeadsetChange::NoiseMode(NoiseMode::Anc),
            ]),
            mouse: Some(MouseSettings {
                sleep_minutes: Some(5),
                ..MouseSettings::default()
            }),
            ..Profile::named("P")
        };
        let mut now = Profile::named("");
        now.headset = Some(vec![HeadsetChange::Sidetone(2), HeadsetChange::NoiseMode(NoiseMode::Off)]);
        now.mouse = Some(MouseSettings {
            sleep_minutes: Some(5),
            ..MouseSettings::default()
        });
        apply(&d, &p, &now, &g, &mut Sent::default(), Instant::now()).await;
        assert_eq!(fake.sent(), ["set_headset NoiseMode(Anc)"]);
    }

    #[tokio::test]
    async fn reading_takes_each_section_and_says_why_one_is_missing() {
        let fake = Fake::new();
        {
            let mut d = fake.now.lock().unwrap();
            d.keyboard.lighting = lit(7.0);
            d.headset.controls.sidetone = Some(1);
            d.headset.controls.gain = Some(HeadsetGain::Low);
            d.headset.controls.eq_preset = Some(4);
            d.headset.controls.eq_bands = Some(vec![1.0; 10]);
        }
        let d = Shared(Arc::clone(&fake));
        let (p, missing) = read(&d, &Section::ALL, &Experimental::default()).await;
        assert_eq!(p.lighting, Some(lit(7.0)));
        assert_eq!(
            p.headset,
            Some(vec![HeadsetChange::Sidetone(1), HeadsetChange::EqBands(vec![1.0; 10])]),
            "the gain needs headset_extras"
        );
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].0, Section::Mouse);
        assert!(missing[0].1.contains("Engine page"));
        assert!(p.has(Section::Switches) && p.has(Section::Screen) && p.has(Section::Bindings));
    }

    #[tokio::test]
    async fn what_was_just_sent_counts_over_a_reading_that_lags() {
        let fake = Fake::new();
        let d = Shared(Arc::clone(&fake));
        let g = gates(&d).await;
        let mut sent = Sent::default();
        let t = Instant::now();
        let p = Profile {
            lighting: Some(lit(2.0)),
            headset: Some(vec![HeadsetChange::Sidetone(3)]),
            ..Profile::named("P")
        };
        apply(&d, &p, &Profile::named(""), &g, &mut sent, t).await;
        let (mut read_now, _) = read(&d, &[Section::Lighting, Section::Headset], &g).await;
        assert_eq!(read_now.lighting, Some(Lighting::default()), "the fake still answers the old");
        sent.over(&mut read_now, t, GRACE);
        assert_eq!(read_now.lighting, Some(lit(2.0)));
        // Once the grace has passed, the reading counts again.
        let (mut later, _) = read(&d, &[Section::Lighting], &g).await;
        later.lighting = Some(lit(9.0));
        sent.over(&mut later, t + GRACE, GRACE);
        assert_eq!(later.lighting, Some(lit(9.0)));
    }
}
