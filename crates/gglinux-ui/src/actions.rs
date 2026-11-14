// SPDX-License-Identifier: GPL-3.0-or-later
//! Actions on the spare keys F13–F24, from [`State::actions`]: a card on the
//! Settings page, after the parts, shown once the daemon has said what's bound.
//!
//! The rows are a working copy ([`ActionsForm`]): edited here, checked as the
//! daemon checks them, and saved whole (`set_actions`). The daemon then asks the
//! desktop to bind any new key. A mouse button runs an action once the mouse
//! panel binds the button to the action's key.

use std::rc::Rc;

use gg_ipc::{Action, ActionBinding, ActionsState, Request, ResponseData};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::client::{self, Client, State, TIMEOUT};
use crate::experimental::{panel_error, read_back, Pending};
use crate::{sync_rows, with_forms, ActionRow, ActionsUi, AppWindow, Forms};

/// F13; the keys go up to F24.
const FIRST: u8 = 0x68;
const KEYS: u8 = 12;

/// What an action does, in the order the panel lists them.
pub const KINDS: [&str; 5] = [
    "Run a program",
    "Type text",
    "Apply a profile",
    "Save a clip",
    "Media key",
];
const RUN: usize = 0;
const TYPE: usize = 1;
const PROFILE: usize = 2;
const CLIP: usize = 3;
const MEDIA_KEY: usize = 4;

/// The media keys, as consumer-page usages, in the order the panel lists them.
pub const MEDIA: [(u16, &str); 7] = [
    (0xCD, "Play/pause"),
    (0xB5, "Next track"),
    (0xB6, "Previous track"),
    (0xB7, "Stop"),
    (0xE2, "Mute"),
    (0xE9, "Volume up"),
    (0xEA, "Volume down"),
];

/// One row as the panel edits it. A program's line stays as typed, so a space
/// typed at its end isn't lost before the next word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub key: u8,
    pub label: String,
    pub kind: usize,
    pub value: String,
    pub media: usize,
}

impl Row {
    pub fn of(b: &ActionBinding) -> Row {
        let (kind, value, media) = match &b.action {
            Action::Run { argv } => (RUN, join_words(argv), 0),
            Action::Type { text } => (TYPE, text.clone(), 0),
            Action::ProfileApply { name } => (PROFILE, name.clone(), 0),
            Action::ClipSave => (CLIP, String::new(), 0),
            Action::Media { usage } => (
                MEDIA_KEY,
                String::new(),
                MEDIA.iter().position(|(u, _)| u == usage).unwrap_or(0),
            ),
        };
        Row {
            key: b.key,
            label: b.label.clone(),
            kind,
            value,
            media,
        }
    }

    /// The binding to send. With no name, it's named after what it does.
    pub fn binding(&self) -> ActionBinding {
        let action = match self.kind {
            RUN => Action::Run {
                argv: split_words(&self.value),
            },
            TYPE => Action::Type {
                text: self.value.clone(),
            },
            PROFILE => Action::ProfileApply {
                name: self.value.trim().to_string(),
            },
            CLIP => Action::ClipSave,
            _ => Action::Media {
                usage: MEDIA.get(self.media).map_or(MEDIA[0].0, |m| m.0),
            },
        };
        let label = match self.label.trim() {
            "" => default_label(&action),
            l => l.to_string(),
        };
        ActionBinding {
            key: self.key,
            label,
            action,
        }
    }
}

fn default_label(action: &Action) -> String {
    let label = match action {
        Action::Run { argv } => argv
            .first()
            .map_or("Program", |p| p.rsplit('/').next().unwrap_or(p))
            .to_string(),
        Action::Type { .. } => "Type text".into(),
        Action::ProfileApply { name } => name.clone(),
        Action::ClipSave => "Save clip".into(),
        Action::Media { usage } => MEDIA
            .iter()
            .find(|(u, _)| u == usage)
            .map_or("Media key", |m| m.1)
            .into(),
    };
    label.chars().take(64).collect()
}

