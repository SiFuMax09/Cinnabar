use cinnabar_blocks::blocks::Block;
use cinnabar_blocks::{generic, BlockPos};
use cinnabar_redstone::piston::{self, StructureResolver};
use cinnabar_world::testing::TestWorld;
use cinnabar_world::World;

const Y: i32 = 1;
const Z: i32 = 8;

fn pos(x: i32) -> BlockPos {
    BlockPos::new(x, Y, Z)
}

fn block(name: &str, props: &[(&str, &str)]) -> Block {
    let mut id = generic::default_state(name).unwrap();
    for (prop, value) in props {
        id = generic::with_property(id, prop, value);
    }
    Block::from_id(id)
}

fn piston(sticky: bool, facing: &str) -> Block {
    block(
        if sticky {
            "minecraft:sticky_piston"
        } else {
            "minecraft:piston"
        },
        &[("facing", facing)],
    )
}

fn name(world: &TestWorld, pos: BlockPos) -> &'static str {
    generic::state_name(world.get_block_raw(pos))
}

fn notify(world: &mut TestWorld, pos: BlockPos) {
    cinnabar_redstone::update_surrounding_blocks(world, pos);
}

/// One redstone tick: the scheduled ticks and two game ticks of block events
fn tick(world: &mut TestWorld) {
    for entry in &mut world.to_be_ticked {
        entry.ticks_left = entry.ticks_left.saturating_sub(1);
    }
    while let Some(idx) = world.to_be_ticked.iter().position(|e| e.ticks_left == 0) {
        let entry = world.to_be_ticked.remove(idx);
        cinnabar_redstone::tick(world.get_block(entry.pos), world, entry.pos);
    }
    piston::run_game_tick(world, notify);
    piston::run_game_tick(world, notify);
}

/// Places the block and updates the neighbors like a player placing it would
fn place(world: &mut TestWorld, pos: BlockPos, block: Block) {
    world.set_block(pos, block);
    cinnabar_redstone::update_surrounding_blocks(world, pos);
    cinnabar_redstone::update(world.get_block(pos), world, pos);
}

fn extended(world: &TestWorld, pos: BlockPos) -> bool {
    generic::get_property(world.get_block_raw(pos), "extended") == Some("true")
}

#[test]
fn extend_and_retract_timing() {
    let mut world = TestWorld::new(1, 1, 1);
    let base = pos(3);
    place(&mut world, base, piston(false, "east"));
    world.set_block(pos(4), block("minecraft:stone", &[]));
    let power = BlockPos::new(3, Y, Z - 1);

    place(&mut world, power, Block::RedstoneBlock);
    // The event is only handled at the end of the tick
    assert_eq!(name(&world, base), "minecraft:piston");
    assert!(!extended(&world, base));

    tick(&mut world);
    assert!(extended(&world, base));
    assert_eq!(name(&world, pos(4)), "minecraft:moving_piston");
    assert_eq!(name(&world, pos(5)), "minecraft:moving_piston");

    tick(&mut world);
    assert_eq!(name(&world, pos(4)), "minecraft:piston_head");
    assert_eq!(name(&world, pos(5)), "minecraft:stone");

    world.set_block(power, Block::Air);
    cinnabar_redstone::update_surrounding_blocks(&mut world, power);
    tick(&mut world);
    // Without sticky the head is removed right away and the piston itself is moving back
    assert_eq!(name(&world, base), "minecraft:moving_piston");
    assert_eq!(name(&world, pos(4)), "minecraft:air");
    assert_eq!(name(&world, pos(5)), "minecraft:stone");

    tick(&mut world);
    assert_eq!(name(&world, base), "minecraft:piston");
    assert!(!extended(&world, base));
    assert!(world.piston_queue().moving.is_empty());
}

#[test]
fn quasi_connectivity() {
    let mut world = TestWorld::new(1, 1, 1);
    let base = pos(3);
    place(&mut world, base, piston(false, "east"));
    // Power above the block above the piston, which does not touch the piston itself
    place(&mut world, BlockPos::new(3, Y + 2, Z), Block::RedstoneBlock);
    tick(&mut world);
    assert!(extended(&world, base));
}

#[test]
fn pulse_shorter_than_the_event_does_nothing() {
    let mut world = TestWorld::new(1, 1, 1);
    let base = pos(3);
    place(&mut world, base, piston(false, "east"));
    let power = BlockPos::new(3, Y, Z - 1);
    place(&mut world, power, Block::RedstoneBlock);
    world.set_block(power, Block::Air);
    cinnabar_redstone::update_surrounding_blocks(&mut world, power);
    tick(&mut world);
    assert!(!extended(&world, base));
    assert_eq!(name(&world, pos(4)), "minecraft:air");
}

