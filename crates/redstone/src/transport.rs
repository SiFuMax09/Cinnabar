//! Hoppers, droppers and dispensers.
//!
//! The logic follows `HopperBlockEntity`, `DropperBlock` and `DispenserBlock` of the game so the
//! timing (8 tick cooldown, the order the slots are tried in, the cooldown hoppers skip when
//! they are filled by another hopper) is the same as in vanilla.

use cinnabar_blocks::block_entities::{BlockEntity, ContainerType, Inventory, InventoryEntry};
use cinnabar_blocks::blocks::{Block, HopperFacing};
use cinnabar_blocks::items::Item;
use cinnabar_blocks::{generic, BlockFace, BlockPos};
use cinnabar_world::transport::{SpawnRequest, SpawnedItem, TransportKind};
use cinnabar_world::{TickPriority, World};

/// How many game ticks a hopper waits after it moved an item
const MOVE_ITEM_SPEED: i32 = 8;
/// A dropper or dispenser fires this many ticks after it received power
const DISPENSE_DELAY: u32 = 4;

pub fn opposite(face: BlockFace) -> BlockFace {
    match face {
        BlockFace::Bottom => BlockFace::Top,
        BlockFace::Top => BlockFace::Bottom,
        BlockFace::North => BlockFace::South,
        BlockFace::South => BlockFace::North,
        BlockFace::West => BlockFace::East,
        BlockFace::East => BlockFace::West,
    }
}

pub fn hopper_face(facing: HopperFacing) -> BlockFace {
    match facing {
        HopperFacing::Down => BlockFace::Bottom,
        HopperFacing::North => BlockFace::North,
        HopperFacing::South => BlockFace::South,
        HopperFacing::West => BlockFace::West,
        HopperFacing::East => BlockFace::East,
    }
}

/// The direction a dropper or dispenser faces
pub fn facing_of_generic(id: u32) -> BlockFace {
    match generic::get_property(id, "facing") {
        Some("down") => BlockFace::Bottom,
        Some("up") => BlockFace::Top,
        Some("south") => BlockFace::South,
        Some("west") => BlockFace::West,
        Some("east") => BlockFace::East,
        _ => BlockFace::North,
    }
}

/// Reads the items of the container at the position
pub fn read_inventory(world: &impl World, pos: BlockPos) -> Option<(ContainerType, Inventory)> {
    let ty = ContainerType::for_block(world.get_block(pos))?;
    let num_slots = ty.num_slots() as usize;
    let inventory = match world.get_block_entity(pos) {
        Some(BlockEntity::Container { inventory, .. }) => {
            Inventory::from_entries(inventory, num_slots)
        }
        _ => Inventory::new(num_slots),
    };
    Some((ty, inventory))
}

/// Stores the items of the container and lets the comparators next to it know
pub fn write_inventory(
    world: &mut impl World,
    pos: BlockPos,
    ty: ContainerType,
    inventory: Inventory,
) {
    let old_signal = match world.get_block_entity(pos) {
        Some(BlockEntity::Container {
            comparator_override,
            ..
        }) => *comparator_override,
        _ => 0,
    };
    let block_entity = BlockEntity::container(inventory.into_entries(), ty);
    let new_signal = match &block_entity {
        BlockEntity::Container {
            comparator_override,
            ..
        } => *comparator_override,
        _ => 0,
    };
    world.set_block_entity(pos, block_entity);
    if old_signal != new_signal {
        update_comparators_around(world, pos);
    }
}

/// Updates the comparators that read the container, which are next to it or behind a block
/// next to it
pub fn update_comparators_around(world: &mut impl World, pos: BlockPos) {
    for face in BlockFace::values() {
        if !face.is_horizontal() {
            continue;
        }
        let neighbor_pos = pos.offset(face);
        let neighbor = world.get_block(neighbor_pos);
        super::update(neighbor, world, neighbor_pos);
        if neighbor.is_solid() {
            let far_pos = neighbor_pos.offset(face);
            let far = world.get_block(far_pos);
            super::update(far, world, far_pos);
        }
    }
}

