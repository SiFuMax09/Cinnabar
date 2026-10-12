//! Normal and sticky pistons, ported from the vanilla `PistonBaseBlock`,
//! `PistonStructureResolver` and `PistonMovingBlockEntity`.
//!
//! The timing is the one of vanilla, which counts in game ticks. A redstone tick is two game
//! ticks, so [`run_game_tick`] has to be called twice for every redstone tick, after the scheduled
//! ticks: a piston that gets powered queues a block event, the event is handled at the end of the
//! game tick (this puts `moving_piston` blocks in place) and the moved blocks arrive two game ticks
//! after that, which is one redstone tick later.
//!
//! The server has no automatic neighbor updates, so everything that changes blocks takes a
//! `notify` function that sends the shape and redstone updates around a position.

use crate::{get_redstone_power, update};
use mchprs_blocks::block_entities::{BlockEntity, MovingPistonEntity};
use mchprs_blocks::blocks::Block;
use mchprs_blocks::generic::{self, PushReaction};
use mchprs_blocks::{BlockFace, BlockPos};
use mchprs_world::{PistonEvent, World};

/// Updates the blocks around a position where a block changed
pub type Notify<W> = fn(&mut W, BlockPos);

const PUSH_LIMIT: usize = 12;
/// A safety net against pistons that keep triggering each other within a single game tick
const MAX_EVENTS_PER_TICK: usize = 100_000;

const PISTON: &str = "minecraft:piston";
const STICKY_PISTON: &str = "minecraft:sticky_piston";
const PISTON_HEAD: &str = "minecraft:piston_head";
const MOVING_PISTON: &str = "minecraft:moving_piston";
const SLIME_BLOCK: &str = "minecraft:slime_block";
const HONEY_BLOCK: &str = "minecraft:honey_block";

/// The order of `Direction.values()` in vanilla, which decides the order blocks are pushed in
const DIRECTIONS: [BlockFace; 6] = [
    BlockFace::Bottom,
    BlockFace::Top,
    BlockFace::North,
    BlockFace::South,
    BlockFace::West,
    BlockFace::East,
];

fn offset(pos: BlockPos, face: BlockFace, amount: i32) -> BlockPos {
    let (dx, dy, dz) = match face {
        BlockFace::Bottom => (0, -1, 0),
        BlockFace::Top => (0, 1, 0),
        BlockFace::North => (0, 0, -1),
        BlockFace::South => (0, 0, 1),
        BlockFace::West => (-1, 0, 0),
        BlockFace::East => (1, 0, 0),
    };
    BlockPos::new(pos.x + dx * amount, pos.y + dy * amount, pos.z + dz * amount)
}

fn face_name(face: BlockFace) -> &'static str {
    match face {
        BlockFace::Bottom => "down",
        BlockFace::Top => "up",
        BlockFace::North => "north",
        BlockFace::South => "south",
        BlockFace::West => "west",
        BlockFace::East => "east",
    }
}

fn face_from_name(name: &str) -> Option<BlockFace> {
    DIRECTIONS.into_iter().find(|face| face_name(*face) == name)
}

fn face_from_data(data: u8) -> BlockFace {
    BlockFace::from_id(if data < 6 { data as u32 } else { 2 })
}

/// The block state at `pos`, which is air outside of the world
fn state(world: &impl World, pos: BlockPos) -> u32 {
    if world.in_world_bounds(pos) {
        world.get_block_raw(pos)
    } else {
        0
    }
}

fn is_air(id: u32) -> bool {
    matches!(
        generic::state_name(id),
        "minecraft:air" | "minecraft:cave_air" | "minecraft:void_air"
    )
}

fn is_sticky(id: u32) -> bool {
    matches!(generic::state_name(id), SLIME_BLOCK | HONEY_BLOCK)
}

fn can_stick_to_each_other(a: u32, b: u32) -> bool {
    match (generic::state_name(a), generic::state_name(b)) {
        (HONEY_BLOCK, SLIME_BLOCK) | (SLIME_BLOCK, HONEY_BLOCK) => false,
        _ => is_sticky(a) || is_sticky(b),
    }
}

