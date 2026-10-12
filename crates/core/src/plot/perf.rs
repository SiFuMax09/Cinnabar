//! Per-player performance display (boss bar or action bar) and the cheap statistics behind it.
//!
//! Statistics are sampled in the plot thread once per `SAMPLE_INTERVAL` from counters the plot
//! already maintains, so no extra work happens per tick or per packet.

use crate::player::{PacketSender, Player};
use mchprs_network::packets::clientbound::{
    BossBarColor, BossBarOverlay, CBossEvent, CSystemChatMessage, ClientBoundPacket,
};
use mchprs_redpiler::CompilerOptions;
use mchprs_save_data::plot_data::Tps;
use mchprs_text::{ColorCode, TextComponent};
use rustc_hash::FxHashMap;
use std::sync::atomic::AtomicUsize;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

pub const SAMPLE_INTERVAL: Duration = Duration::from_millis(500);
/// Bar ids only matter to the receiving client, so every player can use the same one.
const BAR_UUID: u128 = 0x6d63_6870_7273_0000_7065_7266_0000_0001;
/// Weight of the newest 500ms window in the exponential moving average.
const EMA_ALPHA: f64 = 0.4;

/// Server wide counters, published by the server thread and read by plot threads.
pub static LOADED_PLOTS: AtomicUsize = AtomicUsize::new(0);
pub static ONLINE_PLAYERS: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum PerfMode {
    Off,
    #[default]
    Bar,
    ActionBar,
}

// Kept in memory only: the player file is a fixed bincode layout and changing it would reset
// every existing player's data.
static PREFS: LazyLock<Mutex<FxHashMap<u128, PerfMode>>> = LazyLock::new(Default::default);

pub fn mode_for(uuid: u128) -> PerfMode {
    PREFS
        .lock()
        .unwrap()
        .get(&uuid)
        .copied()
        .unwrap_or_default()
}

pub fn set_mode(uuid: u128, mode: PerfMode) {
    PREFS.lock().unwrap().insert(uuid, mode);
}

fn ema(old: Option<f64>, new: f64) -> f64 {
    match old {
        Some(old) => old + (new - old) * EMA_ALPHA,
        None => new,
    }
}

/// Cumulative counters the sampler turns into rates.
#[derive(Clone, Copy, Default)]
pub struct Counters {
    pub redstone_ticks: u64,
    pub entity_ticks: u64,
    pub entity_busy: Duration,
}

/// Rolling averages of loop frequency, loop busy time, achieved redstone ticks per second and
/// the 20 Hz entity loop.
#[derive(Default)]
pub struct Sampler {
    window_start: Option<Instant>,
    updates: u32,
    busy: Duration,
    last: Counters,
    pub loop_hz: Option<f64>,
    pub loop_mspt: Option<f64>,
    pub rtps: Option<f64>,
    pub entity_tps: Option<f64>,
    pub entity_mspt: Option<f64>,
}

impl Sampler {
    pub fn record_update(&mut self, busy: Duration) {
        self.updates += 1;
        self.busy += busy;
    }

    /// Closes the window if it is long enough. Returns true when the averages changed.
    pub fn sample(&mut self, now: Instant, counters: Counters) -> bool {
        let Some(start) = self.window_start else {
            self.window_start = Some(now);
            self.last = counters;
            return false;
        };
        let elapsed = now.duration_since(start);
        if elapsed < SAMPLE_INTERVAL {
            return false;
        }
        let secs = elapsed.as_secs_f64();
        if self.updates > 0 {
            self.loop_hz = Some(ema(self.loop_hz, f64::from(self.updates) / secs));
            let mspt = self.busy.as_secs_f64() * 1000.0 / f64::from(self.updates);
            self.loop_mspt = Some(ema(self.loop_mspt, mspt));
        }
        let ticks = counters
            .redstone_ticks
            .saturating_sub(self.last.redstone_ticks);
        self.rtps = Some(ema(self.rtps, ticks as f64 / secs));
        let ent_ticks = counters.entity_ticks.saturating_sub(self.last.entity_ticks);
        if ent_ticks > 0 {
            self.entity_tps = Some(ema(self.entity_tps, ent_ticks as f64 / secs));
            let busy = counters.entity_busy.saturating_sub(self.last.entity_busy);
            let mspt = busy.as_secs_f64() * 1000.0 / ent_ticks as f64;
            self.entity_mspt = Some(ema(self.entity_mspt, mspt));
        }
        self.window_start = Some(now);
        self.updates = 0;
        self.busy = Duration::ZERO;
        self.last = counters;
        true
    }