fn is_fuel(name: &str) -> bool {
    let name = name.trim_start_matches("minecraft:");
    matches!(
        name,
        "coal"
            | "charcoal"
            | "coal_block"
            | "lava_bucket"
            | "blaze_rod"
            | "dried_kelp_block"
            | "stick"
            | "bamboo"
            | "scaffolding"
    ) || name.ends_with("_planks")
        || name.ends_with("_log")
        || name.ends_with("_wood")
        || name.ends_with("_slab") && !name.contains("stone")
}

/// The slots of the container that can be reached from the face, in the order they are tried
fn slots_for_face(ty: ContainerType, face: BlockFace) -> Vec<usize> {
    match ty {
        ContainerType::Furnace => match face {
            BlockFace::Bottom => vec![2, 1],
            BlockFace::Top => vec![0],
            _ => vec![1],
        },
        _ => (0..ty.num_slots() as usize).collect(),
    }
}

fn can_place(ty: ContainerType, slot: usize, entry: &InventoryEntry, through_face: bool) -> bool {
    let name = Item::from_id(entry.id).get_name();
    match ty {
        ContainerType::Furnace => {
            slot != 2 && (slot != 1 || !through_face || is_fuel(name) || name.ends_with("bucket"))
        }
        ContainerType::ShulkerBox => !name.ends_with("shulker_box"),
        _ => true,
    }
}

fn can_take(ty: ContainerType, slot: usize, entry: &InventoryEntry) -> bool {
    // Only the empty bucket of the lava bucket the fuel slot held can be taken
    ty != ContainerType::Furnace
        || slot != 1
        || Item::from_id(entry.id).get_name() == "minecraft:bucket"
}

fn container_at(world: &impl World, pos: BlockPos) -> Option<ContainerType> {
    ContainerType::for_block(world.get_block(pos))
}

/// Adds the item to the container the way hoppers do, trying the slots from the face in order.
/// Returns what did not fit. `from_hopper` is the hopper the item comes from, if there is one.
pub fn add_item(
    world: &mut impl World,
    from_hopper: Option<BlockPos>,
    target: BlockPos,
    entry: InventoryEntry,
    through_face: Option<BlockFace>,
) -> Option<InventoryEntry> {
    let Some((ty, mut inventory)) = read_inventory(world, target) else {
        return Some(entry);
    };
    let slots = match through_face {
        Some(face) => slots_for_face(ty, face),
        None => (0..inventory.len()).collect(),
    };
    let was_empty = inventory.is_empty();
    let mut remaining = Some(entry);
    let mut success = false;
    for slot in slots {
        let Some(entry) = remaining.take() else {
            break;
        };
        if !can_place(ty, slot, &entry, through_face.is_some()) {
            remaining = Some(entry);
            continue;
        }
        let before = entry.count;
        remaining = inventory.insert_into_slot(slot, entry);
        success |= remaining.as_ref().map_or(true, |rest| rest.count != before);
    }
    if !success {
        return remaining;
    }

    if was_empty && ty == ContainerType::Hopper {
        let from_ticked = from_hopper.and_then(|from| {
            world
                .transport()
                .hopper(from)
                .map(|state| state.ticked_game_time)
        });
        if let Some(state) = world.transport().hopper(target)
            && state.cooldown <= MOVE_ITEM_SPEED
        {
            let skip = from_ticked.is_some_and(|from| state.ticked_game_time >= from) as i32;
            state.cooldown = MOVE_ITEM_SPEED - skip;
        }
    }
    write_inventory(world, target, ty, inventory);
    remaining
}

fn is_full_for_face(inventory: &Inventory, ty: ContainerType, face: BlockFace) -> bool {
    slots_for_face(ty, face).into_iter().all(|slot| {
        inventory
            .get(slot)
            .is_some_and(|entry| entry.count >= Inventory::max_stack(entry))
    })
}

fn one_of(entry: &InventoryEntry) -> InventoryEntry {
    InventoryEntry {
        count: 1,
        ..entry.clone()
    }
}

