//! Placement of blocks that have no dedicated variant in the `Block` enum, which is every block
//! except for the redstone components and a few more. The state for the block is derived from the
//! properties it has, in the same way the matching vanilla block classes do it.

use crate::interaction::{BlockHit, UseOnBlockContext};
use cinnabar_blocks::block_entities::BlockEntity;
use cinnabar_blocks::blocks::Block;
use cinnabar_blocks::generic;
use cinnabar_blocks::items::Item;
use cinnabar_blocks::{BlockDirection, BlockFace, BlockFacing, BlockPos};
use cinnabar_world::game_event::{block_center, EventContext, GameEvent};
use cinnabar_world::World;

/// A block state id that is being built up
struct State(u32);

impl State {
    fn has(&self, prop: &str) -> bool {
        !generic::property_values(self.0, prop).is_empty()
    }

    fn has_value(&self, prop: &str, value: &str) -> bool {
        generic::property_values(self.0, prop).contains(&value)
    }

    fn set(&mut self, prop: &str, value: &str) {
        self.0 = generic::with_property(self.0, prop, value);
    }

    fn get(&self, prop: &str) -> Option<&'static str> {
        generic::get_property(self.0, prop)
    }
}

fn direction_name(dir: BlockDirection) -> &'static str {
    match dir {
        BlockDirection::North => "north",
        BlockDirection::South => "south",
        BlockDirection::West => "west",
        BlockDirection::East => "east",
    }
}

fn facing_name(facing: BlockFacing) -> &'static str {
    match facing {
        BlockFacing::North => "north",
        BlockFacing::East => "east",
        BlockFacing::South => "south",
        BlockFacing::West => "west",
        BlockFacing::Up => "up",
        BlockFacing::Down => "down",
    }
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

fn opposite(dir: &str) -> &'static str {
    match dir {
        "north" => "south",
        "south" => "north",
        "east" => "west",
        "west" => "east",
        "up" => "down",
        _ => "up",
    }
}

fn clockwise_name(dir: &str) -> &'static str {
    match dir {
        "north" => "east",
        "east" => "south",
        "south" => "west",
        _ => "north",
    }
}

fn counter_clockwise(dir: &str) -> &'static str {
    match dir {
        "north" => "west",
        "west" => "south",
        "south" => "east",
        _ => "north",
    }
}

fn dir_from_name(name: &str) -> Option<BlockFace> {
    Some(match name {
        "north" => BlockFace::North,
        "south" => BlockFace::South,
        "west" => BlockFace::West,
        "east" => BlockFace::East,
        "up" => BlockFace::Top,
        "down" => BlockFace::Bottom,
        _ => return None,
    })
}

const HORIZONTAL: [&str; 4] = ["north", "east", "south", "west"];

fn is_pane(name: &str) -> bool {
    name.ends_with("_pane") || name.ends_with("_bars")
}

fn is_fence(name: &str) -> bool {
    name.ends_with("_fence")
}

fn is_fence_gate(name: &str) -> bool {
    name.ends_with("_fence_gate")
}

fn is_wall(name: &str) -> bool {
    name.ends_with("_wall")
}

fn is_stairs(name: &str) -> bool {
    name.ends_with("_stairs")
}

/// Blocks that are attached to the face of the block they were placed against
fn attaches_to_clicked_face(name: &str) -> bool {
    name.ends_with("end_rod")
        || name.ends_with("lightning_rod")
        || name.ends_with("shulker_box")
        || name.contains("amethyst_cluster")
        || name.contains("amethyst_bud")
}

/// Blocks that have a separate block for when they are placed on the side of another block
fn wall_variant(name: &str) -> Option<String> {
    let (namespace, name) = name.split_once(':')?;
    let variant = if let Some(prefix) = name.strip_suffix("_sign") {
        format!("{prefix}_wall_sign")
    } else if let Some(prefix) = name.strip_suffix("_hanging_sign") {
        format!("{prefix}_wall_hanging_sign")
    } else if name == "torch" {
        "wall_torch".to_owned()
    } else if let Some(prefix) = name.strip_suffix("_torch") {
        format!("{prefix}_wall_torch")
    } else if let Some(prefix) = name.strip_suffix("_banner") {
        format!("{prefix}_wall_banner")
    } else if let Some(prefix) = name.strip_suffix("_skull") {
        format!("{prefix}_wall_skull")
    } else if let Some(prefix) = name.strip_suffix("_head") {
        format!("{prefix}_wall_head")
    } else if let Some(prefix) = name.strip_suffix("_coral_fan") {
        format!("{prefix}_coral_wall_fan")
    } else {
        return None;
    };
    Some(format!("{namespace}:{variant}"))
}

