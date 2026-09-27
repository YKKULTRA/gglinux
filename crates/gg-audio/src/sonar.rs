//! Sonar: the channels, routing apps into them, their volumes, and ChatMix.
//!
//! PipeWire hosts the graph (see [`config`]); this drives it through
//! [`PwTools`], so every action is a command a person could type, and the whole
//! flow is tested against a fake.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use gg_ipc::{AppStream, AudioOutput, Channel, ChannelEq, ChannelState, EqBand, HeadphoneEq, SonarState};
use tracing::{debug, info, warn};

use crate::graph::Graph;
use crate::settings::{self, Settings};
use crate::tools::{self, PwTools};
use crate::{config, eq, mix};

/// How long PipeWire gets to bring nodes up after a restart.
const APPEAR_TIMEOUT: Duration = Duration::from_secs(5);
const APPEAR_POLL: Duration = Duration::from_millis(250);

pub struct Sonar<T: PwTools> {
    tools: T,
    settings: Settings,
    settings_path: PathBuf,
    dropin_path: PathBuf,
    graph: Graph,
    /// Where each app stream (by node id) was last sent, so each is routed once.
    routed: HashMap<u32, Channel>,
    available: bool,
    /// Whether saved volumes and ChatMix have been pushed to the channels
    /// PipeWire has now. Cleared when they vanish, so a PipeWire restart gglinux
    /// did not do — a reboot, say — gets the levels back once they reappear.
    levels_applied: bool,
}

impl<T: PwTools> Sonar<T> {
    /// Load saved settings and look at PipeWire once. If settings say enabled
    /// but the drop-in is gone, the user removed it by hand: become disabled
    /// rather than put it back.
    pub fn new(tools: T, settings_path: PathBuf, dropin_path: PathBuf) -> Self {
        let settings = settings::load(&settings_path);
        let mut sonar = Sonar {
            tools,
            settings,
            settings_path,
            dropin_path,
            graph: Graph::default(),
            routed: HashMap::new(),
            available: false,
            levels_applied: false,
        };
        sonar.refresh();
        if sonar.settings.enabled && !sonar.dropin_path.exists() {
            info!("Sonar's PipeWire drop-in is gone; treating Sonar as off");
            sonar.settings.enabled = false;
            sonar.persist();
        } else if sonar.settings.enabled {
            sonar.upgrade_dropin();
            sonar.keep_levels();
        }
        sonar
    }

    pub fn is_enabled(&self) -> bool {
        self.settings.enabled
    }

    /// One routing pass: re-read PipeWire, send new app streams to their channels.
    pub fn tick(&mut self) {
        self.refresh();
        if self.settings.enabled {
            self.keep_levels();
            self.route();
        }
    }

    /// Current state. Re-reads PipeWire when Sonar is off, since no tick keeps
    /// the graph fresh then.
    pub fn state(&mut self) -> SonarState {
        if !self.settings.enabled {
            self.refresh();
        }
        self.snapshot()
    }

    /// State from the last read, without touching PipeWire.
    pub fn snapshot(&self) -> SonarState {
        SonarState {
            available: self.available,
            enabled: self.settings.enabled,
            output: self.settings.output.clone(),
            outputs: self
                .graph
                .physical_sinks()
                .map(|n| AudioOutput {
                    name: n.name.clone(),
                    description: n.description.clone().unwrap_or_else(|| n.name.clone()),
                })
                .collect(),
            channels: Channel::ALL
                .iter()
                .map(|&channel| {
                    let node = self.graph.node(&config::sink_name(channel));
                    ChannelState { channel, present: node.is_some(), volume: node.and_then(|n| n.volume) }
                })
                .collect(),
            chatmix: self.settings.chatmix,
            apps: self
                .graph
                .app_streams()
                .map(|s| {
                    let key = s.route_key();
                    AppStream {
                        id: s.id,
                        name: s.app_name.clone().unwrap_or_else(|| s.name.clone()),
                        channel: self.settings.rules.channel_for(key.as_deref()),
                        binary: key,
                    }
                })
                .collect(),
            eq: Channel::ALL
                .iter()
                .map(|&channel| ChannelEq { channel, bands: self.settings.channel_eq(channel) })
                .collect(),
            headphone: self.settings.headphone.clone(),
        }
    }