/// Moves the first item of the hopper into the container it points at
fn eject_items(world: &mut impl World, pos: BlockPos, facing: BlockFace) -> bool {
    let target_pos = pos.offset(facing);
    let Some(target_ty) = container_at(world, target_pos) else {
        return false;
    };
    let face = opposite(facing);
    let (_, target_inventory) = read_inventory(world, target_pos).unwrap();
    if is_full_for_face(&target_inventory, target_ty, face) {
        return false;
    }
    let (ty, mut hopper) = read_inventory(world, pos).unwrap();
    for slot in 0..hopper.len() {
        let Some(entry) = hopper.get(slot).cloned() else {
            continue;
        };
        if add_item(world, Some(pos), target_pos, one_of(&entry), Some(face)).is_none() {
            remove_one(&mut hopper, slot, entry);
            write_inventory(world, pos, ty, hopper);
            return true;
        }
    }
    false
}

fn remove_one(inventory: &mut Inventory, slot: usize, mut entry: InventoryEntry) {
    entry.count -= 1;
    inventory.set(slot, (entry.count > 0).then_some(entry));
}

/// Whether the box of an item entity touches the area a hopper picks items up from, which
/// reaches from the top of its bowl up to two blocks above the hopper
fn in_suck_area(hopper: BlockPos, item: (f64, f64, f64)) -> bool {
    const ITEM_WIDTH: f64 = 0.25;
    let (x, y, z) = item;
    let half = ITEM_WIDTH / 2.0;
    x + half > hopper.x as f64
        && x - half < hopper.x as f64 + 1.0
        && z + half > hopper.z as f64
        && z - half < hopper.z as f64 + 1.0
        && y + ITEM_WIDTH > hopper.y as f64 + 11.0 / 16.0
        && y < hopper.y as f64 + 2.0
}

/// Pulls one item from the container above the hopper or picks up item entities
fn suck_items(world: &mut impl World, pos: BlockPos) -> bool {
    let above = pos.offset(BlockFace::Top);
    if let Some((above_ty, mut source)) = read_inventory(world, above) {
        for slot in slots_for_face(above_ty, BlockFace::Bottom) {
            let Some(entry) = source.get(slot).cloned() else {
                continue;
            };
            if !can_take(above_ty, slot, &entry) {
                continue;
            }
            if add_item(world, Some(above), pos, one_of(&entry), None).is_none() {
                remove_one(&mut source, slot, entry);
                write_inventory(world, above, above_ty, source);
                return true;
            }
        }
        return false;
    }

    let blocked = world.get_block(above).is_solid();
    if blocked {
        return false;
    }
    let candidates: Vec<usize> = world
        .transport()
        .items
        .iter()
        .enumerate()
        .filter(|(_, item)| item.entry.count > 0 && in_suck_area(pos, item.pos))
        .map(|(idx, _)| idx)
        .collect();
    for idx in candidates {
        let entry = world.transport().items[idx].entry.clone();
        let rest = add_item(world, None, pos, entry, None);
        let transport = world.transport();
        let item = &mut transport.items[idx];
        let consumed = rest.is_none();
        match rest {
            Some(rest) => item.entry.count = rest.count,
            None => item.entry.count = 0,
        }
        transport.changed_items.insert(item.entity_id);
        if consumed {
            return true;
        }
    }
    false
}

fn try_move_items(world: &mut impl World, pos: BlockPos, facing: BlockFace, enabled: bool) {
    let cooldown = world
        .transport()
        .hopper(pos)
        .map_or(0, |state| state.cooldown);
    if cooldown > 0 || !enabled {
        return;
    }
    let mut changed = false;
    if !read_inventory(world, pos).unwrap().1.is_empty() {
        changed = eject_items(world, pos, facing);
    }
    if !read_inventory(world, pos).unwrap().1.is_full() {
        changed |= suck_items(world, pos);
    }
    if changed && let Some(state) = world.transport().hopper(pos) {
        state.cooldown = MOVE_ITEM_SPEED;
    }
}

/// Ticks all hoppers of the world once. Call this once per game tick after the scheduled ticks.
pub fn tick_hoppers(world: &mut impl World) {
    world.transport().game_time += 1;
    let game_time = world.transport().game_time;
    for pos in world.transport().hopper_positions() {
        let Block::Hopper { facing, enabled } = world.get_block(pos) else {
            continue;
        };
        let Some(state) = world.transport().hopper(pos) else {
            continue;
        };
        state.cooldown = (state.cooldown - 1).max(-1);
        state.ticked_game_time = game_time;
        if state.cooldown > 0 {
            continue;
        }
        state.cooldown = 0;
        try_move_items(world, pos, hopper_face(facing), enabled);
    }
}

