//! The time of day and weather of a plot. Both run at 20 Hz independently of the redstone tick
//! rate and are only ever visible on the client, there is no server side light update.

use super::Plot;
use crate::player::{PacketSender, Player};
use mchprs_network::packets::clientbound::{
    CGameEvent, CGameEventType, ClientBoundPacket, ClockNetworkState, UpdateTime,
};
use mchprs_network::packets::PacketEncoder;
use mchprs_save_data::plot_data::{EnvironmentData, WeatherData};
use std::time::{Duration, Instant};

pub const DAY_TICKS: u64 = 24000;
const TICK_INTERVAL: Duration = Duration::from_millis(50);
const MAX_CATCH_UP_TICKS: u32 = 5;
/// How often the clock is resent so the clients do not drift away from it
const SYNC_INTERVAL_TICKS: u32 = 20;
/// The rain and thunder level change per tick, as in vanilla
const WEATHER_FADE_PER_TICK: f32 = 0.01;
/// The rain level above which it counts as raining, as in vanilla
const RAINING_THRESHOLD: f32 = 0.2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Clock {
    pub ticks: u64,
    pub frozen: bool,
}

impl Clock {
    pub fn tick(&mut self) {
        if !self.frozen {
            self.ticks += 1;
        }
    }

    pub fn daytime(&self) -> u64 {
        self.ticks % DAY_TICKS
    }

    pub fn day(&self) -> u64 {
        self.ticks / DAY_TICKS
    }

    /// Moves the clock to the given time of day, in the current day if it has not passed yet and
    /// in the next one otherwise (the same as the vanilla time markers).
    pub fn move_to_daytime(&mut self, daytime: u64) {
        let daytime = daytime % DAY_TICKS;
        let current = self.daytime();
        self.ticks += if daytime > current {
            daytime - current
        } else {
            DAY_TICKS + daytime - current
        };
    }

    /// Adds (or subtracts) ticks, the clock never goes below zero.
    pub fn add(&mut self, ticks: i64) {
        self.ticks = self.ticks.saturating_add_signed(ticks);
    }

    pub fn network_state(&self) -> ClockNetworkState {
        ClockNetworkState {
            total_ticks: self.ticks as i64,
            partial_tick: 0.0,
            rate: if self.frozen { 0.0 } else { 1.0 },
        }
    }
}

/// A time given to `/time set`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeSpec {
    /// A time of day with a named marker, which moves forward to the next occurrence
    Marker(u64),
    Ticks(u64),
}

impl TimeSpec {
    pub fn parse(arg: &str) -> Option<TimeSpec> {
        Some(match arg {
            "day" => TimeSpec::Marker(1000),
            "noon" => TimeSpec::Marker(6000),
            "night" => TimeSpec::Marker(13000),
            "midnight" => TimeSpec::Marker(18000),
            _ => TimeSpec::Ticks(parse_duration(arg)?),
        })
    }
}

