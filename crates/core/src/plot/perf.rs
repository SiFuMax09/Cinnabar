//! Per-player performance display (boss bar or action bar) and the cheap statistics behind it.
//!
//! Statistics are sampled in the plot thread once per `SAMPLE_INTERVAL` from counters the plot
//! already maintains, so no extra work happens per tick or per packet.

use crate::player::{PacketSender, Player};
use cinnabar_network::packets::clientbound::{
    BossBarColor, BossBarOverlay, CBossEvent, CSystemChatMessage, ClientBoundPacket,
};
use cinnabar_redpiler::CompilerOptions;
use cinnabar_save_data::plot_data::Tps;
use cinnabar_text::{ColorCode, TextComponent};
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
    /// A compact bar: `TPS: 20.00 MSPT: 2.62 Ping: 0ms`
    #[default]
    Bar,
    /// A bar with all details
    Full,
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

#[derive(Clone)]
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

/// A redstone tick is two game ticks, like in vanilla Minecraft. The compact display uses game
/// ticks so that 20 TPS is the normal speed, as everyone expects.
const GAME_TICKS_PER_REDSTONE_TICK: f64 = 2.0;

/// How long a game tick may take at most: the time that the target tick rate leaves
fn tick_budget_ms(target: Tps) -> f64 {
    match target {
        Tps::Limited(target) if target > 0.0 => {
            1000.0 / (f64::from(target) * GAME_TICKS_PER_REDSTONE_TICK)
        }
        _ => 50.0,
    }
}

fn ping_color(ping_ms: u32) -> ColorCode {
    if ping_ms < 100 {
        ColorCode::Green
    } else if ping_ms < 250 {
        ColorCode::Gold
    } else {
        ColorCode::Red
    }
}

impl PerfSnapshot {
    /// Milliseconds a game tick takes, which is half of a redstone tick
    fn mspt(&self) -> f64 {
        self.nspt.map_or(0.0, |nspt| {
            nspt.as_nanos() as f64 / 1_000_000.0 / GAME_TICKS_PER_REDSTONE_TICK
        })
    }

    /// How much of the time budget of a tick is used
    fn budget_used(&self) -> f64 {
        self.mspt() / tick_budget_ms(self.target)
    }

    fn tps_color(&self) -> ColorCode {
        match self.target {
            Tps::Limited(target) if target > 0.0 => {
                let ratio = self.rtps / f64::from(target);
                if ratio >= 0.95 {
                    ColorCode::Green
                } else if ratio >= 0.75 {
                    ColorCode::Gold
                } else {
                    ColorCode::Red
                }
            }
            _ => ColorCode::Green,
        }
    }

    fn mspt_color(&self) -> ColorCode {
        let used = self.budget_used();
        if used < 0.5 {
            ColorCode::Green
        } else if used < 1.0 {
            ColorCode::Gold
        } else {
            ColorCode::Red
        }
    }

    /// The bar fills up with the share of the tick budget that is used, and its colour is the
    /// worst of the tick rate and the tick time.
    fn compact_progress(&self) -> f32 {
        self.budget_used().clamp(0.0, 1.0) as f32
    }

    fn compact_color(&self) -> BossBarColor {
        let rank = |code: ColorCode| match code {
            ColorCode::Green => 0,
            ColorCode::Gold => 1,
            _ => 2,
        };
        match rank(self.tps_color()).max(rank(self.mspt_color())) {
            0 => BossBarColor::Green,
            1 => BossBarColor::Yellow,
            _ => BossBarColor::Red,
        }
    }