#[test]
fn sticky_piston_pulls_block() {
    let mut world = TestWorld::new(1, 1, 1);
    let base = pos(3);
    place(&mut world, base, piston(true, "east"));
    world.set_block(pos(4), block("minecraft:stone", &[]));
    let power = BlockPos::new(3, Y, Z - 1);

    place(&mut world, power, Block::RedstoneBlock);
    tick(&mut world);
    tick(&mut world);
    assert_eq!(name(&world, pos(5)), "minecraft:stone");

    world.set_block(power, Block::Air);
    cinnabar_redstone::update_surrounding_blocks(&mut world, power);
    tick(&mut world);
    assert_eq!(name(&world, pos(4)), "minecraft:moving_piston");
    assert_eq!(name(&world, pos(5)), "minecraft:air");
    tick(&mut world);
    assert_eq!(name(&world, pos(4)), "minecraft:stone");
    assert_eq!(name(&world, base), "minecraft:sticky_piston");
}

#[test]
fn sticky_piston_drops_block_on_short_pulse() {
    let mut world = TestWorld::new(1, 1, 1);
    let base = pos(3);
    place(&mut world, base, piston(true, "east"));
    world.set_block(pos(4), block("minecraft:stone", &[]));
    let power = BlockPos::new(3, Y, Z - 1);

    place(&mut world, power, Block::RedstoneBlock);
    tick(&mut world);
    // The pushed block is still on its way when the power goes away
    world.set_block(power, Block::Air);
    cinnabar_redstone::update_surrounding_blocks(&mut world, power);
    tick(&mut world);
    tick(&mut world);
    assert_eq!(name(&world, pos(5)), "minecraft:stone");
    assert_eq!(name(&world, pos(4)), "minecraft:air");
    assert_eq!(name(&world, base), "minecraft:sticky_piston");
    assert!(!extended(&world, base));
}

#[test]
fn moved_piston_extends_when_it_arrives() {
    let mut world = TestWorld::new(1, 1, 1);
    // Pushing a powered piston sideways: the second piston keeps its power source next to it
    place(&mut world, pos(3), piston(false, "east"));
    world.set_block(pos(4), block("minecraft:stone", &[]));
    let power = BlockPos::new(3, Y, Z - 1);
    place(&mut world, power, Block::RedstoneBlock);
    tick(&mut world);
    tick(&mut world);
    assert_eq!(name(&world, pos(5)), "minecraft:stone");
}

fn resolve(world: &TestWorld, extending: bool, facing: &str) -> Option<StructureResolver> {
    let direction = match facing {
        "east" => cinnabar_blocks::BlockFace::East,
        _ => cinnabar_blocks::BlockFace::West,
    };
    let mut resolver = StructureResolver::new(pos(1), direction, extending);
    resolver.resolve(world).then_some(resolver)
}

fn row(world: &mut TestWorld, from: i32, count: i32, name: &str) {
    for x in from..from + count {
        world.set_block(pos(x), block(name, &[]));
    }
}

#[test]
fn push_limit() {
    let mut world = TestWorld::new(1, 1, 1);
    world.set_block(pos(1), piston(false, "east"));
    row(&mut world, 2, 12, "minecraft:stone");
    let resolver = resolve(&world, true, "east").expect("12 blocks can be pushed");
    assert_eq!(resolver.to_push.len(), 12);
    assert_eq!(resolver.to_push[0], pos(2));

    world.set_block(pos(14), block("minecraft:stone", &[]));
    assert!(
        resolve(&world, true, "east").is_none(),
        "13 blocks are too many"
    );
}

#[test]
fn immovable_blocks() {
    for name in [
        "minecraft:obsidian",
        "minecraft:bedrock",
        "minecraft:crying_obsidian",
        "minecraft:chest",
        "minecraft:oak_sign",
        "minecraft:anvil",
    ] {
        let mut world = TestWorld::new(1, 1, 1);
        world.set_block(pos(1), piston(false, "east"));
        world.set_block(pos(2), block(name, &[]));
        assert!(resolve(&world, true, "east").is_none(), "{name} was pushed");

        // Also if it is further down the line
        world.set_block(pos(2), block("minecraft:stone", &[]));
        world.set_block(pos(3), block(name, &[]));
        assert!(resolve(&world, true, "east").is_none(), "{name} was pushed");
    }
}