    /// Forget the window, e.g. after the plot sat empty and did not tick.
    pub fn restart(&mut self) {
        *self = Sampler::default();
    }
}

pub struct PerfSnapshot {
    pub rtps: f64,
    pub target: Tps,
    pub nspt: Option<Duration>,
    pub behind: bool,
    pub loop_hz: f64,
    pub loop_mspt: f64,
    /// Entity loop rate and tick time, once entities have been ticked
    pub entity_loop: Option<(f64, f64)>,
    pub redpiler_flags: Option<String>,
    pub entities: usize,
    pub players: usize,
    pub online_players: usize,
    pub loaded_plots: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Health {
    Good,
    Warn,
    Bad,
}

impl Health {
    fn code(self) -> ColorCode {
        match self {
            Health::Good => ColorCode::Green,
            Health::Warn => ColorCode::Yellow,
            Health::Bad => ColorCode::Red,
        }
    }

    fn bar_color(self) -> BossBarColor {
        match self {
            Health::Good => BossBarColor::Green,
            Health::Warn => BossBarColor::Yellow,
            Health::Bad => BossBarColor::Red,
        }
    }
}

/// Short flag letters, the same ones `/redpiler compile` takes.
pub fn flag_letters(options: &CompilerOptions) -> String {
    [
        (options.optimize, 'o'),
        (options.export, 'e'),
        (options.io_only, 'i'),
        (options.update, 'u'),
        (options.wire_dot_out, 'd'),
        (options.illegal_states_out, 'l'),
        (options.wire_cross_out, 'c'),
    ]
    .iter()
    .filter(|(set, _)| *set)
    .map(|(_, c)| *c)
    .collect()
}

impl PerfSnapshot {
    pub fn health(&self) -> Health {
        if self.behind {
            return Health::Bad;
        }
        match self.target {
            Tps::Limited(target) if target > 0.0 => {
                let ratio = self.rtps / f64::from(target);
                if ratio >= 0.95 {
                    Health::Good
                } else if ratio >= 0.75 {
                    Health::Warn
                } else {
                    Health::Bad
                }
            }
            Tps::Limited(_) => Health::Good,
            Tps::Unlimited if self.loop_mspt > 50.0 => Health::Warn,
            Tps::Unlimited => Health::Good,
        }
    }

    /// Actual / target redstone rate in `0..=1`; unlimited rates always show a full bar.
    pub fn progress(&self) -> f32 {
        match self.target {
            Tps::Limited(target) if target > 0.0 => {
                (self.rtps / f64::from(target)).clamp(0.0, 1.0) as f32
            }
            Tps::Limited(_) => 0.0,
            Tps::Unlimited => 1.0,
        }
    }