    /// `TPS: 20.00 RTPS: 10.00 MSPT: 2.62 Ping: 0ms`
    ///
    /// TPS counts game ticks (20 is the normal speed) and RTPS redstone ticks, which are two game
    /// ticks long.
    pub fn compact_title(&self, ping_ms: u32) -> TextComponent {
        let rate = |rtps: f64| {
            if rtps >= 1000.0 {
                format!("{rtps:.0}")
            } else {
                format!("{rtps:.2}")
            }
        };
        TextComponent {
            extra: vec![
                seg("TPS: ", ColorCode::Gray),
                seg(
                    &rate(self.rtps * GAME_TICKS_PER_REDSTONE_TICK),
                    self.tps_color(),
                ),
                seg(" RTPS: ", ColorCode::Gray),
                seg(&rate(self.rtps), self.tps_color()),
                seg(" MSPT: ", ColorCode::Gray),
                seg(&format!("{:.2}", self.mspt()), self.mspt_color()),
                seg(" Ping: ", ColorCode::Gray),
                seg(&format!("{ping_ms}ms"), ping_color(ping_ms)),
            ],
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

/// The bar a player currently has on their screen
#[derive(Clone, Copy, PartialEq)]
struct ShownBar {
    color: BossBarColor,
    overlay: BossBarOverlay,
}

/// Which players currently have a bar on their screen, so we never add one twice or leave one
/// behind (the client throws on a duplicate id).
#[derive(Default)]
pub struct PerfDisplay {
    pub sampler: Sampler,
    shown: FxHashMap<u128, ShownBar>,
    last: Option<PerfSnapshot>,
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

    /// Removes the bar when a player leaves the plot. Disconnected players use `forget`.
    pub fn remove_player(&mut self, player: &Player) {
        if self.shown.remove(&player.uuid).is_some() {
            player.send_packet(&CBossEvent::Remove { uuid: BAR_UUID }.encode());
        }
    }

    pub fn forget(&mut self, uuid: u128) {
        self.shown.remove(&uuid);
    }

    /// The title, progress, colour and style of the bar of a player
    fn bar_for(
        snapshot: &PerfSnapshot,
        mode: PerfMode,
        ping_ms: u32,
    ) -> (TextComponent, f32, ShownBar) {
        match mode {
            PerfMode::Full => (
                snapshot.title(),
                snapshot.progress(),
                ShownBar {
                    color: snapshot.health().bar_color(),
                    overlay: BossBarOverlay::Progress,
                },
            ),
            _ => (
                snapshot.compact_title(ping_ms),
                snapshot.compact_progress(),
                ShownBar {
                    color: snapshot.compact_color(),
                    overlay: BossBarOverlay::Notched20,
                },
            ),
        }
    }

    /// Reconciles every player with their preference and, if `snapshot` is given, pushes the
    /// new numbers.
    pub fn refresh(&mut self, players: &[Player], snapshot: Option<PerfSnapshot>) {
        self.dirty = false;
        if let Some(snapshot) = snapshot {
            self.last = Some(snapshot);
        }

        for player in players {
            let mode = mode_for(player.uuid);
            let wants_bar = matches!(mode, PerfMode::Bar | PerfMode::Full);
            if !wants_bar {
                if self.shown.remove(&player.uuid).is_some() {
                    player.send_packet(&CBossEvent::Remove { uuid: BAR_UUID }.encode());
                }
            }
            let Some(snapshot) = &self.last else {
                continue;
            };
            let (title, progress, style) = Self::bar_for(snapshot, mode, player.ping_ms);
            match mode {
                PerfMode::Off => {}
                PerfMode::ActionBar => {
                    player.send_packet(
                        &CSystemChatMessage {
                            content: snapshot.compact_title(player.ping_ms),
                            overlay: true,
                        }
                        .encode(),
                    );
                }
                PerfMode::Bar | PerfMode::Full => match self.shown.get(&player.uuid).copied() {
                    Some(shown) => {
                        player.send_packet(
                            &CBossEvent::UpdateTitle {
                                uuid: BAR_UUID,
                                title,
                            }
                            .encode(),
                        );
                        player.send_packet(
                            &CBossEvent::UpdateProgress {
                                uuid: BAR_UUID,
                                progress,
                            }
                            .encode(),
                        );
                        if shown != style {
                            player.send_packet(
                                &CBossEvent::UpdateStyle {
                                    uuid: BAR_UUID,
                                    color: style.color,
                                    overlay: style.overlay,
                                }
                                .encode(),
                            );
                            self.shown.insert(player.uuid, style);
                        }
                    }
                    None => {
                        // Remove first in case a previous plot's bar was not cleaned up.
                        player.send_packet(&CBossEvent::Remove { uuid: BAR_UUID }.encode());
                        player.send_packet(
                            &CBossEvent::Add {
                                uuid: BAR_UUID,
                                title,
                                progress,
                                color: style.color,
                                overlay: style.overlay,
                            }
                            .encode(),
                        );
                        self.shown.insert(player.uuid, style);
                    }
                },
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

    /// A snapshot of a plot with the default redstone speed of 10 ticks per second, with a game
    /// tick taking `mspt` milliseconds
    fn compact(rtps: f64, mspt: f64) -> PerfSnapshot {
        PerfSnapshot {
            nspt: Some(Duration::from_nanos((mspt * 2.0 * 1_000_000.0) as u64)),
            ..snap(rtps, Tps::Limited(10.0))
        }
    }

    fn plain(title: &TextComponent) -> String {
        title.extra.iter().map(|part| part.text.as_str()).collect()
    }

    #[test]
    fn compact_title_matches_the_design() {
        assert_eq!(
            plain(&compact(10.0, 2.62).compact_title(0)),
            "TPS: 20.00 RTPS: 10.00 MSPT: 2.62 Ping: 0ms"
        );
    }

    #[test]
    fn compact_bar_shows_the_tick_budget() {
        let good = compact(10.0, 2.62);
        assert!(good.compact_progress() < 0.1);
        assert_eq!(good.compact_color(), BossBarColor::Green);

        let busy = compact(10.0, 29.28);
        assert!((busy.compact_progress() - 0.5856).abs() < 0.001);
        assert_eq!(busy.compact_color(), BossBarColor::Yellow);

        let lagging = compact(8.71, 57.8);
        assert_eq!(lagging.compact_progress(), 1.0);
        assert_eq!(lagging.compact_color(), BossBarColor::Red);
    }

    #[test]
    fn compact_value_colors() {
        let snap = compact(8.71, 57.8);
        assert!(matches!(snap.tps_color(), ColorCode::Gold));
        assert!(matches!(snap.mspt_color(), ColorCode::Red));
        assert!(matches!(compact(10.0, 29.28).mspt_color(), ColorCode::Gold));
        assert!(matches!(ping_color(0), ColorCode::Green));
        assert!(matches!(ping_color(180), ColorCode::Gold));
        assert!(matches!(ping_color(400), ColorCode::Red));
    }
}