#[test]
fn destroyed_blocks_end_the_line() {
    let mut world = TestWorld::new(1, 1, 1);
    world.set_block(pos(1), piston(false, "east"));
    world.set_block(pos(2), block("minecraft:stone", &[]));
    world.set_block(pos(3), block("minecraft:torch", &[]));
    world.set_block(pos(4), block("minecraft:stone", &[]));
    let resolver = resolve(&world, true, "east").unwrap();
    assert_eq!(resolver.to_push, vec![pos(2)]);
    assert_eq!(resolver.to_destroy, vec![pos(3)]);

    // A destroyable block directly in front of the piston is simply broken
    world.set_block(pos(2), block("minecraft:torch", &[]));
    let resolver = resolve(&world, true, "east").unwrap();
    assert!(resolver.to_push.is_empty());
    assert_eq!(resolver.to_destroy, vec![pos(2)]);
}

#[test]
fn extended_pistons_can_not_be_pushed() {
    let mut world = TestWorld::new(1, 1, 1);
    world.set_block(pos(1), piston(false, "east"));
    let extended = block(
        "minecraft:piston",
        &[("facing", "north"), ("extended", "true")],
    );
    world.set_block(pos(2), extended);
    assert!(resolve(&world, true, "east").is_none());
    world.set_block(pos(2), piston(false, "north"));
    assert!(resolve(&world, true, "east").is_some());
}

#[test]
fn slime_pulls_neighbors_along() {
    let mut world = TestWorld::new(1, 1, 1);
    world.set_block(pos(1), piston(false, "east"));
    world.set_block(pos(2), block("minecraft:slime_block", &[]));
    let resolver = resolve(&world, true, "east").unwrap();
    assert_eq!(resolver.to_push, vec![pos(2)]);

    // Any block next to the slime block is pulled along
    world.set_block(BlockPos::new(2, Y, Z + 1), block("minecraft:stone", &[]));
    let resolver = resolve(&world, true, "east").unwrap();
    assert_eq!(resolver.to_push.len(), 2);

    // A second slime block pulls the blocks in front of it along as well
    world.set_block(
        BlockPos::new(2, Y, Z + 1),
        block("minecraft:slime_block", &[]),
    );
    world.set_block(BlockPos::new(3, Y, Z + 1), block("minecraft:stone", &[]));
    let resolver = resolve(&world, true, "east").unwrap();
    assert_eq!(resolver.to_push.len(), 3);
    assert!(resolver.to_push.contains(&BlockPos::new(3, Y, Z + 1)));

    // Slime and honey don't stick together
    world.set_block(BlockPos::new(3, Y, Z + 1), Block::Air);
    world.set_block(
        BlockPos::new(2, Y, Z + 1),
        block("minecraft:honey_block", &[]),
    );
    let resolver = resolve(&world, true, "east").unwrap();
    assert_eq!(resolver.to_push, vec![pos(2)]);
}

#[test]
fn slime_counts_towards_the_limit() {
    let mut world = TestWorld::new(1, 1, 2);
    world.set_block(pos(1), piston(false, "east"));
    world.set_block(pos(2), block("minecraft:slime_block", &[]));
    // 2 blocks in front of the piston plus a row of 11 attached to the slime block
    for z in 1..=11 {
        world.set_block(
            BlockPos::new(2, Y, Z + z),
            block("minecraft:slime_block", &[]),
        );
    }
    assert!(resolve(&world, true, "east").is_some());
    world.set_block(
        BlockPos::new(2, Y, Z + 12),
        block("minecraft:slime_block", &[]),
    );
    assert!(resolve(&world, true, "east").is_none());
}

#[test]
fn retracting_sticky_piston_resolves_the_pulled_blocks() {
    let mut world = TestWorld::new(1, 1, 1);
    world.set_block(
        pos(1),
        block(
            "minecraft:sticky_piston",
            &[("facing", "east"), ("extended", "true")],
        ),
    );
    world.set_block(
        pos(2),
        block(
            "minecraft:piston_head",
            &[("facing", "east"), ("type", "sticky")],
        ),
    );
    world.set_block(pos(3), block("minecraft:stone", &[]));
    // The piston removes its head before it looks for the blocks to pull
    world.set_block(pos(2), Block::Air);
    let mut resolver = StructureResolver::new(pos(1), cinnabar_blocks::BlockFace::East, false);
    assert!(resolver.resolve(&world));
    assert_eq!(resolver.to_push, vec![pos(3)]);
}
