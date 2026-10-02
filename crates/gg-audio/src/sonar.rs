//! Sonar: the channels, routing apps into them, their volumes, and ChatMix.
//!
//! PipeWire hosts the graph (see [`config`]); this drives it through
//! [`PwTools`], so every action is a command a person could type, and the whole
//! flow is tested against a fake.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use gg_ipc::{
    AppStream, AudioDevice, Channel, ChannelEq, ChannelState, EqBand, HeadphoneEq, MicSettings, MicState,
    SmartVolume, SmartVolumeState, SonarState, StreamMix,
};
use tracing::{debug, info, warn};

use crate::graph::Graph;
use crate::settings::{self, Settings};
use crate::tools::{self, PwTools};
use crate::{config, eq, mic, mix, smart};

/// How long PipeWire gets to bring nodes up after a restart.
const APPEAR_TIMEOUT: Duration = Duration::from_secs(5);
const APPEAR_POLL: Duration = Duration::from_millis(250);

/// How far a volume read back from PipeWire may be from the saved one and
/// still be the same: it comes back cubed and to six decimals.
const VOLUME_SLACK: f32 = 0.005;

/// Whether PipeWire has all four channel sinks.
fn channels_up(g: &Graph) -> bool {
    Channel::ALL.iter().all(|&c| g.node(&config::sink_name(c)).is_some())
}

pub struct Sonar<T: PwTools> {
    tools: T,
    settings: Settings,
    settings_path: PathBuf,
    dropin_path: PathBuf,
    graph: Graph,
    /// Where each app stream (by node id and serial, since PipeWire reuses
    /// ids) was last sent, so each is routed once.
    routed: HashMap<(u32, Option<u64>), Channel>,
    available: bool,
    /// Whether saved volumes and ChatMix have been pushed to the channels
    /// PipeWire has now. Cleared when they vanish or PipeWire's cookie
    /// changes, so a PipeWire restart gglinux did not do — a reboot, say —
    /// gets the levels back once they reappear.
    levels_applied: bool,
    /// The same for the stream mix, kept apart so a stream that's late or
    /// missing never holds back the channels' own levels.
    stream_levels_applied: bool,
    /// Whether Smart Volume's compressor was installed at the last look, so
    /// `tick` notices it going.
    compressor_seen: bool,
    /// Whether the channels PipeWire made carry Smart Volume's stage. Read
    /// from the drop-in whenever they come up, since PipeWire made them from
    /// it; the drop-in only gains the stage just before a restart.
    stage_loaded: bool,
    /// The cookie of the PipeWire last read. A restart can be over between
    /// two ticks, so the channels are never seen missing; a new cookie still
    /// tells.
    cookie: Option<u64>,
}