/// A program's line as words, split on spaces outside quotes ("…" or '…'). No
/// shell sees it: nothing is expanded, and `;` or `$HOME` are just words.
pub fn split_words(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('"'), '\\') => match chars.next() {
                Some(n @ ('"' | '\\')) => word.push(n),
                Some(n) => {
                    word.push('\\');
                    word.push(n);
                }
                None => word.push('\\'),
            },
            (Some(_), c) => word.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                in_word = true;
            }
            (None, c) if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            (None, c) => {
                word.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        words.push(word);
    }
    words
}

/// Words as a line [`split_words`] reads back the same.
pub fn join_words(words: &[String]) -> String {
    words
        .iter()
        .map(|w| {
            if !w.is_empty()
                && !w
                    .chars()
                    .any(|c| c.is_whitespace() || c == '"' || c == '\'')
            {
                w.clone()
            } else {
                format!("\"{}\"", w.replace('\\', "\\\\").replace('"', "\\\""))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// What the panel keeps between the daemon's answers.
#[derive(Debug, Default)]
pub struct ActionsForm {
    /// The list being edited; `None` while the panel shows the daemon's.
    pub draft: Option<Vec<Row>>,
    /// A saved list not seen back from the daemon yet.
    pub pending: Pending<Vec<ActionBinding>>,
    /// Why the last save didn't go through.
    pub error: String,
    /// Show the Hyprland lines whatever the desktop (a `shot` step).
    pub hyprland: bool,
}

impl ActionsForm {
    /// The rows to show while the daemon has `daemon`.
    pub fn rows(&mut self, daemon: &[ActionBinding]) -> Vec<Row> {
        match &self.draft {
            Some(rows) => rows.clone(),
            None => self
                .pending
                .show(&daemon.to_vec())
                .iter()
                .map(Row::of)
                .collect(),
        }
    }

    /// Change the rows, starting from what's shown.
    pub fn edit(&mut self, daemon: &[ActionBinding], change: impl FnOnce(&mut Vec<Row>)) {
        let mut rows = self.rows(daemon);
        change(&mut rows);
        self.draft = Some(rows);
    }

    /// A row for the first key without one; none once all twelve have one.
    pub fn add(&mut self, daemon: &[ActionBinding]) {
        self.edit(daemon, |rows| {
            let free = (FIRST..FIRST + KEYS).find(|k| rows.iter().all(|r| r.key != *k));
            if let Some(key) = free {
                rows.push(Row {
                    key,
                    label: String::new(),
                    kind: RUN,
                    value: String::new(),
                    media: 0,
                });
            }
        });
    }

    /// The list to save, or why it can't be saved as it is; `None` when nothing
    /// changed.
    pub fn to_save(&self, daemon: &[ActionBinding]) -> Option<Result<Vec<ActionBinding>, String>> {
        let rows = self.draft.as_ref()?;
        let list: Vec<ActionBinding> = rows.iter().map(Row::binding).collect();
        if list == daemon {
            return None;
        }
        Some(gg_ipc::validate_actions(&list).map(|()| list))
    }

    /// The list went out: show it until the daemon does.
    pub fn saved(&mut self, list: Vec<ActionBinding>) {
        self.draft = None;
        self.pending.sent(list);
    }
}

/// What the panel says about the desktop's shortcuts.
pub fn shortcuts_note(st: &ActionsState, any: bool) -> String {
    match (&st.shortcuts, any) {
        (Some(said), _) => format!("Desktop shortcuts: {said}."),
        (None, false) => "When you save, gglinuxd asks the desktop to bind the new keys (KDE asks you to confirm each one once).".into(),
        (None, true) => "The desktop hasn't bound these keys yet.".into(),
    }
}

/// Whether text can be typed here, as the panel says it.
pub fn typing_note(typing: bool) -> String {
    if typing {
        "Typing text works on this desktop.".into()
    } else {
        "Typing text works on Hyprland and other wlroots desktops, with wtype installed. Plasma needs its remote-desktop permission for typing, which isn't built yet.".into()
    }
}

/// The global shortcut's name in Hyprland's `global` dispatcher.
fn global(key: u8) -> String {
    let name = ActionBinding::key_name(key).unwrap_or_default();
    format!("io.github.ykk.gglinux:action-{}", name.to_ascii_lowercase())
}

/// What hyprland.lua and hyprland.conf need for these keys: one line each.
pub fn hyprland_lines(keys: &[u8]) -> (String, String) {
    let name = |k: u8| ActionBinding::key_name(k).unwrap_or_default();
    let lua: Vec<String> = keys
        .iter()
        .map(|&k| format!("hl.bind(\"{}\", hl.dsp.global(\"{}\"))", name(k), global(k)))
        .collect();
    let conf: Vec<String> = keys
        .iter()
        .map(|&k| format!("bind = , {}, global, {}", name(k), global(k)))
        .collect();
    (lua.join("\n"), conf.join("\n"))
}

fn on_hyprland() -> bool {
    std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|d| d.to_ascii_lowercase().contains("hyprland"))
        || std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some()
}

fn placeholder(kind: usize) -> &'static str {
    match kind {
        RUN => "Program and its arguments, e.g. kitty --single-instance",
        TYPE => "Text to type",
        PROFILE => "Name of a saved profile",
        _ => "",
    }
}

fn ui_row(r: &Row) -> ActionRow {
    ActionRow {
        key: i32::from(r.key - FIRST),
        label: r.label.as_str().into(),
        kind: r.kind as i32,
        value: r.value.as_str().into(),
        placeholder: placeholder(r.kind).into(),
        media: r.media as i32,
    }
}

/// Show the daemon's actions, or the list being edited.
pub fn show(ui: &AppWindow, s: &State) {
    let g = ui.global::<ActionsUi>();
    let Some(st) = &s.actions else {
        // Not read yet, or a daemon from before actions.
        g.set_shown(false);
        return;
    };
    let (rows, save, error, hyprland) = with_forms(|f| {
        let mut form = f.actions.borrow_mut();
        let rows = form.rows(&st.bindings);
        let save = form.to_save(&st.bindings);
        (
            rows,
            save,
            form.error.clone(),
            form.hyprland || on_hyprland(),
        )
    });
    sync_rows(g.get_rows(), rows.iter().map(ui_row).collect(), |m| {
        g.set_rows(m)
    });
    g.set_changed(save.is_some());
    g.set_problem(match &save {
        Some(Err(why)) => capitalised(why).into(),
        _ => SharedString::new(),
    });
    g.set_can_add(rows.len() < usize::from(KEYS));
    g.set_shortcuts(shortcuts_note(st, !rows.is_empty()).into());
    let types = rows.iter().any(|r| r.kind == TYPE);
    g.set_typing(typing_note(st.typing).into());
    g.set_typing_bad(!st.typing && types);
    let keys: Vec<u8> = rows.iter().map(|r| r.key).collect();
    let (lua, conf) = hyprland_lines(&keys);
    g.set_hyprland(hyprland && !keys.is_empty());
    g.set_lua(lua.into());
    g.set_conf(conf.into());
    let error = match (error.is_empty(), &st.error) {
        (true, Some(daemon)) => capitalised(daemon),
        _ => error,
    };
    g.set_error(error.into());
    g.set_shown(true);
}

fn capitalised(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|first| first.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

fn model(items: impl Iterator<Item = String>) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(
        items.map(SharedString::from).collect::<Vec<_>>(),
    ))
}

/// Run `change` on the rows, then redraw the panel.
fn change(ui: &AppWindow, client: &Client, forms: &Forms, edit: impl FnOnce(&mut Vec<Row>)) {
    let s = client.state();
    let daemon = s
        .actions
        .as_ref()
        .map(|a| a.bindings.clone())
        .unwrap_or_default();
    forms.actions.borrow_mut().edit(&daemon, edit);
    show(ui, &s);
}

/// Answer the panel's callbacks.
pub fn bind(ui: &AppWindow, client: &Client, forms: &Rc<Forms>) {
    let g = ui.global::<ActionsUi>();
    // Set once: a list box resets its pick whenever its model is replaced.
    g.set_keys(model(
        (FIRST..FIRST + KEYS).filter_map(ActionBinding::key_name),
    ));
    g.set_kinds(model(KINDS.iter().map(|k| k.to_string())));
    g.set_media(model(MEDIA.iter().map(|m| m.1.to_string())));

    macro_rules! on {
        ($set:ident, |$($arg:ident),*| $rows:ident => $body:expr) => {{
            let (weak, c, forms) = (ui.as_weak(), client.clone(), Rc::clone(forms));
            g.$set(move |$($arg),*| {
                if let Some(ui) = weak.upgrade() {
                    change(&ui, &c, &forms, |$rows: &mut Vec<Row>| $body);
                }
            });
        }};
    }
    let at = |i: i32| usize::try_from(i).ok();
    on!(on_remove, |i| rows => {
        if let Some(i) = at(i).filter(|&i| i < rows.len()) {
            rows.remove(i);
        }
    });
    on!(on_set_key, |i, k| rows => {
        if let (Some(r), Ok(k)) = (at(i).and_then(|i| rows.get_mut(i)), u8::try_from(k)) {
            r.key = FIRST + k.min(KEYS - 1);
        }
    });
    on!(on_set_label, |i, t| rows => {
        if let Some(r) = at(i).and_then(|i| rows.get_mut(i)) {
            r.label = t.to_string();
        }
    });
    on!(on_set_kind, |i, k| rows => {
        if let (Some(r), Some(k)) = (at(i).and_then(|i| rows.get_mut(i)), at(k)) {
            if r.kind != k {
                r.kind = k.min(KINDS.len() - 1);
                r.value.clear();
            }
        }
    });
    on!(on_set_value, |i, t| rows => {
        if let Some(r) = at(i).and_then(|i| rows.get_mut(i)) {
            r.value = t.to_string();
        }
    });
    on!(on_set_media, |i, k| rows => {
        if let (Some(r), Some(k)) = (at(i).and_then(|i| rows.get_mut(i)), at(k)) {
            r.media = k.min(MEDIA.len() - 1);
        }
    });

    let (weak, c, f) = (ui.as_weak(), client.clone(), Rc::clone(forms));
    g.on_add(move || {
        if let Some(ui) = weak.upgrade() {
            let s = c.state();
            let daemon = s
                .actions
                .as_ref()
                .map(|a| a.bindings.clone())
                .unwrap_or_default();
            f.actions.borrow_mut().add(&daemon);
            show(&ui, &s);
        }
    });
    let (weak, c, f) = (ui.as_weak(), client.clone(), Rc::clone(forms));
    g.on_undo(move || {
        if let Some(ui) = weak.upgrade() {
            let mut form = f.actions.borrow_mut();
            form.draft = None;
            form.error.clear();
            drop(form);
            show(&ui, &c.state());
        }
    });
    let (weak, c, f) = (ui.as_weak(), client.clone(), Rc::clone(forms));
    g.on_save(move || {
        let Some(ui) = weak.upgrade() else { return };
        let s = c.state();
        let daemon = s
            .actions
            .as_ref()
            .map(|a| a.bindings.clone())
            .unwrap_or_default();
        let Some(Ok(list)) = f.actions.borrow().to_save(&daemon) else {
            return;
        };
        save(&ui, &c, &f, list);
    });
}

/// Send the list; once the daemon took it, read it back until it shows it.
fn save(ui: &AppWindow, client: &Client, forms: &Forms, list: Vec<ActionBinding>) {
    let g = ui.global::<ActionsUi>();
    g.set_busy(true);
    forms.actions.borrow_mut().error.clear();
    let weak = ui.as_weak();
    let reader = client.clone();
    let sent = list.clone();
    client.command(
        Request::SetActions { bindings: list },
        TIMEOUT,
        move |result| {
            client::runtime().spawn(async move {
                let ok = result.is_ok();
                if ok {
                    let shown = sent.clone();
                    let _ = weak.upgrade_in_event_loop(move |ui| {
                        with_forms(|f| f.actions.borrow_mut().saved(shown));
                        crate::refresh(&ui);
                    });
                    read_back(
                        &reader,
                        || Request::GetActions,
                        |a| match a {
                            ResponseData::Actions { state } => Some(state.bindings),
                            _ => None,
                        },
                        &sent,
                    )
                    .await;
                }
                let error = result.err().map(|e| panel_error(&e)).unwrap_or_default();
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    with_forms(|f| {
                        let mut form = f.actions.borrow_mut();
                        form.pending.settle();
                        form.error = error.to_string();
                    });
                    ui.global::<ActionsUi>().set_busy(false);
                    crate::refresh(&ui);
                });
            });
        },
    );
}