/// Finds the state a block gets when the player places it. Returns air if the item is no block.
pub fn state_for_placement(item: Item, ctx: &UseOnBlockContext<'_>) -> Block {
    let item_name = item.get_name();
    let face = ctx.block_face;

    let wall = face
        .is_horizontal()
        .then(|| wall_variant(item_name))
        .flatten()
        .filter(|name| generic::default_state(name).is_some());
    let on_wall = wall.is_some();
    let name = wall.as_deref().unwrap_or(item_name);
    let Some(default) = generic::default_state(name) else {
        return Block::Air;
    };

    let mut state = State(default);
    let player_dir = ctx.player.get_direction();
    let look_facing = ctx.player.get_facing();
    let clicked_dir = face_name(face);

    if state.has("axis") {
        let axis = match face {
            BlockFace::Top | BlockFace::Bottom => "y",
            BlockFace::North | BlockFace::South => "z",
            BlockFace::East | BlockFace::West => "x",
        };
        state.set("axis", axis);
    }

    if state.has_value("face", "wall") {
        let attach = match face {
            BlockFace::Top => "floor",
            BlockFace::Bottom => "ceiling",
            _ => "wall",
        };
        state.set("face", attach);
    }

    if state.has("facing") {
        let facing = if state.has_value("facing", "up") {
            if attaches_to_clicked_face(name) {
                clicked_dir
            } else {
                opposite(facing_name(look_facing))
            }
        } else if state.get("face").is_some_and(|face| face == "wall")
            || on_wall
            || (face.is_horizontal() && (name.ends_with("_trapdoor") || name.ends_with("ladder")))
        {
            clicked_dir
        } else if state.get("face").is_some()
            || name.ends_with("_door")
            || is_fence_gate(name)
            || name.ends_with("_bed")
            || name == "minecraft:calibrated_sculk_sensor"
        {
            // Buttons and levers on the floor or ceiling, doors, gates, beds and calibrated sculk
            // sensors face the way the player is looking
            direction_name(player_dir)
        } else {
            direction_name(player_dir.opposite())
        };
        state.set("facing", facing);
    }

    let upper_half = match face {
        BlockFace::Bottom => true,
        BlockFace::Top => false,
        _ => ctx.cursor_y > 0.5,
    };
    if state.has_value("half", "top") {
        state.set("half", if upper_half { "top" } else { "bottom" });
    }
    if state.has_value("type", "double") {
        state.set("type", if upper_half { "top" } else { "bottom" });
    }

    if state.has("rotation") {
        let rotation = (((180.0 + ctx.player.yaw) * 16.0 / 360.0) + 0.5).floor() as i32 & 15;
        state.set("rotation", &rotation.to_string());
    }

    if state.has("persistent") {
        state.set("persistent", "true");
    }

    if cinnabar_redstone::rail::is_rail_name(name) {
        // Rails are straight in the direction the player looks, the connections are made when
        // the rail is placed
        let east_west = matches!(player_dir, BlockDirection::East | BlockDirection::West);
        state.set("shape", cinnabar_redstone::rail::placement_shape(east_west));
    }

    Block::from_id(state.0)
}

/// Turns two slabs of the same kind into a double slab. Returns true if that happened.
pub fn try_make_double_slab(
    world: &mut impl World,
    item: Item,
    pos: BlockPos,
    face: BlockFace,
) -> bool {
    if !item.get_name().ends_with("_slab") {
        return false;
    }
    let Block::Generic(id) = world.get_block(pos) else {
        return false;
    };
    if generic::state_name(id) != item.get_name() {
        return false;
    }
    let merges = matches!(
        (generic::get_property(id, "type"), face),
        (Some("bottom"), BlockFace::Top) | (Some("top"), BlockFace::Bottom)
    );
    if merges {
        let id = generic::with_property(id, "type", "double");
        let id = generic::with_property(id, "waterlogged", "false");
        world.set_block(pos, Block::Generic(id));
    }
    merges
}