impl<T: PwTools> Sonar<T> {
    /// Load saved settings and look at PipeWire once. If settings say enabled
    /// but the drop-in is gone, the user removed it by hand: become disabled
    /// rather than put it back. If they say off but the drop-in is there — a
    /// backup from while Sonar was off was restored, say — take it out. With
    /// Sonar on, it also checks that PipeWire took Smart Volume's stage.
    pub fn new(tools: T, settings_path: PathBuf, dropin_path: PathBuf) -> Self {
        let settings = settings::load(&settings_path);
        let compressor_seen = tools.smart_volume_supported();
        let mut sonar = Sonar {
            tools,
            settings,
            settings_path,
            dropin_path,
            graph: Graph::default(),
            routed: HashMap::new(),
            available: false,
            levels_applied: false,
            stream_levels_applied: false,
            compressor_seen,
            stage_loaded: false,
            cookie: None,
        };
        sonar.refresh();
        if sonar.settings.enabled && !sonar.dropin_path.exists() {
            info!("Sonar's PipeWire drop-in is gone; treating Sonar as off");
            sonar.settings.enabled = false;
            sonar.persist();
        } else if sonar.settings.enabled {
            sonar.upgrade_dropin();
            sonar.check_stage();
            sonar.keep_levels();
        } else if sonar.dropin_path.exists() {
            sonar.remove_stray_dropin();
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
            self.follow_compressor();
            self.follow_volumes();
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
                .map(|n| AudioDevice {
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
            mic: MicState {
                available: self.tools.mic_plugins_present(),
                present: self.graph.node(mic::SOURCE).is_some(),
                input: self.settings.mic_input.clone(),
                inputs: self
                    .graph
                    .physical_sources()
                    .map(|n| AudioDevice {
                        name: n.name.clone(),
                        description: n.description.clone().unwrap_or_else(|| n.name.clone()),
                    })
                    .collect(),
                volume: self.graph.node(mic::SOURCE).and_then(|n| n.volume),
                settings: self.settings.mic,
            },
            stream: StreamMix {
                enabled: self.settings.streamer,
                present: self.graph.node(config::STREAM_SINK).is_some(),
                volumes: Channel::ALL.iter().map(|&c| self.settings.stream_volume(c)).collect(),
                master: self.settings.stream_master,
            },
            smart_volume: SmartVolumeState {
                available: self.smart_volume_available(),
                reason: self.smart_volume_unavailable().map(str::to_owned),
                settings: Channel::ALL.iter().map(|&c| self.settings.smart_volume(c)).collect(),
            },
        }
    }

    /// Install the channels and take over the default output.
    pub fn enable(&mut self) -> Result<()> {
        if self.settings.enabled {
            return Ok(());
        }
        self.settings.smart_volume_refused = false; // Sonar is off: a refusal on record is stale
        self.refresh();
        if !self.available {
            bail!("PipeWire is not reachable (pw-dump failed)");
        }
        let previous_default = self.graph.default_sink.clone().filter(|n| !config::is_ours(n));
        let previous_default_source = self.graph.default_source.clone().filter(|n| !config::is_ours(n));
        let output = self.choose_output().context("no output device for the channels to play into")?;
        let mic_input = self.choose_mic();

        let text = config::dropin(&output, mic_input.as_deref(), self.settings.streamer, self.stage_wanted())?;
        if let Some(dir) = self.dropin_path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        std::fs::write(&self.dropin_path, text)
            .with_context(|| format!("writing {}", self.dropin_path.display()))?;
        info!(%output, path = %self.dropin_path.display(), "enabling Sonar; restarting PipeWire");
        if let Err(e) = self.tools.run(&tools::restart_pipewire()) {
            warn!("could not restart PipeWire; undoing");
            self.undo_enable();
            return Err(e);
        }

        let want_mic = mic_input.is_some();
        let all_up = |g: &Graph| channels_up(g) && (!want_mic || g.node(mic::SOURCE).is_some());
        if !self.wait_until(all_up) {
            if channels_up(&self.graph) {
                // The mic module is nofail: the channels are fine without it.
                warn!("the Sonar mic did not load; continuing without it");
            } else if !self.drop_refused_stage(&output, mic_input.as_deref()) {
                warn!("Sonar channels did not appear; undoing");
                self.undo_enable();
                bail!(
                    "PipeWire did not create the Sonar channels within {} s, so the change was undone",
                    APPEAR_TIMEOUT.as_secs()
                );
            }
            // Otherwise PipeWire refused Smart Volume's stage: the channels run without it.
        }
        self.stage_loaded = self.staged();
        if let Err(e) = self.take_defaults() {
            warn!("could not make the Sonar channels the defaults; undoing");
            self.undo_enable();
            return Err(e);
        }

        self.settings.enabled = true;
        self.settings.output = Some(output);
        self.settings.previous_default = previous_default;
        self.settings.previous_default_source = previous_default_source;
        self.settings.mic_input = mic_input;
        self.persist();
        self.apply_levels()?;
        self.levels_applied = true;
        self.keep_levels(); // the stream levels, if the stream mix is up yet
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
        self.settings.smart_volume_refused = false; // turning Sonar on again tries the stage again
        self.routed.clear();
        self.levels_applied = false;
        self.stream_levels_applied = false;
        // Only a device PipeWire had before the restart comes back with it: one
        // that's away isn't waited for. WirePlumber remembers it was chosen and
        // makes it the default again when it returns.
        let here = |name: &String| self.graph.node(name).is_some();
        let previous = self.settings.previous_default.take().filter(here);
        let previous_source = self.settings.previous_default_source.take().filter(here);
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
        if let Some(name) = previous_source {
            if self.wait_until(|g| g.node(&name).is_some()) {
                if let Some(node) = self.graph.node(&name) {
                    self.tools.run(&tools::set_default(node.id))?;
                }
            } else {
                warn!(%name, "the previous default input did not come back");
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
        let text = self.running_dropin(name)?;
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

    pub fn set_mic_settings(&mut self, settings: MicSettings) -> Result<()> {
        mic::validate(&settings)?;
        self.settings.mic = settings;
        self.persist();
        self.apply_mic()
    }

    pub fn set_mic_volume(&mut self, volume: f32) -> Result<()> {
        if !volume.is_finite() {
            bail!("volume must be a number");
        }
        self.settings.mic_volume = Some(volume.clamp(0.0, 1.0));
        self.persist();
        self.apply_mic()
    }

    /// Read a different physical mic, now and from the next PipeWire start.
    pub fn set_mic_input(&mut self, name: &str) -> Result<()> {
        self.refresh();
        if !self.graph.physical_sources().any(|n| n.name == name) {
            bail!("no input device named {name}");
        }
        self.settings.mic_input = Some(name.to_owned());
        self.persist();
        if self.settings.enabled {
            if let Some(output) = self.settings.output.clone() {
                let text = self.running_dropin(&output)?;
                std::fs::write(&self.dropin_path, text)
                    .with_context(|| format!("writing {}", self.dropin_path.display()))?;
            }
            if let Some(node) = self.graph.node(mic::CAPTURE) {
                self.tools.run(&tools::set_target(node.id, name))?;
            }
        }
        Ok(())
    }

    /// Turn streamer mode on or off. While Sonar is on this rewrites the
    /// drop-in and restarts PipeWire (1–2 s of silence), like turning Sonar on.
    pub fn set_streamer(&mut self, enabled: bool) -> Result<()> {
        if self.settings.streamer == enabled {
            return Ok(());
        }
        self.settings.streamer = enabled;
        self.persist();
        if !self.settings.enabled {
            return Ok(());
        }
        let output = self.settings.output.clone().context("Sonar has no output device")?;
        let text = self.expected_dropin(&output)?;
        std::fs::write(&self.dropin_path, text)
            .with_context(|| format!("writing {}", self.dropin_path.display()))?;
        info!(enabled, "streamer mode changed; restarting PipeWire");
        // The channels come back with the drop-in's defaults: push it all again,
        // even if a step below fails and returns early — the next tick does it then.
        self.levels_applied = false;
        self.stream_levels_applied = false;
        self.routed.clear();
        self.tools.run(&tools::restart_pipewire())?;
        let ready = |g: &Graph| channels_up(g) && (!enabled || g.node(config::STREAM_SINK).is_some());
        if !self.wait_until(ready) {
            let mic = self.dropin_mic();
            if channels_up(&self.graph) || !self.drop_refused_stage(&output, mic.as_deref()) {
                // Never undone for this: the channels may be fine without the stream.
                warn!(enabled, "the channels or the stream mix did not appear after the restart");
            }
        }
        self.take_defaults()?;
        self.keep_levels();
        self.route();
        Ok(())
    }

    /// A channel's level in the stream mix, independent of its own volume.
    pub fn set_stream_volume(&mut self, channel: Channel, volume: f32) -> Result<()> {
        if !volume.is_finite() {
            bail!("volume must be a number");
        }
        let volume = volume.clamp(0.0, 1.0);
        self.settings.stream_volumes.insert(channel, volume);
        self.persist();
        if let Some(node) = self.graph.node(&config::stream_feed_name(channel)) {
            self.tools.run(&tools::set_volume(node.id, volume))?;
        }
        Ok(())
    }

    /// The stream mix's master level: the stream device's own volume.
    pub fn set_stream_master(&mut self, volume: f32) -> Result<()> {
        if !volume.is_finite() {
            bail!("volume must be a number");
        }
        self.settings.stream_master = volume.clamp(0.0, 1.0);
        self.persist();
        if let Some(node) = self.graph.node(config::STREAM_SINK) {
            self.tools.run(&tools::set_volume(node.id, self.settings.stream_master))?;
        }
        Ok(())
    }

    /// Turn Smart Volume on or off for a channel, or change its level: live,
    /// through the stage's own controls, never a PipeWire restart.
    pub fn set_smart_volume(&mut self, channel: Channel, smart_volume: SmartVolume) -> Result<()> {
        if let Some(why) = self.smart_volume_unavailable() {
            bail!("{why}");
        }
        self.settings.smart_volume.insert(channel, smart_volume);
        self.persist();
        self.apply_smart_volume(channel)
    }

    // --- internals ------------------------------------------------------------

    /// Take back a half-done `enable`: remove the drop-in and restart PipeWire
    /// without it, so Sonar is wholly off, as its settings say.
    fn undo_enable(&mut self) {
        let _ = std::fs::remove_file(&self.dropin_path);
        let _ = self.tools.run(&tools::restart_pipewire());
        // Not the stage's fault: turning Sonar on again tries it again.
        self.settings.smart_volume_refused = false;
        self.persist();
    }

    /// Sonar is off but its drop-in is there: remove it, and restart PipeWire
    /// if it runs the channels, so Sonar is off for real and not just on paper.
    fn remove_stray_dropin(&mut self) {
        info!(path = %self.dropin_path.display(), "Sonar is off but its PipeWire drop-in is there; removing it");
        if let Err(e) = std::fs::remove_file(&self.dropin_path) {
            warn!(error = %e, "could not remove Sonar's drop-in");
            return;
        }
        if self.graph.nodes.iter().any(|n| config::is_ours(&n.name)) {
            info!("restarting PipeWire to take the Sonar channels down");
            match self.tools.run(&tools::restart_pipewire()) {
                Ok(()) => self.refresh(),
                Err(e) => warn!(error = %format!("{e:#}"), "could not restart PipeWire"),
            }
        }
    }

    /// Make the Game channel the default output and gglinux Mic the default input.
    fn take_defaults(&self) -> Result<()> {
        if let Some(node) = self.graph.node(&config::sink_name(Channel::Game)) {
            self.tools.run(&tools::set_default(node.id))?;
        }
        if let Some(node) = self.graph.node(mic::SOURCE) {
            self.tools.run(&tools::set_default(node.id))?;
        }
        Ok(())
    }

    /// The input the mic should read, if the plugins exist: the saved one if it
    /// is still there, else the default input if it is a real one — the one in
    /// use while the configured one is away — else the first.
    fn choose_mic(&self) -> Option<String> {
        if !self.tools.mic_plugins_present() {
            return None;
        }
        let physical = |name: &String| self.graph.physical_sources().any(|n| &n.name == name);
        self.settings
            .mic_input
            .clone()
            .filter(physical)
            .or_else(|| self.graph.default_source.clone().filter(physical))
            .or_else(|| self.graph.current_source.clone().filter(physical))
            .or_else(|| self.graph.physical_sources().next().map(|n| n.name.clone()))
    }

    /// Why Smart Volume can't be used here, if it can't.
    fn smart_volume_unavailable(&self) -> Option<&'static str> {
        if !self.tools.smart_volume_supported() {
            Some(
                "Smart Volume needs the SC4 compressor from swh-plugins (sc4m_1916.so), \
                 which isn't installed. Install swh-plugins, then turn Sonar off and on.",
            )
        } else if self.settings.smart_volume_refused {
            Some(
                "PipeWire wouldn't make the channels with Smart Volume, so they run without it. \
                 Turn Sonar off and on to try again.",
            )
        } else if self.settings.enabled && !self.stage_loaded {
            Some(
                "The Sonar channels were made before swh-plugins was installed, so they run \
                 without Smart Volume. Turn Sonar off and on to add it.",
            )
        } else {
            None
        }
    }

    /// Whether Smart Volume can be switched: the channels carry its stage, or
    /// will when Sonar turns on.
    fn smart_volume_available(&self) -> bool {
        self.smart_volume_unavailable().is_none()
    }

    /// Whether the drop-in should carry Smart Volume's stage at the next
    /// PipeWire start: the compressor is there and PipeWire hasn't refused it.
    fn stage_wanted(&self) -> bool {
        self.tools.smart_volume_supported() && !self.settings.smart_volume_refused
    }

    /// The mic the drop-in carries: the saved one, if the plugins exist.
    fn dropin_mic(&self) -> Option<String> {
        self.settings.mic_input.clone().filter(|_| self.tools.mic_plugins_present())
    }

    /// The drop-in this version writes for the saved output and mic.
    fn expected_dropin(&self, output: &str) -> Result<String> {
        config::dropin(output, self.dropin_mic().as_deref(), self.settings.streamer, self.stage_wanted())
    }

    /// The drop-in for a write PipeWire isn't restarted for: it keeps the
    /// stage only if the channels already carry it, so it never gains the
    /// stage PipeWire hasn't loaded.
    fn running_dropin(&self, output: &str) -> Result<String> {
        let stage = self.stage_loaded && self.stage_wanted();
        config::dropin(output, self.dropin_mic().as_deref(), self.settings.streamer, stage)
    }

    fn apply_mic(&self) -> Result<()> {
        if let Some(node) = self.graph.node(mic::CAPTURE) {
            self.tools.run(&tools::set_params(node.id, &mic::props(&self.settings.mic)))?;
        }
        if let (Some(volume), Some(node)) = (self.settings.mic_volume, self.graph.node(mic::SOURCE)) {
            self.tools.run(&tools::set_volume(node.id, volume))?;
        }
        Ok(())
    }

    /// If the drop-in on disk was written by an older gglinux, rewrite it and
    /// restart PipeWire once so the channels get the current graph.
    fn upgrade_dropin(&mut self) {
        let Some(output) = self.settings.output.clone() else { return };
        // Plugins installed since Sonar was turned on: give it a mic now.
        if self.settings.mic_input.is_none() {
            self.settings.mic_input = self.choose_mic();
            if self.settings.mic_input.is_some() {
                self.persist();
            }
        }
        let Ok(expected) = self.expected_dropin(&output) else { return };
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

    /// Whether the drop-in on disk carries Smart Volume's stage.
    fn staged(&self) -> bool {
        std::fs::read_to_string(&self.dropin_path).is_ok_and(|t| t.contains(smart::INPUTS[0]))
    }

    /// On startup with Sonar on, whether or not the drop-in was just rewritten:
    /// a PipeWire upgraded since may refuse a stage it took before.
    fn check_stage(&mut self) {
        let Some(output) = self.settings.output.clone() else { return };
        if self.staged() && !self.wait_until(channels_up) {
            let mic = self.dropin_mic();
            self.drop_refused_stage(&output, mic.as_deref());
        }
    }

    /// The channels didn't come. If the drop-in carries Smart Volume's stage
    /// and PipeWire answers, PipeWire refused the stage: the channel modules
    /// that carry it are `nofail`, so it started without them. Remember that
    /// (first, so a crash can't bring the stage back), write the drop-in
    /// without the stage and restart PipeWire once more. Returns whether the
    /// channels are up now.
    fn drop_refused_stage(&mut self, output: &str, mic: Option<&str>) -> bool {
        if !self.staged() || !self.available {
            return false;
        }
        warn!("PipeWire did not make the Sonar channels with Smart Volume's stage; going on without it");
        self.settings.smart_volume_refused = true;
        self.stage_loaded = false;
        self.persist();
        let result = config::dropin(output, mic, self.settings.streamer, false)
            .and_then(|text| {
                std::fs::write(&self.dropin_path, text)
                    .with_context(|| format!("writing {}", self.dropin_path.display()))
            })
            .and_then(|()| self.tools.run(&tools::restart_pipewire()));
        if let Err(e) = result {
            warn!(error = %format!("{e:#}"), "could not take Smart Volume's stage out");
            return false;
        }
        self.wait_until(channels_up)
    }

    /// `swh-plugins` was removed while Sonar is on: write the drop-in without
    /// Smart Volume's stage at once, so no later PipeWire start, gglinux's or
    /// anyone's, loads a plugin that isn't there. PipeWire keeps the copy it
    /// loaded until then. If it already restarted without the channels,
    /// restart it once more to bring them back.
    fn follow_compressor(&mut self) {
        let present = self.tools.smart_volume_supported();
        let gone = std::mem::replace(&mut self.compressor_seen, present) && !present;
        if !gone || !self.staged() {
            return;
        }
        let Some(output) = self.settings.output.clone() else { return };
        warn!("Smart Volume's compressor (swh-plugins) is gone; taking it out of the Sonar channels");
        let written = self.expected_dropin(&output).and_then(|text| {
            std::fs::write(&self.dropin_path, text).with_context(|| format!("writing {}", self.dropin_path.display()))
        });
        if let Err(e) = written {
            warn!(error = %format!("{e:#}"), "could not take Smart Volume's stage out");
            return;
        }
        // PipeWire may still run the stage, but the next start won't have it.
        self.stage_loaded = false;
        if !channels_up(&self.graph) {
            info!("restarting PipeWire to bring the Sonar channels back without Smart Volume");
            match self.tools.run(&tools::restart_pipewire()) {
                Ok(()) => {
                    // `keep_levels` and `route` never see the channels gone: push it all again.
                    self.levels_applied = false;
                    self.stream_levels_applied = false;
                    self.routed.clear();
                    self.wait_until(channels_up);
                }
                Err(e) => warn!(error = %format!("{e:#}"), "could not restart PipeWire"),
            }
        }
    }

    /// Re-read PipeWire. A failure means PipeWire or its tools are unavailable.
    fn refresh(&mut self) {
        match self.tools.dump().and_then(|json| Graph::parse(&json)) {
            Ok(graph) => {
                if graph.cookie.is_some() && graph.cookie != self.cookie {
                    // A PipeWire started since the last look: it made the channels
                    // with the drop-in's defaults and knows none of the routes.
                    self.levels_applied = false;
                    self.stream_levels_applied = false;
                    self.routed.clear();
                    self.cookie = graph.cookie;
                }
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
    /// real device — the one in use while the configured one is away — else
    /// the first real device.
    fn choose_output(&self) -> Option<String> {
        let physical = |name: &String| self.graph.physical_sinks().any(|n| &n.name == name);
        self.settings
            .output
            .clone()
            .filter(physical)
            .or_else(|| self.graph.default_sink.clone().filter(physical))
            .or_else(|| self.graph.current_sink.clone().filter(physical))
            .or_else(|| self.graph.physical_sinks().next().map(|n| n.name.clone()))
    }

    /// Send every app stream whose channel changed (or is new) to its channel.
    fn route(&mut self) {
        let present: Vec<(u32, Option<u64>)> = self.graph.app_streams().map(|s| (s.id, s.serial)).collect();
        self.routed.retain(|stream, _| present.contains(stream));
        let moves: Vec<((u32, Option<u64>), Channel)> = self
            .graph
            .app_streams()
            .map(|s| ((s.id, s.serial), self.settings.rules.channel_for(s.route_key().as_deref())))
            .filter(|(stream, channel)| self.routed.get(stream) != Some(channel))
            .collect();
        for ((id, serial), channel) in moves {
            match self.tools.run(&tools::set_target(id, &config::sink_name(channel))) {
                Ok(()) => {
                    debug!(id, ?channel, "routed app stream");
                    self.routed.insert((id, serial), channel);
                }
                Err(e) => warn!(id, error = %format!("{e:#}"), "could not route app stream"),
            }
        }
    }

    /// Save the channel and mic volumes PipeWire has, once the saved ones were
    /// pushed to these channels: the desktop's mixer, the volume keys or an
    /// app may have changed them since, and the levels pushed after the next
    /// PipeWire restart should be the ones the user last had.
    fn follow_volumes(&mut self) {
        if !self.levels_applied || !channels_up(&self.graph) {
            return;
        }
        let differs = |saved: Option<f32>, now: f32| saved.is_none_or(|s| (s - now).abs() > VOLUME_SLACK);
        let mut changed = false;
        for channel in Channel::ALL {
            let Some(now) = self.graph.node(&config::sink_name(channel)).and_then(|n| n.volume) else { continue };
            let now = now.clamp(0.0, 1.0);
            if differs(self.settings.volumes.get(&channel).copied(), now) {
                self.settings.volumes.insert(channel, now);
                changed = true;
            }
        }
        if let Some(now) = self.graph.node(mic::SOURCE).and_then(|n| n.volume) {
            let now = now.clamp(0.0, 1.0);
            if differs(self.settings.mic_volume, now) {
                self.settings.mic_volume = Some(now);
                changed = true;
            }
        }
        if changed {
            self.persist();
        }
    }

    /// Push the saved levels once per appearance of the channels, and the
    /// stream levels once per appearance of the stream mix (while streamer mode
    /// is on). Each waits only for its own nodes. Each appearance of the
    /// channels also says whether they carry Smart Volume's stage.
    fn keep_levels(&mut self) {
        let channels = Channel::ALL.iter().all(|&c| self.graph.node(&config::sink_name(c)).is_some());
        if !channels {
            self.levels_applied = false;
        } else if !self.levels_applied {
            self.stage_loaded = self.staged();
            match self.apply_levels() {
                Ok(()) => self.levels_applied = true,
                Err(e) => warn!(error = %format!("{e:#}"), "could not restore Sonar volumes"),
            }
        }
        let stream = self.settings.streamer
            && self.graph.node(config::STREAM_SINK).is_some()
            && Channel::ALL.iter().all(|&c| self.graph.node(&config::stream_feed_name(c)).is_some());
        if !stream {
            self.stream_levels_applied = false;
        } else if !self.stream_levels_applied {
            match self.apply_stream_levels() {
                Ok(()) => self.stream_levels_applied = true,
                Err(e) => warn!(error = %format!("{e:#}"), "could not restore the stream levels"),
            }
        }
    }

    /// Push saved channel volumes, EQ, Smart Volume, the mic and ChatMix to PipeWire.
    fn apply_levels(&self) -> Result<()> {
        for (&channel, &volume) in &self.settings.volumes {
            if let Some(node) = self.graph.node(&config::sink_name(channel)) {
                self.tools.run(&tools::set_volume(node.id, volume))?;
            }
        }
        for channel in Channel::ALL {
            self.apply_chain(channel)?;
            self.apply_smart_volume(channel)?;
        }
        self.apply_mic()?;
        self.apply_chatmix()
    }

    /// Push the saved stream levels to the feeds and the master to the stream
    /// device.
    fn apply_stream_levels(&self) -> Result<()> {
        for channel in Channel::ALL {
            if let Some(node) = self.graph.node(&config::stream_feed_name(channel)) {
                self.tools.run(&tools::set_volume(node.id, self.settings.stream_volume(channel)))?;
            }
        }
        if let Some(node) = self.graph.node(config::STREAM_SINK) {
            self.tools.run(&tools::set_volume(node.id, self.settings.stream_master))?;
        }
        Ok(())
    }

    /// Set a channel's whole filter chain, if PipeWire has the channel now.
    fn apply_chain(&self, channel: Channel) -> Result<()> {
        if let Some(node) = self.graph.node(&config::sink_name(channel)) {
            let props = eq::props(&self.settings.channel_eq(channel), &self.settings.headphone);
            self.tools.run(&tools::set_params(node.id, &props))?;
        }
        Ok(())
    }

    /// Set a channel's Smart Volume controls, if PipeWire has the channel and
    /// it carries the stage.
    fn apply_smart_volume(&self, channel: Channel) -> Result<()> {
        if !self.smart_volume_available() {
            return Ok(());
        }
        if let Some(node) = self.graph.node(&config::sink_name(channel)) {
            let props = smart::props(&self.settings.smart_volume(channel));
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
    use crate::testing::{dump_with_source, scratch, Scratch};
    use gg_ipc::{HeadphoneEq, MicSettings, SmartLevel};
    use serde_json::{json, Value};
    use std::cell::RefCell;

    const HEADSET: &str = "alsa_output.usb-SteelSeries_Arctis";
    const BT: &str = "bluez_output.1";
    const MIC: &str = "alsa_input.usb-arctis";

    /// PipeWire stand-in. A restart brings the channels up if the drop-in exists
    /// (and `channels_come_up`), or takes them down if it does not.
    struct Fake {
        dropin: PathBuf,
        streams: RefCell<Vec<(u32, &'static str)>>,
        up: RefCell<bool>,
        channels_come_up: bool,
        mic_plugins: bool,
        mic_comes_up: bool,
        /// Whether swh's compressor is installed.
        smart_volume: RefCell<bool>,
        /// A PipeWire that refuses Smart Volume's stage: with `sv_comp` in the
        /// drop-in it starts without the channels (their modules are `nofail`).
        refuses_stage: RefCell<bool>,
        /// Off holds the stream mix back while the channels are up.
        stream_comes_up: RefCell<bool>,
        /// Commands starting with these words fail, after doing what they do.
        fails: RefCell<&'static [&'static str]>,
        /// The running PipeWire's `info.cookie`: new at each start.
        cookie: RefCell<u64>,
        /// Volumes set since PipeWire started, by node id; the rest are full.
        volumes: RefCell<HashMap<u32, f32>>,
        /// `object.serial` by node id, where it isn't the id.
        serials: RefCell<HashMap<u32, u64>>,
        /// The default output the user chose; WirePlumber plays into `BT`
        /// while it's away.
        configured_sink: &'static str,
        /// How many times gglinux waited for PipeWire.
        pauses: RefCell<usize>,
        runs: RefCell<Vec<Vec<String>>>,
    }

    impl Fake {
        fn new(dropin: PathBuf, streams: &[(u32, &'static str)]) -> Self {
            Fake {
                dropin,
                streams: RefCell::new(streams.to_vec()),
                up: RefCell::new(false),
                channels_come_up: true,
                mic_plugins: true,
                mic_comes_up: true,
                smart_volume: RefCell::new(true),
                refuses_stage: RefCell::new(false),
                stream_comes_up: RefCell::new(true),
                fails: RefCell::new(&[]),
                cookie: RefCell::new(1),
                volumes: RefCell::new(HashMap::new()),
                serials: RefCell::new(HashMap::new()),
                configured_sink: BT,
                pauses: RefCell::new(0),
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
            let mut nodes: Vec<(u32, &str, &str, Option<&str>)> = vec![
                (10, HEADSET, "Audio/Sink", None),
                (11, BT, "Audio/Sink", None),
                (12, MIC, "Audio/Source", None),
            ];
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
                let wrote_mic = std::fs::read_to_string(&self.dropin).is_ok_and(|t| t.contains("gglinux_mic"));
                if wrote_mic && self.mic_comes_up {
                    nodes.extend([
                        (24, "gglinux_mic", "Audio/Source", None),
                        (34, "gglinux_mic.in", "Stream/Input/Audio", None),
                    ]);
                }
                let wrote_stream = std::fs::read_to_string(&self.dropin).is_ok_and(|t| t.contains("gglinux_stream"));
                if wrote_stream && *self.stream_comes_up.borrow() {
                    nodes.extend([
                        (25, "gglinux_stream", "Audio/Sink", None),
                        (35, "gglinux_game.stream", "Stream/Output/Audio", None),
                        (36, "gglinux_chat.stream", "Stream/Output/Audio", None),
                        (37, "gglinux_media.stream", "Stream/Output/Audio", None),
                        (38, "gglinux_aux.stream", "Stream/Output/Audio", None),
                        (45, "gglinux_game.stream_in", "Stream/Input/Audio", None),
                        (46, "gglinux_chat.stream_in", "Stream/Input/Audio", None),
                        (47, "gglinux_media.stream_in", "Stream/Input/Audio", None),
                        (48, "gglinux_aux.stream_in", "Stream/Input/Audio", None),
                    ]);
                }
            }
            let streams = self.streams.borrow();
            nodes.extend(streams.iter().map(|&(id, bin)| (id, bin, "Stream/Output/Audio", Some(bin))));
            let dump = dump_with_source(&nodes, self.configured_sink, Some(MIC));
            let Value::Array(mut objects) = serde_json::from_str(&dump)? else { unreachable!() };
            for object in objects.iter_mut().filter(|o| o["type"] == "PipeWire:Interface:Metadata") {
                let in_use = |kind: &str, name: &str| {
                    json!({ "subject": 0, "key": format!("default.audio.{kind}"), "type": "Spa:String:JSON", "value": { "name": name } })
                };
                let entries = object["metadata"].as_array_mut().unwrap();
                entries.extend([in_use("sink", BT), in_use("source", MIC)]);
            }
            for object in objects.iter_mut().filter(|o| o["type"] == "PipeWire:Interface:Node") {
                let id = object["id"].as_u64().unwrap() as u32;
                let serial = self.serials.borrow().get(&id).copied().unwrap_or(u64::from(id));
                object["info"]["props"]["object.serial"] = json!(serial);
                if let Some(&v) = self.volumes.borrow().get(&id) {
                    let linear = f64::from(v).powi(3);
                    object["info"]["params"]["Props"][0]["channelVolumes"] = json!([linear, linear]);
                }
            }
            let core = json!({ "id": 0, "type": "PipeWire:Interface:Core", "info": { "cookie": *self.cookie.borrow() } });
            objects.insert(0, core);
            Ok(Value::Array(objects).to_string())
        }

        fn run(&self, argv: &[String]) -> Result<()> {
            if argv[..2] == ["wpctl", "set-volume"] {
                self.volumes.borrow_mut().insert(argv[2].parse().unwrap(), argv[3].parse().unwrap());
            }
            if argv[0] == "systemctl" {
                *self.cookie.borrow_mut() += 1;
                self.volumes.borrow_mut().clear();
                let refused = *self.refuses_stage.borrow()
                    && std::fs::read_to_string(&self.dropin).is_ok_and(|t| t.contains("sv_comp"));
                *self.up.borrow_mut() = self.channels_come_up && self.dropin.exists() && !refused;
            }
            self.runs.borrow_mut().push(argv.to_vec());
            let fails = *self.fails.borrow();
            if !fails.is_empty() && argv.len() >= fails.len() && argv.iter().zip(fails).all(|(a, f)| a == f) {
                bail!("`{}` failed", argv.join(" "));
            }
            Ok(())
        }

        fn pause(&self, _: Duration) {
            *self.pauses.borrow_mut() += 1;
        }

        fn mic_plugins_present(&self) -> bool {
            self.mic_plugins
        }

        fn smart_volume_supported(&self) -> bool {
            *self.smart_volume.borrow()
        }
    }

    /// One test's settings and drop-in paths, in a directory removed when the guard goes.
    fn paths(name: &str) -> (Scratch, PathBuf, PathBuf) {
        let dir = scratch(name);
        let (settings, dropin) = (dir.join("sonar.json"), dir.join("pipewire.conf.d").join(config::FILE_NAME));
        (dir, settings, dropin)
    }

    const APPS: &[(u32, &str)] = &[(40, "Discord"), (41, "firefox"), (42, "game.bin")];

    #[test]
    fn enable_writes_the_dropin_restarts_and_makes_game_default() {
        let (_dir, settings_path, dropin) = paths("enable");
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
        let (_dir, settings_path, dropin) = paths("rollback");
        let mut fake = Fake::new(dropin.clone(), APPS);
        fake.channels_come_up = false;
        let mut sonar = Sonar::new(&fake, settings_path.clone(), dropin.clone());
        assert!(sonar.enable().is_err());
        assert!(!dropin.exists(), "the drop-in is removed again");
        assert_eq!(fake.count("systemctl"), 3, "to apply, again without Smart Volume's stage, then to undo");
        let saved = settings::load(&settings_path);
        assert!(!saved.enabled && !saved.smart_volume_refused, "the stage wasn't to blame");
    }

    #[test]
    fn enable_undoes_itself_when_a_later_step_fails() {
        let (_dir, settings_path, dropin) = paths("enable-fails");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path.clone(), dropin.clone());
        *fake.fails.borrow_mut() = &["wpctl", "set-default"];
        assert!(sonar.enable().is_err());
        assert!(!dropin.exists(), "the drop-in is removed again");
        assert!(sonar.state().channels.iter().all(|c| !c.present), "and the channels are gone");
        // systemctl reports a failure though PipeWire did restart (no pipewire-pulse, say).
        *fake.fails.borrow_mut() = &["systemctl"];
        assert!(sonar.enable().is_err());
        assert!(!dropin.exists());
        assert!(sonar.state().channels.iter().all(|c| !c.present));
        assert!(!settings::load(&settings_path).enabled);
    }

    #[test]
    fn streams_are_routed_once() {
        let (_dir, settings_path, dropin) = paths("once");
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
    fn a_new_stream_under_a_reused_id_is_routed() {
        let (_dir, settings_path, dropin) = paths("reused-id");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path, dropin);
        sonar.enable().unwrap();
        sonar.tick();
        fake.runs.borrow_mut().clear();
        // Within one tick Firefox's stream closes and mpv's opens, and PipeWire hands it the same id.
        fake.streams.borrow_mut().retain(|&(id, _)| id != 41);
        fake.streams.borrow_mut().push((41, "mpv"));
        fake.serials.borrow_mut().insert(41, 5152);
        sonar.tick();
        assert!(fake.ran(&["pw-metadata", "41", "target.object", "gglinux_media"]));
    }

    #[test]
    fn outlets_are_never_routed() {
        let (_dir, settings_path, dropin) = paths("outlets");
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
        let (_dir, settings_path, dropin) = paths("move");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path.clone(), dropin);
        sonar.enable().unwrap();
        sonar.set_app_channel("firefox", Channel::Game).unwrap();
        assert!(fake.ran(&["pw-metadata", "41", "target.object", "gglinux_game"]));
        assert_eq!(settings::load(&settings_path).rules.channel_for(Some("firefox")), Channel::Game);
    }

    #[test]
    fn chatmix_turns_down_the_other_side() {
        let (_dir, settings_path, dropin) = paths("chatmix");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path, dropin);
        sonar.enable().unwrap();
        sonar.set_chatmix(0.5).unwrap();
        assert!(fake.ran(&["wpctl", "set-volume", "30", "0.500"]), "Game outlet down");
        assert!(fake.ran(&["wpctl", "set-volume", "31", "1.000"]), "Chat outlet full");
    }

    #[test]
    fn volume_is_saved_and_applied_to_the_channel_sink() {
        let (_dir, settings_path, dropin) = paths("volume");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path.clone(), dropin);
        sonar.enable().unwrap();
        sonar.set_volume(Channel::Chat, 0.4).unwrap();
        assert!(fake.ran(&["wpctl", "set-volume", "21", "0.400"]));
        assert_eq!(settings::load(&settings_path).volumes.get(&Channel::Chat), Some(&0.4));
    }

    #[test]
    fn output_change_retargets_outlets_without_a_restart() {
        let (_dir, settings_path, dropin) = paths("output");
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
        let (_dir, settings_path, dropin) = paths("disable");
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
    fn an_absent_default_output_gives_way_to_the_one_in_use() {
        let (_dir, settings_path, dropin) = paths("default-away");
        let mut fake = Fake::new(dropin.clone(), APPS);
        // The user's default is Bluetooth headphones that are off; WirePlumber plays into BT meanwhile.
        fake.configured_sink = "bluez_output.away";
        let mut sonar = Sonar::new(&fake, settings_path.clone(), dropin);
        sonar.enable().unwrap();
        let saved = settings::load(&settings_path);
        assert_eq!(saved.output.as_deref(), Some(BT), "the channels play where the user hears audio now");
        assert_eq!(saved.previous_default.as_deref(), Some("bluez_output.away"), "still the default to give back");
        let waits = *fake.pauses.borrow();
        sonar.disable().unwrap();
        assert_eq!(*fake.pauses.borrow(), waits, "no waiting for a device that isn't there");
    }

    #[test]
    fn a_removed_dropin_means_sonar_is_off() {
        let (_dir, settings_path, dropin) = paths("removed");
        let saved = Settings { enabled: true, ..Settings::default() };
        settings::save(&settings_path, &saved).unwrap();
        let fake = Fake::new(dropin.clone(), APPS);
        let sonar = Sonar::new(&fake, settings_path.clone(), dropin);
        assert!(!sonar.is_enabled());
        assert!(!settings::load(&settings_path).enabled);
        assert_eq!(fake.count("systemctl"), 0, "nothing re-installed behind the user's back");
    }

    #[test]
    fn a_dropin_left_while_sonar_is_off_is_taken_out_on_startup() {
        let (_dir, settings_path, dropin) = paths("off-with-dropin");
        // A backup from before Sonar was on was restored over its settings.
        settings::save(&settings_path, &Settings::default()).unwrap();
        std::fs::create_dir_all(dropin.parent().unwrap()).unwrap();
        std::fs::write(&dropin, config::dropin(BT, Some(MIC), false, true).unwrap()).unwrap();
        let fake = Fake::new(dropin.clone(), APPS);
        *fake.up.borrow_mut() = true; // PipeWire runs the drop-in
        let mut sonar = Sonar::new(&fake, settings_path.clone(), dropin.clone());
        assert!(!sonar.is_enabled());
        assert!(!dropin.exists());
        assert_eq!(fake.count("systemctl"), 1, "one restart takes the channels down");
        assert!(sonar.state().channels.iter().all(|c| !c.present));
        assert!(!settings::load(&settings_path).enabled);
    }

    #[test]
    fn levels_are_restored_when_the_channels_come_back() {
        let (_dir, settings_path, dropin) = paths("comeback");
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
    fn a_pipewire_restart_between_two_ticks_gets_everything_back() {
        let (_dir, settings_path, dropin) = paths("restart-unseen");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path, dropin);
        sonar.enable().unwrap();
        sonar.set_chatmix(0.5).unwrap();
        sonar.tick();
        fake.runs.borrow_mut().clear();
        // PipeWire restarted by something else, so quickly that no tick saw the channels gone.
        *fake.cookie.borrow_mut() += 1;
        sonar.tick();
        assert!(fake.ran(&["pw-cli", "set-param", "20", "Props", &chain_of(&sonar, Channel::Game)]), "the EQ");
        assert!(fake.ran(&["wpctl", "set-volume", "30", "0.500"]), "ChatMix");
        assert!(fake.ran(&["pw-metadata", "40", "target.object", "gglinux_chat"]), "the apps routed again");
        fake.runs.borrow_mut().clear();
        sonar.tick();
        assert!(fake.runs.borrow().is_empty(), "and only once");
    }

    #[test]
    fn a_volume_changed_elsewhere_is_the_one_brought_back() {
        let (_dir, settings_path, dropin) = paths("volume-elsewhere");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path.clone(), dropin);
        sonar.enable().unwrap();
        sonar.set_volume(Channel::Game, 0.9).unwrap();
        sonar.set_mic_volume(0.8).unwrap();
        sonar.tick();
        assert_eq!(settings::load(&settings_path).volumes.get(&Channel::Game), Some(&0.9), "the slider's level stands");
        // The volume keys turn Game down, and an app's AGC the mic.
        fake.volumes.borrow_mut().extend([(20, 0.15), (24, 0.5)]);
        sonar.tick();
        // PipeWire restarts: the channels come back at full volume.
        fake.volumes.borrow_mut().clear();
        *fake.cookie.borrow_mut() += 1;
        fake.runs.borrow_mut().clear();
        sonar.tick();
        assert!(fake.ran(&["wpctl", "set-volume", "20", "0.150"]), "Game as the user left it");
        assert!(fake.ran(&["wpctl", "set-volume", "24", "0.500"]), "and the mic");
        let saved = settings::load(&settings_path);
        assert!((saved.volumes[&Channel::Game] - 0.15).abs() < 1e-3, "{saved:?}");
        assert!(saved.mic_volume.is_some_and(|v| (v - 0.5).abs() < 1e-3), "{saved:?}");
    }

    #[test]
    fn state_lists_outputs_channels_and_apps() {
        let (_dir, settings_path, dropin) = paths("state");
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
        let (_dir, settings_path, dropin) = paths("eq-enable");
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
        let (_dir, settings_path, dropin) = paths("eq-channel");
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
        let (_dir, settings_path, dropin) = paths("eq-headphone");
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
        let (_dir, settings_path, dropin) = paths("eq-import");
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
        let (_dir, settings_path, dropin) = paths("upgrade");
        let saved = Settings { enabled: true, output: Some(BT.into()), ..Settings::default() };
        settings::save(&settings_path, &saved).unwrap();
        std::fs::create_dir_all(dropin.parent().unwrap()).unwrap();
        std::fs::write(&dropin, "# an M3a loopback drop-in\n").unwrap();
        let fake = Fake::new(dropin.clone(), APPS);
        let sonar = Sonar::new(&fake, settings_path, dropin.clone());
        assert!(sonar.is_enabled());
        assert_eq!(std::fs::read_to_string(&dropin).unwrap(), config::dropin(BT, Some(MIC), false, true).unwrap());
        assert_eq!(fake.count("systemctl"), 1);
    }

    #[test]
    fn a_current_dropin_is_left_alone() {
        let (_dir, settings_path, dropin) = paths("current");
        let saved = Settings {
            enabled: true,
            output: Some(BT.into()),
            mic_input: Some(MIC.into()),
            ..Settings::default()
        };
        settings::save(&settings_path, &saved).unwrap();
        std::fs::create_dir_all(dropin.parent().unwrap()).unwrap();
        std::fs::write(&dropin, config::dropin(BT, Some(MIC), false, true).unwrap()).unwrap();
        let fake = Fake::new(dropin.clone(), APPS);
        *fake.up.borrow_mut() = true; // PipeWire already runs this drop-in
        let _sonar = Sonar::new(&fake, settings_path, dropin);
        assert_eq!(fake.count("systemctl"), 0);
    }

    #[test]
    fn enable_installs_the_mic_and_makes_it_the_default_input() {
        let (_dir, settings_path, dropin) = paths("mic-enable");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path.clone(), dropin.clone());
        sonar.enable().unwrap();
        let text = std::fs::read_to_string(&dropin).unwrap();
        assert!(text.contains(&format!("target.object      = \"{MIC}\"")));
        assert!(fake.ran(&["wpctl", "set-default", "24"]), "gglinux Mic becomes the default input");
        let props = crate::mic::props(&MicSettings::default());
        assert!(fake.ran(&["pw-cli", "set-param", "34", "Props", &props]), "controls on the capture node");
        let saved = settings::load(&settings_path);
        assert_eq!(saved.mic_input.as_deref(), Some(MIC));
        assert_eq!(saved.previous_default_source.as_deref(), Some(MIC));
        assert!(sonar.snapshot().mic.present);
    }

    #[test]
    fn without_mic_plugins_the_channels_still_work() {
        let (_dir, settings_path, dropin) = paths("mic-none");
        let mut fake = Fake::new(dropin.clone(), APPS);
        fake.mic_plugins = false;
        let mut sonar = Sonar::new(&fake, settings_path, dropin.clone());
        sonar.enable().unwrap();
        assert!(!std::fs::read_to_string(&dropin).unwrap().contains("gglinux_mic"));
        let mic = sonar.snapshot().mic;
        assert!(!mic.available && !mic.present);
        assert!(!fake.ran(&["wpctl", "set-default", "24"]));
    }

    #[test]
    fn channels_up_without_the_mic_keeps_sonar_on() {
        let (_dir, settings_path, dropin) = paths("mic-failed");
        let mut fake = Fake::new(dropin.clone(), APPS);
        fake.mic_comes_up = false;
        let mut sonar = Sonar::new(&fake, settings_path, dropin);
        sonar.enable().unwrap();
        assert!(sonar.is_enabled());
        assert_eq!(fake.count("systemctl"), 1, "no rollback");
        assert!(!sonar.snapshot().mic.present);
    }

    #[test]
    fn mic_settings_are_validated_saved_and_set_live() {
        let (_dir, settings_path, dropin) = paths("mic-settings");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path.clone(), dropin);
        sonar.enable().unwrap();
        let on = MicSettings { gate: true, gate_threshold_db: -40.0, ..MicSettings::default() };
        sonar.set_mic_settings(on).unwrap();
        assert!(fake.ran(&["pw-cli", "set-param", "34", "Props", &crate::mic::props(&on)]));
        assert_eq!(settings::load(&settings_path).mic, on);
        let before = fake.runs.borrow().len();
        assert!(sonar.set_mic_settings(MicSettings { makeup_db: 40.0, ..on }).is_err());
        assert_eq!(fake.runs.borrow().len(), before);
        assert_eq!(settings::load(&settings_path).mic, on);
    }

    #[test]
    fn mic_volume_goes_to_the_virtual_source() {
        let (_dir, settings_path, dropin) = paths("mic-volume");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path, dropin);
        sonar.enable().unwrap();
        sonar.set_mic_volume(0.6).unwrap();
        assert!(fake.ran(&["wpctl", "set-volume", "24", "0.600"]));
    }

    #[test]
    fn disable_restores_the_default_input() {
        let (_dir, settings_path, dropin) = paths("mic-disable");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path, dropin);
        sonar.enable().unwrap();
        sonar.disable().unwrap();
        assert!(fake.ran(&["wpctl", "set-default", "12"]), "back to the physical mic");
    }

    #[test]
    fn plugins_installed_later_add_the_mic_on_startup() {
        let (_dir, settings_path, dropin) = paths("mic-later");
        let saved = Settings { enabled: true, output: Some(BT.into()), ..Settings::default() };
        settings::save(&settings_path, &saved).unwrap();
        std::fs::create_dir_all(dropin.parent().unwrap()).unwrap();
        std::fs::write(&dropin, config::dropin(BT, None, false, true).unwrap()).unwrap();
        let fake = Fake::new(dropin.clone(), APPS);
        let _sonar = Sonar::new(&fake, settings_path.clone(), dropin.clone());
        assert!(std::fs::read_to_string(&dropin).unwrap().contains("gglinux_mic"));
        assert_eq!(fake.count("systemctl"), 1);
        assert_eq!(settings::load(&settings_path).mic_input.as_deref(), Some(MIC));
    }

    #[test]
    fn streamer_while_sonar_is_off_only_saves() {
        let (_dir, settings_path, dropin) = paths("streamer-off");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path.clone(), dropin.clone());
        sonar.set_streamer(true).unwrap();
        assert_eq!(fake.count("systemctl"), 0, "no PipeWire restart");
        assert!(settings::load(&settings_path).streamer);
        sonar.enable().unwrap();
        assert!(std::fs::read_to_string(&dropin).unwrap().contains("gglinux_stream"), "applied at the next enable");
    }

    #[test]
    fn streamer_on_with_sonar_on_rewrites_restarts_and_sets_levels() {
        let (_dir, settings_path, dropin) = paths("streamer-on");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path, dropin.clone());
        sonar.enable().unwrap();
        sonar.set_stream_volume(Channel::Chat, 0.2).unwrap();
        let restarts = fake.count("systemctl");
        sonar.set_streamer(true).unwrap();
        assert_eq!(fake.count("systemctl"), restarts + 1, "one restart");
        assert!(std::fs::read_to_string(&dropin).unwrap().contains("gglinux_stream"));
        assert!(fake.ran(&["wpctl", "set-volume", "36", "0.200"]), "Chat's stream level");
        assert!(fake.ran(&["wpctl", "set-volume", "25", "1.000"]), "the master level");
        let defaults = fake.runs.borrow().iter().filter(|r| r[..] == ["wpctl", "set-default", "20"]).count();
        assert!(defaults >= 2, "Game is made the default again after the extra restart");
        let s = sonar.snapshot();
        assert!(s.stream.enabled && s.stream.present);
        assert_eq!(s.stream.volumes, vec![1.0, 0.2, 1.0, 1.0]);
    }

    #[test]
    fn stream_volumes_go_to_the_feed_nodes() {
        let (_dir, settings_path, dropin) = paths("stream-volumes");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path, dropin);
        sonar.enable().unwrap();
        sonar.set_streamer(true).unwrap();
        sonar.set_stream_volume(Channel::Game, 0.5).unwrap();
        sonar.set_stream_master(0.8).unwrap();
        assert!(fake.ran(&["wpctl", "set-volume", "35", "0.500"]));
        assert!(fake.ran(&["wpctl", "set-volume", "25", "0.800"]));
        assert!(sonar.set_stream_volume(Channel::Game, f32::NAN).is_err());
    }

    #[test]
    fn routing_ignores_the_stream_feeds() {
        let (_dir, settings_path, dropin) = paths("stream-routing");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path, dropin);
        sonar.enable().unwrap();
        sonar.set_streamer(true).unwrap();
        sonar.tick();
        for id in [35, 36, 37, 38, 45, 46, 47, 48] {
            assert!(
                !fake.runs.borrow().iter().any(|r| r[0] == "pw-metadata" && r[1] == id.to_string()),
                "node {id} was routed"
            );
        }
    }