/// Whether the hopper, dropper or dispenser has to do something because of a neighbor update
pub fn update_hopper(world: &mut impl World, pos: BlockPos) {
    let Block::Hopper { facing, enabled } = world.get_block(pos) else {
        return;
    };
    let should_be_enabled = !super::redstone_lamp_should_be_lit(world, pos);
    if enabled != should_be_enabled {
        world.set_block(
            pos,
            Block::Hopper {
                facing,
                enabled: should_be_enabled,
            },
        );
    }
}

pub fn update_dispenser(id: u32, world: &mut impl World, pos: BlockPos) {
    let should_trigger = super::redstone_lamp_should_be_lit(world, pos)
        || super::redstone_lamp_should_be_lit(world, pos.offset(BlockFace::Top));
    let triggered = generic::get_property(id, "triggered") == Some("true");
    if should_trigger != triggered {
        if should_trigger {
            world.schedule_tick(pos, DISPENSE_DELAY, TickPriority::Normal);
        }
        let value = if should_trigger { "true" } else { "false" };
        world.set_block(
            pos,
            Block::Generic(generic::with_property(id, "triggered", value)),
        );
    }
}

/// Picks a random non-empty slot the way the game does, where `random(n)` returns a number from
/// 0 to n - 1
pub fn random_slot(inventory: &Inventory, mut random: impl FnMut(u32) -> u32) -> Option<usize> {
    let mut chosen = None;
    let mut odds = 1;
    for slot in 0..inventory.len() {
        if inventory.get(slot).is_some() {
            if random(odds) == 0 {
                chosen = Some(slot);
            }
            odds += 1;
        }
    }
    chosen
}

fn random_below(n: u32) -> u32 {
    (rand::random::<f64>() * n as f64) as u32
}

/// A number around `center` that is more likely to be close to it, like `RandomSource.triangle`
fn triangle(center: f64, spread: f64) -> f64 {
    center + spread * (rand::random::<f64>() - rand::random::<f64>())
}

/// What a dispenser does with an item, other than throwing it out
fn dispense_special(
    world: &mut impl World,
    pos: BlockPos,
    facing: BlockFace,
    entry: &InventoryEntry,
) -> bool {
    let name = Item::from_id(entry.id).get_name();
    let (dx, dy, dz) = face_vector(facing);
    let spawn = (
        pos.x as f64 + 0.5 + dx * 0.7,
        pos.y as f64 + 0.5 + dy * 0.7,
        pos.z as f64 + 0.5 + dz * 0.7,
    );
    match name {
        "minecraft:tnt" => {
            world
                .transport()
                .spawn_requests
                .push(SpawnRequest::PrimedTnt { pos: spawn });
            true
        }
        "minecraft:ender_pearl" => {
            const POWER: f64 = 1.1;
            const UNCERTAINTY: f64 = 0.0172275 * 6.0;
            let vel = (
                triangle(dx * POWER, UNCERTAINTY),
                triangle(dy * POWER + 0.1, UNCERTAINTY),
                triangle(dz * POWER, UNCERTAINTY),
            );
            world
                .transport()
                .spawn_requests
                .push(SpawnRequest::EnderPearl {
                    pos: (spawn.0, spawn.1 - 0.15, spawn.2),
                    vel,
                });
            true
        }
        _ => false,
    }
}

fn face_vector(face: BlockFace) -> (f64, f64, f64) {
    match face {
        BlockFace::Bottom => (0.0, -1.0, 0.0),
        BlockFace::Top => (0.0, 1.0, 0.0),
        BlockFace::North => (0.0, 0.0, -1.0),
        BlockFace::South => (0.0, 0.0, 1.0),
        BlockFace::West => (-1.0, 0.0, 0.0),
        BlockFace::East => (1.0, 0.0, 0.0),
    }
}