/// Whether the block is a piston, piston head or moving piston
pub fn is_piston_family(id: u32) -> bool {
    matches!(
        generic::state_name(id),
        PISTON | STICKY_PISTON | PISTON_HEAD | MOVING_PISTON
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PistonBase {
    pub sticky: bool,
    pub facing: BlockFace,
    pub extended: bool,
}

/// The properties of the block if it is a piston base
pub fn piston_base(id: u32) -> Option<PistonBase> {
    let sticky = match generic::state_name(id) {
        PISTON => false,
        STICKY_PISTON => true,
        _ => return None,
    };
    Some(PistonBase {
        sticky,
        facing: generic::get_property(id, "facing")
            .and_then(face_from_name)
            .unwrap_or(BlockFace::North),
        extended: generic::get_property(id, "extended") == Some("true"),
    })
}

fn default_state(name: &str) -> u32 {
    generic::default_state(name).expect("piston blocks exist")
}

fn base_state(sticky: bool, facing: BlockFace, extended: bool) -> u32 {
    let id = default_state(if sticky { STICKY_PISTON } else { PISTON });
    let id = generic::with_property(id, "facing", face_name(facing));
    generic::with_property(id, "extended", if extended { "true" } else { "false" })
}

fn head_state(sticky: bool, facing: BlockFace) -> u32 {
    let id = generic::with_property(default_state(PISTON_HEAD), "facing", face_name(facing));
    generic::with_property(id, "type", if sticky { "sticky" } else { "normal" })
}

fn moving_state(sticky: bool, facing: BlockFace) -> u32 {
    let id = generic::with_property(default_state(MOVING_PISTON), "facing", face_name(facing));
    generic::with_property(id, "type", if sticky { "sticky" } else { "normal" })
}

fn is_moving_piston(id: u32) -> bool {
    generic::state_name(id) == MOVING_PISTON
}

fn moving_entity(world: &impl World, pos: BlockPos) -> Option<&MovingPistonEntity> {
    match world.get_block_entity(pos) {
        Some(BlockEntity::MovingPiston(entity)) => Some(entity),
        _ => None,
    }
}

/// Port of `PistonBaseBlock.isPushable`
fn is_pushable(
    world: &impl World,
    pos: BlockPos,
    id: u32,
    direction: BlockFace,
    allow_destroyable: bool,
    connection_direction: BlockFace,
) -> bool {
    if !world.in_world_bounds(pos) {
        return false;
    }
    if is_air(id) {
        return true;
    }
    let name = generic::state_name(id);
    if matches!(
        name,
        "minecraft:obsidian"
            | "minecraft:crying_obsidian"
            | "minecraft:respawn_anchor"
            | "minecraft:reinforced_deepslate"
    ) {
        return false;
    }
    if direction == BlockFace::Bottom && pos.y == 0 {
        return false;
    }
    if direction == BlockFace::Top && !world.in_world_bounds(offset(pos, BlockFace::Top, 1)) {
        return false;
    }

    if matches!(name, PISTON | STICKY_PISTON) {
        if piston_base(id).is_some_and(|piston| piston.extended) {
            return false;
        }
    } else {
        if generic::is_indestructible(id) {
            return false;
        }
        match generic::push_reaction(id) {
            PushReaction::Block => return false,
            PushReaction::Destroy => return allow_destroyable,
            PushReaction::PushOnly => return direction == connection_direction,
            PushReaction::Normal => {}
        }
    }
    !generic::has_block_entity(id)
}

/// Port of `PistonStructureResolver`: finds the blocks a piston moves and the blocks it breaks
pub struct StructureResolver {
    piston_pos: BlockPos,
    piston_direction: BlockFace,
    extending: bool,
    start_pos: BlockPos,
    push_direction: BlockFace,
    pub to_push: Vec<BlockPos>,
    pub to_destroy: Vec<BlockPos>,
}

impl StructureResolver {
    pub fn new(piston_pos: BlockPos, direction: BlockFace, extending: bool) -> StructureResolver {
        let (push_direction, start_pos) = if extending {
            (direction, offset(piston_pos, direction, 1))
        } else {
            (direction.opposite(), offset(piston_pos, direction, 2))
        };
        StructureResolver {
            piston_pos,
            piston_direction: direction,
            extending,
            start_pos,
            push_direction,
            to_push: Vec::new(),
            to_destroy: Vec::new(),
        }
    }

    pub fn push_direction(&self) -> BlockFace {
        self.push_direction
    }

    pub fn resolve(&mut self, world: &impl World) -> bool {
        self.to_push.clear();
        self.to_destroy.clear();
        let next = state(world, self.start_pos);
        if !is_pushable(
            world,
            self.start_pos,
            next,
            self.push_direction,
            false,
            self.piston_direction,
        ) {
            if self.extending && generic::push_reaction(next) == PushReaction::Destroy {
                self.to_destroy.push(self.start_pos);
                return true;
            }
            return false;
        }
        if !self.add_block_line(world, self.start_pos, self.push_direction) {
            return false;
        }
        // The list grows while branching blocks are added
        let mut i = 0;
        while i < self.to_push.len() {
            let pos = self.to_push[i];
            if is_sticky(state(world, pos)) && !self.add_branching_blocks(world, pos) {
                return false;
            }
            i += 1;
        }
        true
    }

    fn add_block_line(&mut self, world: &impl World, start: BlockPos, direction: BlockFace) -> bool {
        let mut next = state(world, start);
        if is_air(next) {
            return true;
        }
        if !is_pushable(world, start, next, self.push_direction, false, direction) {
            return true;
        }
        if start == self.piston_pos || self.to_push.contains(&start) {
            return true;
        }

        let behind = self.push_direction.opposite();
        let mut block_count = 1;
        if block_count + self.to_push.len() > PUSH_LIMIT {
            return false;
        }
        while is_sticky(next) {
            let pos = offset(start, behind, block_count as i32);
            let previous = next;
            next = state(world, pos);
            if is_air(next)
                || !can_stick_to_each_other(previous, next)
                || !is_pushable(world, pos, next, self.push_direction, false, behind)
                || pos == self.piston_pos
            {
                break;
            }
            block_count += 1;
            if block_count + self.to_push.len() > PUSH_LIMIT {
                return false;
            }
        }

        let mut blocks_added = 0;
        for i in (0..block_count).rev() {
            self.to_push.push(offset(start, behind, i as i32));
            blocks_added += 1;
        }

        let mut i = 1;
        loop {
            let pos = offset(start, self.push_direction, i);
            if let Some(collision) = self.to_push.iter().position(|other| *other == pos) {
                self.reorder_list_at_collision(blocks_added, collision);
                for j in 0..=collision + blocks_added {
                    let pos = self.to_push[j];
                    if is_sticky(state(world, pos)) && !self.add_branching_blocks(world, pos) {
                        return false;
                    }
                }
                return true;
            }

            next = state(world, pos);
            if is_air(next) {
                return true;
            }
            if !is_pushable(world, pos, next, self.push_direction, true, self.push_direction)
                || pos == self.piston_pos
            {
                return false;
            }
            if generic::push_reaction(next) == PushReaction::Destroy {
                self.to_destroy.push(pos);
                return true;
            }
            if self.to_push.len() >= PUSH_LIMIT {
                return false;
            }
            self.to_push.push(pos);
            blocks_added += 1;
            i += 1;
        }
    }

    fn reorder_list_at_collision(&mut self, blocks_added: usize, collision: usize) {
        let len = self.to_push.len();
        let mut reordered = Vec::with_capacity(len);
        reordered.extend_from_slice(&self.to_push[..collision]);
        reordered.extend_from_slice(&self.to_push[len - blocks_added..]);
        reordered.extend_from_slice(&self.to_push[collision..len - blocks_added]);
        self.to_push = reordered;
    }

    fn add_branching_blocks(&mut self, world: &impl World, from: BlockPos) -> bool {
        let from_state = state(world, from);
        for direction in DIRECTIONS {
            if axis(direction) != axis(self.push_direction) {
                let neighbor = offset(from, direction, 1);
                let neighbor_state = state(world, neighbor);
                if can_stick_to_each_other(neighbor_state, from_state)
                    && !self.add_block_line(world, neighbor, direction)
                {
                    return false;
                }
            }
        }
        true
    }
}

fn axis(face: BlockFace) -> u8 {
    match face {
        BlockFace::West | BlockFace::East => 0,
        BlockFace::Bottom | BlockFace::Top => 1,
        BlockFace::North | BlockFace::South => 2,
    }
}

/// Port of `PistonBaseBlock.getNeighborSignal`, which includes quasi-connectivity
fn get_neighbor_signal(world: &impl World, pos: BlockPos, push_direction: BlockFace) -> bool {
    let powered = |pos: BlockPos, side: BlockFace| {
        get_redstone_power(world.get_block(pos), world, pos, side) > 0
    };
    for direction in DIRECTIONS {
        if direction != push_direction && powered(offset(pos, direction, 1), direction) {
            return true;
        }
    }
    if powered(pos, BlockFace::Bottom) {
        return true;
    }
    let above = offset(pos, BlockFace::Top, 1);
    for direction in DIRECTIONS {
        if direction != BlockFace::Bottom && powered(offset(above, direction, 1), direction) {
            return true;
        }
    }
    false
}

/// Port of `PistonBaseBlock.checkIfExtend`. Has to be called when the piston is placed and when
/// one of its neighbors changes.
pub fn check_if_extend(world: &mut impl World, pos: BlockPos) {
    let Some(piston) = piston_base(state(world, pos)) else {
        return;
    };
    let direction = piston.facing;
    let extend = get_neighbor_signal(world, pos, direction);
    let ty = if extend && !piston.extended {
        if !StructureResolver::new(pos, direction, true).resolve(world) {
            return;
        }
        0
    } else if !extend && piston.extended {
        let pushed_pos = offset(pos, direction, 2);
        // A piston that retracts before the block it pushed arrived doesn't pull the block back.
        // Vanilla also looks at the progress and the game time if the world isn't being ticked at
        // the moment, but all the redstone here happens during a tick.
        let drops = is_moving_piston(state(world, pushed_pos))
            && moving_entity(world, pushed_pos)
                .is_some_and(|moving| moving.extending && moving.facing == direction as u8);
        if drops { 2 } else { 1 }
    } else {
        return;
    };
    world.piston_queue().push_event(PistonEvent {
        pos,
        sticky: piston.sticky,
        ty,
        data: direction as u8,
    });
}

/// Handles the queued block events and moves the moving pistons forward by one game tick
pub fn run_game_tick<W: World>(world: &mut W, notify: Notify<W>) {
    for _ in 0..MAX_EVENTS_PER_TICK {
        let Some(event) = world.piston_queue().events.pop_front() else {
            break;
        };
        if trigger_event(world, event, notify) {
            world.send_piston_event(event);
        }
    }
    tick_moving_pistons(world, notify);
}

/// Port of `PistonBaseBlock.triggerEvent`. Returns whether the event should be sent to the
/// players.
fn trigger_event<W: World>(world: &mut W, event: PistonEvent, notify: Notify<W>) -> bool {
    let pos = event.pos;
    let Some(piston) = piston_base(state(world, pos)) else {
        return false;
    };
    if piston.sticky != event.sticky {
        return false;
    }
    let direction = piston.facing;
    let extended_state = base_state(piston.sticky, direction, true);

    let extend = get_neighbor_signal(world, pos, direction);
    if extend && (event.ty == 1 || event.ty == 2) {
        world.set_block(pos, Block::from_id(extended_state));
        return false;
    }
    if !extend && event.ty == 0 {
        return false;
    }

    if event.ty == 0 {
        if !move_blocks(world, pos, direction, true, piston.sticky, notify) {
            return false;
        }
        world.set_block(pos, Block::from_id(extended_state));
        notify(world, pos);
    } else {
        let arm_pos = offset(pos, direction, 1);
        if moving_entity(world, arm_pos).is_some() {
            final_tick(world, arm_pos, notify);
        }

        let new_facing = face_from_data(event.data & 7);
        start_moving(
            world,
            pos,
            MovingPistonEntity {
                moved_state: base_state(piston.sticky, new_facing, false),
                moved_entity: None,
                facing: direction as u8,
                extending: false,
                source: true,
                ticks: 0,
            },
            moving_state(piston.sticky, direction),
        );
        notify(world, pos);

        if piston.sticky {
            let two_pos = offset(pos, direction, 2);
            let two_state = state(world, two_pos);
            let mut piston_piece = false;
            if is_moving_piston(two_state)
                && moving_entity(world, two_pos)
                    .is_some_and(|moving| moving.facing == direction as u8 && moving.extending)
            {
                final_tick(world, two_pos, notify);
                piston_piece = true;
            }

            if !piston_piece {
                let pullable = event.ty == 1
                    && !is_air(two_state)
                    && is_pushable(world, two_pos, two_state, direction.opposite(), false, direction)
                    && (generic::push_reaction(two_state) == PushReaction::Normal
                        || piston_base(two_state).is_some());
                if pullable {
                    move_blocks(world, pos, direction, false, true, notify);
                } else {
                    remove_block(world, arm_pos, notify);
                }
            }
        } else {
            remove_block(world, arm_pos, notify);
        }
    }
    true
}

fn remove_block<W: World>(world: &mut W, pos: BlockPos, notify: Notify<W>) {
    if world.set_block(pos, Block::Air) {
        world.delete_block_entity(pos);
        notify(world, pos);
    }
}

/// Port of `PistonBaseBlock.moveBlocks`
fn move_blocks<W: World>(
    world: &mut W,
    piston_pos: BlockPos,
    direction: BlockFace,
    extending: bool,
    sticky: bool,
    notify: Notify<W>,
) -> bool {
    let arm_pos = offset(piston_pos, direction, 1);
    if !extending && generic::state_name(state(world, arm_pos)) == PISTON_HEAD {
        world.set_block(arm_pos, Block::Air);
    }

    let mut resolver = StructureResolver::new(piston_pos, direction, extending);
    if !resolver.resolve(world) {
        return false;
    }
    let to_push = std::mem::take(&mut resolver.to_push);
    let to_destroy = std::mem::take(&mut resolver.to_destroy);
    let push_direction = resolver.push_direction();

    // Everything has to be read before the first block moves
    let states: Vec<u32> = to_push.iter().map(|pos| state(world, *pos)).collect();
    let mut entities: Vec<Option<BlockEntity>> = to_push
        .iter()
        .map(|pos| world.get_block_entity(*pos).cloned())
        .collect();
    let mut delete_after_move = to_push.clone();

    for pos in to_destroy.iter().rev() {
        world.set_block(*pos, Block::Air);
        world.delete_block_entity(*pos);
    }
    for pos in &to_push {
        world.delete_block_entity(*pos);
    }

    for i in (0..to_push.len()).rev() {
        let pos = offset(to_push[i], push_direction, 1);
        delete_after_move.retain(|other| *other != pos);
        start_moving(
            world,
            pos,
            MovingPistonEntity {
                moved_state: states[i],
                moved_entity: entities[i].take().map(Box::new),
                facing: direction as u8,
                extending,
                source: false,
                ticks: 0,
            },
            moving_state(false, direction),
        );
    }

    if extending {
        delete_after_move.retain(|other| *other != arm_pos);
        start_moving(
            world,
            arm_pos,
            MovingPistonEntity {
                moved_state: head_state(sticky, direction),
                moved_entity: None,
                facing: direction as u8,
                extending: true,
                source: true,
                ticks: 0,
            },
            moving_state(sticky, direction),
        );
    }

    for pos in delete_after_move {
        world.set_block(pos, Block::Air);
    }

    for pos in to_destroy.iter().rev() {
        notify(world, *pos);
    }
    for pos in to_push.iter().rev() {
        notify(world, *pos);
    }
    if extending {
        notify(world, arm_pos);
    }
    true
}

/// Puts a moving piston block with its block entity at `pos`
fn start_moving(world: &mut impl World, pos: BlockPos, entity: MovingPistonEntity, block: u32) {
    world.set_block(pos, Block::from_id(block));
    world.set_block_entity(pos, BlockEntity::MovingPiston(Box::new(entity)));
    let moving = &mut world.piston_queue().moving;
    moving.retain(|other| *other != pos);
    moving.push(pos);
}

/// Replaces the moving piston at `pos` with what it moved. `source` blocks vanish if the
/// movement is cut short.
fn finish<W: World>(world: &mut W, pos: BlockPos, forced: bool, notify: Notify<W>) {
    world.piston_queue().moving.retain(|other| *other != pos);
    let Some(BlockEntity::MovingPiston(entity)) = world.get_block_entity(pos).cloned() else {
        return;
    };
    world.delete_block_entity(pos);
    if !is_moving_piston(state(world, pos)) {
        return;
    }
    let new_state = if forced && entity.source {
        0
    } else {
        entity.moved_state
    };
    world.set_block(pos, Block::from_id(new_state));
    if let Some(moved_entity) = entity.moved_entity {
        world.set_block_entity(pos, *moved_entity);
    }
    notify(world, pos);
    // The block that arrived is told about its neighbors, which is what makes pistons that were
    // moved react to power
    let block = world.get_block(pos);
    update(block, world, pos);
}

/// Port of `PistonMovingBlockEntity.finalTick`
fn final_tick<W: World>(world: &mut W, pos: BlockPos, notify: Notify<W>) {
    finish(world, pos, true, notify);
}

fn tick_moving_pistons<W: World>(world: &mut W, notify: Notify<W>) {
    if world.piston_queue().moving.is_empty() {
        return;
    }
    // Movements that start while this runs are first ticked in the next game tick
    let moving = world.piston_queue().moving.clone();
    for pos in moving {
        let Some(BlockEntity::MovingPiston(entity)) = world.get_block_entity(pos) else {
            world.piston_queue().moving.retain(|other| *other != pos);
            continue;
        };
        if !is_moving_piston(state(world, pos)) {
            // The block was replaced by something else
            world.delete_block_entity(pos);
            world.piston_queue().moving.retain(|other| *other != pos);
        } else if entity.ticks >= 2 {
            finish(world, pos, false, notify);
        } else {
            let mut entity = entity.as_ref().clone();
            entity.ticks += 1;
            world.set_block_entity(pos, BlockEntity::MovingPiston(Box::new(entity)));
        }
    }
}