    /// Install the channels and take over the default output.
    pub fn enable(&mut self) -> Result<()> {
        if self.settings.enabled {
            return Ok(());
        }
        self.refresh();
        if !self.available {
            bail!("PipeWire is not reachable (pw-dump failed)");
        }
        let previous_default = self.graph.default_sink.clone().filter(|n| !config::is_ours(n));
        let output = self.choose_output().context("no output device for the channels to play into")?;

        let text = config::dropin(&output)?;
        if let Some(dir) = self.dropin_path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        std::fs::write(&self.dropin_path, text)
            .with_context(|| format!("writing {}", self.dropin_path.display()))?;
        info!(%output, path = %self.dropin_path.display(), "enabling Sonar; restarting PipeWire");
        self.tools.run(&tools::restart_pipewire())?;

        let all_up = |g: &Graph| Channel::ALL.iter().all(|&c| g.node(&config::sink_name(c)).is_some());
        if !self.wait_until(all_up) {
            warn!("Sonar channels did not appear; undoing");
            let _ = std::fs::remove_file(&self.dropin_path);
            let _ = self.tools.run(&tools::restart_pipewire());
            bail!(
                "PipeWire did not create the Sonar channels within {} s, so the change was undone",
                APPEAR_TIMEOUT.as_secs()
            );
        }
        if let Some(node) = self.graph.node(&config::sink_name(Channel::Game)) {
            self.tools.run(&tools::set_default(node.id))?;
        }

        self.settings.enabled = true;
        self.settings.output = Some(output);
        self.settings.previous_default = previous_default;
        self.persist();
        self.apply_levels()?;
        self.levels_applied = true;
        self.routed.clear();
        self.route();
        Ok(())
    }

