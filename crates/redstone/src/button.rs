//! Buttons that have no dedicated variant in `Block`: the wooden buttons and the polished
//! blackstone button. They behave like the stone button, which is a dedicated block.

use crate::{emit_event, update_surrounding_blocks};
use mchprs_blocks::blocks::Block;
use mchprs_blocks::generic;
use mchprs_blocks::{BlockFace, BlockPos};
use mchprs_world::game_event::GameEvent;
use mchprs_world::{TickPriority, World};

pub fn is_button(id: u32) -> bool {
    generic::state_name(id).ends_with("_button")
}

fn is_pressed(id: u32) -> bool {
    generic::get_property(id, "powered") == Some("true")
}

/// How many redstone ticks a button stays pressed. Wooden buttons stay pressed for 30 game ticks,
/// stone ones for 20.
fn press_duration(id: u32) -> u32 {
    let name = generic::state_name(id);
    if name.contains("stone") {
        10
    } else {
        15
    }
}

fn face_from_name(name: &str) -> Option<BlockFace> {
    Some(match name {
        "north" => BlockFace::North,
        "south" => BlockFace::South,
        "west" => BlockFace::West,
        "east" => BlockFace::East,
        _ => return None,
    })
}

/// The block the button is attached to, as the direction from the button
fn attached_side(id: u32) -> Option<BlockFace> {
    match generic::get_property(id, "face")? {
        "floor" => Some(BlockFace::Bottom),
        "ceiling" => Some(BlockFace::Top),
        _ => {
            let facing = face_from_name(generic::get_property(id, "facing")?)?;
            Some(facing.opposite())
        }
    }
}

/// A pressed button powers the blocks around it weakly
pub fn weak_power(id: u32) -> u8 {
    if is_pressed(id) {
        15
    } else {
        0
    }
}

/// A pressed button powers the block it is attached to strongly. `side` is the direction from
/// the block that is asked to the button.
pub fn strong_power(id: u32, side: BlockFace) -> u8 {
    // The block the button is attached to looks at the button in the opposite direction
    if is_pressed(id) && attached_side(id).is_some_and(|attached| attached.opposite() == side) {
        15
    } else {
        0
    }
}

fn set_pressed(world: &mut impl World, pos: BlockPos, id: u32, pressed: bool) {
    let value = if pressed { "true" } else { "false" };
    world.set_block(
        pos,
        Block::Generic(generic::with_property(id, "powered", value)),
    );
    emit_event(
        world,
        if pressed {
            GameEvent::BlockActivate
        } else {
            GameEvent::BlockDeactivate
        },
        pos,
    );
    update_surrounding_blocks(world, pos);
    if let Some(attached) = attached_side(id) {
        update_surrounding_blocks(world, pos.offset(attached));
    }
}

/// Presses the button. Returns whether the block was a button.
pub fn press(world: &mut impl World, pos: BlockPos, id: u32) -> bool {
    if !is_button(id) {
        return false;
    }
    if !is_pressed(id) {
        set_pressed(world, pos, id, true);
        world.schedule_tick(pos, press_duration(id), TickPriority::Normal);
    }
    true
}

/// Lets the button go after the time it was pressed for
pub fn release(world: &mut impl World, pos: BlockPos, id: u32) {
    if is_button(id) && is_pressed(id) {
        set_pressed(world, pos, id, false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn button(name: &str, face: &str, facing: &str) -> u32 {
        let id = generic::default_state(name).unwrap();
        let id = generic::with_property(id, "face", face);
        generic::with_property(id, "facing", facing)
    }

    #[test]
    fn durations() {
        assert_eq!(press_duration(button("minecraft:oak_button", "wall", "north")), 15);
        assert_eq!(
            press_duration(button("minecraft:polished_blackstone_button", "wall", "north")),
            10
        );
    }

    #[test]
    fn attached_block() {
        // A button on the floor is attached to the block below it
        assert_eq!(
            attached_side(button("minecraft:oak_button", "floor", "north")),
            Some(BlockFace::Bottom)
        );
        // A button on a wall facing north is attached to the block south of it
        assert_eq!(
            attached_side(button("minecraft:oak_button", "wall", "north")),
            Some(BlockFace::South)
        );
    }

    #[test]
    fn only_pressed_buttons_power() {
        let off = button("minecraft:birch_button", "wall", "east");
        assert_eq!(weak_power(off), 0);
        let on = generic::with_property(off, "powered", "true");
        assert_eq!(weak_power(on), 15);
        // Attached to the block west of it, which looks east at the button
        assert_eq!(strong_power(on, BlockFace::East), 15);
        assert_eq!(strong_power(on, BlockFace::West), 0);
    }
}
