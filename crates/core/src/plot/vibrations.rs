//! Vibrations that sculk sensors and calibrated sculk sensors listen to.
//!
//! Things that happen in the plot are reported to the world as game events (see
//! `World::game_event`). Every tick they are handed to the sensors that are close enough to hear
//! them. A sensor picks the closest vibration of a tick, the vibration travels at one block per
//! tick and when it arrives the sensor activates, see `mchprs_redstone::sculk` for what that does
//! to the block. This follows `VibrationSystem` of Minecraft.
//!
//! Sensors are found by `PlotWorld::set_block_raw` and by scanning the plot when it is loaded.
//! The sensors keep their own timers instead of using the scheduled ticks of the world, because
//! those are handed over to redpiler which knows nothing about sensors. Redpiler is reset
//! whenever a sensor changes its state for the same reason.

use super::Plot;
use crate::player::{Gamemode, PlayerPos};
use mchprs_blocks::blocks::Block;
use mchprs_blocks::{generic, BlockFace, BlockPos};
use mchprs_proc_macros::protocol_id;
use mchprs_redstone::sculk::{self, Phase};
use mchprs_world::game_event::{
    block_center, dampens_vibrations, occludes_vibrations, redstone_strength_for_distance,
    travel_time, EventContext, EventPos, GameEvent,
};
use mchprs_world::storage::Chunk;
use mchprs_world::World;
use rustc_hash::FxHashMap;

/// How many game events are kept until the next tick
const MAX_PENDING_EVENTS: usize = 4096;
/// The sound category of blocks
const SOUND_CATEGORY_BLOCKS: i32 = 4;
/// How far a player has to fall to be heard hitting the ground
const MIN_HEARD_FALL: f64 = 0.9;

pub(super) struct PendingEvent {
    event: GameEvent,
    pos: EventPos,
    context: EventContext,
}

