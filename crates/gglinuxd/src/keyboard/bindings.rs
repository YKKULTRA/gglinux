// SPDX-License-Identifier: GPL-3.0-or-later
//! Key bindings on the keyboard: the worker's part ([`Store`], [`on_command`],
//! [`on_connect`]) and the clients' requests ([`handle`]).
//!
//! The keyboard keeps its bindings in RAM, so gglinux sends them again whenever it
//! connects, like the switches. A layer is written whole, so every write starts from a
//! read made on this connection: the keyboard's own bindings, with the user's on top
//! (`gg_device_keyboard::bindings::merge`). The keyboard's own shortcuts on the Fn layer
//! are kept byte for byte. Only the layers that differ from what the keyboard holds are
//! written, and the Fn key and its lit keys only when an Fn key is chosen.
//!
//! Everything here that talks to the keyboard waits for `keyboard_bindings`: a request
//! is refused with [`gg_ipc::Experimental::refusal`] while it's off, and the open
//! keyboard's allowlist lets the reports out only while it's on.

use std::path::{Path, PathBuf};

use gg_device_keyboard::bindings::{self as kb, Action, Block, Change};
use gg_device_keyboard::layout::ANSI_TKL;
use gg_device_keyboard::Keyboard;
use gg_ipc::{
    Bindings, BindingsState, Event, Experimental, Feature, KeyAction, KeyBinding, Layer, Request,
    ResponseData,
};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tracing::{info, warn};

use super::{Command, Handle, Reply};

/// The three layers, in the order the keyboard's are read and written.
const LAYERS: [(Layer, kb::Layer); 3] = [
    (Layer::Normal, kb::Layer::Normal),
    (Layer::Fn, kb::Layer::Fn),
    (Layer::Second, kb::Layer::Second),
];

/// One block list per layer, in [`LAYERS`] order.
type Layers = [Vec<Block>; 3];

/// What `bindings.json` holds.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Saved {
    enabled: bool,
    bindings: Bindings,
}

/// What the keyboard worker keeps for the bindings.
pub struct Store {
    state: BindingsState,
    path: PathBuf,
    /// The keyboard's own bindings: the first read on this connection, made before
    /// gglinux wrote anything. Every write starts from it.
    base: Option<Layers>,
    /// What gglinux last wrote on each layer on this connection.
    written: [Option<Vec<Block>>; 3],
    /// The Fn key and its lit keys as last sent on this connection.
    fn_sent: Option<(u8, [u8; 32])>,
}

impl Store {
    /// The saved bindings in `config_dir`, read when the worker starts. An unreadable
    /// file is set aside and the defaults (off) are used.
    pub fn load(config_dir: &Path) -> Store {
        let path = config_dir.join("bindings.json");
        let saved: Saved = super::load(&path, "key binding");
        Store {
            state: BindingsState {
                enabled: saved.enabled,
                bindings: saved.bindings,
                read: None,
                error: None,
            },
            path,
            base: None,
            written: Default::default(),
            fn_sent: None,
        }
    }

    fn save(&self) {
        let saved = Saved {
            enabled: self.state.enabled,
            bindings: self.state.bindings.clone(),
        };
        super::save(&self.path, &saved, "key binding");
    }

    /// Read all three layers; the first read on a connection becomes [`Store::base`].
    fn read(&mut self, device: &Keyboard) -> Result<(), String> {
        let mut layers: Layers = Default::default();
        for (blocks, (_, layer)) in layers.iter_mut().zip(LAYERS) {
            *blocks = device.read_bindings(layer).map_err(|e| format!("{e:#}"))?;
        }
        self.state.read = Some(decode(&layers));
        if self.base.is_none() {
            self.base = Some(layers);
        }
        Ok(())
    }

