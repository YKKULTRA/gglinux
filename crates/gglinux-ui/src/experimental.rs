// SPDX-License-Identifier: GPL-3.0-or-later
//! The experimental switches, from [`State::experimental`]: features not tried on
//! real hardware yet stay off until turned on here. A card on the Settings page,
//! after the actions, shown once the daemon has said which are on.
//!
//! A switch sends the whole set at once (`set_experimental`), the daemon's with
//! that one changed. The panel shows what it sent until the daemon shows it too
//! ([`Pending`]): a daemon may still answer the first read after a change with
//! what it had before.

use std::rc::Rc;
use std::time::Duration;

use gg_ipc::{Experimental, Feature, Request, ResponseData};
use slint::{ComponentHandle, SharedString};

use crate::client::{self, Client, State, TIMEOUT};
use crate::{sync_rows, with_forms, AppWindow, ExperimentalUi, Forms, GateRow};

/// How long to wait before reading a change back a second time.
pub(crate) const READ_AGAIN: Duration = Duration::from_millis(300);

/// A change sent to the daemon and not seen back yet. Shown in place of what the
/// daemon says until the daemon says the same, or until [`Pending::settle`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending<T>(Option<T>);

impl<T> Default for Pending<T> {
    fn default() -> Self {
        Pending(None)
    }
}

impl<T: Clone + PartialEq> Pending<T> {
    pub fn sent(&mut self, value: T) {
        self.0 = Some(value);
    }

    /// What to show while the daemon says `now`.
    pub fn show(&mut self, now: &T) -> T {
        match &self.0 {
            Some(sent) if sent != now => sent.clone(),
            _ => {
                self.0 = None;
                now.clone()
            }
        }
    }

    /// The reads after a change are over: what the daemon says is what stands.
    pub fn settle(&mut self) {
        self.0 = None;
    }

    pub fn waiting(&self) -> bool {
        self.0.is_some()
    }
}

/// What the panel keeps between the daemon's answers.
#[derive(Debug, Default)]
pub struct ExperimentalForm {
    pub pending: Pending<Experimental>,
}

/// The gates the panel shows for what the daemon says.
pub fn shown(form: &mut ExperimentalForm, daemon: &Experimental) -> Experimental {
    form.pending.show(daemon)
}

/// One row per feature, in [`Feature::ALL`] order.
pub fn rows(e: &Experimental) -> Vec<GateRow> {
    Feature::ALL
        .iter()
        .map(|&f| GateRow {
            label: f.label().into(),
            on: e.on(f),
        })
        .collect()
}

/// The set to send for turning feature `i` on or off, from what's shown.
pub fn toggled(now: Experimental, i: usize, on: bool) -> Option<Experimental> {
    let mut e = now;
    e.set(*Feature::ALL.get(i)?, on);
    Some(e)
}

/// Show the daemon's experimental switches.
pub fn show(ui: &AppWindow, s: &State) {
    let g = ui.global::<ExperimentalUi>();
    let Some(daemon) = s.experimental else {
        // Not read yet, or a daemon from before them.
        g.set_shown(false);
        return;
    };
    let e = with_forms(|f| shown(&mut f.experimental.borrow_mut(), &daemon));
    sync_rows(g.get_gates(), rows(&e), |m| g.set_gates(m));
    g.set_shown(true);
}

/// Read the gates back until the daemon shows `sent`: twice at most, a moment apart.
pub(crate) async fn read_back<T: PartialEq>(
    client: &Client,
    req: fn() -> Request,
    pick: fn(ResponseData) -> Option<T>,
    sent: &T,
) {
    for n in 0..2 {
        if n > 0 {
            tokio::time::sleep(READ_AGAIN).await;
        }
        let (tx, rx) = tokio::sync::oneshot::channel();
        // The client keeps the answer, which redraws the panel.
        client.request(req(), TIMEOUT, move |answer| {
            let _ = tx.send(answer);
        });
        if let Ok(Ok(answer)) = rx.await {
            if pick(answer).as_ref() == Some(sent) {
                return;
            }
        }
    }
}

/// The error a panel shows for a change: none when the daemon went away, as the
/// window shows the offline view then.
pub(crate) fn panel_error(error: &str) -> SharedString {
    if client::lost_connection(error) {
        SharedString::new()
    } else {
        error.into()
    }
}