fn generic_id(world: &impl World, pos: BlockPos) -> Option<u32> {
    match world.get_block(pos) {
        Block::Generic(id) => Some(id),
        _ => None,
    }
}

/// Blocks that fall when there is nothing below them
pub fn is_gravity_block(block: Block) -> bool {
    let Block::Generic(id) = block else {
        return false;
    };
    let name = generic::state_name(id);
    matches!(
        name,
        "minecraft:sand"
            | "minecraft:red_sand"
            | "minecraft:gravel"
            | "minecraft:suspicious_sand"
            | "minecraft:suspicious_gravel"
            | "minecraft:anvil"
            | "minecraft:chipped_anvil"
            | "minecraft:damaged_anvil"
            | "minecraft:dragon_egg"
    ) || name.ends_with("_concrete_powder")
}

/// Opens or closes doors, trapdoors and fence gates. Returns true if the block was used.
pub fn use_block(
    world: &mut impl World,
    pos: BlockPos,
    id: u32,
    item_in_hand: Option<Item>,
    hit: BlockHit,
) -> bool {
    let name = generic::state_name(id);
    if name == "minecraft:tnt"
        && item_in_hand.is_some_and(|item| {
            matches!(
                item.get_name(),
                "minecraft:flint_and_steel" | "minecraft:fire_charge"
            )
        })
    {
        world.ignite_tnt(pos);
        return true;
    }
    if name == "minecraft:chiseled_bookshelf" {
        return use_chiseled_bookshelf(world, pos, id, item_in_hand, hit);
    }
    let openable = name.ends_with("_door") || name.ends_with("_trapdoor") || is_fence_gate(name);
    // Iron doors and trapdoors can only be opened with redstone
    if !openable || name.contains("iron") {
        return false;
    }
    let state = State(id);
    let Some(open) = state.get("open") else {
        return false;
    };
    let new_open = if open == "true" { "false" } else { "true" };
    world.set_block(
        pos,
        Block::Generic(generic::with_property(id, "open", new_open)),
    );
    world.game_event(
        if new_open == "true" {
            GameEvent::BlockOpen
        } else {
            GameEvent::BlockClose
        },
        block_center(pos),
        EventContext::NONE,
    );

    // The other half of a door opens as well
    let partner_pos = match state.get("half") {
        Some("lower") if name.ends_with("_door") => Some(pos.offset(BlockFace::Top)),
        Some("upper") if name.ends_with("_door") => Some(pos.offset(BlockFace::Bottom)),
        _ => None,
    };
    if let Some(partner_pos) = partner_pos
        && let Some(partner) = generic_id(world, partner_pos)
        && generic::state_name(partner) == name
    {
        world.set_block(
            partner_pos,
            Block::Generic(generic::with_property(partner, "open", new_open)),
        );
    }
    true
}

/// The slot of the bookshelf that is at the position of the click, as a number from 0 to 5
fn bookshelf_slot(facing: &str, hit: BlockHit) -> Option<usize> {
    if face_name(hit.face) != facing {
        return None;
    }
    let (x, y, z) = hit.cursor;
    let horizontal = match facing {
        "north" => 1.0 - x,
        "south" => x,
        "west" => z,
        "east" => 1.0 - z,
        _ => return None,
    };
    let column = if horizontal < 0.375 {
        0
    } else if horizontal < 0.6875 {
        1
    } else {
        2
    };
    let row = if y >= 0.5 { 0 } else { 1 };
    Some(column + row * 3)
}

fn is_bookshelf_book(item: Item) -> bool {
    matches!(
        item.get_name(),
        "minecraft:book"
            | "minecraft:written_book"
            | "minecraft:writable_book"
            | "minecraft:enchanted_book"
            | "minecraft:knowledge_book"
    )
}

/// Puts a book into or takes one out of the slot of a chiseled bookshelf that was clicked
fn use_chiseled_bookshelf(
    world: &mut impl World,
    pos: BlockPos,
    id: u32,
    item_in_hand: Option<Item>,
    hit: BlockHit,
) -> bool {
    let Some(slot) = State(id)
        .get("facing")
        .and_then(|facing| bookshelf_slot(facing, hit))
    else {
        return false;
    };
    let property = format!("slot_{slot}_occupied");
    let occupied = generic::get_property(id, &property) == Some("true");
    if !occupied && !item_in_hand.is_some_and(is_bookshelf_book) {
        return false;
    }

    let new_id = generic::with_property(id, &property, if occupied { "false" } else { "true" });
    world.set_block(pos, Block::Generic(new_id));
    world.set_block_entity(
        pos,
        BlockEntity::ChiseledBookshelf {
            last_interacted_slot: slot as i8,
        },
    );
    cinnabar_redstone::update_surrounding_blocks(world, pos);
    true
}