    /// The layers to write for `new`, coming from `old`, on top of the base read.
    fn merged(&self, new: &Bindings, old: &Bindings) -> Result<Layers, String> {
        let Some(base) = self.base.as_ref() else {
            return Err("the keyboard's bindings haven't been read on this connection".into());
        };
        let mut out: Layers = Default::default();
        for (i, (layer, kl)) in LAYERS.into_iter().enumerate() {
            out[i] =
                kb::merge(kl, &base[i], &changes(new, old, layer)).map_err(|e| e.to_string())?;
        }
        Ok(out)
    }

    /// Send the bindings to the keyboard, read first if it hasn't been on this
    /// connection. `old` is what was applied before: a key dropped since goes back to
    /// its default. Only what differs from what the keyboard holds is written.
    fn apply(&mut self, device: &Keyboard, old: &Bindings) -> Result<(), String> {
        if self.base.is_none() {
            self.read(device)?;
        }
        let merged = self.merged(&self.state.bindings, old)?;
        let base = self.base.as_ref().expect("read above");
        for (i, (_, layer)) in LAYERS.into_iter().enumerate() {
            let now = self.written[i].as_ref().unwrap_or(&base[i]);
            if merged[i] != *now {
                device
                    .write_bindings(layer, &merged[i])
                    .map_err(|e| format!("{e:#}"))?;
                self.written[i] = Some(merged[i].clone());
            }
        }
        if let Some(key) = self.state.bindings.fn_key {
            let mask = kb::fn_mask(&merged[1]);
            if self.fn_sent != Some((key, mask)) {
                device
                    .set_fn_key(key)
                    .and_then(|()| device.set_fn_mask(&mask))
                    .map_err(|e| format!("{e:#}"))?;
                self.fn_sent = Some((key, mask));
            }
        }
        Ok(())
    }

    /// New bindings, checked and saved, and sent if they're on and the keyboard is here.
    /// With the keyboard here they're checked against what it holds first: one that
    /// can't be applied (a change to one of its own shortcuts) saves nothing.
    fn set(
        &mut self,
        bindings: Bindings,
        enabled: bool,
        device: Option<&Keyboard>,
    ) -> Result<(), String> {
        let device = device.filter(|_| enabled);
        if let Some(d) = device {
            if self.base.is_none() {
                if let Err(e) = self.read(d) {
                    self.state.error = Some(e.clone());
                    return Err(e);
                }
            }
            self.merged(&bindings, &self.state.bindings)?;
        }
        let old = std::mem::replace(&mut self.state.bindings, bindings);
        self.state.enabled = enabled;
        self.state.error = None;
        self.save();
        if let Some(d) = device {
            if let Err(e) = self.apply(d, &old) {
                warn!(error = %e, "the keyboard did not take the key bindings");
                self.state.error = Some(e.clone());
                return Err(e);
            }
            info!("key bindings sent");
        }
        Ok(())
    }

    /// A keyboard that (re)connects has its own bindings: nothing read or written yet.
    fn forget(&mut self) {
        self.base = None;
        self.written = Default::default();
        self.fn_sent = None;
        self.state.read = None;
        self.state.error = None;
    }
}

/// The keyboard worker's commands for the bindings.
pub enum Cmd {
    Get(Reply<BindingsState>),
    /// Read all three layers now.
    Read(Reply<BindingsState>),
    Set {
        bindings: Bindings,
        enabled: bool,
        reply: Reply<()>,
    },
}

/// Why a command that talks to the keyboard isn't sent, if it isn't.
fn gate(exp: &Experimental) -> Result<(), String> {
    exp.refusal(Feature::KeyboardBindings).map_or(Ok(()), Err)
}

fn connected(device: Option<&Keyboard>) -> Result<&Keyboard, String> {
    device.ok_or_else(|| "the keyboard isn't connected".to_string())
}