/// Start the panel's working copy afresh (see [`Forms::reset`]).
pub fn reset(forms: &Forms) {
    *forms.actions.borrow_mut() = ActionsForm::default();
}

/// The `shot` example's scripted steps for this panel (see [`crate::poke`]):
/// `actions.add`, `actions.save`, `actions.undo`, `actions.remove:N`,
/// `actions.kind:N` (of the last row), `actions.hyprland` (show its lines here),
/// and `actions.demo`, which fills in two rows.
pub fn poke(ui: &AppWindow, name: &str, n: i32) -> bool {
    let g = ui.global::<ActionsUi>();
    let last = g.get_rows().row_count() as i32 - 1;
    match name {
        "actions.add" => g.invoke_add(),
        "actions.save" => g.invoke_save(),
        "actions.undo" => g.invoke_undo(),
        "actions.remove" => g.invoke_remove(n),
        "actions.kind" => g.invoke_set_kind(last, n),
        "actions.hyprland" => {
            with_forms(|f| f.actions.borrow_mut().hyprland = true);
            crate::refresh(ui);
        }
        "actions.demo" => {
            g.invoke_add();
            g.invoke_set_label(0, "Terminal".into());
            g.invoke_set_value(0, "kitty --single-instance".into());
            g.invoke_add();
            g.invoke_set_label(1, "Pause the music".into());
            g.invoke_set_kind(1, MEDIA_KEY as i32);
        }
        _ => return false,
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding(key: u8, label: &str, action: Action) -> ActionBinding {
        ActionBinding {
            key,
            label: label.into(),
            action,
        }
    }

    #[test]
    fn a_program_line_splits_into_words_without_a_shell_and_joins_back() {
        assert_eq!(
            split_words("kitty --single-instance"),
            ["kitty", "--single-instance"]
        );
        assert_eq!(
            split_words(r#"sh -c "echo hi; rm -rf ~" '$HOME' a\b"#),
            ["sh", "-c", "echo hi; rm -rf ~", "$HOME", r"a\b"]
        );
        assert_eq!(
            split_words(r#"say "a \"b\" c" """#),
            ["say", r#"a "b" c"#, ""]
        );
        assert!(split_words("   ").is_empty());
        for words in [
            vec!["kitty".to_string()],
            vec![
                "echo".into(),
                "; rm -rf ~".into(),
                "".into(),
                r#"x"y'z"#.into(),
                r"a\b".into(),
            ],
        ] {
            assert_eq!(split_words(&join_words(&words)), words, "{words:?}");
        }
    }

    #[test]
    fn rows_and_bindings_turn_into_each_other() {
        let list = vec![
            binding(
                0x68,
                "Terminal",
                Action::Run {
                    argv: vec!["kitty".into(), "--hold".into()],
                },
            ),
            binding(
                0x69,
                "Hi",
                Action::Type {
                    text: "hello there".into(),
                },
            ),
            binding(
                0x6A,
                "Night",
                Action::ProfileApply {
                    name: "Night".into(),
                },
            ),
            binding(0x6B, "Clip", Action::ClipSave),
            binding(0x6C, "Louder", Action::Media { usage: 0xE9 }),
        ];
        for b in &list {
            assert_eq!(Row::of(b).binding(), *b);
        }
        assert_eq!(Row::of(&list[0]).value, "kitty --hold");
        assert_eq!(Row::of(&list[4]).media, 5);
        // No name: named after what it does.
        let mut r = Row::of(&list[0]);
        r.label = " ".into();
        r.value = "/usr/bin/foot".into();
        assert_eq!(r.binding().label, "foot");
    }

    #[test]
    fn edits_start_from_the_daemons_list_and_save_only_a_change_that_checks_out() {
        let daemon = vec![binding(
            0x68,
            "Terminal",
            Action::Run {
                argv: vec!["kitty".into()],
            },
        )];
        let mut form = ActionsForm::default();
        assert_eq!(form.rows(&daemon), [Row::of(&daemon[0])]);
        assert_eq!(form.to_save(&daemon), None, "nothing edited");
        form.add(&daemon);
        let rows = form.rows(&daemon);
        assert_eq!((rows.len(), rows[1].key), (2, 0x69), "the first free key");
        // A new row with no program can't be saved yet, and says why.
        let why = form.to_save(&daemon).unwrap().unwrap_err();
        assert_eq!(why, "the action on F14 needs a program to run");
        form.edit(&daemon, |rows| rows[1].value = "foot".into());
        let list = form.to_save(&daemon).unwrap().unwrap();
        assert_eq!(
            list[1].action,
            Action::Run {
                argv: vec!["foot".into()]
            }
        );
        // Editing it back is no change.
        form.edit(&daemon, |rows| {
            rows.pop();
        });
        assert_eq!(form.to_save(&daemon), None);
        // Two rows on one key are refused before anything is sent.
        form.add(&daemon);
        form.edit(&daemon, |rows| {
            rows[1].key = 0x68;
            rows[1].kind = CLIP;
        });
        assert_eq!(
            form.to_save(&daemon).unwrap().unwrap_err(),
            "F13 has two actions"
        );
    }

    #[test]
    fn a_saved_list_shows_until_the_daemon_shows_it_even_past_a_stale_read() {
        let old = vec![binding(
            0x68,
            "Terminal",
            Action::Run {
                argv: vec!["kitty".into()],
            },
        )];
        let mut new = old.clone();
        new.push(binding(0x69, "Clip", Action::ClipSave));
        let mut form = ActionsForm::default();
        form.add(&old);
        form.saved(new.clone());
        assert_eq!(form.draft, None);
        let shown: Vec<ActionBinding> = form.rows(&old).iter().map(Row::binding).collect();
        assert_eq!(shown, new, "the daemon's first read still has the old list");
        assert_eq!(form.rows(&new).len(), 2);
        form.pending.settle();
        assert_eq!(
            form.rows(&old).len(),
            1,
            "the reads are over: the daemon's list stands"
        );
    }

    #[test]
    fn only_twelve_keys_can_have_actions() {
        let mut form = ActionsForm::default();
        for _ in 0..14 {
            form.add(&[]);
        }
        let rows = form.rows(&[]);
        assert_eq!(rows.len(), 12);
        assert_eq!(rows.last().unwrap().key, 0x73);
    }

    #[test]
    fn the_notes_say_what_the_desktop_bound_whether_typing_works_and_the_hyprland_lines() {
        let mut st = ActionsState::default();
        assert!(shortcuts_note(&st, false).contains("KDE asks you to confirm"));
        assert_eq!(
            shortcuts_note(&st, true),
            "The desktop hasn't bound these keys yet."
        );
        st.shortcuts = Some("bound (fake)".into());
        assert_eq!(
            shortcuts_note(&st, true),
            "Desktop shortcuts: bound (fake)."
        );
        assert!(typing_note(false).contains("Plasma needs its remote-desktop permission"));
        assert!(typing_note(false).contains("wtype"));
        let (lua, conf) = hyprland_lines(&[0x68, 0x73]);
        assert_eq!(
            lua,
            "hl.bind(\"F13\", hl.dsp.global(\"io.github.ykk.gglinux:action-f13\"))\n\
             hl.bind(\"F24\", hl.dsp.global(\"io.github.ykk.gglinux:action-f24\"))"
        );
        assert_eq!(
            conf,
            "bind = , F13, global, io.github.ykk.gglinux:action-f13\n\
             bind = , F24, global, io.github.ykk.gglinux:action-f24"
        );
    }
}