/// Places the other parts of blocks that consist of multiple blocks, and updates the shape of
/// the blocks that connect to their neighbors.
pub fn after_place(world: &mut impl World, pos: BlockPos) {
    let Some(id) = generic_id(world, pos) else {
        return;
    };
    let name = generic::state_name(id);
    let state = State(id);

    // Doors and tall plants
    if state.get("half") == Some("lower") {
        let mut upper = State(id);
        upper.set("half", "upper");
        let upper_pos = pos.offset(BlockFace::Top);
        if world.get_block(upper_pos).can_place_block_in() {
            world.set_block(upper_pos, Block::Generic(upper.0));
        }
    }

    // Beds
    if state.get("part") == Some("foot")
        && let Some(facing) = state.get("facing").and_then(dir_from_name)
    {
        let mut head = State(id);
        head.set("part", "head");
        let head_pos = pos.offset(facing);
        if world.get_block(head_pos).can_place_block_in() {
            world.set_block(head_pos, Block::Generic(head.0));
        }
    }

    // Fence gates lower into walls next to them
    if is_fence_gate(name)
        && let Some(facing) = state.get("facing")
    {
        let sides = [clockwise_name(facing), counter_clockwise(facing)];
        let in_wall = sides.iter().any(|side| {
            let neighbor = world.get_block(pos.offset(dir_from_name(side).unwrap()));
            is_wall(neighbor.get_name())
        });
        let mut gate = State(id);
        gate.set("in_wall", if in_wall { "true" } else { "false" });
        world.set_block(pos, Block::Generic(gate.0));
    }

    if is_gravity_block(Block::Generic(id)) {
        world.schedule_falling_block(pos);
    }

    if is_stairs(name) || is_pane(name) || is_fence(name) || is_wall(name) {
        update_shape(world, pos);
        for dir in HORIZONTAL {
            update_shape(world, pos.offset(dir_from_name(dir).unwrap()));
        }
    }
}

/// Removes the other part of a door, tall plant or bed when one part is destroyed
pub fn remove_partner(world: &mut impl World, block: Block, pos: BlockPos) {
    let Block::Generic(id) = block else {
        return;
    };
    let state = State(id);
    let name = generic::state_name(id);
    let partner_pos = match (state.get("half"), state.get("part")) {
        (Some("lower"), _) => Some(pos.offset(BlockFace::Top)),
        (Some("upper"), _) => Some(pos.offset(BlockFace::Bottom)),
        (_, Some(part)) => state.get("facing").and_then(dir_from_name).map(|facing| {
            if part == "foot" {
                pos.offset(facing)
            } else {
                pos.offset(dir_from_name(opposite(face_name(facing))).unwrap())
            }
        }),
        _ => None,
    };
    if let Some(partner_pos) = partner_pos
        && generic_id(world, partner_pos).is_some_and(|other| generic::state_name(other) == name)
    {
        world.set_block(partner_pos, Block::Air);
    }
}

/// Removes the head of a piston that is broken and the piston of a head that is broken
pub fn remove_piston_partner(world: &mut impl World, block: Block, pos: BlockPos) {
    let Block::Generic(id) = block else {
        return;
    };
    let Some(facing) = generic::get_property(id, "facing").and_then(dir_from_name) else {
        return;
    };
    let name = generic::state_name(id);
    let partner = if name == "minecraft:piston_head" {
        let base = pos.offset(dir_from_name(opposite(face_name(facing))).unwrap());
        generic_id(world, base)
            .is_some_and(|other| {
                cinnabar_redstone::piston::piston_base(other)
                    .is_some_and(|piston| piston.extended && piston.facing == facing)
            })
            .then_some(base)
    } else if cinnabar_redstone::piston::piston_base(id).is_some_and(|piston| piston.extended) {
        let head = pos.offset(facing);
        generic_id(world, head)
            .is_some_and(|other| generic::state_name(other) == "minecraft:piston_head")
            .then_some(head)
    } else {
        None
    };
    if let Some(partner) = partner {
        world.set_block(partner, Block::Air);
    }
}