impl PendingEvent {
    pub(super) fn new(event: GameEvent, pos: EventPos, context: EventContext) -> PendingEvent {
        PendingEvent {
            event,
            pos,
            context,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Vibration {
    event: GameEvent,
    /// The distance to the sensor in blocks
    distance: f32,
    origin: EventPos,
}

#[derive(Clone, Copy, Debug)]
struct Candidate {
    vibration: Vibration,
    tick: u64,
}

#[derive(Default, Debug)]
struct Sensor {
    /// The best vibration heard this tick, which the sensor starts to receive in the next one
    candidate: Option<Candidate>,
    /// The vibration that is on its way to the sensor, a sensor only receives one at a time
    current: Option<Vibration>,
    travel_ticks: u32,
    /// The ticks left until the phase of the sensor ends, or 0
    phase_ticks: u32,
}

/// What has to be done to a sensor block
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Activate {
        pos: BlockPos,
        power: u8,
        frequency: u8,
    },
    /// The time of the current phase is over
    Advance(BlockPos),
}

#[derive(Debug)]
struct Walker {
    was_on_ground: bool,
    /// The highest point since the player left the ground
    peak_y: f64,
    move_dist: f64,
    next_step: f64,
}

impl Default for Walker {
    fn default() -> Walker {
        Walker {
            was_on_ground: true,
            peak_y: 0.0,
            move_dist: 0.0,
            next_step: 1.0,
        }
    }
}

#[derive(Default)]
pub struct Vibrations {
    sensors: FxHashMap<BlockPos, Sensor>,
    /// The number of ticks the sensors have seen
    now: u64,
    walkers: FxHashMap<u128, Walker>,
}

fn sensor_id_at(world: &impl World, pos: BlockPos) -> Option<u32> {
    match world.get_block(pos) {
        Block::Generic(id) if sculk::is_sensor(id) => Some(id),
        _ => None,
    }
}

fn dist_sq(a: BlockPos, b: BlockPos) -> i64 {
    let dx = (a.x - b.x) as i64;
    let dy = (a.y - b.y) as i64;
    let dz = (a.z - b.z) as i64;
    dx * dx + dy * dy + dz * dz
}

fn floor_pos(pos: EventPos) -> BlockPos {
    BlockPos::new(
        pos.0.floor() as i32,
        pos.1.floor() as i32,
        pos.2.floor() as i32,
    )
}

fn distance(a: EventPos, b: EventPos) -> f32 {
    let (dx, dy, dz) = (a.0 - b.0, a.1 - b.1, a.2 - b.2);
    (dx * dx + dy * dy + dz * dz).sqrt() as f32
}

fn occludes(world: &impl World, pos: BlockPos) -> bool {
    matches!(world.get_block(pos), Block::Generic(id) if occludes_vibrations(generic::state_name(id)))
}

/// Whether a line from `from` to `to` passes through a block that stops vibrations. This walks
/// through the blocks the same way `BlockGetter.traverseBlocks` does.
fn line_blocked(world: &impl World, from: EventPos, to: EventPos) -> bool {
    if from == to {
        return false;
    }
    // The line is shortened slightly on both ends, exactly like in the game
    let lerp = |delta: f64, start: f64, end: f64| start + delta * (end - start);
    let shorten = |start: EventPos, end: EventPos| {
        (
            lerp(-1.0E-7, start.0, end.0),
            lerp(-1.0E-7, start.1, end.1),
            lerp(-1.0E-7, start.2, end.2),
        )
    };
    let (new_from, new_to) = (shorten(from, to), shorten(to, from));
    let (from, to) = (new_from, new_to);

    let mut block = floor_pos(from);
    if occludes(world, block) {
        return true;
    }

    let (dx, dy, dz) = (to.0 - from.0, to.1 - from.1, to.2 - from.2);
    let sign = |d: f64| {
        if d == 0.0 {
            0
        } else if d > 0.0 {
            1
        } else {
            -1
        }
    };
    let (sx, sy, sz) = (sign(dx), sign(dy), sign(dz));
    let delta = |sign: i32, d: f64| {
        if sign == 0 {
            f64::MAX
        } else {
            sign as f64 / d
        }
    };
    let (dt_x, dt_y, dt_z) = (delta(sx, dx), delta(sy, dy), delta(sz, dz));
    let frac = |x: f64| x - x.floor();
    let mut tx = dt_x
        * if sx > 0 {
            1.0 - frac(from.0)
        } else {
            frac(from.0)
        };
    let mut ty = dt_y
        * if sy > 0 {
            1.0 - frac(from.1)
        } else {
            frac(from.1)
        };
    let mut tz = dt_z
        * if sz > 0 {
            1.0 - frac(from.2)
        } else {
            frac(from.2)
        };

    while tx <= 1.0 || ty <= 1.0 || tz <= 1.0 {
        if tx < ty {
            if tx < tz {
                block.x += sx;
                tx += dt_x;
            } else {
                block.z += sz;
                tz += dt_z;
            }
        } else if ty < tz {
            block.y += sy;
            ty += dt_y;
        } else {
            block.z += sz;
            tz += dt_z;
        }
        if occludes(world, block) {
            return true;
        }
    }
    false
}

/// Whether the vibration from `origin` can't reach `dest` because wool is in the way. It is only
/// blocked if that is the case for lines from every side of the block of the origin.
fn is_occluded(world: &impl World, origin: EventPos, dest: EventPos) -> bool {
    let center = |pos: EventPos| {
        (
            pos.0.floor() + 0.5,
            pos.1.floor() + 0.5,
            pos.2.floor() + 0.5,
        )
    };
    let from = center(origin);
    let to = center(dest);
    const NUDGE: f64 = 1.0E-5;
    [
        (NUDGE, 0.0, 0.0),
        (-NUDGE, 0.0, 0.0),
        (0.0, NUDGE, 0.0),
        (0.0, -NUDGE, 0.0),
        (0.0, 0.0, NUDGE),
        (0.0, 0.0, -NUDGE),
    ]
    .into_iter()
    .all(|(x, y, z)| line_blocked(world, (from.0 + x, from.1 + y, from.2 + z), to))
}

impl Vibrations {
    pub fn is_empty(&self) -> bool {
        self.sensors.is_empty()
    }

    /// Starts listening with the sensor at the position, if there is one
    pub fn register(&mut self, world: &impl World, pos: BlockPos) {
        let Some(id) = sensor_id_at(world, pos) else {
            return;
        };
        self.sensors.entry(pos).or_insert_with(|| Sensor {
            // A sensor that was saved or pasted while active has to end up inactive again
            phase_ticks: match sculk::phase(id) {
                Phase::Inactive => 0,
                Phase::Active => sculk::active_ticks(id),
                Phase::Cooldown => sculk::COOLDOWN_TICKS,
            },
            ..Sensor::default()
        });
    }

    /// Hands a game event to the sensors that are in range of it
    pub fn hear(
        &mut self,
        world: &impl World,
        event: GameEvent,
        origin: EventPos,
        context: EventContext,
    ) {
        let frequency = event.frequency();
        if frequency == 0
            || context.spectator
            || context.dampened
            || (context.sneaking && event.ignored_when_sneaking())
        {
            return;
        }
        let event_block = floor_pos(origin);
        let now = self.now;
        self.sensors.retain(|&pos, sensor| {
            let Some(id) = sensor_id_at(world, pos) else {
                return false;
            };
            let radius = sculk::listener_radius(id) as i64;
            if dist_sq(pos, event_block) > radius * radius || sensor.current.is_some() {
                return true;
            }
            // The sensor doesn't hear the block it is in being placed or destroyed
            if pos == event_block
                && matches!(event, GameEvent::BlockDestroy | GameEvent::BlockPlace)
            {
                return true;
            }
            if !sculk::can_activate(id) {
                return true;
            }
            if sculk::is_calibrated(id) {
                let wanted = sculk::back_signal(world, pos, id);
                if wanted != 0 && wanted != frequency {
                    return true;
                }
            }
            let dest = block_center(pos);
            if is_occluded(world, origin, dest) {
                return true;
            }
            sensor.add_candidate(
                Vibration {
                    event,
                    distance: distance(origin, dest),
                    origin,
                },
                now,
            );
            true
        });
    }

    /// Makes the sensor at the position hear an event no matter how far away it is or what is in
    /// between, which is what happens when something steps on it
    pub fn force(&mut self, world: &impl World, pos: BlockPos, event: GameEvent, origin: EventPos) {
        let Some(id) = sensor_id_at(world, pos) else {
            return;
        };
        let Some(sensor) = self.sensors.get_mut(&pos) else {
            return;
        };
        if sensor.current.is_some() || !sculk::can_activate(id) {
            return;
        }
        if sculk::is_calibrated(id) {
            let wanted = sculk::back_signal(world, pos, id);
            if wanted != 0 && wanted != event.frequency() {
                return;
            }
        }
        sensor.add_candidate(
            Vibration {
                event,
                distance: distance(origin, block_center(pos)),
                origin,
            },
            self.now,
        );
    }

    /// Advances all sensors by a tick and returns what has to be done to their blocks
    fn tick(&mut self, world: &impl World) -> Vec<Action> {
        self.now += 1;
        let now = self.now;
        let mut actions = Vec::new();
        self.sensors.retain(|&pos, sensor| {
            let Some(id) = sensor_id_at(world, pos) else {
                return false;
            };
            let mut phase = sculk::phase(id);

            // This is the scheduled tick of the block, which comes before the vibrations
            if sensor.phase_ticks > 0 {
                sensor.phase_ticks -= 1;
                if sensor.phase_ticks == 0 {
                    match phase {
                        Phase::Active => {
                            actions.push(Action::Advance(pos));
                            phase = Phase::Cooldown;
                            sensor.phase_ticks = sculk::COOLDOWN_TICKS;
                        }
                        Phase::Cooldown => {
                            actions.push(Action::Advance(pos));
                            phase = Phase::Inactive;
                        }
                        Phase::Inactive => {}
                    }
                }
            } else if phase != Phase::Inactive {
                // Something else made the sensor active
                sensor.phase_ticks = match phase {
                    Phase::Active => sculk::active_ticks(id),
                    _ => sculk::COOLDOWN_TICKS,
                };
            }

            if sensor.current.is_none()
                && let Some(candidate) = sensor.candidate
                // The vibrations of this tick are heard in the next one
                && candidate.tick < now
            {
                sensor.current = Some(candidate.vibration);
                sensor.travel_ticks = travel_time(candidate.vibration.distance);
                sensor.candidate = None;
            }

            if let Some(vibration) = sensor.current {
                sensor.travel_ticks = sensor.travel_ticks.saturating_sub(1);
                if sensor.travel_ticks == 0 {
                    sensor.current = None;
                    if phase == Phase::Inactive {
                        let blocks_away =
                            (dist_sq(floor_pos(vibration.origin), pos) as f64).sqrt() as f32;
                        actions.push(Action::Activate {
                            pos,
                            power: redstone_strength_for_distance(
                                blocks_away,
                                sculk::listener_radius(id),
                            ),
                            frequency: vibration.event.frequency(),
                        });
                        sensor.phase_ticks = sculk::active_ticks(id);
                    }
                }
            }
            true
        });
        actions
    }
}

impl Sensor {
    /// Keeps the vibration if it is closer than the one that was heard this tick, or as close but
    /// of a higher frequency
    fn add_candidate(&mut self, vibration: Vibration, now: u64) {
        let replaces = match self.candidate {
            None => true,
            Some(previous) if previous.tick != now => false,
            Some(previous) => {
                vibration.distance < previous.vibration.distance
                    || (vibration.distance == previous.vibration.distance
                        && vibration.event.frequency() > previous.vibration.event.frequency())
            }
        };
        if replaces {
            self.candidate = Some(Candidate {
                vibration,
                tick: now,
            });
        }
    }
}

/// Finds the sensors in the chunks of a plot that was just loaded
pub(super) fn find_sensors(chunks: &[Chunk]) -> Vec<BlockPos> {
    let mut found = Vec::new();
    for chunk in chunks {
        for (section_idx, section) in chunk.sections.iter().enumerate() {
            // The palette is not used by sections with many different blocks
            let palette = section.palette();
            if !palette.is_empty() && !palette.iter().any(|&id| sculk::is_sensor(id)) {
                continue;
            }
            for y in 0..16 {
                for z in 0..16 {
                    for x in 0..16 {
                        if sculk::is_sensor(section.get_block(x, y, z)) {
                            found.push(BlockPos::new(
                                chunk.x * 16 + x as i32,
                                section_idx as i32 * 16 + y as i32,
                                chunk.z * 16 + z as i32,
                            ));
                        }
                    }
                }
            }
        }
    }
    found
}

impl Plot {
    /// Hands over what the world heard since the last time and registers new sensors
    pub(super) fn dispatch_game_events(&mut self) {
        for pos in std::mem::take(&mut self.world.new_sensors) {
            self.vibrations.register(&self.world, pos);
        }
        for event in std::mem::take(&mut self.world.pending_game_events) {
            self.vibrations
                .hear(&self.world, event.event, event.pos, event.context);
        }
        self.world.listening = !self.vibrations.is_empty();
    }

    /// Ticks the sensors. This is done before the redstone of the world is ticked.
    pub(super) fn tick_vibrations(&mut self) {
        self.dispatch_game_events();
        if self.vibrations.is_empty() {
            return;
        }
        let actions = self.vibrations.tick(&self.world);
        self.apply_sensor_actions(actions);
    }

    fn apply_sensor_actions(&mut self, actions: Vec<Action>) {
        if actions.is_empty() {
            return;
        }
        // Redpiler doesn't know about sensors, so the blocks it controls have to be handed back
        // to the world first
        self.reset_redpiler();

        for action in actions {
            match action {
                Action::Activate {
                    pos,
                    power,
                    frequency,
                } => {
                    let waterlogged = self.is_waterlogged(pos);
                    if !sculk::activate(&mut self.world, pos, power, frequency) {
                        continue;
                    }
                    if !waterlogged {
                        self.play_sensor_sound(
                            pos,
                            protocol_id!(
                                "minecraft:sound_event",
                                "minecraft:block.sculk_sensor.clicking"
                            ),
                        );
                    }
                    // Amethyst next to the sensor resonates, which calibrated sensors can hear
                    for face in BlockFace::values() {
                        let neighbor = pos.offset(face);
                        if self.world.get_block(neighbor).get_name() == "minecraft:amethyst_block" {
                            self.world.game_event(
                                GameEvent::Resonate(frequency),
                                block_center(neighbor),
                                EventContext::NONE,
                            );
                        }
                    }
                }
                Action::Advance(pos) => {
                    let waterlogged = self.is_waterlogged(pos);
                    if sculk::advance_phase(&mut self.world, pos) == Some(Phase::Inactive)
                        && !waterlogged
                    {
                        self.play_sensor_sound(
                            pos,
                            protocol_id!(
                                "minecraft:sound_event",
                                "minecraft:block.sculk_sensor.clicking_stop"
                            ),
                        );
                    }
                }
            }
        }
    }

    fn is_waterlogged(&self, pos: BlockPos) -> bool {
        matches!(
            self.world.get_block(pos),
            Block::Generic(id) if generic::get_property(id, "waterlogged") == Some("true")
        )
    }

    fn play_sensor_sound(&mut self, pos: BlockPos, sound_id: i32) {
        let pitch = 0.8 + rand::random::<f32>() * 0.2;
        self.world
            .play_sound(pos, sound_id, SOUND_CATEGORY_BLOCKS, 1.0, pitch);
    }

    /// Turns the movement of a player into steps, swimming and landing, which sensors can hear
    pub(super) fn track_player_vibrations(
        &mut self,
        player_idx: usize,
        old: PlayerPos,
        new: PlayerPos,
    ) {
        if !self.world.listening {
            self.vibrations.walkers.clear();
            return;
        }
        let player = &self.players[player_idx];
        if matches!(player.gamemode, Gamemode::Spectator) {
            return;
        }
        let on_ground = player.on_ground;
        let context = EventContext {
            sneaking: player.crouching,
            spectator: false,
            dampened: false,
        };
        let uuid = player.uuid;

        let feet_pos = new.block_pos();
        let feet = self.world.get_block(feet_pos);
        let block_on_pos = BlockPos::new(
            new.x.floor() as i32,
            (new.y - 1.0E-5).floor() as i32,
            new.z.floor() as i32,
        );
        let block_on = self.world.get_block(block_on_pos);
        // Walking on and in wool and carpet is silent
        let dampened =
            dampens_vibrations(block_on.get_name()) || dampens_vibrations(feet.get_name());
        let in_water = feet.get_name() == "minecraft:water";
        let horizontal = ((new.x - old.x).powi(2) + (new.z - old.z).powi(2)).sqrt();
        let origin = (new.x, new.y, new.z);

        let walker = self.vibrations.walkers.entry(uuid).or_default();
        let mut events = Vec::new();
        if on_ground {
            if !walker.was_on_ground && walker.peak_y - new.y >= MIN_HEARD_FALL {
                events.push(GameEvent::HitGround);
            }
            walker.peak_y = new.y;
            walker.move_dist += horizontal * 0.6;
            if walker.move_dist > walker.next_step {
                walker.next_step = walker.move_dist + 1.0;
                events.push(GameEvent::Step);
            }
        } else if in_water {
            walker.peak_y = new.y;
            walker.move_dist += horizontal * 0.6;
            if walker.move_dist > walker.next_step {
                walker.next_step = walker.move_dist + 1.0;
                events.push(GameEvent::Swim);
            }
        } else {
            if walker.was_on_ground {
                walker.peak_y = old.y;
            }
            walker.peak_y = walker.peak_y.max(new.y);
        }
        walker.was_on_ground = on_ground;

        if !dampened {
            for event in events {
                self.world.game_event(event, origin, context);
            }
        }
        // Walking on a sensor always activates it
        if on_ground && horizontal > 1.0E-3 && block_on.is_sculk_sensor() {
            self.vibrations
                .force(&self.world, block_on_pos, GameEvent::Step, origin);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mchprs_world::testing::TestWorld;

    const SENSOR: &str = "minecraft:sculk_sensor";
    const CALIBRATED: &str = "minecraft:calibrated_sculk_sensor";

    fn world() -> TestWorld {
        TestWorld::new(2, 1, 2)
    }

    fn place_sensor(world: &mut TestWorld, vibrations: &mut Vibrations, pos: BlockPos, name: &str) {
        let id = generic::default_state(name).unwrap();
        world.set_block(pos, Block::Generic(id));
        vibrations.register(world, pos);
    }

    fn place_named(world: &mut TestWorld, pos: BlockPos, name: &str) {
        world.set_block(pos, Block::Generic(generic::default_state(name).unwrap()));
    }

    /// Runs a tick and applies what happens to the sensors to the world, like the plot does
    fn tick(world: &mut TestWorld, vibrations: &mut Vibrations) -> Vec<Action> {
        let actions = vibrations.tick(world);
        for action in &actions {
            match *action {
                Action::Activate {
                    pos,
                    power,
                    frequency,
                } => {
                    sculk::activate(world, pos, power, frequency);
                }
                Action::Advance(pos) => {
                    sculk::advance_phase(world, pos);
                }
            }
        }
        actions
    }

    fn phase_at(world: &TestWorld, pos: BlockPos) -> Phase {
        sculk::phase(sensor_id_at(world, pos).unwrap())
    }

    fn power_at(world: &TestWorld, pos: BlockPos) -> u8 {
        sculk::power(sensor_id_at(world, pos).unwrap())
    }

    fn event_at(pos: BlockPos) -> EventPos {
        block_center(pos)
    }

    #[test]
    fn nearby_vibration_activates_after_a_tick() {
        let mut world = world();
        let mut vibrations = Vibrations::default();
        let sensor = BlockPos::new(8, 2, 8);
        place_sensor(&mut world, &mut vibrations, sensor, SENSOR);

        let source = BlockPos::new(9, 2, 8);
        vibrations.hear(
            &world,
            GameEvent::BlockActivate,
            event_at(source),
            EventContext::NONE,
        );
        // The vibration is heard in the tick after the event
        let actions = tick(&mut world, &mut vibrations);
        assert_eq!(
            actions,
            vec![Action::Activate {
                pos: sensor,
                power: 14,
                frequency: 10
            }]
        );
        assert_eq!(phase_at(&world, sensor), Phase::Active);
        assert_eq!(power_at(&world, sensor), 14);
    }

    #[test]
    fn vibrations_need_a_tick_per_block_to_arrive() {
        let mut world = world();
        let mut vibrations = Vibrations::default();
        let sensor = BlockPos::new(8, 2, 8);
        place_sensor(&mut world, &mut vibrations, sensor, SENSOR);

        // Five blocks away
        vibrations.hear(
            &world,
            GameEvent::BlockDestroy,
            event_at(BlockPos::new(13, 2, 8)),
            EventContext::NONE,
        );
        // The tick that picks the vibration up counts as one of the five
        for _ in 0..4 {
            assert!(tick(&mut world, &mut vibrations).is_empty());
            assert_eq!(phase_at(&world, sensor), Phase::Inactive);
        }
        let actions = tick(&mut world, &mut vibrations);
        assert_eq!(
            actions,
            vec![Action::Activate {
                pos: sensor,
                power: 6,
                frequency: 12
            }]
        );
    }

    #[test]
    fn phases_last_thirty_and_ten_ticks() {
        let mut world = world();
        let mut vibrations = Vibrations::default();
        let sensor = BlockPos::new(8, 2, 8);
        place_sensor(&mut world, &mut vibrations, sensor, SENSOR);
        vibrations.hear(
            &world,
            GameEvent::Explode,
            event_at(BlockPos::new(8, 2, 8)),
            EventContext::NONE,
        );
        // The sensor doesn't hear the explosion if it is in the exact same place, it is a block
        // event only that is ignored there
        tick(&mut world, &mut vibrations);
        assert_eq!(phase_at(&world, sensor), Phase::Active);

        for _ in 0..29 {
            tick(&mut world, &mut vibrations);
            assert_eq!(phase_at(&world, sensor), Phase::Active);
        }
        tick(&mut world, &mut vibrations);
        assert_eq!(phase_at(&world, sensor), Phase::Cooldown);
        assert_eq!(power_at(&world, sensor), 0);

        // Vibrations in the cooldown phase are not heard
        vibrations.hear(
            &world,
            GameEvent::Explode,
            event_at(BlockPos::new(9, 2, 8)),
            EventContext::NONE,
        );
        for _ in 0..9 {
            tick(&mut world, &mut vibrations);
            assert_eq!(phase_at(&world, sensor), Phase::Cooldown);
        }
        tick(&mut world, &mut vibrations);
        assert_eq!(phase_at(&world, sensor), Phase::Inactive);
        assert!(vibrations.sensors[&sensor].current.is_none());
    }

    #[test]
    fn calibrated_sensor_is_active_for_ten_ticks() {
        let mut world = world();
        let mut vibrations = Vibrations::default();
        let sensor = BlockPos::new(8, 2, 8);
        place_sensor(&mut world, &mut vibrations, sensor, CALIBRATED);
        vibrations.hear(
            &world,
            GameEvent::BlockPlace,
            event_at(BlockPos::new(9, 2, 8)),
            EventContext::NONE,
        );
        tick(&mut world, &mut vibrations);
        assert_eq!(phase_at(&world, sensor), Phase::Active);
        for _ in 0..9 {
            tick(&mut world, &mut vibrations);
            assert_eq!(phase_at(&world, sensor), Phase::Active);
        }
        tick(&mut world, &mut vibrations);
        assert_eq!(phase_at(&world, sensor), Phase::Cooldown);
    }

    #[test]
    fn range_is_eight_blocks_and_sixteen_for_calibrated() {
        let mut world = world();
        let mut vibrations = Vibrations::default();
        let normal = BlockPos::new(2, 2, 2);
        let calibrated = BlockPos::new(2, 2, 20);
        place_sensor(&mut world, &mut vibrations, normal, SENSOR);
        place_sensor(&mut world, &mut vibrations, calibrated, CALIBRATED);

        // Which of the sensors heard an explosion at the position
        let heard_by = |vibrations: &mut Vibrations, x: i32, y: i32, z: i32| {
            for sensor in vibrations.sensors.values_mut() {
                sensor.candidate = None;
            }
            vibrations.hear(
                &world,
                GameEvent::Explode,
                event_at(BlockPos::new(x, y, z)),
                EventContext::NONE,
            );
            (
                vibrations.sensors[&normal].candidate.is_some(),
                vibrations.sensors[&calibrated].candidate.is_some(),
            )
        };

        assert_eq!(heard_by(&mut vibrations, 10, 2, 2), (true, false));
        assert_eq!(heard_by(&mut vibrations, 11, 2, 2), (false, false));
        // The range is a sphere
        assert_eq!(heard_by(&mut vibrations, 7, 7, 4), (false, false));
        assert_eq!(heard_by(&mut vibrations, 6, 6, 4), (true, false));
        // The calibrated sensor is at the edge of its range of 16 blocks
        assert_eq!(heard_by(&mut vibrations, 18, 2, 20), (false, true));
        assert_eq!(heard_by(&mut vibrations, 19, 2, 20), (false, false));
    }

    #[test]
    fn block_events_in_the_sensor_itself_are_ignored() {
        let mut world = world();
        let mut vibrations = Vibrations::default();
        let sensor = BlockPos::new(8, 2, 8);
        place_sensor(&mut world, &mut vibrations, sensor, SENSOR);
        for event in [GameEvent::BlockPlace, GameEvent::BlockDestroy] {
            vibrations.hear(&world, event, event_at(sensor), EventContext::NONE);
            assert!(vibrations.sensors[&sensor].candidate.is_none());
        }
        vibrations.hear(
            &world,
            GameEvent::BlockActivate,
            event_at(sensor),
            EventContext::NONE,
        );
        assert!(vibrations.sensors[&sensor].candidate.is_some());
    }

    #[test]
    fn sneaking_and_dampened_events_are_not_heard() {
        let mut world = world();
        let mut vibrations = Vibrations::default();
        let sensor = BlockPos::new(8, 2, 8);
        place_sensor(&mut world, &mut vibrations, sensor, SENSOR);
        let near = event_at(BlockPos::new(9, 2, 8));

        let sneaking = EventContext {
            sneaking: true,
            ..EventContext::NONE
        };
        vibrations.hear(&world, GameEvent::Step, near, sneaking);
        assert!(vibrations.sensors[&sensor].candidate.is_none());
        // Sneaking doesn't silence everything
        vibrations.hear(&world, GameEvent::BlockPlace, near, sneaking);
        assert!(vibrations.sensors[&sensor].candidate.is_some());
        vibrations.sensors.get_mut(&sensor).unwrap().candidate = None;

        let dampened = EventContext {
            dampened: true,
            ..EventContext::NONE
        };
        vibrations.hear(&world, GameEvent::BlockPlace, near, dampened);
        assert!(vibrations.sensors[&sensor].candidate.is_none());

        let spectator = EventContext {
            spectator: true,
            ..EventContext::NONE
        };
        vibrations.hear(&world, GameEvent::BlockPlace, near, spectator);
        assert!(vibrations.sensors[&sensor].candidate.is_none());

        // The events that no sensor reacts to
        vibrations.hear(
            &world,
            GameEvent::SculkSensorTendrilsClicking,
            near,
            EventContext::NONE,
        );
        assert!(vibrations.sensors[&sensor].candidate.is_none());
    }

    #[test]
    fn wool_blocks_vibrations() {
        let mut world = world();
        let mut vibrations = Vibrations::default();
        let sensor = BlockPos::new(8, 2, 8);
        place_sensor(&mut world, &mut vibrations, sensor, SENSOR);
        let source = BlockPos::new(8, 2, 12);
        let between = BlockPos::new(8, 2, 10);
        assert!(!is_occluded(&world, event_at(source), event_at(sensor)));

        // A block of wool in the way is enough
        place_named(&mut world, between, "minecraft:white_wool");
        assert!(is_occluded(&world, event_at(source), event_at(sensor)));
        vibrations.hear(
            &world,
            GameEvent::BlockActivate,
            event_at(source),
            EventContext::NONE,
        );
        assert!(vibrations.sensors[&sensor].candidate.is_none());

        // Wool next to the line doesn't matter
        place_named(&mut world, between, "minecraft:air");
        place_named(&mut world, BlockPos::new(9, 2, 10), "minecraft:white_wool");
        assert!(!is_occluded(&world, event_at(source), event_at(sensor)));

        // Other blocks, even carpets and solid ones, let vibrations through
        place_named(&mut world, between, "minecraft:white_carpet");
        assert!(!is_occluded(&world, event_at(source), event_at(sensor)));
        place_named(&mut world, between, "minecraft:stone");
        assert!(!is_occluded(&world, event_at(source), event_at(sensor)));

        // A diagonal line is blocked too
        place_named(&mut world, between, "minecraft:air");
        let source = BlockPos::new(12, 4, 12);
        assert!(!is_occluded(&world, event_at(source), event_at(sensor)));
        place_named(&mut world, BlockPos::new(10, 3, 10), "minecraft:red_wool");
        assert!(is_occluded(&world, event_at(source), event_at(sensor)));
    }

    #[test]
    fn closest_vibration_wins_and_frequency_breaks_ties() {
        let mut world = world();
        let mut vibrations = Vibrations::default();
        let sensor = BlockPos::new(8, 2, 8);
        place_sensor(&mut world, &mut vibrations, sensor, SENSOR);

        vibrations.hear(
            &world,
            GameEvent::Explode,
            event_at(BlockPos::new(8, 2, 13)),
            EventContext::NONE,
        );
        vibrations.hear(
            &world,
            GameEvent::Step,
            event_at(BlockPos::new(8, 2, 11)),
            EventContext::NONE,
        );
        let chosen = vibrations.sensors[&sensor].candidate.unwrap().vibration;
        assert_eq!(chosen.event, GameEvent::Step);

        // Same distance, higher frequency
        vibrations.hear(
            &world,
            GameEvent::BlockPlace,
            event_at(BlockPos::new(11, 2, 8)),
            EventContext::NONE,
        );
        let chosen = vibrations.sensors[&sensor].candidate.unwrap().vibration;
        assert_eq!(chosen.event, GameEvent::BlockPlace);
        vibrations.hear(
            &world,
            GameEvent::Step,
            event_at(BlockPos::new(5, 2, 8)),
            EventContext::NONE,
        );
        let chosen = vibrations.sensors[&sensor].candidate.unwrap().vibration;
        assert_eq!(chosen.event, GameEvent::BlockPlace);

        // Only the vibration that is picked up counts, the others of the tick are lost
        tick(&mut world, &mut vibrations);
        assert_eq!(phase_at(&world, sensor), Phase::Inactive);
        for _ in 0..3 {
            tick(&mut world, &mut vibrations);
        }
        assert_eq!(phase_at(&world, sensor), Phase::Active);
        assert_eq!(power_at(&world, sensor), 10);
        let Some(mchprs_blocks::block_entities::BlockEntity::SculkSensor {
            last_vibration_frequency,
            ..
        }) = world.get_block_entity(sensor)
        else {
            panic!("the sensor has no block entity");
        };
        assert_eq!(*last_vibration_frequency, 13);
    }

    #[test]
    fn calibrated_sensor_only_hears_the_frequency_behind_it() {
        let mut world = world();
        let mut vibrations = Vibrations::default();
        let sensor = BlockPos::new(8, 2, 8);
        let id = generic::with_property(
            generic::default_state(CALIBRATED).unwrap(),
            "facing",
            "north",
        );
        world.set_block(sensor, Block::Generic(id));
        vibrations.register(&world, sensor);
        let near = event_at(BlockPos::new(8, 2, 6));

        // The sensor listens to everything without a signal
        vibrations.hear(&world, GameEvent::Step, near, EventContext::NONE);
        assert!(vibrations.sensors[&sensor].candidate.is_some());
        vibrations.sensors.get_mut(&sensor).unwrap().candidate = None;

        // A redstone block behind it is a signal of 15, which is the frequency of explosions
        world.set_block(sensor.offset(BlockFace::South), Block::RedstoneBlock);
        vibrations.hear(&world, GameEvent::Step, near, EventContext::NONE);
        assert!(vibrations.sensors[&sensor].candidate.is_none());
        vibrations.hear(&world, GameEvent::BlockPlace, near, EventContext::NONE);
        assert!(vibrations.sensors[&sensor].candidate.is_none());
        vibrations.hear(&world, GameEvent::Explode, near, EventContext::NONE);
        assert!(vibrations.sensors[&sensor].candidate.is_some());
    }

    #[test]
    fn forced_vibrations_ignore_range_and_occlusion() {
        let mut world = world();
        let mut vibrations = Vibrations::default();
        let sensor = BlockPos::new(8, 2, 8);
        place_sensor(&mut world, &mut vibrations, sensor, SENSOR);
        vibrations.force(&world, sensor, GameEvent::Step, (8.5, 2.5, 8.5));
        assert!(tick(&mut world, &mut vibrations).len() == 1);
        assert_eq!(phase_at(&world, sensor), Phase::Active);
        assert_eq!(power_at(&world, sensor), 15);
    }

    #[test]
    fn sensors_that_are_gone_are_forgotten() {
        let mut world = world();
        let mut vibrations = Vibrations::default();
        let sensor = BlockPos::new(8, 2, 8);
        place_sensor(&mut world, &mut vibrations, sensor, SENSOR);
        assert!(!vibrations.is_empty());
        world.set_block(sensor, Block::Air);
        tick(&mut world, &mut vibrations);
        assert!(vibrations.is_empty());
    }

    #[test]
    fn sensors_that_were_loaded_active_end_their_phase() {
        let mut world = world();
        let mut vibrations = Vibrations::default();
        let sensor = BlockPos::new(8, 2, 8);
        let id = generic::with_property(
            generic::default_state(SENSOR).unwrap(),
            "sculk_sensor_phase",
            "cooldown",
        );
        world.set_block(sensor, Block::Generic(id));
        vibrations.register(&world, sensor);
        for _ in 0..sculk::COOLDOWN_TICKS {
            assert_eq!(phase_at(&world, sensor), Phase::Cooldown);
            tick(&mut world, &mut vibrations);
        }
        assert_eq!(phase_at(&world, sensor), Phase::Inactive);
    }

    #[test]
    fn sensors_are_found_in_chunks() {
        let mut world = world();
        let pos = BlockPos::new(19, 5, 3);
        place_named(&mut world, pos, CALIBRATED);
        place_named(&mut world, BlockPos::new(1, 1, 1), "minecraft:sculk");
        for chunk in &mut world.chunks {
            chunk.flush();
        }
        assert_eq!(find_sensors(&world.chunks), vec![pos]);
    }
}