    #[test]
    fn stream_levels_come_back_after_a_restart() {
        let (_dir, settings_path, dropin) = paths("stream-restart");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path, dropin);
        sonar.enable().unwrap();
        sonar.set_streamer(true).unwrap();
        sonar.set_stream_volume(Channel::Media, 0.3).unwrap();
        let media = |fake: &Fake| fake.runs.borrow().iter().filter(|r| r[..] == ["wpctl", "set-volume", "37", "0.300"]).count();
        let before = media(&fake);
        *fake.up.borrow_mut() = false; // PipeWire restarted elsewhere…
        sonar.tick();
        *fake.stream_comes_up.borrow_mut() = false;
        *fake.up.borrow_mut() = true; // …the channels are back first…
        sonar.tick();
        *fake.stream_comes_up.borrow_mut() = true; // …then the stream mix
        sonar.tick();
        assert_eq!(media(&fake), before + 1);
    }

    #[test]
    fn a_missing_stream_mix_leaves_the_channels_their_levels() {
        let (_dir, settings_path, dropin) = paths("stream-missing");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path, dropin);
        sonar.enable().unwrap();
        sonar.set_volume(Channel::Chat, 0.4).unwrap();
        sonar.set_chatmix(0.5).unwrap();
        *fake.stream_comes_up.borrow_mut() = false;
        fake.runs.borrow_mut().clear();
        sonar.set_streamer(true).unwrap();
        assert!(sonar.is_enabled(), "never rolled back for the stream");
        assert!(!sonar.snapshot().stream.present);
        assert!(fake.ran(&["wpctl", "set-volume", "21", "0.400"]), "Chat's own volume");
        assert!(fake.ran(&["pw-cli", "set-param", "20", "Props", &chain_of(&sonar, Channel::Game)]), "the EQ");
        assert!(fake.ran(&["wpctl", "set-volume", "30", "0.500"]), "ChatMix");

        // The stream mix turns up later: only its own levels are pushed.
        fake.runs.borrow_mut().clear();
        *fake.stream_comes_up.borrow_mut() = true;
        sonar.tick();
        assert!(fake.ran(&["wpctl", "set-volume", "25", "1.000"]), "the master level");
        assert!(!fake.ran(&["wpctl", "set-volume", "21", "0.400"]), "the channels are not pushed again");
    }

    #[test]
    fn a_streamer_switch_that_fails_after_the_restart_still_gets_the_levels_back() {
        let (_dir, settings_path, dropin) = paths("streamer-fails");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path, dropin);
        sonar.enable().unwrap();
        sonar.set_chatmix(0.5).unwrap();
        // PipeWire restarts with the new drop-in, then the defaults can't be set.
        *fake.fails.borrow_mut() = &["wpctl", "set-default"];
        assert!(sonar.set_streamer(true).is_err());
        *fake.fails.borrow_mut() = &[];
        fake.runs.borrow_mut().clear();
        sonar.tick();
        assert!(fake.ran(&["pw-cli", "set-param", "20", "Props", &chain_of(&sonar, Channel::Game)]), "the EQ");
        assert!(fake.ran(&["wpctl", "set-volume", "30", "0.500"]), "ChatMix");
        assert!(fake.ran(&["pw-metadata", "40", "target.object", "gglinux_chat"]), "the apps routed again");
    }

    #[test]
    fn enable_sets_every_channels_smart_volume() {
        let (_dir, settings_path, dropin) = paths("smart-enable");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path, dropin.clone());
        sonar.enable().unwrap();
        assert!(std::fs::read_to_string(&dropin).unwrap().contains("name = sv_comp"));
        let off = crate::smart::props(&SmartVolume::default());
        for id in ["20", "21", "22", "23"] {
            assert!(fake.ran(&["pw-cli", "set-param", id, "Props", &off]), "channel {id} starts off");
        }
    }

    #[test]
    fn smart_volume_switches_live_without_a_restart() {
        let (_dir, settings_path, dropin) = paths("smart-live");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path.clone(), dropin);
        sonar.enable().unwrap();
        let restarts = fake.count("systemctl");
        let loud = SmartVolume { enabled: true, level: SmartLevel::Loud };
        sonar.set_smart_volume(Channel::Media, loud).unwrap();
        assert!(fake.ran(&["pw-cli", "set-param", "22", "Props", &crate::smart::props(&loud)]));
        let off = SmartVolume { enabled: false, ..loud };
        sonar.set_smart_volume(Channel::Media, off).unwrap();
        assert!(fake.ran(&["pw-cli", "set-param", "22", "Props", &crate::smart::props(&off)]));
        assert_eq!(fake.count("systemctl"), restarts, "never a PipeWire restart");
        assert_eq!(settings::load(&settings_path).smart_volume(Channel::Media), off);
        let s = sonar.snapshot().smart_volume;
        assert!(s.available && s.reason.is_none());
        assert_eq!(s.settings, vec![SmartVolume::default(), SmartVolume::default(), off, SmartVolume::default()]);
    }

    #[test]
    fn without_the_compressor_the_channels_are_as_before() {
        let (_dir, settings_path, dropin) = paths("smart-none");
        let fake = Fake::new(dropin.clone(), APPS);
        *fake.smart_volume.borrow_mut() = false;
        let mut sonar = Sonar::new(&fake, settings_path, dropin.clone());
        sonar.enable().unwrap();
        assert_eq!(std::fs::read_to_string(&dropin).unwrap(), config::dropin(BT, Some(MIC), false, false).unwrap());
        let s = sonar.snapshot().smart_volume;
        assert!(!s.available, "{s:?}");
        assert!(s.reason.as_deref().is_some_and(|r| r.contains("Install swh-plugins")), "asks for the package: {s:?}");
        let on = SmartVolume { enabled: true, ..SmartVolume::default() };
        assert!(sonar.set_smart_volume(Channel::Game, on).is_err());
        assert!(
            !fake.runs.borrow().iter().any(|r| r[0] == "pw-cli" && r[4].contains("sv_mix")),
            "no Smart Volume controls sent"
        );
    }

    #[test]
    fn a_compressor_that_disappears_takes_the_stage_out_on_startup() {
        let (_dir, settings_path, dropin) = paths("smart-gone");
        let saved = Settings { enabled: true, output: Some(BT.into()), mic_input: Some(MIC.into()), ..Settings::default() };
        settings::save(&settings_path, &saved).unwrap();
        std::fs::create_dir_all(dropin.parent().unwrap()).unwrap();
        std::fs::write(&dropin, config::dropin(BT, Some(MIC), false, true).unwrap()).unwrap();
        let fake = Fake::new(dropin.clone(), APPS);
        *fake.smart_volume.borrow_mut() = false; // swh-plugins removed while gglinux was off
        let _sonar = Sonar::new(&fake, settings_path, dropin.clone());
        assert_eq!(std::fs::read_to_string(&dropin).unwrap(), config::dropin(BT, Some(MIC), false, false).unwrap());
        assert_eq!(fake.count("systemctl"), 1, "one restart");
    }

    #[test]
    fn a_compressor_that_appears_adds_the_stage_on_startup() {
        let (_dir, settings_path, dropin) = paths("smart-new");
        let saved = Settings { enabled: true, output: Some(BT.into()), mic_input: Some(MIC.into()), ..Settings::default() };
        settings::save(&settings_path, &saved).unwrap();
        std::fs::create_dir_all(dropin.parent().unwrap()).unwrap();
        std::fs::write(&dropin, config::dropin(BT, Some(MIC), false, false).unwrap()).unwrap();
        let fake = Fake::new(dropin.clone(), APPS);
        let sonar = Sonar::new(&fake, settings_path, dropin.clone());
        assert_eq!(std::fs::read_to_string(&dropin).unwrap(), config::dropin(BT, Some(MIC), false, true).unwrap());
        assert_eq!(fake.count("systemctl"), 1, "one restart");
        assert!(sonar.snapshot().smart_volume.available, "the channels came back with the stage");
    }

    #[test]
    fn a_refused_stage_is_taken_out_when_sonar_turns_on() {
        let (_dir, settings_path, dropin) = paths("smart-refused");
        let fake = Fake::new(dropin.clone(), APPS);
        *fake.refuses_stage.borrow_mut() = true;
        let mut sonar = Sonar::new(&fake, settings_path.clone(), dropin.clone());
        sonar.enable().unwrap();
        assert!(sonar.is_enabled(), "the channels run, without Smart Volume");
        assert_eq!(std::fs::read_to_string(&dropin).unwrap(), config::dropin(BT, Some(MIC), false, false).unwrap());
        assert_eq!(fake.count("systemctl"), 2, "once with the stage, once without");
        assert!(settings::load(&settings_path).smart_volume_refused);
        let s = sonar.snapshot().smart_volume;
        assert!(!s.available && s.reason.as_deref().is_some_and(|r| r.contains("off and on")), "{s:?}");
        let on = SmartVolume { enabled: true, ..SmartVolume::default() };
        assert!(sonar.set_smart_volume(Channel::Game, on).is_err());
        sonar.disable().unwrap();
        assert!(!settings::load(&settings_path).smart_volume_refused, "turning Sonar on again tries the stage again");
    }

    #[test]
    fn a_refused_stage_is_taken_out_on_startup() {
        let (_dir, settings_path, dropin) = paths("smart-refused-upgrade");
        let saved = Settings { enabled: true, output: Some(BT.into()), mic_input: Some(MIC.into()), ..Settings::default() };
        settings::save(&settings_path, &saved).unwrap();
        std::fs::create_dir_all(dropin.parent().unwrap()).unwrap();
        // Sonar was on before Smart Volume, so the startup upgrade adds the stage.
        let before = config::dropin(BT, Some(MIC), false, false).unwrap();
        std::fs::write(&dropin, &before).unwrap();
        let fake = Fake::new(dropin.clone(), APPS);
        *fake.refuses_stage.borrow_mut() = true;
        let sonar = Sonar::new(&fake, settings_path.clone(), dropin.clone());
        assert!(sonar.is_enabled());
        assert_eq!(std::fs::read_to_string(&dropin).unwrap(), before);
        assert_eq!(fake.count("systemctl"), 2, "the upgrade, then the stage taken out");
        assert!(settings::load(&settings_path).smart_volume_refused);
        assert!(!sonar.snapshot().smart_volume.available);
    }

    #[test]
    fn a_stage_refused_at_login_is_taken_out_on_startup() {
        let (_dir, settings_path, dropin) = paths("smart-refused-login");
        let saved = Settings { enabled: true, output: Some(BT.into()), mic_input: Some(MIC.into()), ..Settings::default() };
        settings::save(&settings_path, &saved).unwrap();
        std::fs::create_dir_all(dropin.parent().unwrap()).unwrap();
        // The drop-in is current, but this PipeWire (a newer one, say) refused it at login.
        std::fs::write(&dropin, config::dropin(BT, Some(MIC), false, true).unwrap()).unwrap();
        let fake = Fake::new(dropin.clone(), APPS);
        *fake.refuses_stage.borrow_mut() = true;
        let _sonar = Sonar::new(&fake, settings_path.clone(), dropin.clone());
        assert_eq!(std::fs::read_to_string(&dropin).unwrap(), config::dropin(BT, Some(MIC), false, false).unwrap());
        assert_eq!(fake.count("systemctl"), 1, "one restart, without the stage");
        assert!(settings::load(&settings_path).smart_volume_refused);
    }

    #[test]
    fn a_stage_refused_after_a_streamer_restart_is_taken_out() {
        let (_dir, settings_path, dropin) = paths("smart-refused-streamer");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path.clone(), dropin.clone());
        sonar.enable().unwrap();
        *fake.refuses_stage.borrow_mut() = true; // PipeWire was upgraded since
        sonar.set_streamer(true).unwrap();
        assert_eq!(std::fs::read_to_string(&dropin).unwrap(), config::dropin(BT, Some(MIC), true, false).unwrap());
        assert_eq!(fake.count("systemctl"), 3, "on, streamer mode, then without the stage");
        assert!(sonar.is_enabled() && settings::load(&settings_path).smart_volume_refused);
    }

    #[test]
    fn a_compressor_removed_while_sonar_runs_leaves_the_dropin_at_once() {
        let (_dir, settings_path, dropin) = paths("smart-removed");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path, dropin.clone());
        sonar.enable().unwrap();
        let restarts = fake.count("systemctl");
        *fake.smart_volume.borrow_mut() = false; // swh-plugins removed
        sonar.tick();
        assert_eq!(std::fs::read_to_string(&dropin).unwrap(), config::dropin(BT, Some(MIC), false, false).unwrap());
        assert_eq!(fake.count("systemctl"), restarts, "PipeWire still runs the stage it loaded: no restart");
        assert!(!sonar.snapshot().smart_volume.available);
        sonar.tick();
        assert_eq!(fake.count("systemctl"), restarts, "once is enough");
        *fake.smart_volume.borrow_mut() = true; // installed again
        sonar.tick();
        assert!(!sonar.snapshot().smart_volume.available, "the drop-in lost the stage: it waits for Sonar off and on");
    }

    #[test]
    fn a_compressor_installed_while_sonar_runs_waits_for_sonar_off_and_on() {
        let (_dir, settings_path, dropin) = paths("smart-installed");
        let fake = Fake::new(dropin.clone(), APPS);
        *fake.smart_volume.borrow_mut() = false;
        let mut sonar = Sonar::new(&fake, settings_path, dropin.clone());
        sonar.enable().unwrap();
        *fake.smart_volume.borrow_mut() = true; // swh-plugins installed
        sonar.tick();
        let s = sonar.snapshot().smart_volume;
        assert!(!s.available && s.reason.as_deref().is_some_and(|r| r.contains("before swh-plugins")), "{s:?}");
        let on = SmartVolume { enabled: true, ..SmartVolume::default() };
        assert!(sonar.set_smart_volume(Channel::Game, on).is_err());
        assert!(
            !fake.runs.borrow().iter().any(|r| r[0] == "pw-cli" && r[4].contains("sv_mix")),
            "no Smart Volume controls sent"
        );
        sonar.set_output(HEADSET).unwrap();
        assert!(!std::fs::read_to_string(&dropin).unwrap().contains("sv_comp"), "the stage comes only with a restart");

        sonar.disable().unwrap();
        sonar.enable().unwrap();
        assert!(sonar.snapshot().smart_volume.available);
        sonar.set_smart_volume(Channel::Game, on).unwrap();
        assert!(fake.ran(&["pw-cli", "set-param", "20", "Props", &crate::smart::props(&on)]));
    }

    #[test]
    fn a_compressor_removed_before_a_pipewire_restart_brings_the_channels_back() {
        let (_dir, settings_path, dropin) = paths("smart-removed-restart");
        let fake = Fake::new(dropin.clone(), APPS);
        let mut sonar = Sonar::new(&fake, settings_path, dropin.clone());
        sonar.enable().unwrap();
        sonar.set_chatmix(0.5).unwrap();
        fake.runs.borrow_mut().clear();
        // swh-plugins removed, and PipeWire restarted by something else before gglinux
        // looked: it skipped the channel modules.
        *fake.smart_volume.borrow_mut() = false;
        *fake.up.borrow_mut() = false;
        sonar.tick();
        assert_eq!(std::fs::read_to_string(&dropin).unwrap(), config::dropin(BT, Some(MIC), false, false).unwrap());
        assert_eq!(fake.count("systemctl"), 1, "one restart, without the stage");
        assert!(sonar.snapshot().channels.iter().all(|c| c.present));
        assert!(fake.ran(&["pw-cli", "set-param", "20", "Props", &chain_of(&sonar, Channel::Game)]), "the EQ is pushed again");
        assert!(fake.ran(&["wpctl", "set-volume", "30", "0.500"]), "and ChatMix");
        assert!(fake.ran(&["pw-metadata", "40", "target.object", "gglinux_chat"]), "and the apps routed again");
    }
}