    pub fn title(&self) -> TextComponent {
        let sep = || seg(" | ", ColorCode::DarkGray);
        let target = match self.target {
            Tps::Limited(t) => format!("{}", (t * 10.0).round() / 10.0),
            Tps::Unlimited => "unlimited".to_string(),
        };
        let mut extra = vec![
            seg("RTPS ", ColorCode::Gray),
            seg(&format!("{:.1}", self.rtps), self.health().code()),
            seg(&format!("/{target}"), ColorCode::Gray),
        ];
        if let Some(nspt) = self.nspt {
            extra.push(seg(
                &format!(" ({} ns/t)", nspt.as_nanos()),
                ColorCode::Gray,
            ));
        }
        extra.push(sep());
        extra.push(seg("Loop ", ColorCode::Gray));
        extra.push(seg(&format!("{:.0} Hz", self.loop_hz), ColorCode::White));
        extra.push(seg(
            &format!(" {:.1} ms", self.loop_mspt),
            ms_color(self.loop_mspt),
        ));
        extra.push(sep());
        extra.push(seg(&format!("Entities {}", self.entities), ColorCode::Gray));
        if let Some((tps, mspt)) = self.entity_loop {
            extra.push(seg(&format!(" ({tps:.1} TPS "), ColorCode::Gray));
            extra.push(seg(&format!("{mspt:.2} ms"), ms_color(mspt)));
            extra.push(seg(")", ColorCode::Gray));
        }
        extra.push(sep());
        match &self.redpiler_flags {
            Some(flags) if flags.is_empty() => extra.push(seg("RP on", ColorCode::Green)),
            Some(flags) => extra.push(seg(&format!("RP on -{flags}"), ColorCode::Green)),
            None => extra.push(seg("RP off", ColorCode::LightPurple)),
        }
        extra.push(sep());
        extra.push(seg(
            &format!(
                "Players {} | Online {} | Plots {}",
                self.players, self.online_players, self.loaded_plots
            ),
            ColorCode::Gray,
        ));
        TextComponent {
            extra,
            ..Default::default()
        }
    }
}

fn ms_color(ms: f64) -> ColorCode {
    if ms > 50.0 {
        ColorCode::Red
    } else if ms > 25.0 {
        ColorCode::Yellow
    } else {
        ColorCode::Green
    }
}

fn seg(text: &str, color: ColorCode) -> TextComponent {
    TextComponent {
        text: text.to_string(),
        color: Some(color.into()),
        ..Default::default()
    }
}

/// Which players currently have a bar on their screen, so we never add one twice or leave one
/// behind (the client throws on a duplicate id).
#[derive(Default)]
pub struct PerfDisplay {
    pub sampler: Sampler,
    /// uuid -> whether the player has our bar
    shown: FxHashMap<u128, bool>,
    last_color: Option<BossBarColor>,
    last: Option<(TextComponent, f32, BossBarColor)>,
    dirty: bool,
}

impl PerfDisplay {
    /// Request a refresh on the next plot update (new player, `/perf` changed).
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    fn add_packet(&self) -> Option<CBossEvent> {
        self.last
            .as_ref()
            .map(|(title, progress, color)| CBossEvent::Add {
                uuid: BAR_UUID,
                title: title.clone(),
                progress: *progress,
                color: *color,
                overlay: BossBarOverlay::Progress,
            })
    }

    /// Removes the bar when a player leaves the plot. Disconnected players use `forget`.
    pub fn remove_player(&mut self, player: &Player) {
        if self.shown.remove(&player.uuid).unwrap_or(false) {
            player.send_packet(&CBossEvent::Remove { uuid: BAR_UUID }.encode());
        }
    }

    pub fn forget(&mut self, uuid: u128) {
        self.shown.remove(&uuid);
    }

