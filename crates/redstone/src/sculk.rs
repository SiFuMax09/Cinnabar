//! The block side of sculk sensors and calibrated sculk sensors: the phases they go through, the
//! redstone signal they output and what a comparator reads from them. Listening for vibrations
//! and the timing of the phases is up to the plot, see `vibrations` in `mchprs_core`.

use mchprs_blocks::block_entities::BlockEntity;
use mchprs_blocks::blocks::Block;
use mchprs_blocks::{generic, BlockFace, BlockPos};
use mchprs_world::World;

/// How long a sensor stays in the cooldown phase
pub const COOLDOWN_TICKS: u32 = 10;
const ACTIVE_TICKS: u32 = 30;
const CALIBRATED_ACTIVE_TICKS: u32 = 10;
const LISTENER_RADIUS: u32 = 8;
const CALIBRATED_LISTENER_RADIUS: u32 = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Inactive,
    Active,
    Cooldown,
}

pub fn is_sensor(id: u32) -> bool {
    generic::is_sculk_sensor(id)
}

pub fn is_calibrated(id: u32) -> bool {
    generic::is_calibrated_sculk_sensor(id)
}

/// How many ticks a sensor stays active after it received a vibration
pub fn active_ticks(id: u32) -> u32 {
    if is_calibrated(id) {
        CALIBRATED_ACTIVE_TICKS
    } else {
        ACTIVE_TICKS
    }
}

/// How far away a sensor can hear vibrations
pub fn listener_radius(id: u32) -> u32 {
    if is_calibrated(id) {
        CALIBRATED_LISTENER_RADIUS
    } else {
        LISTENER_RADIUS
    }
}

pub fn phase(id: u32) -> Phase {
    match generic::get_property(id, "sculk_sensor_phase") {
        Some("active") => Phase::Active,
        Some("cooldown") => Phase::Cooldown,
        _ => Phase::Inactive,
    }
}

pub fn can_activate(id: u32) -> bool {
    phase(id) == Phase::Inactive
}

pub fn power(id: u32) -> u8 {
    generic::get_property(id, "power")
        .and_then(|power| power.parse().ok())
        .unwrap_or(0)
}

/// The direction a calibrated sensor faces, which is away from the side it takes its input from
pub fn facing(id: u32) -> Option<BlockFace> {
    Some(match generic::get_property(id, "facing")? {
        "north" => BlockFace::North,
        "south" => BlockFace::South,
        "west" => BlockFace::West,
        "east" => BlockFace::East,
        _ => return None,
    })
}

/// The redstone power the sensor outputs to the block on the `side` of the block that asks.
/// `side` is the direction from the asking block towards the sensor.
pub fn weak_power(id: u32, side: BlockFace) -> u8 {
    if phase(id) != Phase::Active || facing(id) == Some(side) {
        return 0;
    }
    power(id)
}

/// The sensor powers the block below it strongly, but no others
pub fn strong_power(id: u32, side: BlockFace) -> u8 {
    if side == BlockFace::Top {
        weak_power(id, side)
    } else {
        0
    }
}

/// The signal strength a comparator reads from the sensor, which is the frequency of the last
/// vibration while it is active
pub fn analog_output(world: &impl World, pos: BlockPos, id: u32) -> u8 {
    if phase(id) != Phase::Active {
        return 0;
    }
    match world.get_block_entity(pos) {
        Some(BlockEntity::SculkSensor {
            last_vibration_frequency,
            ..
        }) => *last_vibration_frequency,
        _ => 0,
    }
}

/// The redstone signal behind a calibrated sensor, which is the only frequency it listens to.
/// Zero means it listens to everything.
pub fn back_signal(world: &impl World, pos: BlockPos, id: u32) -> u8 {
    let Some(facing) = facing(id) else {
        return 0;
    };
    let back = facing.unwrap_direction().opposite().block_face();
    let back_pos = pos.offset(back);
    super::get_redstone_power(world.get_block(back_pos), world, back_pos, back)
}

fn update_neighbors(world: &mut impl World, pos: BlockPos) {
    super::update_surrounding_blocks(world, pos);
    super::update_surrounding_blocks(world, pos.offset(BlockFace::Bottom));
}

/// Updates the blocks around a sensor that stops existing while it is outputting power
pub fn on_removed(world: &mut impl World, pos: BlockPos) {
    super::update_surrounding_blocks(world, pos.offset(BlockFace::Bottom));
}