    /// Remove the channels and give the default output back.
    pub fn disable(&mut self) -> Result<()> {
        if !self.settings.enabled {
            return Ok(());
        }
        match std::fs::remove_file(&self.dropin_path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("removing {}", self.dropin_path.display())),
        }
        info!("disabling Sonar; restarting PipeWire");
        self.tools.run(&tools::restart_pipewire())?;
        self.settings.enabled = false;
        self.routed.clear();
        self.levels_applied = false;
        let previous = self.settings.previous_default.take();
        self.persist();
        if let Some(name) = previous {
            if self.wait_until(|g| g.node(&name).is_some()) {
                if let Some(node) = self.graph.node(&name) {
                    self.tools.run(&tools::set_default(node.id))?;
                }
            } else {
                warn!(%name, "the previous default output did not come back");
            }
        }
        Ok(())
    }

    /// Play the channels into a different physical output.
    pub fn set_output(&mut self, name: &str) -> Result<()> {
        self.refresh();
        if !self.graph.physical_sinks().any(|n| n.name == name) {
            bail!("no output device named {name}");
        }
        let text = config::dropin(name)?;
        self.settings.output = Some(name.to_owned());
        self.persist();
        if self.settings.enabled {
            // Re-target now, and rewrite the drop-in for the next PipeWire start.
            std::fs::write(&self.dropin_path, text)
                .with_context(|| format!("writing {}", self.dropin_path.display()))?;
            for channel in Channel::ALL {
                if let Some(node) = self.graph.node(&config::outlet_name(channel)) {
                    self.tools.run(&tools::set_target(node.id, name))?;
                }
            }
        }
        Ok(())
    }

    pub fn set_volume(&mut self, channel: Channel, volume: f32) -> Result<()> {
        if !volume.is_finite() {
            bail!("volume must be a number");
        }
        let volume = volume.clamp(0.0, 1.0);
        self.settings.volumes.insert(channel, volume);
        self.persist();
        if let Some(node) = self.graph.node(&config::sink_name(channel)) {
            self.tools.run(&tools::set_volume(node.id, volume))?;
        }
        Ok(())
    }

    pub fn set_chatmix(&mut self, balance: f32) -> Result<()> {
        if !balance.is_finite() {
            bail!("ChatMix balance must be a number");
        }
        self.settings.chatmix = balance.clamp(-1.0, 1.0);
        self.persist();
        self.apply_chatmix()
    }

    /// Route an app to a channel now and from now on.
    pub fn set_app_channel(&mut self, binary: &str, channel: Channel) -> Result<()> {
        if binary.trim().is_empty() {
            bail!("no app to route");
        }
        self.settings.rules.set(binary, channel);
        self.persist();
        if self.settings.enabled {
            self.route();
        }
        Ok(())
    }

    /// Replace a channel's EQ, and hear it now.
    pub fn set_channel_eq(&mut self, channel: Channel, bands: Vec<EqBand>) -> Result<()> {
        eq::validate(&bands, &eq::channel_layout(), eq::CHANNEL_GAIN_LIMIT)?;
        self.settings.eq.insert(channel, bands);
        self.persist();
        self.apply_chain(channel)
    }

    /// Replace the headphone correction on every channel.
    pub fn set_headphone_eq(&mut self, headphone: HeadphoneEq) -> Result<()> {
        eq::validate_headphone(&headphone)?;
        self.settings.headphone = headphone;
        self.persist();
        for channel in Channel::ALL {
            self.apply_chain(channel)?;
        }
        Ok(())
    }

    /// Make an AutoEQ `ParametricEQ.txt` the headphone correction.
    pub fn import_autoeq(&mut self, text: &str, name: Option<String>) -> Result<()> {
        let mut headphone = eq::parse_autoeq(text)?;
        headphone.name = name.map(|n| n.trim().to_owned()).filter(|n| !n.is_empty());
        self.set_headphone_eq(headphone)
    }

    // --- internals ------------------------------------------------------------

    /// If the drop-in on disk was written by an older gglinux, rewrite it and
    /// restart PipeWire once so the channels get the current graph.
    fn upgrade_dropin(&mut self) {
        let Some(output) = self.settings.output.clone() else { return };
        let Ok(expected) = config::dropin(&output) else { return };
        if std::fs::read_to_string(&self.dropin_path).ok().as_deref() == Some(expected.as_str()) {
            return;
        }
        info!(path = %self.dropin_path.display(), "updating Sonar's PipeWire drop-in; restarting PipeWire");
        let result = std::fs::write(&self.dropin_path, expected)
            .map_err(anyhow::Error::from)
            .and_then(|()| self.tools.run(&tools::restart_pipewire()));
        match result {
            Ok(()) => self.refresh(),
            Err(e) => warn!(error = %format!("{e:#}"), "could not update Sonar's drop-in"),
        }
    }

    /// Re-read PipeWire. A failure means PipeWire or its tools are unavailable.
    fn refresh(&mut self) {
        match self.tools.dump().and_then(|json| Graph::parse(&json)) {
            Ok(graph) => {
                self.graph = graph;
                self.available = true;
            }
            Err(e) => {
                debug!(error = %format!("{e:#}"), "PipeWire unavailable");
                self.graph = Graph::default();
                self.available = false;
            }
        }
    }

    /// Re-read PipeWire until `ready` holds, for up to [`APPEAR_TIMEOUT`].
    fn wait_until(&mut self, ready: impl Fn(&Graph) -> bool) -> bool {
        let tries = (APPEAR_TIMEOUT.as_millis() / APPEAR_POLL.as_millis()) as usize;
        for _ in 0..tries {
            self.refresh();
            if ready(&self.graph) {
                return true;
            }
            self.tools.pause(APPEAR_POLL);
        }
        false
    }

    /// The saved output if it still exists, else the current default if it is a
    /// real device, else the first real device.
    fn choose_output(&self) -> Option<String> {
        let physical = |name: &String| self.graph.physical_sinks().any(|n| &n.name == name);
        self.settings
            .output
            .clone()
            .filter(physical)
            .or_else(|| self.graph.default_sink.clone().filter(physical))
            .or_else(|| self.graph.physical_sinks().next().map(|n| n.name.clone()))
    }

    /// Send every app stream whose channel changed (or is new) to its channel.
    fn route(&mut self) {
        let present: Vec<u32> = self.graph.app_streams().map(|s| s.id).collect();
        self.routed.retain(|id, _| present.contains(id));
        let moves: Vec<(u32, Channel)> = self
            .graph
            .app_streams()
            .map(|s| (s.id, self.settings.rules.channel_for(s.route_key().as_deref())))
            .filter(|(id, channel)| self.routed.get(id) != Some(channel))
            .collect();
        for (id, channel) in moves {
            match self.tools.run(&tools::set_target(id, &config::sink_name(channel))) {
                Ok(()) => {
                    debug!(id, ?channel, "routed app stream");
                    self.routed.insert(id, channel);
                }
                Err(e) => warn!(id, error = %format!("{e:#}"), "could not route app stream"),
            }
        }
    }

    /// Push the saved levels once per appearance of the channels.
    fn keep_levels(&mut self) {
        let present = Channel::ALL.iter().all(|&c| self.graph.node(&config::sink_name(c)).is_some());
        if !present {
            self.levels_applied = false;
        } else if !self.levels_applied {
            match self.apply_levels() {
                Ok(()) => self.levels_applied = true,
                Err(e) => warn!(error = %format!("{e:#}"), "could not restore Sonar volumes"),
            }
        }
    }

    /// Push saved channel volumes, EQ and ChatMix to PipeWire.
    fn apply_levels(&self) -> Result<()> {
        for (&channel, &volume) in &self.settings.volumes {
            if let Some(node) = self.graph.node(&config::sink_name(channel)) {
                self.tools.run(&tools::set_volume(node.id, volume))?;
            }
        }
        for channel in Channel::ALL {
            self.apply_chain(channel)?;
        }
        self.apply_chatmix()
    }

    /// Set a channel's whole filter chain, if PipeWire has the channel now.
    fn apply_chain(&self, channel: Channel) -> Result<()> {
        if let Some(node) = self.graph.node(&config::sink_name(channel)) {
            let props = eq::props(&self.settings.channel_eq(channel), &self.settings.headphone);
            self.tools.run(&tools::set_params(node.id, &props))?;
        }
        Ok(())
    }

    /// ChatMix lives on the Game and Chat outlets, so it multiplies with the
    /// channel volumes instead of overwriting them.
    fn apply_chatmix(&self) -> Result<()> {
        let (game, chat) = mix::chatmix_gains(self.settings.chatmix);
        for (channel, gain) in [(Channel::Game, game), (Channel::Chat, chat)] {
            if let Some(node) = self.graph.node(&config::outlet_name(channel)) {
                self.tools.run(&tools::set_volume(node.id, gain))?;
            }
        }
        Ok(())
    }

    fn persist(&self) {
        if let Err(e) = settings::save(&self.settings_path, &self.settings) {
            warn!(error = %format!("{e:#}"), "could not save Sonar settings");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{dump, scratch};
    use gg_ipc::HeadphoneEq;
    use std::cell::RefCell;

    const HEADSET: &str = "alsa_output.usb-SteelSeries_Arctis";
    const BT: &str = "bluez_output.1";

    /// PipeWire stand-in. A restart brings the channels up if the drop-in exists
    /// (and `channels_come_up`), or takes them down if it does not.
    struct Fake {
        dropin: PathBuf,
        streams: RefCell<Vec<(u32, &'static str)>>,
        up: RefCell<bool>,
        channels_come_up: bool,
        runs: RefCell<Vec<Vec<String>>>,
    }

    impl Fake {
        fn new(dropin: PathBuf, streams: &[(u32, &'static str)]) -> Self {
            Fake {
                dropin,
                streams: RefCell::new(streams.to_vec()),
                up: RefCell::new(false),
                channels_come_up: true,
                runs: RefCell::new(Vec::new()),
            }
        }

        fn ran(&self, argv: &[&str]) -> bool {
            self.runs.borrow().iter().any(|r| r == argv)
        }

        fn count(&self, program: &str) -> usize {
            self.runs.borrow().iter().filter(|r| r[0] == program).count()
        }
    }

    impl PwTools for &Fake {
        fn dump(&self) -> Result<String> {
            let mut nodes: Vec<(u32, &str, &str, Option<&str>)> =
                vec![(10, HEADSET, "Audio/Sink", None), (11, BT, "Audio/Sink", None)];
            if *self.up.borrow() {
                nodes.extend([
                    (20, "gglinux_game", "Audio/Sink", None),
                    (21, "gglinux_chat", "Audio/Sink", None),
                    (22, "gglinux_media", "Audio/Sink", None),
                    (23, "gglinux_aux", "Audio/Sink", None),
                    (30, "gglinux_game.out", "Stream/Output/Audio", None),
                    (31, "gglinux_chat.out", "Stream/Output/Audio", None),
                    (32, "gglinux_media.out", "Stream/Output/Audio", None),
                    (33, "gglinux_aux.out", "Stream/Output/Audio", None),
                ]);
            }
            let streams = self.streams.borrow();
            nodes.extend(streams.iter().map(|&(id, bin)| (id, bin, "Stream/Output/Audio", Some(bin))));
            Ok(dump(&nodes, BT))
        }

        fn run(&self, argv: &[String]) -> Result<()> {
            if argv[0] == "systemctl" {
                *self.up.borrow_mut() = self.channels_come_up && self.dropin.exists();
            }
            self.runs.borrow_mut().push(argv.to_vec());
            Ok(())
        }

        fn pause(&self, _: Duration) {}
    }

    fn paths(name: &str) -> (PathBuf, PathBuf) {
        let dir = scratch(name);
        (dir.join("sonar.json"), dir.join("pipewire.conf.d").join(config::FILE_NAME))
    }

    const APPS: &[(u32, &str)] = &[(40, "Discord"), (41, "firefox"), (42, "game.bin")];

    #[test]
    fn enable_writes_the_dropin_restarts_and_makes_game_default() {
        let (settings_path, dropin) = paths("enable");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path.clone(), dropin.clone());
        sonar.enable().unwrap();

        let text = std::fs::read_to_string(&dropin).unwrap();
        assert!(text.contains(&format!("target.object      = \"{BT}\"")), "plays into the default output");
        assert!(fake.ran(&["systemctl", "--user", "restart", "pipewire.service", "pipewire-pulse.service", "wireplumber.service"]));
        assert!(fake.ran(&["wpctl", "set-default", "20"]), "Game becomes the default");
        assert!(fake.ran(&["pw-metadata", "40", "target.object", "gglinux_chat"]));
        assert!(fake.ran(&["pw-metadata", "41", "target.object", "gglinux_media"]));
        assert!(fake.ran(&["pw-metadata", "42", "target.object", "gglinux_game"]));

        let saved = settings::load(&settings_path);
        assert!(saved.enabled);
        assert_eq!(saved.output.as_deref(), Some(BT));
        assert_eq!(saved.previous_default.as_deref(), Some(BT));
    }

    #[test]
    fn enable_rolls_back_when_the_channels_never_appear() {
        let (settings_path, dropin) = paths("rollback");
        let mut fake = Fake::new(dropin.clone(), APPS);
        fake.channels_come_up = false;
        let mut sonar = Sonar::new(&fake, settings_path.clone(), dropin.clone());
        assert!(sonar.enable().is_err());
        assert!(!dropin.exists(), "the drop-in is removed again");
        assert_eq!(fake.count("systemctl"), 2, "restarted once to apply, once to undo");
        assert!(!settings::load(&settings_path).enabled);
    }

    #[test]
    fn streams_are_routed_once() {
        let (settings_path, dropin) = paths("once");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path, dropin);
        sonar.enable().unwrap();
        let before = fake.count("pw-metadata");
        sonar.tick();
        assert_eq!(fake.count("pw-metadata"), before, "nothing new, nothing sent");
        fake.streams.borrow_mut().push((43, "mpv"));
        sonar.tick();
        assert_eq!(fake.count("pw-metadata"), before + 1);
        assert!(fake.ran(&["pw-metadata", "43", "target.object", "gglinux_media"]));
    }

    #[test]
    fn outlets_are_never_routed() {
        let (settings_path, dropin) = paths("outlets");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path, dropin);
        sonar.enable().unwrap();
        sonar.tick();
        for id in ["30", "31", "32", "33"] {
            assert!(
                !fake.runs.borrow().iter().any(|r| r[0] == "pw-metadata" && r[1] == id),
                "outlet {id} was routed into a channel"
            );
        }
    }

    #[test]
    fn changing_an_apps_channel_moves_it_now_and_is_remembered() {
        let (settings_path, dropin) = paths("move");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path.clone(), dropin);
        sonar.enable().unwrap();
        sonar.set_app_channel("firefox", Channel::Game).unwrap();
        assert!(fake.ran(&["pw-metadata", "41", "target.object", "gglinux_game"]));
        assert_eq!(settings::load(&settings_path).rules.channel_for(Some("firefox")), Channel::Game);
    }

    #[test]
    fn chatmix_turns_down_the_other_side() {
        let (settings_path, dropin) = paths("chatmix");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path, dropin);
        sonar.enable().unwrap();
        sonar.set_chatmix(0.5).unwrap();
        assert!(fake.ran(&["wpctl", "set-volume", "30", "0.500"]), "Game outlet down");
        assert!(fake.ran(&["wpctl", "set-volume", "31", "1.000"]), "Chat outlet full");
    }

    #[test]
    fn volume_is_saved_and_applied_to_the_channel_sink() {
        let (settings_path, dropin) = paths("volume");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path.clone(), dropin);
        sonar.enable().unwrap();
        sonar.set_volume(Channel::Chat, 0.4).unwrap();
        assert!(fake.ran(&["wpctl", "set-volume", "21", "0.400"]));
        assert_eq!(settings::load(&settings_path).volumes.get(&Channel::Chat), Some(&0.4));
    }

    #[test]
    fn output_change_retargets_outlets_without_a_restart() {
        let (settings_path, dropin) = paths("output");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path, dropin.clone());
        sonar.enable().unwrap();
        let restarts = fake.count("systemctl");
        sonar.set_output(HEADSET).unwrap();
        for id in ["30", "31", "32", "33"] {
            assert!(fake.ran(&["pw-metadata", id, "target.object", HEADSET]));
        }
        assert_eq!(fake.count("systemctl"), restarts);
        assert!(std::fs::read_to_string(&dropin).unwrap().contains(HEADSET));
        assert!(sonar.set_output("no_such_sink").is_err());
    }

    #[test]
    fn disable_removes_the_dropin_and_restores_the_default() {
        let (settings_path, dropin) = paths("disable");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path.clone(), dropin.clone());
        sonar.enable().unwrap();
        sonar.disable().unwrap();
        assert!(!dropin.exists());
        assert_eq!(fake.count("systemctl"), 2);
        assert!(fake.ran(&["wpctl", "set-default", "11"]), "back to the Bluetooth output");
        assert!(!settings::load(&settings_path).enabled);
    }

    #[test]
    fn a_removed_dropin_means_sonar_is_off() {
        let (settings_path, dropin) = paths("removed");
        let saved = Settings { enabled: true, ..Settings::default() };
        settings::save(&settings_path, &saved).unwrap();
        let fake = Fake::new(dropin.clone(), APPS);
        let sonar = Sonar::new(&fake, settings_path.clone(), dropin);
        assert!(!sonar.is_enabled());
        assert!(!settings::load(&settings_path).enabled);
        assert_eq!(fake.count("systemctl"), 0, "nothing re-installed behind the user's back");
    }

    #[test]
    fn levels_are_restored_when_the_channels_come_back() {
        let (settings_path, dropin) = paths("comeback");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path, dropin);
        sonar.enable().unwrap();
        sonar.set_chatmix(0.5).unwrap();
        // PipeWire goes away and comes back: a reboot, or a restart gglinux didn't do.
        *fake.up.borrow_mut() = false;
        sonar.tick();
        fake.runs.borrow_mut().clear();
        *fake.up.borrow_mut() = true;
        sonar.tick();
        assert!(fake.ran(&["wpctl", "set-volume", "30", "0.500"]), "ChatMix re-applied");
        fake.runs.borrow_mut().clear();
        sonar.tick();
        assert_eq!(fake.count("wpctl"), 0, "and only once");
    }

    #[test]
    fn state_lists_outputs_channels_and_apps() {
        let (settings_path, dropin) = paths("state");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path, dropin);
        sonar.enable().unwrap();
        let state = sonar.state();
        assert!(state.available && state.enabled);
        assert_eq!(state.outputs.len(), 2);
        assert!(state.channels.iter().all(|c| c.present));
        let discord = state.apps.iter().find(|a| a.id == 40).unwrap();
        assert_eq!(discord.channel, Channel::Chat);
        assert_eq!(state.apps.len(), 3, "outlets are not apps");
    }

    fn chain_of(sonar: &Sonar<&Fake>, channel: Channel) -> String {
        let s = sonar.snapshot();
        let bands = &s.eq.iter().find(|c| c.channel == channel).unwrap().bands;
        crate::eq::props(bands, &s.headphone)
    }

    #[test]
    fn enable_sets_every_channels_chain() {
        let (settings_path, dropin) = paths("eq-enable");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path, dropin);
        sonar.enable().unwrap();
        for (id, channel) in [("20", Channel::Game), ("21", Channel::Chat), ("22", Channel::Media), ("23", Channel::Aux)] {
            let props = chain_of(&sonar, channel);
            assert!(fake.ran(&["pw-cli", "set-param", id, "Props", &props]), "{channel:?}");
        }
    }

    #[test]
    fn channel_eq_is_validated_saved_and_set_live() {
        let (settings_path, dropin) = paths("eq-channel");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path.clone(), dropin);
        sonar.enable().unwrap();
        let mut bands = crate::eq::flat_channel();
        bands[3].gain = 4.0;
        sonar.set_channel_eq(Channel::Chat, bands.clone()).unwrap();
        assert!(fake.ran(&["pw-cli", "set-param", "21", "Props", &chain_of(&sonar, Channel::Chat)]));
        assert_eq!(settings::load(&settings_path).eq.get(&Channel::Chat), Some(&bands));

        let before = fake.runs.borrow().len();
        bands[3].gain = 40.0;
        assert!(sonar.set_channel_eq(Channel::Chat, bands).is_err());
        assert_eq!(fake.runs.borrow().len(), before, "nothing sent");
        assert_eq!(settings::load(&settings_path).eq.get(&Channel::Chat).unwrap()[3].gain, 4.0, "nothing stored");
    }

    #[test]
    fn headphone_eq_reaches_every_channel() {
        let (settings_path, dropin) = paths("eq-headphone");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path, dropin);
        sonar.enable().unwrap();
        fake.runs.borrow_mut().clear();
        let hp = HeadphoneEq { preamp: -3.0, ..crate::eq::flat_headphone() };
        sonar.set_headphone_eq(hp).unwrap();
        assert_eq!(fake.count("pw-cli"), 4);
    }

    #[test]
    fn autoeq_import_names_the_correction() {
        let (settings_path, dropin) = paths("eq-import");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path, dropin);
        sonar.enable().unwrap();
        sonar
            .import_autoeq("Preamp: -6.2 dB\nFilter 1: ON PK Fc 180 Hz Gain -3.4 dB Q 0.53", Some("HD 650".into()))
            .unwrap();
        let hp = sonar.snapshot().headphone;
        assert_eq!(hp.name.as_deref(), Some("HD 650"));
        assert_eq!(hp.preamp, -6.2);
        assert!(sonar.import_autoeq("not an eq", None).is_err());
    }

    #[test]
    fn an_outdated_dropin_is_rewritten_with_one_restart() {
        let (settings_path, dropin) = paths("upgrade");
        let saved = Settings { enabled: true, output: Some(BT.into()), ..Settings::default() };
        settings::save(&settings_path, &saved).unwrap();
        std::fs::create_dir_all(dropin.parent().unwrap()).unwrap();
        std::fs::write(&dropin, "# an M3a loopback drop-in\n").unwrap();
        let fake = Fake::new(dropin.clone(), APPS);
        let sonar = Sonar::new(&fake, settings_path, dropin.clone());
        assert!(sonar.is_enabled());
        assert_eq!(std::fs::read_to_string(&dropin).unwrap(), config::dropin(BT).unwrap());
        assert_eq!(fake.count("systemctl"), 1);
    }

    #[test]
    fn a_current_dropin_is_left_alone() {
        let (settings_path, dropin) = paths("current");
        let saved = Settings { enabled: true, output: Some(BT.into()), ..Settings::default() };
        settings::save(&settings_path, &saved).unwrap();
        std::fs::create_dir_all(dropin.parent().unwrap()).unwrap();
        std::fs::write(&dropin, config::dropin(BT).unwrap()).unwrap();
        let fake = Fake::new(dropin.clone(), APPS);
        let _sonar = Sonar::new(&fake, settings_path, dropin);
        assert_eq!(fake.count("systemctl"), 0);
    }
}