/// One command, on the keyboard worker's thread. `device` is the open keyboard, if any.
pub fn on_command(
    cmd: Cmd,
    store: &mut Store,
    device: Option<&Keyboard>,
    exp: &Experimental,
    event_tx: &broadcast::Sender<Event>,
) {
    let before = store.state.clone();
    match cmd {
        Cmd::Get(reply) => {
            let _ = reply.send(Ok(store.state.clone()));
        }
        Cmd::Read(reply) => {
            let result = gate(exp).and_then(|()| connected(device)).and_then(|d| {
                store
                    .read(d)
                    .inspect_err(|e| store.state.error = Some(e.clone()))
            });
            let _ = reply.send(result.map(|()| store.state.clone()));
        }
        Cmd::Set {
            bindings,
            enabled,
            reply,
        } => {
            let result = gate(exp)
                .and_then(|()| check(&bindings))
                .and_then(|()| store.set(bindings, enabled, device));
            let _ = reply.send(result);
        }
    }
    publish(store, &before, event_tx);
}

/// The keyboard just opened and its switches were sent again. Bindings live in its RAM,
/// so here is where they're sent again too, if they're on and so is the gate.
pub fn on_connect(
    store: &mut Store,
    device: &Keyboard,
    exp: &Experimental,
    event_tx: &broadcast::Sender<Event>,
) {
    let before = store.state.clone();
    store.forget();
    if store.state.enabled && gate(exp).is_ok() {
        let now = store.state.bindings.clone();
        match store.apply(device, &now) {
            Ok(()) => info!("key bindings sent"),
            Err(e) => {
                warn!(error = %e, "the keyboard did not take the key bindings");
                store.state.error = Some(e);
            }
        }
    }
    publish(store, &before, event_tx);
}

fn publish(store: &Store, before: &BindingsState, event_tx: &broadcast::Sender<Event>) {
    if store.state != *before {
        let _ = event_tx.send(Event::BindingsChanged {
            state: store.state.clone(),
        });
    }
}

/// A bindings request from a client; `main` has already refused it if the keyboard part
/// is off.
pub async fn handle(req: Request, kb: &Handle) -> ResponseData {
    let answer = match req {
        Request::GetBindings => kb
            .send(|reply| Command::Bindings(Cmd::Get(reply)))
            .await
            .map(|state| ResponseData::Bindings { state }),
        Request::ReadBindings => kb
            .send(|reply| Command::Bindings(Cmd::Read(reply)))
            .await
            .map(|state| ResponseData::Bindings { state }),
        Request::SetBindings { bindings, enabled } => kb
            .send(|reply| {
                Command::Bindings(Cmd::Set {
                    bindings,
                    enabled,
                    reply,
                })
            })
            .await
            .map(|()| ResponseData::Success),
        _ => Err("not a key-bindings request".to_string()),
    };
    answer.unwrap_or_else(|message| ResponseData::Error { message })
}

/// A key's name on the keyboard picture, for messages.
fn label(code: u8) -> String {
    match ANSI_TKL.iter().find(|k| k.0 == code) {
        Some((_, "", ..)) => "Space".into(),
        Some((_, label, ..)) => (*label).to_owned(),
        None => format!("key {code:#04x}"),
    }
}

/// What can be checked without the keyboard: the contract's checks, then that every key
/// is on this keyboard's layer and the Fn key is a key it has.
fn check(bindings: &Bindings) -> Result<(), String> {
    bindings.validate()?;
    for (layer, kl) in LAYERS {
        if let Some(k) = bindings
            .layer(layer)
            .iter()
            .find(|k| !kl.keys().contains(&k.code))
        {
            return Err(match layer {
                Layer::Second => format!(
                    "{} has a fixed switch, so it has no deep press",
                    label(k.code)
                ),
                Layer::Normal | Layer::Fn => {
                    format!("{} can't be bound on this keyboard", label(k.code))
                }
            });
        }
    }
    match bindings.fn_key {
        Some(k) if !kb::KEYS.contains(&k) => Err(format!("{} can't hold the Fn layer", label(k))),
        _ => Ok(()),
    }
}