/// Throws one item out of the block the way `DefaultDispenseItemBehavior` does
fn spawn_item(world: &mut impl World, pos: BlockPos, facing: BlockFace, entry: InventoryEntry) {
    const ACCURACY: f64 = 6.0;
    let (dx, dy, dz) = face_vector(facing);
    let y_offset = if dy != 0.0 { 0.125 } else { 0.15625 };
    let power = rand::random::<f64>() * 0.1 + 0.2;
    world.transport().spawned.push(SpawnedItem {
        pos: (
            pos.x as f64 + 0.5 + dx * 0.7,
            pos.y as f64 + 0.5 + dy * 0.7 - y_offset,
            pos.z as f64 + 0.5 + dz * 0.7,
        ),
        vel: (
            triangle(dx * power, 0.0172275 * ACCURACY),
            triangle(0.2, 0.0172275 * ACCURACY),
            triangle(dz * power, 0.0172275 * ACCURACY),
        ),
        entry,
    });
}

/// The scheduled tick of a dropper or dispenser
pub fn dispense(world: &mut impl World, pos: BlockPos) {
    dispense_with(world, pos, random_below);
}

pub fn dispense_with(world: &mut impl World, pos: BlockPos, random: impl FnMut(u32) -> u32) {
    let Block::Generic(id) = world.get_block(pos) else {
        return;
    };
    let Some(kind) = TransportKind::of_state(id) else {
        return;
    };
    let Some((ty, mut inventory)) = read_inventory(world, pos) else {
        return;
    };
    let Some(slot) = random_slot(&inventory, random) else {
        return;
    };
    let facing = facing_of_generic(id);
    let entry = inventory.get(slot).cloned().unwrap();

    let mut taken = false;
    if kind == TransportKind::Dropper && container_at(world, pos.offset(facing)).is_some() {
        // The dropper puts the item into the container instead of dropping it
        if add_item(
            world,
            None,
            pos.offset(facing),
            one_of(&entry),
            Some(opposite(facing)),
        )
        .is_none()
        {
            taken = true;
        }
        // The container may have been changed in a way that changed this one too
        inventory = read_inventory(world, pos).unwrap().1;
    } else if kind == TransportKind::Dispenser && dispense_special(world, pos, facing, &entry) {
        taken = true;
    } else {
        spawn_item(world, pos, facing, one_of(&entry));
        taken = true;
    }

    if taken {
        let mut rest = entry;
        rest.count -= 1;
        inventory.set(slot, (rest.count > 0).then_some(rest));
        write_inventory(world, pos, ty, inventory);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cinnabar_world::testing::TestWorld;

    fn stone(count: i8) -> InventoryEntry {
        InventoryEntry {
            id: Item::from_name("minecraft:stone").unwrap().get_id(),
            slot: 0,
            count,
            nbt: None,
        }
    }

    fn generic_block(name: &str) -> Block {
        Block::Generic(generic::default_state(name).unwrap())
    }

    fn hopper(facing: HopperFacing) -> Block {
        Block::Hopper {
            facing,
            enabled: true,
        }
    }

    fn fill(world: &mut TestWorld, pos: BlockPos, entries: &[(usize, i8)]) {
        let (ty, mut inventory) = read_inventory(world, pos).unwrap();
        for (slot, count) in entries {
            inventory.set(*slot, (*count > 0).then(|| stone(*count)));
        }
        write_inventory(world, pos, ty, inventory);
    }

    fn count_items(world: &TestWorld, pos: BlockPos) -> i32 {
        let (_, inventory) = read_inventory(world, pos).unwrap();
        (0..inventory.len())
            .filter_map(|slot| inventory.get(slot))
            .map(|entry| entry.count as i32)
            .sum()
    }

    #[test]
    fn hopper_moves_one_item_every_eight_ticks() {
        let mut world = TestWorld::new(1, 1, 1);
        let chest = BlockPos::new(2, 5, 2);
        let hopper_pos = BlockPos::new(2, 4, 2);
        let barrel = BlockPos::new(2, 3, 2);
        world.set_block(chest, generic_block("minecraft:chest"));
        world.set_block(hopper_pos, hopper(HopperFacing::Down));
        world.set_block(
            barrel,
            Block::Barrel {
                facing: cinnabar_blocks::BlockFacing::Up,
                open: false,
            },
        );
        fill(&mut world, chest, &[(0, 3)]);

        tick_hoppers(&mut world);
        assert_eq!(count_items(&world, chest), 2);
        assert_eq!(count_items(&world, hopper_pos), 1);
        assert_eq!(count_items(&world, barrel), 0);

        // The cooldown of 8 ticks lasts for 7 more ticks
        for _ in 0..7 {
            tick_hoppers(&mut world);
            assert_eq!(count_items(&world, barrel), 0);
        }
        // Then the item is pushed down and the next one pulled from the chest
        tick_hoppers(&mut world);
        assert_eq!(count_items(&world, barrel), 1);
        assert_eq!(count_items(&world, hopper_pos), 1);
        assert_eq!(count_items(&world, chest), 1);
    }

    #[test]
    fn locked_hopper_does_not_move_items() {
        let mut world = TestWorld::new(1, 1, 1);
        let chest = BlockPos::new(2, 5, 2);
        let hopper_pos = BlockPos::new(2, 4, 2);
        world.set_block(chest, generic_block("minecraft:chest"));
        world.set_block(
            hopper_pos,
            Block::Hopper {
                facing: HopperFacing::Down,
                enabled: false,
            },
        );
        fill(&mut world, chest, &[(0, 3)]);
        for _ in 0..20 {
            tick_hoppers(&mut world);
        }
        assert_eq!(count_items(&world, chest), 3);
    }

    #[test]
    fn hopper_pulls_slots_in_order_and_merges_stacks() {
        let mut world = TestWorld::new(1, 1, 1);
        let chest = BlockPos::new(2, 5, 2);
        let hopper_pos = BlockPos::new(2, 4, 2);
        world.set_block(chest, generic_block("minecraft:chest"));
        world.set_block(hopper_pos, hopper(HopperFacing::Down));
        fill(&mut world, chest, &[(4, 2), (9, 5)]);
        tick_hoppers(&mut world);
        let (_, chest_inv) = read_inventory(&world, chest).unwrap();
        // The lowest slot is emptied first
        assert_eq!(chest_inv.get(4).unwrap().count, 1);
        assert_eq!(chest_inv.get(9).unwrap().count, 5);
        for _ in 0..8 {
            tick_hoppers(&mut world);
        }
        // The hopper merges both items into its first slot
        let (_, hopper_inv) = read_inventory(&world, hopper_pos).unwrap();
        assert_eq!(hopper_inv.get(0).unwrap().count, 2);
        assert!(hopper_inv.get(1).is_none());
    }

    #[test]
    fn hopper_pushes_into_the_container_it_faces() {
        let mut world = TestWorld::new(1, 1, 1);
        let hopper_pos = BlockPos::new(2, 4, 2);
        let target = BlockPos::new(3, 4, 2);
        world.set_block(hopper_pos, hopper(HopperFacing::East));
        world.set_block(target, generic_block("minecraft:dropper"));
        fill(&mut world, hopper_pos, &[(2, 4)]);
        tick_hoppers(&mut world);
        assert_eq!(count_items(&world, hopper_pos), 3);
        assert_eq!(count_items(&world, target), 1);
    }

    #[test]
    fn hopper_filled_by_a_hopper_gets_the_same_cooldown_in_either_tick_order() {
        for upper_first in [true, false] {
            let mut world = TestWorld::new(1, 1, 1);
            let upper = BlockPos::new(2, 5, 2);
            let lower = BlockPos::new(3, 5, 2);
            if upper_first {
                world.set_block(upper, hopper(HopperFacing::East));
                world.set_block(lower, hopper(HopperFacing::Down));
            } else {
                world.set_block(lower, hopper(HopperFacing::Down));
                world.set_block(upper, hopper(HopperFacing::East));
            }
            fill(&mut world, upper, &[(0, 2)]);
            tick_hoppers(&mut world);
            assert_eq!(count_items(&world, lower), 1);
            assert_eq!(world.transport().hopper(upper).unwrap().cooldown, 8);
            // A hopper that was ticked in the same game tick already is not counted down again
            assert_eq!(world.transport().hopper(lower).unwrap().cooldown, 7);
        }
    }

    #[test]
    fn hopper_picks_up_item_entities_above() {
        let mut world = TestWorld::new(1, 1, 1);
        let hopper_pos = BlockPos::new(2, 4, 2);
        world.set_block(hopper_pos, hopper(HopperFacing::Down));
        world
            .transport()
            .items
            .push(cinnabar_world::transport::WorldItem {
                entity_id: 7,
                pos: (2.5, 5.2, 2.5),
                entry: stone(3),
            });
        tick_hoppers(&mut world);
        assert_eq!(count_items(&world, hopper_pos), 3);
        assert_eq!(world.transport().items[0].entry.count, 0);
        assert!(world.transport().changed_items.contains(&7));
    }

    #[test]
    fn dropper_inserts_into_container_or_drops() {
        let mut world = TestWorld::new(1, 1, 1);
        let dropper = BlockPos::new(2, 4, 2);
        let id = generic::with_property(
            generic::default_state("minecraft:dropper").unwrap(),
            "facing",
            "east",
        );
        world.set_block(dropper, Block::Generic(id));
        fill(&mut world, dropper, &[(3, 2)]);

        // Nothing in front: an item entity is spawned
        dispense_with(&mut world, dropper, |_| 0);
        assert_eq!(count_items(&world, dropper), 1);
        assert_eq!(world.transport().spawned.len(), 1);
        assert_eq!(world.transport().spawned[0].entry.count, 1);

        // A container in front receives it instead
        let target = BlockPos::new(3, 4, 2);
        world.set_block(target, generic_block("minecraft:chest"));
        dispense_with(&mut world, dropper, |_| 0);
        assert_eq!(count_items(&world, dropper), 0);
        assert_eq!(count_items(&world, target), 1);
        assert_eq!(world.transport().spawned.len(), 1);
    }

    #[test]
    fn random_slot_picks_among_filled_slots() {
        let mut inventory = Inventory::new(9);
        inventory.set(1, Some(stone(1)));
        inventory.set(5, Some(stone(1)));
        assert_eq!(random_slot(&inventory, |_| 0), Some(5));
        assert_eq!(random_slot(&inventory, |n| n - 1), Some(1));
        assert_eq!(random_slot(&Inventory::new(9), |_| 0), None);
    }

    #[test]
    fn dropper_fires_four_ticks_after_getting_power() {
        let mut world = TestWorld::new(1, 1, 1);
        let dropper = BlockPos::new(2, 4, 2);
        world.set_block(dropper, generic_block("minecraft:dropper"));
        world.set_block(BlockPos::new(3, 4, 2), Block::RedstoneBlock);
        let Block::Generic(id) = world.get_block(dropper) else {
            unreachable!()
        };
        update_dispenser(id, &mut world, dropper);
        let Block::Generic(id) = world.get_block(dropper) else {
            unreachable!()
        };
        assert_eq!(generic::get_property(id, "triggered"), Some("true"));
        assert_eq!(world.to_be_ticked.len(), 1);
        assert_eq!(world.to_be_ticked[0].ticks_left, 4);
    }

    #[test]
    fn comparator_signal_follows_the_fullness() {
        let mut world = TestWorld::new(1, 1, 1);
        let hopper_pos = BlockPos::new(2, 4, 2);
        world.set_block(hopper_pos, hopper(HopperFacing::Down));
        fill(
            &mut world,
            hopper_pos,
            &[(0, 64), (1, 64), (2, 64), (3, 64), (4, 64)],
        );
        let Some(BlockEntity::Container {
            comparator_override,
            ..
        }) = world.get_block_entity(hopper_pos)
        else {
            panic!("no container");
        };
        assert_eq!(*comparator_override, 15);
        fill(
            &mut world,
            hopper_pos,
            &[(0, 1), (1, 0), (2, 0), (3, 0), (4, 0)],
        );
        let Some(BlockEntity::Container {
            comparator_override,
            ..
        }) = world.get_block_entity(hopper_pos)
        else {
            panic!("no container");
        };
        // 1/64 of one slot of five still gives a signal
        assert_eq!(*comparator_override, 1);
    }
}