/// Answer the panel's switches.
pub fn bind(ui: &AppWindow, client: &Client, forms: &Rc<Forms>) {
    let g = ui.global::<ExperimentalUi>();
    let weak = ui.as_weak();
    let c = client.clone();
    let forms = Rc::clone(forms);
    g.on_set(move |i, on| {
        let Some(ui) = weak.upgrade() else { return };
        let Some(daemon) = c.state().experimental else {
            return;
        };
        let now = shown(&mut forms.experimental.borrow_mut(), &daemon);
        let Some(experimental) = usize::try_from(i).ok().and_then(|i| toggled(now, i, on)) else {
            return;
        };
        let g = ui.global::<ExperimentalUi>();
        g.set_busy(true);
        g.set_error(SharedString::new());
        forms.experimental.borrow_mut().pending.sent(experimental);
        show(&ui, &c.state());
        let weak = ui.as_weak();
        let reader = c.clone();
        c.command(
            Request::SetExperimental { experimental },
            TIMEOUT,
            move |result| {
                let read = result.is_ok();
                client::runtime().spawn(async move {
                    if read {
                        read_back(
                            &reader,
                            || Request::GetExperimental,
                            |a| match a {
                                ResponseData::Experimental { experimental } => Some(experimental),
                                _ => None,
                            },
                            &experimental,
                        )
                        .await;
                    }
                    let error = result.err().map(|e| panel_error(&e)).unwrap_or_default();
                    let _ = weak.upgrade_in_event_loop(move |ui| {
                        with_forms(|f| f.experimental.borrow_mut().pending.settle());
                        let g = ui.global::<ExperimentalUi>();
                        g.set_busy(false);
                        g.set_error(error);
                        crate::refresh(&ui);
                    });
                });
            },
        );
    });
}

/// Start the panel's working copy afresh (see [`Forms::reset`]).
pub fn reset(forms: &Forms) {
    *forms.experimental.borrow_mut() = ExperimentalForm::default();
}

/// The `shot` example's scripted steps for this panel (see [`crate::poke`]):
/// `experimental.on:N` turns feature N on, `experimental.off:N` off.
pub fn poke(ui: &AppWindow, name: &str, n: i32) -> bool {
    let g = ui.global::<ExperimentalUi>();
    match name {
        "experimental.on" => g.invoke_set(n, true),
        "experimental.off" => g.invoke_set(n, false),
        _ => return false,
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn there_is_a_row_for_every_feature_in_order() {
        let mut e = Experimental::default();
        e.set(Feature::MousePower, true);
        let rows = rows(&e);
        assert_eq!(rows.len(), Feature::ALL.len());
        for (row, f) in rows.iter().zip(Feature::ALL) {
            assert_eq!(row.label, f.label());
            assert_eq!(row.on, f == Feature::MousePower, "{f:?}");
        }
    }

    #[test]
    fn a_switch_sends_the_whole_set_with_only_that_one_changed() {
        let mut now = Experimental::default();
        now.set(Feature::KeyboardOnboard, true);
        let sent = toggled(now, 4, true).unwrap();
        assert!(sent.on(Feature::MousePower), "Feature::ALL[4]");
        assert!(sent.on(Feature::KeyboardOnboard), "the others as they were");
        assert_eq!(Feature::ALL.iter().filter(|&&f| sent.on(f)).count(), 2);
        let past = Feature::ALL.len();
        assert_eq!(toggled(now, past, true), None, "no feature past the last");
    }

    #[test]
    fn what_was_sent_shows_until_the_daemon_shows_it_even_past_a_stale_read() {
        let before = Experimental::default();
        let mut after = before;
        after.set(Feature::HeadsetExtras, true);
        let mut form = ExperimentalForm::default();
        form.pending.sent(after);
        assert_eq!(shown(&mut form, &before), after, "a lagging read");
        assert_eq!(shown(&mut form, &after), after);
        assert!(!form.pending.waiting(), "seen back: done");
        assert_eq!(
            shown(&mut form, &before),
            before,
            "a later change elsewhere shows"
        );

        // Never seen back: once the reads are over, the daemon's word stands.
        form.pending.sent(after);
        form.pending.settle();
        assert_eq!(shown(&mut form, &before), before);
    }
}