/// The changes `new` makes on `layer`: its own bindings, and every key `old` bound there
/// that `new` doesn't, back to its default.
fn changes(new: &Bindings, old: &Bindings, layer: Layer) -> Vec<(u8, Change)> {
    let now = new.layer(layer);
    let mut out: Vec<(u8, Change)> = now.iter().map(|b| (b.code, change(&b.action))).collect();
    out.extend(
        old.layer(layer)
            .iter()
            .filter(|b| !now.iter().any(|n| n.code == b.code))
            .map(|b| (b.code, Change::Default)),
    );
    out
}

fn change(action: &KeyAction) -> Change {
    Change::To(match action {
        KeyAction::Default => return Change::Default,
        KeyAction::Keys { codes } => Action::Keys(codes.clone()),
        KeyAction::Consumer { usage } => Action::Consumer(*usage),
        KeyAction::Mouse { button } => Action::Mouse(*button),
        KeyAction::Cpi => Action::Cpi,
        KeyAction::WheelUp => Action::WheelUp,
        KeyAction::WheelDown => Action::WheelDown,
        KeyAction::PanLeft => Action::PanLeft,
        KeyAction::PanRight => Action::PanRight,
        KeyAction::Firmware { raw } => Action::Firmware(*raw),
        KeyAction::Macro { slot } => Action::Macro(*slot),
        KeyAction::Unbound => Action::Unbound,
    })
}

/// What a block does, as the contract says it; `None` for what it has no word for (a
/// host-assisted function, or one the source doesn't name). The write keeps those as read.
fn key_action(action: Action) -> Option<KeyAction> {
    Some(match action {
        Action::Unbound => KeyAction::Unbound,
        Action::Mouse(button) => KeyAction::Mouse { button },
        Action::Cpi => KeyAction::Cpi,
        Action::WheelUp => KeyAction::WheelUp,
        Action::WheelDown => KeyAction::WheelDown,
        Action::PanLeft => KeyAction::PanLeft,
        Action::PanRight => KeyAction::PanRight,
        Action::Keys(codes) => KeyAction::Keys { codes },
        Action::Consumer(usage) => KeyAction::Consumer { usage },
        Action::Firmware(raw) => KeyAction::Firmware { raw },
        Action::Macro(slot) => KeyAction::Macro { slot },
        Action::External(_) | Action::Other { .. } => return None,
    })
}

