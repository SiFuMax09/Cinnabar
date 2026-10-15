//! Copper bulbs, all oxidation and waxed variants. A bulb toggles `lit` every time it gets
//! powered (rising edge) and only follows the signal in `powered`.

use cinnabar_blocks::blocks::Block;
use cinnabar_blocks::{generic, BlockPos};
use cinnabar_world::World;

const SOUND_CATEGORY_BLOCKS: i32 = 4;

pub fn is_bulb(id: u32) -> bool {
    generic::state_name(id).ends_with("copper_bulb")
}

fn flag(id: u32, name: &str) -> bool {
    generic::get_property(id, name) == Some("true")
}

/// The signal a comparator reads from a bulb
pub fn analog_output(id: u32) -> u8 {
    if flag(id, "lit") {
        15
    } else {
        0
    }
}

/// Compares the bulb with the power around it and toggles it on a rising edge. Called when the
/// bulb is placed and whenever a neighbor changes.
pub fn check_and_flip(world: &mut impl World, pos: BlockPos, id: u32) {
    let powered = crate::redstone_lamp_should_be_lit(world, pos);
    if powered == flag(id, "powered") {
        return;
    }
    let mut new_id = id;
    if powered {
        let lit = !flag(id, "lit");
        new_id = generic::with_property(new_id, "lit", if lit { "true" } else { "false" });
        world.play_sound(
            pos,
            generic::copper_bulb_sound(lit),
            SOUND_CATEGORY_BLOCKS,
            1.0,
            1.0,
        );
    }
    new_id = generic::with_property(new_id, "powered", if powered { "true" } else { "false" });
    world.set_block(pos, Block::Generic(new_id));
}

#[cfg(test)]
mod tests {
    use super::*;
    use cinnabar_world::testing::TestWorld;

    fn bulb(world: &mut TestWorld, pos: BlockPos, name: &str) {
        let id = generic::default_state(name).unwrap();
        // Vanilla defaults: lit=false, powered=false
        let id = generic::with_property(
            generic::with_property(id, "lit", "false"),
            "powered",
            "false",
        );
        world.set_block(pos, Block::Generic(id));
    }

    fn state(world: &TestWorld, pos: BlockPos) -> (bool, bool) {
        match world.get_block(pos) {
            Block::Generic(id) => (flag(id, "lit"), flag(id, "powered")),
            other => panic!("no bulb but {other:?}"),
        }
    }

    #[test]
    fn toggles_on_rising_edge_only() {
        for name in [
            "minecraft:copper_bulb",
            "minecraft:waxed_oxidized_copper_bulb",
        ] {
            let mut world = TestWorld::new(4, 4, 4);
            let pos = BlockPos::new(1, 1, 1);
            let source = BlockPos::new(2, 1, 1);
            bulb(&mut world, pos, name);
            assert_eq!(state(&world, pos), (false, false));

            world.set_block(source, Block::RedstoneBlock);
            crate::update(world.get_block(pos), &mut world, pos);
            assert_eq!(state(&world, pos), (true, true));

            world.set_block(source, Block::Air);
            crate::update(world.get_block(pos), &mut world, pos);
            assert_eq!(state(&world, pos), (true, false));

            world.set_block(source, Block::RedstoneBlock);
            crate::update(world.get_block(pos), &mut world, pos);
            assert_eq!(state(&world, pos), (false, true));
        }
    }

    #[test]
    fn comparator_output_follows_lit() {
        let lit = generic::with_property(
            generic::default_state("minecraft:copper_bulb").unwrap(),
            "lit",
            "true",
        );
        let unlit = generic::with_property(lit, "lit", "false");
        assert_eq!(analog_output(lit), 15);
        assert_eq!(analog_output(unlit), 0);
    }
}