    /// Reconciles every player with their preference and, if `snapshot` is given, pushes the
    /// new numbers.
    pub fn refresh(&mut self, players: &[Player], snapshot: Option<PerfSnapshot>) {
        self.dirty = false;
        let mut update = None;
        if let Some(snapshot) = snapshot {
            let color = snapshot.health().bar_color();
            let progress = snapshot.progress();
            let title = snapshot.title();
            let style_changed = self.last.is_some() && self.last_color != Some(color);
            self.last_color = Some(color);
            self.last = Some((title.clone(), progress, color));
            update = Some((
                CBossEvent::UpdateTitle {
                    uuid: BAR_UUID,
                    title: title.clone(),
                }
                .encode(),
                CBossEvent::UpdateProgress {
                    uuid: BAR_UUID,
                    progress,
                }
                .encode(),
                style_changed.then(|| {
                    CBossEvent::UpdateStyle {
                        uuid: BAR_UUID,
                        color,
                        overlay: BossBarOverlay::Progress,
                    }
                    .encode()
                }),
                CSystemChatMessage {
                    content: title,
                    overlay: true,
                }
                .encode(),
            ));
        }

        let add_packet = self.add_packet().map(|p| p.encode());
        for player in players {
            let wanted = mode_for(player.uuid);
            let has_bar = self.shown.entry(player.uuid).or_default();
            if wanted != PerfMode::Bar && *has_bar {
                player.send_packet(&CBossEvent::Remove { uuid: BAR_UUID }.encode());
                *has_bar = false;
            }
            match wanted {
                PerfMode::Off => {}
                PerfMode::ActionBar => {
                    if let Some((_, _, _, action)) = &update {
                        player.send_packet(action);
                    }
                }
                PerfMode::Bar if *has_bar => {
                    if let Some((title, progress, style, _)) = &update {
                        player.send_packet(title);
                        player.send_packet(progress);
                        if let Some(style) = style {
                            player.send_packet(style);
                        }
                    }
                }
                PerfMode::Bar => {
                    if let Some(add) = &add_packet {
                        // Remove first in case a previous plot's bar was not cleaned up.
                        player.send_packet(&CBossEvent::Remove { uuid: BAR_UUID }.encode());
                        player.send_packet(add);
                        *has_bar = true;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(rtps: f64, target: Tps) -> PerfSnapshot {
        PerfSnapshot {
            rtps,
            target,
            nspt: Some(Duration::from_nanos(1234)),
            behind: false,
            loop_hz: 20.0,
            loop_mspt: 1.5,
            entity_loop: Some((20.0, 0.25)),
            redpiler_flags: Some("ou".into()),
            entities: 2,
            players: 1,
            online_players: 5,
            loaded_plots: 3,
        }
    }

    fn text(snap: &PerfSnapshot) -> String {
        snap.title().extra.iter().map(|c| c.text.as_str()).collect()
    }

    #[test]
    fn health_thresholds() {
        assert_eq!(snap(20.0, Tps::Limited(20.0)).health(), Health::Good);
        assert_eq!(snap(19.0, Tps::Limited(20.0)).health(), Health::Good);
        assert_eq!(snap(16.0, Tps::Limited(20.0)).health(), Health::Warn);
        assert_eq!(snap(10.0, Tps::Limited(20.0)).health(), Health::Bad);
        assert_eq!(snap(1e6, Tps::Unlimited).health(), Health::Good);
        let mut s = snap(20.0, Tps::Limited(20.0));
        s.behind = true;
        assert_eq!(s.health(), Health::Bad);
    }

    #[test]
    fn progress_is_clamped_ratio() {
        assert_eq!(snap(10.0, Tps::Limited(20.0)).progress(), 0.5);
        assert_eq!(snap(30.0, Tps::Limited(20.0)).progress(), 1.0);
        assert_eq!(snap(5.0, Tps::Limited(0.0)).progress(), 0.0);
        assert_eq!(snap(5.0, Tps::Unlimited).progress(), 1.0);
    }

    #[test]
    fn title_text() {
        assert_eq!(
            text(&snap(19.96, Tps::Limited(20.0))),
            "RTPS 20.0/20 (1234 ns/t) | Loop 20 Hz 1.5 ms | Entities 2 (20.0 TPS 0.25 ms) | \
             RP on -ou | Players 1 | Online 5 | Plots 3"
        );
        let mut s = snap(3.0, Tps::Unlimited);
        s.redpiler_flags = None;
        s.nspt = None;
        s.entity_loop = None;
        let t = text(&s);
        assert!(t.starts_with("RTPS 3.0/unlimited | Loop"));
        assert!(t.contains("Entities 2 | RP off"));
    }

    #[test]
    fn flags() {
        let opts = CompilerOptions {
            optimize: true,
            update: true,
            ..Default::default()
        };
        assert_eq!(flag_letters(&opts), "ou");
        assert_eq!(flag_letters(&Default::default()), "");
    }

    #[test]
    fn sampler_rolling_average() {
        let mut s = Sampler::default();
        let t0 = Instant::now();
        let c = |ticks| Counters {
            redstone_ticks: ticks,
            ..Default::default()
        };
        assert!(!s.sample(t0, c(0)));
        for _ in 0..10 {
            s.record_update(Duration::from_millis(2));
        }
        assert!(!s.sample(t0 + Duration::from_millis(100), c(2)));
        assert!(s.sample(
            t0 + Duration::from_millis(500),
            Counters {
                redstone_ticks: 10,
                entity_ticks: 10,
                entity_busy: Duration::from_millis(5),
            }
        ));
        assert!((s.rtps.unwrap() - 20.0).abs() < 1e-6);
        assert!((s.loop_hz.unwrap() - 20.0).abs() < 1e-6);
        assert!((s.loop_mspt.unwrap() - 2.0).abs() < 1e-6);
        assert!((s.entity_tps.unwrap() - 20.0).abs() < 1e-6);
        assert!((s.entity_mspt.unwrap() - 0.5).abs() < 1e-6);
        // A slow window only moves the average part of the way.
        s.record_update(Duration::from_millis(4));
        assert!(s.sample(
            t0 + Duration::from_millis(1000),
            Counters {
                redstone_ticks: 10,
                entity_ticks: 10,
                entity_busy: Duration::from_millis(5),
            }
        ));
        assert!((s.rtps.unwrap() - 12.0).abs() < 1e-6);
        assert!((s.loop_mspt.unwrap() - 2.8).abs() < 1e-6);
    }

    #[test]
    fn prefs_default_on() {
        assert_eq!(mode_for(0xdead_beef_u128), PerfMode::Bar);
        set_mode(0xdead_beef_u128, PerfMode::Off);
        assert_eq!(mode_for(0xdead_beef_u128), PerfMode::Off);
    }
}