fn set_phase(world: &mut impl World, pos: BlockPos, id: u32, phase: Phase, power: u8) {
    let phase = match phase {
        Phase::Inactive => "inactive",
        Phase::Active => "active",
        Phase::Cooldown => "cooldown",
    };
    let id = generic::with_property(id, "sculk_sensor_phase", phase);
    let id = generic::with_property(id, "power", &power.to_string());
    world.set_block(pos, Block::Generic(id));
}

/// Makes an inactive sensor output `power` for the vibration of `frequency` it received.
/// Returns false if the block is no sensor that can be activated.
pub fn activate(world: &mut impl World, pos: BlockPos, power: u8, frequency: u8) -> bool {
    let Block::Generic(id) = world.get_block(pos) else {
        return false;
    };
    if !is_sensor(id) || !can_activate(id) {
        return false;
    }
    world.set_block_entity(
        pos,
        BlockEntity::SculkSensor {
            last_vibration_frequency: frequency,
            calibrated: is_calibrated(id),
        },
    );
    set_phase(world, pos, id, Phase::Active, power);
    update_neighbors(world, pos);
    true
}

/// Moves the sensor on to the next phase when the time of its current phase is over: an active
/// sensor goes into cooldown and a sensor in cooldown becomes inactive again. Returns the phase
/// it is in now, or `None` if nothing changed.
pub fn advance_phase(world: &mut impl World, pos: BlockPos) -> Option<Phase> {
    let Block::Generic(id) = world.get_block(pos) else {
        return None;
    };
    if !is_sensor(id) {
        return None;
    }
    match phase(id) {
        Phase::Active => {
            set_phase(world, pos, id, Phase::Cooldown, 0);
            update_neighbors(world, pos);
            Some(Phase::Cooldown)
        }
        Phase::Cooldown => {
            set_phase(world, pos, id, Phase::Inactive, 0);
            Some(Phase::Inactive)
        }
        Phase::Inactive => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::comparator;
    use mchprs_blocks::blocks::{Comparator, ComparatorMode};
    use mchprs_blocks::BlockDirection;
    use mchprs_world::testing::TestWorld;

    fn sensor_state(name: &str, props: &[(&str, &str)]) -> u32 {
        let mut id = generic::default_state(name).unwrap();
        for (prop, value) in props {
            id = generic::with_property(id, prop, value);
        }
        id
    }

    fn sensor(world: &mut TestWorld, pos: BlockPos, calibrated: bool) -> u32 {
        let name = if calibrated {
            "minecraft:calibrated_sculk_sensor"
        } else {
            "minecraft:sculk_sensor"
        };
        let id = sensor_state(name, &[("facing", "north")]);
        world.set_block(pos, Block::Generic(id));
        id
    }

    fn id_at(world: &TestWorld, pos: BlockPos) -> u32 {
        match world.get_block(pos) {
            Block::Generic(id) => id,
            other => panic!("no sensor but {other:?}"),
        }
    }

    fn phase_at(world: &TestWorld, pos: BlockPos) -> (Phase, u8) {
        let id = id_at(world, pos);
        (phase(id), power(id))
    }

    #[test]
    fn ids() {
        let normal = sensor_state("minecraft:sculk_sensor", &[]);
        let calibrated = sensor_state("minecraft:calibrated_sculk_sensor", &[]);
        assert!(is_sensor(normal) && !is_calibrated(normal));
        assert!(is_sensor(calibrated) && is_calibrated(calibrated));
        assert!(!is_sensor(sensor_state("minecraft:sculk", &[])));
        assert_eq!(active_ticks(normal), 30);
        assert_eq!(active_ticks(calibrated), 10);
        assert_eq!(listener_radius(normal), 8);
        assert_eq!(listener_radius(calibrated), 16);
    }

    #[test]
    fn phase_state_machine() {
        let mut world = TestWorld::new(1, 1, 1);
        let pos = BlockPos::new(2, 2, 2);
        sensor(&mut world, pos, false);
        assert_eq!(phase_at(&world, pos), (Phase::Inactive, 0));

        assert!(activate(&mut world, pos, 12, 7));
        assert_eq!(phase_at(&world, pos), (Phase::Active, 12));
        // Only an inactive sensor can be activated
        assert!(!activate(&mut world, pos, 3, 1));
        assert_eq!(phase_at(&world, pos), (Phase::Active, 12));

        assert_eq!(advance_phase(&mut world, pos), Some(Phase::Cooldown));
        assert_eq!(phase_at(&world, pos), (Phase::Cooldown, 0));
        assert!(!activate(&mut world, pos, 3, 1));

        assert_eq!(advance_phase(&mut world, pos), Some(Phase::Inactive));
        assert_eq!(phase_at(&world, pos), (Phase::Inactive, 0));
        assert_eq!(advance_phase(&mut world, pos), None);

        assert!(activate(&mut world, pos, 1, 1));
        assert_eq!(phase_at(&world, pos), (Phase::Active, 1));
    }

    #[test]
    fn activate_ignores_other_blocks() {
        let mut world = TestWorld::new(1, 1, 1);
        let pos = BlockPos::new(2, 2, 2);
        assert!(!activate(&mut world, pos, 5, 5));
        world.set_block(pos, Block::RedstoneBlock);
        assert!(!activate(&mut world, pos, 5, 5));
        assert_eq!(advance_phase(&mut world, pos), None);
    }

    #[test]
    fn power_is_only_output_while_active() {
        let mut world = TestWorld::new(1, 1, 1);
        let pos = BlockPos::new(2, 2, 2);
        sensor(&mut world, pos, false);
        assert_eq!(weak_power(id_at(&world, pos), BlockFace::East), 0);

        activate(&mut world, pos, 9, 4);
        let id = id_at(&world, pos);
        for side in BlockFace::values() {
            assert_eq!(weak_power(id, side), 9);
        }
        assert_eq!(strong_power(id, BlockFace::Top), 9);
        for side in [
            BlockFace::Bottom,
            BlockFace::North,
            BlockFace::South,
            BlockFace::East,
            BlockFace::West,
        ] {
            assert_eq!(strong_power(id, side), 0);
        }

        advance_phase(&mut world, pos);
        assert_eq!(weak_power(id_at(&world, pos), BlockFace::East), 0);
    }

    #[test]
    fn calibrated_sensor_does_not_power_in_its_facing_direction() {
        let mut world = TestWorld::new(1, 1, 1);
        let pos = BlockPos::new(2, 2, 2);
        sensor(&mut world, pos, true);
        activate(&mut world, pos, 15, 10);
        let id = id_at(&world, pos);
        assert_eq!(facing(id), Some(BlockFace::North));
        assert_eq!(weak_power(id, BlockFace::North), 0);
        assert_eq!(weak_power(id, BlockFace::South), 15);
        assert_eq!(weak_power(id, BlockFace::Top), 15);
    }

    #[test]
    fn comparator_reads_last_frequency_while_active() {
        let mut world = TestWorld::new(1, 1, 1);
        let pos = BlockPos::new(2, 2, 2);
        sensor(&mut world, pos, false);
        assert!(comparator::has_override(world.get_block(pos)));
        assert_eq!(
            comparator::get_override(world.get_block(pos), &world, pos),
            0
        );

        activate(&mut world, pos, 8, 13);
        assert_eq!(
            comparator::get_override(world.get_block(pos), &world, pos),
            13
        );

        advance_phase(&mut world, pos);
        assert_eq!(
            comparator::get_override(world.get_block(pos), &world, pos),
            0
        );
    }

    #[test]
    fn back_signal_comes_from_behind() {
        let mut world = TestWorld::new(1, 1, 1);
        let pos = BlockPos::new(4, 2, 4);
        let id = sensor(&mut world, pos, true);
        // Facing north, so the input is on the south side
        assert_eq!(back_signal(&world, pos, id), 0);
        world.set_block(pos.offset(BlockFace::South), Block::RedstoneBlock);
        assert_eq!(back_signal(&world, pos, id), 15);
        world.set_block(pos.offset(BlockFace::South), Block::Air);
        world.set_block(pos.offset(BlockFace::North), Block::RedstoneBlock);
        assert_eq!(back_signal(&world, pos, id), 0);
    }

    #[test]
    fn activating_updates_a_neighboring_lamp() {
        let mut world = TestWorld::new(1, 1, 1);
        let pos = BlockPos::new(4, 2, 4);
        sensor(&mut world, pos, false);
        let lamp = pos.offset(BlockFace::East);
        world.set_block(lamp, Block::RedstoneLamp { lit: false });
        activate(&mut world, pos, 6, 3);
        assert_eq!(world.get_block(lamp), Block::RedstoneLamp { lit: true });
    }

    #[test]
    fn comparator_next_to_a_sensor_is_powered() {
        let mut world = TestWorld::new(1, 1, 1);
        let pos = BlockPos::new(4, 2, 4);
        sensor(&mut world, pos, false);
        // The comparator is west of the sensor and looks east towards it
        let comp = Comparator::new(BlockDirection::East, ComparatorMode::Compare, false);
        let comp_pos = pos.offset(BlockFace::West);
        world.set_block(comp_pos, Block::Comparator(comp));
        assert!(!comparator::should_be_powered(comp, &world, comp_pos));
        activate(&mut world, pos, 6, 3);
        assert!(comparator::should_be_powered(comp, &world, comp_pos));
    }
}