/// The bindings a read shows: every key whose block isn't its factory default.
fn decode(layers: &Layers) -> Bindings {
    let mut out = Bindings::default();
    for ((layer, kl), blocks) in LAYERS.into_iter().zip(layers) {
        for b in blocks
            .iter()
            .filter(|b| **b != Block::default_for(kl, b.code))
        {
            if let Some(action) = key_action(b.action()) {
                out.layer_mut(layer).push(KeyBinding {
                    code: b.code,
                    action,
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keyboard::spawn_fake;
    use crate::scratch::Scratch;
    use gg_device_keyboard::bindings::{read_request, write_report, FIRMWARE, STOCK_FN_LAYER};
    use gg_device_keyboard::testing::{FakeTransport, Key, Kind, Read};

    const CAPS: u8 = 0x39;

    fn gate_on() -> Experimental {
        Experimental {
            keyboard_bindings: true,
            ..Experimental::default()
        }
    }

    fn ctrl_a() -> KeyAction {
        KeyAction::Keys {
            codes: vec![0xE0, 0x04],
        }
    }

    fn bind(code: u8, action: KeyAction) -> KeyBinding {
        KeyBinding { code, action }
    }

    /// A stock keyboard's layer: the self-map, the nine shortcuts on Fn, nothing deep.
    fn stock(layer: kb::Layer) -> Vec<Block> {
        layer
            .keys()
            .iter()
            .map(|&code| match STOCK_FN_LAYER.iter().find(|s| s.0 == code) {
                Some(&(_, codes)) if layer == kb::Layer::Fn => Block {
                    code,
                    function: FIRMWARE,
                    codes,
                },
                _ => Block::default_for(layer, code),
            })
            .collect()
    }

    fn reply(layer: kb::Layer, blocks: &[Block]) -> Vec<u8> {
        let mut r = vec![0xB6, 0, layer.byte(), blocks.len() as u8];
        r.extend(blocks.iter().flat_map(|b| b.bytes()));
        r
    }

    /// A fake that answers reads as a stock keyboard does.
    fn stock_keyboard() -> FakeTransport {
        let fake = FakeTransport::new();
        for (_, layer) in LAYERS {
            fake.answer_feature(Key::sub(0xB6, layer.byte()), &reply(layer, &stock(layer)));
        }
        fake
    }

    /// The normal layer with Caps Lock typing Ctrl+A.
    fn caps_layer() -> Vec<Block> {
        let mut blocks = stock(kb::Layer::Normal);
        let caps = blocks.iter_mut().find(|b| b.code == CAPS).unwrap();
        *caps = Block::new(CAPS, &Action::Keys(vec![0xE0, 0x04])).unwrap();
        blocks
    }

    /// Two commands, so the connection's `on_connect` (after the first) has run.
    async fn settle(kb: &Handle) -> BindingsState {
        kb.send(|r| Command::Bindings(Cmd::Get(r))).await.unwrap();
        kb.send(|r| Command::Bindings(Cmd::Get(r))).await.unwrap()
    }

    /// The first bindings event announced so far, passing the others by.
    fn bindings_event(rx: &mut broadcast::Receiver<Event>) -> Option<BindingsState> {
        while let Ok(event) = rx.try_recv() {
            if let Event::BindingsChanged { state } = event {
                return Some(state);
            }
        }
        None
    }

    async fn get(kb: &Handle) -> BindingsState {
        match handle(Request::GetBindings, kb).await {
            ResponseData::Bindings { state } => state,
            other => panic!("{other:?}"),
        }
    }

    fn set(normal: Vec<KeyBinding>, enabled: bool) -> Request {
        Request::SetBindings {
            bindings: Bindings {
                normal,
                ..Bindings::default()
            },
            enabled,
        }
    }

    fn sent_ops(fake: &FakeTransport) -> Vec<(Kind, u8, u8)> {
        fake.sent()
            .iter()
            .map(|s| (s.kind, s.report[1], s.report[2]))
            .collect()
    }

    const READS: [(Kind, u8, u8); 3] = [
        (Kind::Feature, 0xB6, 0x00),
        (Kind::Feature, 0xB6, 0x01),
        (Kind::Feature, 0xB6, 0x02),
    ];

    #[tokio::test]
    async fn with_the_gate_off_reads_and_changes_are_refused_and_nothing_reaches_the_keyboard() {
        let d = Scratch::new("bindings-gate-off");
        let fake = stock_keyboard();
        let (tx, _rx) = broadcast::channel(16);
        let kb = spawn_fake(fake.keyboard(), d.0.clone(), tx, Experimental::default());
        settle(&kb).await;
        for req in [
            Request::ReadBindings,
            set(vec![bind(CAPS, ctrl_a())], true),
            set(vec![], false),
        ] {
            match handle(req, &kb).await {
                ResponseData::Error { message } => {
                    assert!(
                        message.contains("key bindings and deep press")
                            && message.contains("hasn't been tried on your hardware yet"),
                        "{message}"
                    );
                }
                other => panic!("{other:?}"),
            }
        }
        assert!(fake.sent().is_empty(), "{:?}", sent_ops(&fake));
        assert!(!d.0.join("bindings.json").exists(), "nothing saved");
        assert_eq!(get(&kb).await, BindingsState::default());
        kb.send::<()>(Command::Shutdown).await.unwrap();
    }

    #[tokio::test]
    async fn a_change_reads_all_three_layers_then_writes_only_the_one_that_differs() {
        let d = Scratch::new("bindings-write");
        let fake = stock_keyboard();
        let (tx, mut rx) = broadcast::channel(16);
        let kb = spawn_fake(fake.keyboard(), d.0.clone(), tx, gate_on());
        settle(&kb).await;
        assert!(fake.sent().is_empty(), "off: nothing read on connect");

        let answer = handle(set(vec![bind(CAPS, ctrl_a())], true), &kb).await;
        assert!(matches!(answer, ResponseData::Success), "{answer:?}");
        let sent = fake.sent();
        assert_eq!(
            sent_ops(&fake),
            [READS[0], READS[1], READS[2], (Kind::Feature, 0x36, 0x00)]
        );
        for (s, (_, layer)) in sent.iter().zip(LAYERS) {
            assert_eq!(s.report, read_request(layer, layer.keys()).unwrap());
        }
        let write = &sent[3].report;
        assert_eq!(
            *write,
            write_report(kb::Layer::Normal, &caps_layer()).unwrap()
        );
        let at = kb::KEYS.iter().position(|&c| c == CAPS).unwrap();
        assert_eq!(
            write[4 + at * 6..10 + at * 6],
            [0x39, 0x51, 0xE0, 0x04, 0, 0]
        );

        let state = get(&kb).await;
        assert!(state.enabled);
        assert_eq!(state.bindings.normal, [bind(CAPS, ctrl_a())]);
        assert_eq!(state.error, None);
        let read = state.read.unwrap();
        assert!(read.normal.is_empty() && read.second.is_empty());
        assert_eq!(read.fn_layer.len(), 9, "the stock shortcuts");
        assert!(read
            .fn_layer
            .iter()
            .all(|b| matches!(b.action, KeyAction::Firmware { .. })));
        let saved = std::fs::read_to_string(d.0.join("bindings.json")).unwrap();
        assert!(
            saved.contains("\"enabled\": true") && saved.contains("57"),
            "{saved}"
        );
        assert!(bindings_event(&mut rx).unwrap().enabled);
        kb.send::<()>(Command::Shutdown).await.unwrap();
    }

    #[tokio::test]
    async fn the_fn_layer_keeps_the_keyboards_own_shortcuts_and_the_mask_follows_it() {
        let d = Scratch::new("bindings-fn");
        let fake = stock_keyboard();
        let (tx, _rx) = broadcast::channel(16);
        let kb = spawn_fake(fake.keyboard(), d.0.clone(), tx, gate_on());
        settle(&kb).await;
        let fn_f1_mutes = Request::SetBindings {
            bindings: Bindings {
                fn_layer: vec![bind(0x3A, KeyAction::Consumer { usage: 226 })],
                fn_key: Some(0xE7),
                ..Bindings::default()
            },
            enabled: true,
        };
        assert!(matches!(
            handle(fn_f1_mutes, &kb).await,
            ResponseData::Success
        ));
        assert_eq!(
            sent_ops(&fake),
            [
                READS[0],
                READS[1],
                READS[2],
                (Kind::Feature, 0x36, 0x01),
                (Kind::Output, 0x35, 0xE7),
                (Kind::Output, 0x3C, fake.sent()[5].report[2]),
            ]
        );
        let write = &fake.sent()[3].report;
        for (i, block) in stock(kb::Layer::Fn).iter().enumerate() {
            let sent = &write[4 + i * 6..10 + i * 6];
            if block.code == 0x3A {
                assert_eq!(sent, [0x3A, 0x61, 0xE2, 0x00, 0, 0]);
            } else {
                assert_eq!(
                    sent,
                    block.bytes(),
                    "key {:#04x} kept byte for byte",
                    block.code
                );
            }
        }
        let mut fn_layer = stock(kb::Layer::Fn);
        fn_layer
            .iter_mut()
            .find(|b| b.code == 0x3A)
            .unwrap()
            .function = 0x61;
        assert_eq!(fake.sent()[5].report[2..34], kb::fn_mask(&fn_layer));
        assert_eq!(
            fake.sent()[5].report[2 + 0x3A / 8] & (1 << (0x3A & 7)),
            1 << 2
        );

        // A change to one of the shortcuts is refused, and saves and sends nothing.
        let n = fake.sent().len();
        let f9_off = Request::SetBindings {
            bindings: Bindings {
                fn_layer: vec![bind(0x42, KeyAction::Unbound)],
                ..Bindings::default()
            },
            enabled: true,
        };
        match handle(f9_off, &kb).await {
            ResponseData::Error { message } => {
                assert!(message.contains("own shortcuts"), "{message}")
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(fake.sent().len(), n);
        assert_eq!(get(&kb).await.bindings.fn_layer.len(), 1, "F1 still");
        kb.send::<()>(Command::Shutdown).await.unwrap();
    }

    #[tokio::test]
    async fn saved_bindings_go_to_the_keyboard_on_connect_only_with_the_gate_on() {
        let d = Scratch::new("bindings-connect");
        let saved = r#"{"enabled":true,"bindings":{"normal":[{"code":57,"action":{"kind":"keys","codes":[224,4]}}]}}"#;
        std::fs::write(d.0.join("bindings.json"), saved).unwrap();

        let fake = stock_keyboard();
        let (tx, _rx) = broadcast::channel(16);
        let kb = spawn_fake(fake.keyboard(), d.0.clone(), tx, Experimental::default());
        let state = settle(&kb).await;
        assert!(state.enabled && state.read.is_none());
        assert!(fake.sent().is_empty(), "gate off");
        kb.send::<()>(Command::Shutdown).await.unwrap();

        let fake = stock_keyboard();
        let (tx, mut rx) = broadcast::channel(16);
        let kb = spawn_fake(fake.keyboard(), d.0.clone(), tx, gate_on());
        let state = settle(&kb).await;
        assert_eq!(
            sent_ops(&fake),
            [READS[0], READS[1], READS[2], (Kind::Feature, 0x36, 0x00)]
        );
        assert_eq!(
            fake.sent()[3].report,
            write_report(kb::Layer::Normal, &caps_layer()).unwrap()
        );
        assert!(state.read.is_some() && state.error.is_none());
        assert!(bindings_event(&mut rx).is_some());
        kb.send::<()>(Command::Shutdown).await.unwrap();
    }

    #[tokio::test]
    async fn a_lagging_keyboard_shows_the_old_bindings_once_and_writes_arent_trusted_to_reads() {
        let d = Scratch::new("bindings-lag");
        let fake = stock_keyboard();
        fake.change_after(
            Key::sub(0x36, 0x00),
            Read::Feature,
            Key::sub(0xB6, 0x00),
            &reply(kb::Layer::Normal, &caps_layer()),
        );
        let (tx, _rx) = broadcast::channel(16);
        let kb = spawn_fake(fake.keyboard(), d.0.clone(), tx, gate_on());
        settle(&kb).await;
        let caps = || set(vec![bind(CAPS, ctrl_a())], true);
        assert!(matches!(handle(caps(), &kb).await, ResponseData::Success));
        let read = |kb: Handle| async move {
            match handle(Request::ReadBindings, &kb).await {
                ResponseData::Bindings { state } => state.read.unwrap().normal,
                other => panic!("{other:?}"),
            }
        };
        assert!(read(kb.clone()).await.is_empty(), "the first read lags");
        assert_eq!(read(kb.clone()).await, [bind(CAPS, ctrl_a())]);

        // The same again writes nothing: gglinux knows what it wrote.
        fake.clear_sent();
        assert!(matches!(handle(caps(), &kb).await, ResponseData::Success));
        assert!(fake.sent().is_empty(), "{:?}", sent_ops(&fake));

        // Dropped from the list, Caps Lock goes back to itself.
        assert!(matches!(
            handle(set(vec![], true), &kb).await,
            ResponseData::Success
        ));
        assert_eq!(sent_ops(&fake), [(Kind::Feature, 0x36, 0x00)]);
        assert_eq!(
            fake.sent()[0].report,
            write_report(kb::Layer::Normal, &stock(kb::Layer::Normal)).unwrap()
        );
        kb.send::<()>(Command::Shutdown).await.unwrap();
    }

    #[tokio::test]
    async fn a_keyboard_that_answers_badly_is_reported_and_nothing_is_written() {
        let d = Scratch::new("bindings-bad-read");
        let fake = FakeTransport::new(); // answers `B6 00`: no bindings
        let (tx, _rx) = broadcast::channel(16);
        let kb = spawn_fake(fake.keyboard(), d.0.clone(), tx, gate_on());
        settle(&kb).await;
        match handle(set(vec![bind(CAPS, ctrl_a())], true), &kb).await {
            ResponseData::Error { message } => {
                assert!(message.contains("normal layer"), "{message}")
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(sent_ops(&fake), [READS[0]]);
        let state = get(&kb).await;
        assert!(state.error.is_some() && state.bindings.normal.is_empty());
        assert!(!d.0.join("bindings.json").exists());
        kb.send::<()>(Command::Shutdown).await.unwrap();
    }

    #[test]
    fn bindings_must_name_keys_this_keyboard_has_on_that_layer() {
        let with = |layer: Layer, code: u8| {
            let mut b = Bindings::default();
            b.layer_mut(layer).push(bind(code, KeyAction::Cpi));
            check(&b)
        };
        assert!(with(Layer::Normal, CAPS).is_ok());
        assert_eq!(
            with(Layer::Normal, 0x46).unwrap_err(),
            "PrtSc can't be bound on this keyboard"
        );
        assert_eq!(
            with(Layer::Second, 0x29).unwrap_err(),
            "Esc has a fixed switch, so it has no deep press"
        );
        assert!(with(Layer::Second, 0x1A).is_ok());
        assert!(with(Layer::Fn, 0x3A).is_ok());
        let fn_key = Bindings {
            fn_key: Some(0x48),
            ..Bindings::default()
        };
        assert_eq!(check(&fn_key).unwrap_err(), "Pause can't hold the Fn layer");
        let twice = Bindings {
            normal: vec![bind(4, KeyAction::Cpi), bind(4, KeyAction::Unbound)],
            ..Bindings::default()
        };
        assert!(check(&twice).is_err(), "the contract's own checks first");
    }

    #[test]
    fn a_read_shows_only_what_differs_from_the_factory() {
        let mut layers: Layers = LAYERS.map(|(_, l)| stock(l));
        layers[0] = caps_layer();
        layers[2][0] = Block::new(0x04, &Action::Mouse(4)).unwrap();
        layers[0][1] = Block {
            code: 0x05,
            function: 0x72,
            codes: [1, 2, 3, 4],
        };
        let b = decode(&layers);
        assert_eq!(b.normal, [bind(CAPS, ctrl_a())], "the 0x72 one has no word");
        assert_eq!(b.second, [bind(0x04, KeyAction::Mouse { button: 4 })]);
        assert_eq!(b.fn_layer.len(), 9);
        assert_eq!(b.fn_key, None, "the Fn key can't be read back");
    }

    #[test]
    fn a_key_dropped_from_the_list_goes_back_to_its_default() {
        let old = Bindings {
            normal: vec![bind(CAPS, ctrl_a()), bind(0x04, KeyAction::Cpi)],
            ..Bindings::default()
        };
        let new = Bindings {
            normal: vec![bind(0x04, KeyAction::Unbound)],
            ..Bindings::default()
        };
        assert_eq!(
            changes(&new, &old, Layer::Normal),
            [(0x04, Change::To(Action::Unbound)), (CAPS, Change::Default)]
        );
        assert!(changes(&new, &old, Layer::Fn).is_empty());
    }

    #[test]
    fn an_unreadable_bindings_file_is_set_aside_and_the_defaults_used() {
        let d = Scratch::new("bindings-unreadable");
        std::fs::write(d.0.join("bindings.json"), "{not json").unwrap();
        let store = Store::load(&d.0);
        assert_eq!(store.state, BindingsState::default());
        let names: Vec<String> = std::fs::read_dir(&d.0)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            names
                .iter()
                .any(|n| n.starts_with("bindings.json.unreadable")),
            "{names:?}"
        );
    }
}