/// Parses ticks with an optional unit like in vanilla: `t` (default) for ticks, `s` for seconds
/// and `d` for days.
pub fn parse_duration(arg: &str) -> Option<u64> {
    let (number, unit) = match arg.chars().last()? {
        't' => (&arg[..arg.len() - 1], 1.0),
        's' => (&arg[..arg.len() - 1], 20.0),
        'd' => (&arg[..arg.len() - 1], DAY_TICKS as f64),
        _ => (arg, 1.0),
    };
    let value: f64 = number.parse().ok()?;
    if !value.is_finite() || value < 0.0 || value * unit > u32::MAX as f64 {
        return None;
    }
    Some((value * unit).round() as u64)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WeatherKind {
    Clear,
    Rain,
    Thunder,
}

/// What a weather tick changed, to be sent to the clients.
#[derive(Default, Debug, PartialEq)]
pub struct WeatherUpdate {
    pub rain_level: Option<f32>,
    pub thunder_level: Option<f32>,
    /// Some(true) if it started raining, Some(false) if it stopped
    pub raining_changed: Option<bool>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Weather {
    pub raining: bool,
    pub thundering: bool,
    /// Ticks until the weather clears, 0 if it lasts until changed
    pub ticks_left: u32,
    pub rain_level: f32,
    pub thunder_level: f32,
}

impl Weather {
    /// Starts with the levels already at their targets, so there is no fade after loading.
    pub fn from_data(data: WeatherData) -> Weather {
        Weather {
            raining: data.raining,
            thundering: data.thundering,
            ticks_left: data.ticks_left,
            rain_level: data.raining as u8 as f32,
            thunder_level: data.thundering as u8 as f32,
        }
    }

    pub fn data(&self) -> WeatherData {
        WeatherData {
            raining: self.raining,
            thundering: self.thundering,
            ticks_left: self.ticks_left,
        }
    }

    pub fn is_raining(&self) -> bool {
        self.rain_level > RAINING_THRESHOLD
    }

    /// Sets the target weather. A duration of `None` lasts until it is changed again.
    pub fn set(&mut self, kind: WeatherKind, duration: Option<u32>) {
        self.raining = kind != WeatherKind::Clear;
        self.thundering = kind == WeatherKind::Thunder;
        // The timer only ends rain, a clear sky has no use for it
        self.ticks_left = if self.raining {
            duration.unwrap_or(0)
        } else {
            0
        };
    }

    pub fn tick(&mut self) -> WeatherUpdate {
        if self.ticks_left > 0 {
            self.ticks_left -= 1;
            if self.ticks_left == 0 {
                self.raining = false;
                self.thundering = false;
            }
        }

        let was_raining = self.is_raining();
        let old_rain = self.rain_level;
        let old_thunder = self.thunder_level;
        self.thunder_level = fade(self.thunder_level, self.thundering);
        self.rain_level = fade(self.rain_level, self.raining);

        let mut update = WeatherUpdate::default();
        if old_rain != self.rain_level {
            update.rain_level = Some(self.rain_level);
        }
        if old_thunder != self.thunder_level {
            update.thunder_level = Some(self.thunder_level);
        }
        if was_raining != self.is_raining() {
            update.raining_changed = Some(!was_raining);
            // Like vanilla, resend both levels together with the rain state
            update.rain_level = Some(self.rain_level);
            update.thunder_level = Some(self.thunder_level);
        }
        update
    }

    /// The packets that bring a newly joined client to the current weather.
    pub fn sync_packets(&self) -> Vec<PacketEncoder> {
        let mut packets = Vec::new();
        if self.is_raining() {
            packets.push(game_event(CGameEventType::StartRaining, 0.0));
        }
        packets.push(game_event(CGameEventType::RainLevelChange, self.rain_level));
        packets.push(game_event(
            CGameEventType::ThunderLevelChange,
            self.thunder_level,
        ));
        packets
    }
}

impl WeatherUpdate {
    pub fn packets(&self) -> Vec<PacketEncoder> {
        let mut packets = Vec::new();
        if let Some(raining) = self.raining_changed {
            let reason = if raining {
                CGameEventType::StartRaining
            } else {
                CGameEventType::StopRaining
            };
            packets.push(game_event(reason, 0.0));
        }
        if let Some(level) = self.rain_level {
            packets.push(game_event(CGameEventType::RainLevelChange, level));
        }
        if let Some(level) = self.thunder_level {
            packets.push(game_event(CGameEventType::ThunderLevelChange, level));
        }
        packets
    }
}

fn fade(level: f32, active: bool) -> f32 {
    let step = if active {
        WEATHER_FADE_PER_TICK
    } else {
        -WEATHER_FADE_PER_TICK
    };
    (level + step).clamp(0.0, 1.0)
}

fn game_event(reason: CGameEventType, value: f32) -> PacketEncoder {
    CGameEvent { reason, value }.encode()
}

pub struct Environment {
    pub clock: Clock,
    pub weather: Weather,
    /// Ticks since the plot was loaded, sent as world age
    game_time: i64,
    last_tick: Option<Instant>,
    ticks_since_sync: u32,
}

impl Environment {
    pub fn from_data(data: EnvironmentData) -> Environment {
        Environment {
            clock: Clock {
                ticks: data.clock_ticks,
                frozen: data.clock_frozen,
            },
            weather: Weather::from_data(data.weather),
            game_time: 0,
            last_tick: None,
            ticks_since_sync: 0,
        }
    }

    pub fn data(&self) -> EnvironmentData {
        EnvironmentData {
            clock_ticks: self.clock.ticks,
            clock_frozen: self.clock.frozen,
            weather: self.weather.data(),
        }
    }

    pub fn time_packet(&self) -> PacketEncoder {
        UpdateTime {
            world_age: self.game_time,
            overworld_clock: Some(self.clock.network_state()),
        }
        .encode()
    }

    /// Resend the clock on the next tick
    pub fn mark_clock_dirty(&mut self) {
        self.ticks_since_sync = SYNC_INTERVAL_TICKS;
    }

    /// Forget that time passed, called when the plot has been empty
    pub fn pause(&mut self) {
        self.last_tick = None;
    }

    /// Advances by the number of 20 Hz ticks that elapsed. Returns the packets for the clients.
    pub fn tick(&mut self, now: Instant) -> Vec<PacketEncoder> {
        let mut last_tick = *self.last_tick.get_or_insert(now);
        let mut packets = Vec::new();
        let mut ticks = 0;
        while last_tick + TICK_INTERVAL <= now && ticks < MAX_CATCH_UP_TICKS {
            last_tick += TICK_INTERVAL;
            ticks += 1;
            self.game_time += 1;
            self.clock.tick();
            packets.extend(self.weather.tick().packets());
            self.ticks_since_sync += 1;
        }
        self.last_tick = Some(if ticks == MAX_CATCH_UP_TICKS {
            now
        } else {
            last_tick
        });
        if self.ticks_since_sync >= SYNC_INTERVAL_TICKS {
            self.ticks_since_sync = 0;
            packets.push(self.time_packet());
        }
        packets
    }

    /// Sends the time and weather to a player that just entered the plot.
    pub fn sync_player(&self, player: &Player) {
        player.send_packet(&self.time_packet());
        for packet in self.weather.sync_packets() {
            player.send_packet(&packet);
        }
    }
}

impl Plot {
    pub(super) fn tick_environment(&mut self) {
        let packets = self.environment.tick(Instant::now());
        for packet in &packets {
            self.broadcast(packet);
        }
    }

    /// Time and weather are plot settings, so they follow the same rules as editing the plot.
    fn can_change_environment(&self, player: usize) -> bool {
        let player = &self.players[player];
        let allowed = match self.owner {
            Some(owner) => {
                owner == player.uuid || player.has_permission("plots.admin.interact.other")
            }
            None => player.has_permission("plots.admin.interact.unowned"),
        };
        if !allowed {
            player.send_no_permission_message();
        }
        allowed
    }

    pub(super) fn handle_time_command(&mut self, player: usize, args: &[&str]) {
        const USAGE: &str = "Usage: /time <set <day|noon|night|midnight|ticks> | add <ticks> | query <daytime|gametime|day> | freeze | unfreeze>";
        let clock = self.environment.clock;
        match args {
            ["query", what] => {
                let message = match *what {
                    "daytime" => format!("The time is {}", clock.daytime()),
                    "gametime" => format!("The game time is {}", clock.ticks),
                    "day" => format!("The day is {}", clock.day()),
                    _ => return self.players[player].send_error_message(USAGE),
                };
                self.players[player].send_system_message(&message);
                return;
            }
            ["set" | "add" | "freeze" | "unfreeze", ..] if !self.can_change_environment(player) => {
                return;
            }
            ["set", time] => match TimeSpec::parse(time) {
                Some(TimeSpec::Marker(daytime)) => self.environment.clock.move_to_daytime(daytime),
                Some(TimeSpec::Ticks(ticks)) => self.environment.clock.ticks = ticks,
                None => {
                    return self.players[player].send_error_message("Unable to parse the time!")
                }
            },
            ["add", ticks] => match ticks.parse::<i32>() {
                Ok(ticks) => self.environment.clock.add(ticks.into()),
                Err(_) => return self.players[player].send_error_message("Unable to parse ticks!"),
            },
            ["freeze"] => self.environment.clock.frozen = true,
            ["unfreeze"] => self.environment.clock.frozen = false,
            _ => return self.players[player].send_error_message(USAGE),
        }
        self.environment.mark_clock_dirty();
        let clock = self.environment.clock;
        let state = if clock.frozen { "frozen" } else { "running" };
        self.players[player].send_system_message(&format!(
            "The time is now {} (day {}, {})",
            clock.daytime(),
            clock.day(),
            state
        ));
    }

    pub(super) fn handle_weather_command(&mut self, player: usize, args: &[&str]) {
        const USAGE: &str = "Usage: /weather <clear|rain|thunder> [duration]";
        let (kind, duration) = match args {
            [kind, rest @ ..] if rest.len() <= 1 => {
                let kind = match *kind {
                    "clear" => WeatherKind::Clear,
                    "rain" => WeatherKind::Rain,
                    "thunder" => WeatherKind::Thunder,
                    _ => return self.players[player].send_error_message(USAGE),
                };
                let duration = match rest {
                    [duration] => match parse_duration(duration) {
                        // A duration only makes sense when it is at least one tick
                        Some(ticks @ 1..) => Some(ticks as u32),
                        _ => {
                            return self.players[player]
                                .send_error_message("Unable to parse the duration!")
                        }
                    },
                    _ => None,
                };
                (kind, duration)
            }
            _ => return self.players[player].send_error_message(USAGE),
        };
        if !self.can_change_environment(player) {
            return;
        }
        self.environment.weather.set(kind, duration);
        let message = match kind {
            WeatherKind::Clear => "Set the weather to clear",
            WeatherKind::Rain => "Set the weather to rain",
            WeatherKind::Thunder => "Set the weather to thunder",
        };
        self.players[player].send_system_message(message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clock(ticks: u64) -> Clock {
        Clock {
            ticks,
            frozen: false,
        }
    }

    #[test]
    fn clock_advances_and_wraps_day() {
        let mut c = clock(DAY_TICKS - 1);
        assert_eq!((c.day(), c.daytime()), (0, DAY_TICKS - 1));
        c.tick();
        assert_eq!((c.day(), c.daytime()), (1, 0));
    }

    #[test]
    fn frozen_clock_does_not_advance() {
        let mut c = Clock {
            ticks: 5,
            frozen: true,
        };
        c.tick();
        assert_eq!(c.ticks, 5);
        assert_eq!(c.network_state().rate, 0.0);
        assert_eq!(clock(5).network_state().rate, 1.0);
    }

    #[test]
    fn add_never_goes_negative() {
        let mut c = clock(100);
        c.add(-500);
        assert_eq!(c.ticks, 0);
        c.add(24000);
        assert_eq!(c.ticks, 24000);
    }

    #[test]
    fn named_times_move_forward() {
        let mut c = clock(0);
        c.move_to_daytime(6000);
        assert_eq!(c.ticks, 6000);
        // Already at noon: next noon is a day later
        c.move_to_daytime(6000);
        assert_eq!(c.ticks, DAY_TICKS + 6000);
        c.move_to_daytime(1000);
        assert_eq!(c.ticks, 2 * DAY_TICKS + 1000);
    }

    #[test]
    fn parses_time_specs() {
        assert_eq!(TimeSpec::parse("noon"), Some(TimeSpec::Marker(6000)));
        assert_eq!(TimeSpec::parse("midnight"), Some(TimeSpec::Marker(18000)));
        assert_eq!(TimeSpec::parse("night"), Some(TimeSpec::Marker(13000)));
        assert_eq!(TimeSpec::parse("day"), Some(TimeSpec::Marker(1000)));
        assert_eq!(TimeSpec::parse("1200"), Some(TimeSpec::Ticks(1200)));
        assert_eq!(TimeSpec::parse("10s"), Some(TimeSpec::Ticks(200)));
        assert_eq!(TimeSpec::parse("1d"), Some(TimeSpec::Ticks(24000)));
        assert_eq!(TimeSpec::parse("0.5d"), Some(TimeSpec::Ticks(12000)));
        assert_eq!(TimeSpec::parse("-3"), None);
        assert_eq!(TimeSpec::parse("abc"), None);
        assert_eq!(TimeSpec::parse(""), None);
    }

    #[test]
    fn rain_fades_in_and_out() {
        let mut w = Weather::from_data(WeatherData::default());
        w.set(WeatherKind::Rain, None);
        let mut started = None;
        for i in 1..=110 {
            let update = w.tick();
            if update.raining_changed == Some(true) {
                started = Some(i);
            }
        }
        // 0.01 per tick, the f32 sum passes 0.2 after 20 or 21 ticks
        assert!(matches!(started, Some(20 | 21)));
        assert!((w.rain_level - 1.0).abs() < 1e-4);
        assert_eq!(w.thunder_level, 0.0);
        assert!(w.tick() == WeatherUpdate::default());

        w.set(WeatherKind::Clear, None);
        let update = w.tick();
        assert!(update.rain_level.unwrap() < 1.0);
        for _ in 0..100 {
            w.tick();
        }
        assert_eq!(w.rain_level, 0.0);
        assert!(!w.is_raining());
    }

    #[test]
    fn weather_duration_ends_rain() {
        let mut w = Weather::from_data(WeatherData::default());
        w.set(WeatherKind::Thunder, Some(3));
        w.tick();
        w.tick();
        assert!(w.raining && w.thundering);
        w.tick();
        assert!(!w.raining && !w.thundering);
        assert_eq!(w.data(), WeatherData::default());
    }

    #[test]
    fn loaded_weather_has_no_fade() {
        let w = Weather::from_data(WeatherData {
            raining: true,
            thundering: true,
            ticks_left: 10,
        });
        assert_eq!((w.rain_level, w.thunder_level), (1.0, 1.0));
        assert!(w.is_raining());
        // start raining, rain level, thunder level
        assert_eq!(w.sync_packets().len(), 3);
    }

    #[test]
    fn environment_ticks_at_20hz() {
        let mut env = Environment::from_data(EnvironmentData {
            clock_ticks: 0,
            clock_frozen: false,
            weather: WeatherData::default(),
        });
        let start = Instant::now();
        env.tick(start);
        // First call syncs nothing yet, 3 ticks later the clock is at 3
        env.tick(start + TICK_INTERVAL * 3);
        assert_eq!(env.clock.ticks, 3);
        // Too far behind: capped to the catch up limit
        env.tick(start + Duration::from_secs(60));
        assert_eq!(env.clock.ticks, 3 + MAX_CATCH_UP_TICKS as u64);
    }
}