/// Updates the shape of the blocks around a position where a piston changed a block
pub fn update_shapes_around(world: &mut impl World, pos: BlockPos) {
    update_shape(world, pos);
    after_remove(world, pos);
}

/// Updates the blocks next to a position where a block was removed
pub fn after_remove(world: &mut impl World, pos: BlockPos) {
    for dir in HORIZONTAL {
        update_shape(world, pos.offset(dir_from_name(dir).unwrap()));
    }
}

fn update_shape(world: &mut impl World, pos: BlockPos) {
    let Some(id) = generic_id(world, pos) else {
        return;
    };
    let name = generic::state_name(id);
    let new_id = if is_stairs(name) {
        stairs_shape(world, pos, id)
    } else if is_pane(name) || is_fence(name) || is_wall(name) {
        connections(world, pos, id)
    } else {
        return;
    };
    if new_id != id {
        world.set_block(pos, Block::Generic(new_id));
    }
}

fn stairs_shape(world: &impl World, pos: BlockPos, id: u32) -> u32 {
    let state = State(id);
    let (Some(facing), Some(half)) = (state.get("facing"), state.get("half")) else {
        return id;
    };

    let stairs_at = |dir: &str| -> Option<(&'static str, &'static str)> {
        let neighbor = generic_id(world, pos.offset(dir_from_name(dir)?))?;
        is_stairs(generic::state_name(neighbor)).then_some(())?;
        Some((
            generic::get_property(neighbor, "facing")?,
            generic::get_property(neighbor, "half")?,
        ))
    };
    let axis_of = |dir: &str| matches!(dir, "north" | "south");
    // Whether the neighbor on the side `dir` doesn't already provide the same shape
    let can_take_shape = |dir: &str| match stairs_at(dir) {
        Some((other_facing, other_half)) => other_facing != facing || other_half != half,
        None => true,
    };

    let shape = 'shape: {
        if let Some((other_facing, other_half)) = stairs_at(facing)
            && other_half == half
            && axis_of(other_facing) != axis_of(facing)
            && can_take_shape(opposite(other_facing))
        {
            break 'shape if other_facing == counter_clockwise(facing) {
                "outer_left"
            } else {
                "outer_right"
            };
        }
        if let Some((other_facing, other_half)) = stairs_at(opposite(facing))
            && other_half == half
            && axis_of(other_facing) != axis_of(facing)
            && can_take_shape(other_facing)
        {
            break 'shape if other_facing == counter_clockwise(facing) {
                "inner_left"
            } else {
                "inner_right"
            };
        }
        "straight"
    };
    generic::with_property(id, "shape", shape)
}

/// Computes which sides a fence, wall or pane connects to
fn connections(world: &impl World, pos: BlockPos, id: u32) -> u32 {
    let name = generic::state_name(id);
    let mut state = State(id);
    let mut connected = [false; 4];

    for (idx, dir) in HORIZONTAL.iter().enumerate() {
        let neighbor = world.get_block(pos.offset(dir_from_name(dir).unwrap()));
        let neighbor_name = neighbor.get_name();
        let same_family = if is_fence(name) {
            // Wooden fences connect to each other but not to nether brick fences
            is_fence(neighbor_name)
                && (name.contains("nether_brick") == neighbor_name.contains("nether_brick"))
        } else if is_wall(name) {
            is_wall(neighbor_name) || is_pane(neighbor_name)
        } else {
            is_pane(neighbor_name) || is_wall(neighbor_name)
        };
        let gate = (is_fence(name) || is_wall(name)) && is_fence_gate(neighbor_name);
        // Fences don't connect to non-full blocks, which `is_cube` approximates
        connected[idx] = same_family || gate || neighbor.is_cube() && !neighbor.is_transparent();
    }

    for (dir, connect) in HORIZONTAL.iter().zip(connected) {
        let value = if state.has_value(dir, "low") {
            if connect {
                "low"
            } else {
                "none"
            }
        } else if connect {
            "true"
        } else {
            "false"
        };
        state.set(dir, value);
    }

    if state.has("up") {
        let [north, east, south, west] = connected;
        let straight = (north && south && !east && !west) || (east && west && !north && !south);
        state.set("up", if straight { "false" } else { "true" });
    }
    state.0
}
