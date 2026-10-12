//! Water and lava, modelled on the `FlowingFluid` class of the game.
//!
//! The simulation itself only needs a [`World`] and reports the positions it touched, so it can
//! be tested without a plot. The [`Plot`] part drives it at 20 ticks per second, independent of
//! the tick rate of the redstone in the plot, the same way entities are ticked.
//!
//! Differences from the game: fluids only know the bounding box of the collision shape of a block,
//! lava doesn't randomly delay its spreading and the lava specific random ticks (fire) are left
//! out. The blocks with a dedicated variant that can be waterlogged (like signs) are not
//! waterloggable and get washed away instead.

use super::physics::{raycast, Vec3};
use super::{Plot, PLOT_BLOCK_HEIGHT};
use crate::interaction;
use crate::player::PacketSender;
use mchprs_blocks::blocks::Block;
use mchprs_blocks::{generic, BlockFace, BlockPos};
use mchprs_network::packets::serverbound::SUseItem;
use mchprs_proc_macros::protocol_id;
use mchprs_redstone as redstone;
use mchprs_world::World;
use rustc_hash::{FxHashMap, FxHashSet};
use std::collections::BTreeMap;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

const TICK_INTERVAL: Duration = Duration::from_millis(50);
/// How many ticks are caught up on at most after the plot was lagging
const MAX_CATCH_UP_TICKS: u32 = 5;
/// How many fluid blocks are updated per tick at most. What is left is done in the next tick.
const MAX_UPDATES_PER_TICK: usize = 2000;
/// How many positions are checked for fluid neighbors per tick at most
const MAX_WAKES_PER_TICK: usize = 20000;
const REACH: f64 = 5.0;
const SOUND_CATEGORY_BLOCKS: i32 = 4;

const HORIZONTAL: [BlockFace; 4] = [
    BlockFace::North,
    BlockFace::East,
    BlockFace::South,
    BlockFace::West,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Water,
    Lava,
}

impl Kind {
    /// How many ticks it takes for the fluid to spread one block
    pub fn tick_delay(self) -> u64 {
        match self {
            Kind::Water => 5,
            Kind::Lava => 30,
        }
    }

    /// How much less of the fluid there is in the next block
    fn drop_off(self) -> i32 {
        match self {
            Kind::Water => 1,
            Kind::Lava => 2,
        }
    }

    /// How far to look for a place to flow to
    fn slope_find_distance(self) -> i32 {
        match self {
            Kind::Water => 4,
            Kind::Lava => 2,
        }
    }

    /// Whether two sources make a new one
    fn converts_to_source(self) -> bool {
        self == Kind::Water
    }
}

/// The amount of fluid in a block. Sources and falling fluid have the full amount of 8.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FluidState {
    pub kind: Kind,
    pub amount: i32,
    pub source: bool,
    pub falling: bool,
}

impl FluidState {
    fn source(kind: Kind) -> FluidState {
        FluidState {
            kind,
            amount: 8,
            source: true,
            falling: false,
        }
    }

    fn falling(kind: Kind) -> FluidState {
        FluidState {
            kind,
            amount: 8,
            source: false,
            falling: true,
        }
    }

    fn flowing(kind: Kind, amount: i32) -> FluidState {
        FluidState {
            kind,
            amount,
            source: false,
            falling: false,
        }
    }

    /// The value of the `level` property of the block
    fn level(self) -> u32 {
        if self.source {
            0
        } else if self.falling {
            8
        } else {
            8 - self.amount as u32
        }
    }

    fn from_level(kind: Kind, level: u32) -> FluidState {
        match level {
            0 => FluidState::source(kind),
            1..=7 => FluidState::flowing(kind, 8 - level as i32),
            _ => FluidState::falling(kind),
        }
    }

    fn block_id(self) -> u32 {
        let first = match self.kind {
            Kind::Water => *WATER_FIRST,
            Kind::Lava => *LAVA_FIRST,
        };
        first + self.level()
    }
}

fn first_state(name: &str) -> u32 {
    generic::block(generic::default_state(name).unwrap())
        .unwrap()
        .first_id
}

// `level` is the only property of water and lava, so the state ids are in the order of the levels
static WATER_FIRST: LazyLock<u32> = LazyLock::new(|| first_state("minecraft:water"));
static LAVA_FIRST: LazyLock<u32> = LazyLock::new(|| first_state("minecraft:lava"));

fn name_of(id: u32) -> &'static str {
    match generic::state_name(id) {
        // Blocks with a dedicated variant are not in the generic tables
        "minecraft:air" if id != 0 => Block::from_id(id).get_name(),
        name => name,
    }
}

fn is_air_id(id: u32) -> bool {
    id == 0 || matches!(name_of(id), "minecraft:cave_air" | "minecraft:void_air")
}

fn has_waterlogged(id: u32) -> bool {
    !generic::property_values(id, "waterlogged").is_empty()
}

fn is_waterlogged(id: u32) -> bool {
    has_waterlogged(id) && generic::get_property(id, "waterlogged") == Some("true")
}

/// Whether the block can take water by being waterlogged, which is not the case for double slabs
fn is_waterloggable(id: u32) -> bool {
    has_waterlogged(id) && generic::get_property(id, "type") != Some("double")
}

/// The fluid that is in a block with this state, which includes waterlogged blocks
pub fn fluid_at_id(id: u32) -> Option<FluidState> {
    let water = id.wrapping_sub(*WATER_FIRST);
    if water < 16 {
        return Some(FluidState::from_level(Kind::Water, water));
    }
    let lava = id.wrapping_sub(*LAVA_FIRST);
    if lava < 16 {
        return Some(FluidState::from_level(Kind::Lava, lava));
    }
    is_waterlogged(id).then(|| FluidState::source(Kind::Water))
}

/// Whether the fluid has to be updated when a block next to it changes
pub fn holds_fluid(id: u32) -> bool {
    fluid_at_id(id).is_some()
}

fn raw(world: &impl World, pos: BlockPos) -> u32 {
    if !(0..PLOT_BLOCK_HEIGHT).contains(&pos.y) {
        return 0;
    }
    world.get_block_raw(pos)
}

pub fn fluid_at(world: &impl World, pos: BlockPos) -> Option<FluidState> {
    fluid_at_id(raw(world, pos))
}

fn opposite(face: BlockFace) -> BlockFace {
    match face {
        BlockFace::Bottom => BlockFace::Top,
        BlockFace::Top => BlockFace::Bottom,
        BlockFace::North => BlockFace::South,
        BlockFace::South => BlockFace::North,
        BlockFace::West => BlockFace::East,
        BlockFace::East => BlockFace::West,
    }
}

/// The axis (0 = x, 1 = y, 2 = z) a face is on and whether it is on the positive side
fn axis_of(face: BlockFace) -> (usize, bool) {
    match face {
        BlockFace::Bottom => (1, false),
        BlockFace::Top => (1, true),
        BlockFace::North => (2, false),
        BlockFace::South => (2, true),
        BlockFace::West => (0, false),
        BlockFace::East => (0, true),
    }
}

/// The part of the face of the box on the side of `face` as an interval on each of the other two
/// axes, if the box reaches the edge of the block there
fn face_of(bb: Option<[f64; 6]>, face: BlockFace) -> Option<[(f64, f64); 2]> {
    let bb = bb?;
    let (axis, positive) = axis_of(face);
    let touches = if positive {
        bb[axis + 3] >= 1.0
    } else {
        bb[axis] <= 0.0
    };
    if !touches {
        return None;
    }
    let mut others = (0..3).filter(|a| *a != axis).map(|a| (bb[a], bb[a + 3]));
    Some([others.next()?, others.next()?])
}

/// Whether the two faces together cover the whole side of the block
fn faces_cover(a: [(f64, f64); 2], b: [(f64, f64); 2]) -> bool {
    let full = |i: (f64, f64)| i.0 <= 0.0 && i.1 >= 1.0;
    let joined = |i: (f64, f64), j: (f64, f64)| {
        i.0.min(j.0) <= 0.0 && i.1.max(j.1) >= 1.0 && i.0.max(j.0) <= i.1.min(j.1)
    };
    (full(a[0]) && full(a[1]))
        || (full(b[0]) && full(b[1]))
        || (full(a[0]) && full(b[0]) && joined(a[1], b[1]))
        || (full(a[1]) && full(b[1]) && joined(a[0], b[0]))
}

fn is_full_block(bb: Option<[f64; 6]>) -> bool {
    bb == Some([0.0, 0.0, 0.0, 1.0, 1.0, 1.0])
}

/// Whether fluid can get from the block with the state `from` into the neighboring block in
/// the direction `dir`, which depends on the shapes of the blocks
fn can_pass_through_wall(dir: BlockFace, from: u32, to: u32) -> bool {
    let to_box = generic::collision_box(to);
    if is_full_block(to_box) {
        return false;
    }
    let from_box = generic::collision_box(from);
    if is_full_block(from_box) {
        return false;
    }
    match (face_of(from_box, dir), face_of(to_box, opposite(dir))) {
        (Some(a), Some(b)) => !faces_cover(a, b),
        _ => true,
    }
}

/// Whether the block could hold fluid when it's not waterlogged, which is the case for blocks
/// the player can walk through
fn can_hold_any_fluid(id: u32) -> bool {
    if is_waterloggable(id) {
        return true;
    }
    let name = name_of(id);
    if name.ends_with("_door")
        || name.ends_with("_sign")
        || name.ends_with("_hanging_sign")
        || matches!(
            name,
            "minecraft:ladder"
                | "minecraft:sugar_cane"
                | "minecraft:bubble_column"
                | "minecraft:nether_portal"
                | "minecraft:end_portal"
                | "minecraft:end_gateway"
                | "minecraft:structure_void"
        )
    {
        return false;
    }
    generic::collision_box(id).is_none()
}

/// Whether the fluid can be put into the block, which depends on waterlogging
fn can_hold_specific_fluid(id: u32, kind: Kind) -> bool {
    if is_waterloggable(id) {
        kind == Kind::Water && !is_waterlogged(id)
    } else {
        true
    }
}

fn can_hold_fluid(id: u32, kind: Kind) -> bool {
    can_hold_any_fluid(id) && can_hold_specific_fluid(id, kind)
}

fn is_source_of(fluid: Option<FluidState>, kind: Kind) -> bool {
    fluid.is_some_and(|f| f.kind == kind && f.source)
}

fn can_maybe_pass_through(
    kind: Kind,
    from_id: u32,
    dir: BlockFace,
    to_id: u32,
    to_fluid: Option<FluidState>,
) -> bool {
    !is_source_of(to_fluid, kind)
        && can_hold_any_fluid(to_id)
        && can_pass_through_wall(dir, from_id, to_id)
}

fn can_pass_through(
    kind: Kind,
    from_id: u32,
    dir: BlockFace,
    to_id: u32,
    to_fluid: Option<FluidState>,
) -> bool {
    can_maybe_pass_through(kind, from_id, dir, to_id, to_fluid)
        && can_hold_specific_fluid(to_id, kind)
}

/// What the fluid in the block at the position should be, given its neighbors
fn new_liquid(world: &impl World, pos: BlockPos, id: u32, kind: Kind) -> Option<FluidState> {
    let mut highest = 0;
    let mut sources = 0;
    for dir in HORIZONTAL {
        let neighbor_id = raw(world, pos.offset(dir));
        if let Some(fluid) = fluid_at_id(neighbor_id)
            && fluid.kind == kind
            && can_pass_through_wall(dir, id, neighbor_id)
        {
            if fluid.source {
                sources += 1;
            }
            highest = highest.max(fluid.amount);
        }
    }

    if sources >= 2 && kind.converts_to_source() {
        let below_id = raw(world, pos.offset(BlockFace::Bottom));
        if generic::is_solid(below_id)
            || generic::is_cube(below_id)
            || is_source_of(fluid_at_id(below_id), kind)
        {
            return Some(FluidState::source(kind));
        }
    }

    let above_id = raw(world, pos.offset(BlockFace::Top));
    if fluid_at_id(above_id).is_some_and(|f| f.kind == kind)
        && can_pass_through_wall(BlockFace::Top, id, above_id)
    {
        return Some(FluidState::falling(kind));
    }

    let amount = highest - kind.drop_off();
    (amount > 0).then(|| FluidState::flowing(kind, amount))
}

/// Whether the fluid at the position can flow down there
fn is_hole(world: &impl World, pos: BlockPos, id: u32, kind: Kind) -> bool {
    let below = pos.offset(BlockFace::Bottom);
    let below_id = raw(world, below);
    if !can_pass_through_wall(BlockFace::Bottom, id, below_id) {
        return false;
    }
    fluid_at_id(below_id).is_some_and(|f| f.kind == kind) || can_hold_fluid(below_id, kind)
}

/// Remembers which positions around the spreading fluid are holes
struct SpreadContext<'a, W: World> {
    world: &'a W,
    origin: BlockPos,
    kind: Kind,
    holes: FxHashMap<(i32, i32), bool>,
}

impl<W: World> SpreadContext<'_, W> {
    fn is_hole(&mut self, pos: BlockPos, id: u32) -> bool {
        let key = (pos.x - self.origin.x, pos.z - self.origin.z);
        let (world, kind) = (self.world, self.kind);
        *self
            .holes
            .entry(key)
            .or_insert_with(|| is_hole(world, pos, id, kind))
    }
}

/// How many blocks the fluid has to flow from the position to reach a hole, or 1000
fn slope_distance<W: World>(
    ctx: &mut SpreadContext<'_, W>,
    pos: BlockPos,
    pass: i32,
    from: BlockFace,
    id: u32,
) -> i32 {
    let mut lowest = 1000;
    for dir in HORIZONTAL {
        if dir == from {
            continue;
        }
        let test = pos.offset(dir);
        let test_id = raw(ctx.world, test);
        if can_pass_through(ctx.kind, id, dir, test_id, fluid_at_id(test_id)) {
            if ctx.is_hole(test, test_id) {
                return pass;
            }
            if pass < ctx.kind.slope_find_distance() {
                let distance = slope_distance(ctx, test, pass + 1, opposite(dir), test_id);
                lowest = lowest.min(distance);
            }
        }
    }
    lowest
}

/// Whether the existing fluid gets replaced when the other fluid flows into the block
fn can_be_replaced(
    world: &impl World,
    pos: BlockPos,
    existing: Option<FluidState>,
    other: Kind,
    dir: BlockFace,
) -> bool {
    match existing {
        None => true,
        Some(fluid) if fluid.kind == Kind::Water => {
            dir == BlockFace::Bottom && other != Kind::Water
        }
        Some(fluid) => {
            let same_above =
                fluid_at(world, pos.offset(BlockFace::Top)).is_some_and(|f| f.kind == Kind::Lava);
            // Lava is replaced by water once it's at least 4/9 of a block high
            (same_above || fluid.amount >= 4) && other == Kind::Water
        }
    }
}

/// The directions the fluid at the position flows to: the ones with the shortest way to a hole
fn spread_directions(
    world: &impl World,
    pos: BlockPos,
    id: u32,
    kind: Kind,
) -> Vec<(BlockFace, FluidState)> {
    let mut lowest = 1000;
    let mut result = Vec::new();
    let mut ctx = SpreadContext {
        world,
        origin: pos,
        kind,
        holes: FxHashMap::default(),
    };
    for dir in HORIZONTAL {
        let test = pos.offset(dir);
        let test_id = raw(world, test);
        let test_fluid = fluid_at_id(test_id);
        if !can_maybe_pass_through(kind, id, dir, test_id, test_fluid) {
            continue;
        }
        let Some(new_fluid) = new_liquid(world, test, test_id, kind) else {
            continue;
        };
        if !can_hold_specific_fluid(test_id, new_fluid.kind) {
            continue;
        }
        let distance = if ctx.is_hole(test, test_id) {
            0
        } else {
            slope_distance(&mut ctx, test, 1, opposite(dir), test_id)
        };
        if distance < lowest {
            result.clear();
        }
        if distance <= lowest {
            if can_be_replaced(world, test, test_fluid, new_fluid.kind, dir) {
                result.push((dir, new_fluid));
            }
            lowest = distance;
        }
    }
    result
}

/// Adds the position and its neighbors to the positions that have to be looked at again
fn touch(out: &mut Vec<BlockPos>, pos: BlockPos) {
    out.push(pos);
    out.extend(BlockFace::values().map(|face| pos.offset(face)));
}

/// Puts fluid into the block at the position, replacing what is there
pub fn place_fluid(
    world: &mut impl World,
    pos: BlockPos,
    fluid: FluidState,
    out: &mut Vec<BlockPos>,
) {
    let old_id = raw(world, pos);
    if is_waterloggable(old_id) {
        if fluid.kind == Kind::Water && !is_waterlogged(old_id) {
            let id = generic::with_property(old_id, "waterlogged", "true");
            world.set_block_raw(pos, id);
        }
        touch(out, pos);
        return;
    }

    let old_block = world.get_block(pos);
    let replaced = !is_air_id(old_id) && fluid_at_id(old_id).is_none();
    if replaced {
        world.delete_block_entity(pos);
    }
    world.set_block_raw(pos, fluid.block_id());
    if replaced {
        if let Block::RedstoneWire { .. } = old_block {
            redstone::update_wire_neighbors(world, pos);
        }
        interaction::change_neighbors(world, pos);
    }
    touch(out, pos);
}

fn remove_fluid(world: &mut impl World, pos: BlockPos, out: &mut Vec<BlockPos>) {
    world.set_block_raw(pos, 0);
    touch(out, pos);
}

fn spread_to(
    world: &mut impl World,
    pos: BlockPos,
    dir: BlockFace,
    fluid: FluidState,
    out: &mut Vec<BlockPos>,
) {
    if dir == BlockFace::Bottom && fluid.kind == Kind::Lava {
        // Lava that flows onto water turns it into stone
        let id = raw(world, pos);
        if fluid_at_id(id).is_some_and(|f| f.kind == Kind::Water) {
            if !is_waterlogged(id) {
                world.set_block_raw(pos, generic::default_state("minecraft:stone").unwrap());
                touch(out, pos);
            }
            return;
        }
    }
    place_fluid(world, pos, fluid, out);
}

fn spread_to_sides(
    world: &mut impl World,
    pos: BlockPos,
    id: u32,
    fluid: FluidState,
    out: &mut Vec<BlockPos>,
) {
    let next = if fluid.falling {
        7
    } else {
        fluid.amount - fluid.kind.drop_off()
    };
    if next <= 0 {
        return;
    }
    for (dir, new_fluid) in spread_directions(world, pos, id, fluid.kind) {
        spread_to(world, pos.offset(dir), dir, new_fluid, out);
    }
}

fn spread(
    world: &mut impl World,
    pos: BlockPos,
    id: u32,
    fluid: FluidState,
    out: &mut Vec<BlockPos>,
) {
    let kind = fluid.kind;
    let below = pos.offset(BlockFace::Bottom);
    let below_id = raw(world, below);
    let below_fluid = fluid_at_id(below_id);
    if can_maybe_pass_through(kind, id, BlockFace::Bottom, below_id, below_fluid)
        && let Some(new_below) = new_liquid(world, below, below_id, kind)
        && can_be_replaced(world, below, below_fluid, new_below.kind, BlockFace::Bottom)
        && can_hold_specific_fluid(below_id, new_below.kind)
    {
        spread_to(world, below, BlockFace::Bottom, new_below, out);
        let sources = HORIZONTAL
            .iter()
            .filter(|dir| is_source_of(fluid_at(world, pos.offset(**dir)), kind))
            .count();
        if sources >= 3 {
            spread_to_sides(world, pos, id, fluid, out);
        }
        return;
    }

    if fluid.source || !is_hole(world, pos, id, kind) {
        spread_to_sides(world, pos, id, fluid, out);
    }
}

/// Updates the fluid at the position the way the game does when a scheduled tick of it is due.
/// The positions that have to be looked at again are appended to `out`.
pub fn tick(world: &mut impl World, pos: BlockPos, out: &mut Vec<BlockPos>) {
    let id = raw(world, pos);
    let Some(mut fluid) = fluid_at_id(id) else {
        return;
    };
    if !fluid.source {
        match new_liquid(world, pos, id, fluid.kind) {
            None => {
                remove_fluid(world, pos, out);
                return;
            }
            Some(new_fluid) if new_fluid != fluid => {
                fluid = new_fluid;
                world.set_block_raw(pos, fluid.block_id());
                touch(out, pos);
            }
            Some(_) => {}
        }
    }
    let id = raw(world, pos);
    spread(world, pos, id, fluid, out);
}

fn change_block(world: &mut impl World, pos: BlockPos, name: &str) {
    world.set_block_raw(pos, generic::default_state(name).unwrap());
    world.delete_block_entity(pos);
    interaction::change_neighbors(world, pos);
}

/// Lets lava that is next to water turn into stone. Returns the fluid at the position if there is
/// still one after that.
pub fn wake(world: &mut impl World, pos: BlockPos) -> Option<FluidState> {
    let id = raw(world, pos);
    let fluid = fluid_at_id(id)?;
    if fluid.kind != Kind::Lava || id.wrapping_sub(*LAVA_FIRST) >= 16 {
        return Some(fluid);
    }

    let over_soul_soil =
        name_of(raw(world, pos.offset(BlockFace::Bottom))) == "minecraft:soul_soil";
    for dir in [
        BlockFace::Top,
        BlockFace::North,
        BlockFace::South,
        BlockFace::East,
        BlockFace::West,
    ] {
        let neighbor_id = raw(world, pos.offset(dir));
        if fluid_at_id(neighbor_id).is_some_and(|f| f.kind == Kind::Water) {
            let result = if fluid.source {
                "minecraft:obsidian"
            } else {
                "minecraft:cobblestone"
            };
            change_block(world, pos, result);
            return None;
        }
        if over_soul_soil && name_of(neighbor_id) == "minecraft:blue_ice" {
            change_block(world, pos, "minecraft:basalt");
            return None;
        }
    }
    Some(fluid)
}

/// Makes a block that is placed into water waterlogged if it can be
pub fn waterlog_on_place(world: &mut impl World, pos: BlockPos, block: Block) -> Block {
    let Block::Generic(id) = block else {
        return block;
    };
    if !is_waterloggable(id)
        || !fluid_at(world, pos).is_some_and(|f| f.kind == Kind::Water && f.amount == 8)
    {
        return block;
    }
    world.schedule_fluid_tick(pos);
    Block::Generic(generic::with_property(id, "waterlogged", "true"))
}

/// The fluid that scheduled ticks are waiting for
#[derive(Default)]
pub struct Fluids {
    tick: u64,
    queue: BTreeMap<u64, Vec<BlockPos>>,
    scheduled: FxHashSet<BlockPos>,
    last_tick: Option<Instant>,
}

impl Fluids {
    fn schedule(&mut self, pos: BlockPos, delay: u64) {
        if self.scheduled.insert(pos) {
            self.queue.entry(self.tick + delay).or_default().push(pos);
        }
    }

    /// Takes the positions whose tick is due, up to the limit of updates per tick
    fn take_due(&mut self) -> Vec<BlockPos> {
        let mut due = Vec::new();
        while let Some(mut entry) = self.queue.first_entry() {
            if *entry.key() > self.tick || due.len() >= MAX_UPDATES_PER_TICK {
                break;
            }
            let room = MAX_UPDATES_PER_TICK - due.len();
            let positions = entry.get_mut();
            let split = positions.len().saturating_sub(room);
            due.extend(positions.drain(split..));
            if positions.is_empty() {
                entry.remove();
            }
        }
        for pos in &due {
            self.scheduled.remove(pos);
        }
        due
    }
}

impl Plot {
    /// Schedules the ticks of the fluids that were woken up since the last time. Lava that touches
    /// water turns into stone at this point.
    fn process_fluid_wakes(&mut self, mut work: Vec<BlockPos>) {
        let mut checked = 0;
        while let Some(pos) = work.pop() {
            if checked >= MAX_WAKES_PER_TICK {
                // The rest is looked at in the next tick
                self.world.pending_fluids.insert(pos);
                self.world.pending_fluids.extend(work.drain(..));
                break;
            }
            checked += 1;
            if let Some(fluid) = wake(&mut self.world, pos) {
                self.fluids.schedule(pos, fluid.kind.tick_delay());
            }
            // Changes that happened because of the wake up have been queued in the world
            work.extend(std::mem::take(&mut self.world.pending_fluids));
        }
    }

    fn tick_fluids_once(&mut self) {
        self.fluids.tick += 1;
        let due = self.fluids.take_due();
        if due.is_empty() {
            return;
        }
        self.reset_redpiler();
        let mut out = Vec::new();
        for pos in due {
            tick(&mut self.world, pos, &mut out);
        }
        let mut work = std::mem::take(&mut self.world.pending_fluids);
        work.extend(out);
        self.process_fluid_wakes(work.into_iter().collect());
    }

    /// Ticks the fluids of the plot if enough time has passed since the last tick
    pub(super) fn tick_fluids(&mut self) {
        if !self.world.pending_fluids.is_empty() {
            let work = std::mem::take(&mut self.world.pending_fluids);
            if self.redpiler.is_active() {
                self.reset_redpiler();
            }
            self.process_fluid_wakes(work.into_iter().collect());
        }
        if self.fluids.queue.is_empty() {
            self.fluids.last_tick = None;
            return;
        }

        let now = Instant::now();
        let mut last_tick = *self.fluids.last_tick.get_or_insert(now);
        let mut ticks = 0;
        while last_tick + TICK_INTERVAL <= now && ticks < MAX_CATCH_UP_TICKS {
            last_tick += TICK_INTERVAL;
            ticks += 1;
            self.tick_fluids_once();
        }
        self.fluids.last_tick = Some(if ticks == MAX_CATCH_UP_TICKS {
            // The plot is too far behind to catch up
            now
        } else {
            last_tick
        });
    }

    fn in_plot(&self, pos: BlockPos) -> bool {
        (0..PLOT_BLOCK_HEIGHT).contains(&pos.y)
            && Plot::in_plot_bounds(self.world.x, self.world.z, pos.x, pos.z)
    }

    fn play_bucket_sound(&mut self, pos: BlockPos, sound: i32) {
        self.world
            .play_sound(pos, sound, SOUND_CATEGORY_BLOCKS, 1.0, 1.0);
    }

    /// Handles a player using a bucket while not pointing at a block it could be used on
    pub(super) fn use_bucket(&mut self, player: usize, item_name: &str, use_item: &SUseItem) {
        if !matches!(
            item_name,
            "minecraft:bucket" | "minecraft:water_bucket" | "minecraft:lava_bucket"
        ) {
            return;
        }
        let owner = &self.players[player];
        let eye_height = if owner.crouching { 1.27 } else { 1.62 };
        let eye = Vec3::new(owner.pos.x, owner.pos.y + eye_height, owner.pos.z);
        let dir = Vec3::from_rotation(use_item.yaw, use_item.pitch);

        match self.redpiler.current_flags() {
            Some(flags) if flags.io_only => {
                self.players[player].send_error_message(super::ERROR_IO_ONLY);
                return;
            }
            _ => {}
        }

        let changed = match item_name {
            "minecraft:bucket" => self.pick_up_fluid(eye, dir),
            "minecraft:water_bucket" => self.empty_bucket(eye, dir, Kind::Water),
            _ => self.empty_bucket(eye, dir, Kind::Lava),
        };
        if changed {
            self.world.flush_block_changes();
        }
    }

    /// Takes the fluid source the player looks at. Buckets are not consumed in creative mode, so
    /// the bucket in the hand stays what it is.
    fn pick_up_fluid(&mut self, eye: Vec3, dir: Vec3) -> bool {
        let mut origin = eye;
        let mut remaining = REACH;
        // Flowing fluid is looked through
        for _ in 0..16 {
            let Some(hit) = raycast(&self.world, origin, dir, remaining, true) else {
                return false;
            };
            let id = raw(&self.world, hit.pos);
            match fluid_at_id(id) {
                Some(fluid) if fluid.source => {
                    if !self.in_plot(hit.pos) {
                        return false;
                    }
                    self.reset_redpiler();
                    let new_id = if is_waterlogged(id) {
                        generic::with_property(id, "waterlogged", "false")
                    } else {
                        0
                    };
                    self.world.set_block_raw(hit.pos, new_id);
                    let mut out = Vec::new();
                    touch(&mut out, hit.pos);
                    for pos in out {
                        self.world.schedule_fluid_tick(pos);
                    }
                    let sound = match fluid.kind {
                        Kind::Water => {
                            protocol_id!("minecraft:sound_event", "minecraft:item.bucket.fill")
                        }
                        Kind::Lava => {
                            protocol_id!("minecraft:sound_event", "minecraft:item.bucket.fill_lava")
                        }
                    };
                    self.play_bucket_sound(hit.pos, sound);
                    return true;
                }
                Some(_) if !is_waterlogged(id) => {
                    let travelled = (hit.point - origin).length() + 0.01;
                    origin = hit.point + dir.normalized() * 0.01;
                    remaining -= travelled;
                    if remaining <= 0.0 {
                        return false;
                    }
                }
                _ => return false,
            }
        }
        false
    }

    /// Puts the fluid of the bucket where the player looks
    fn empty_bucket(&mut self, eye: Vec3, dir: Vec3, kind: Kind) -> bool {
        let Some(hit) = raycast(&self.world, eye, dir, REACH, false) else {
            return false;
        };
        let clicked_id = raw(&self.world, hit.pos);
        let clicked_takes_fluid = if is_waterloggable(clicked_id) {
            can_hold_specific_fluid(clicked_id, kind)
        } else {
            self.world.get_block(hit.pos).can_place_block_in()
        };
        let target = if clicked_takes_fluid {
            hit.pos
        } else {
            hit.pos.offset(hit.face)
        };
        if !self.in_plot(target) {
            return false;
        }
        let target_id = raw(&self.world, target);
        let fits = if is_waterloggable(target_id) {
            can_hold_specific_fluid(target_id, kind)
        } else {
            self.world.get_block(target).can_place_block_in()
                && !is_source_of(fluid_at_id(target_id), kind)
        };
        if !fits {
            return false;
        }

        self.reset_redpiler();
        let mut out = Vec::new();
        place_fluid(&mut self.world, target, FluidState::source(kind), &mut out);
        for pos in out {
            self.world.schedule_fluid_tick(pos);
        }
        let sound = match kind {
            Kind::Water => protocol_id!("minecraft:sound_event", "minecraft:item.bucket.empty"),
            Kind::Lava => protocol_id!("minecraft:sound_event", "minecraft:item.bucket.empty_lava"),
        };
        self.play_bucket_sound(target, sound);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mchprs_world::testing::TestWorld;
    use std::collections::VecDeque;

    fn water(level: u32) -> u32 {
        *WATER_FIRST + level
    }

    fn lava(level: u32) -> u32 {
        *LAVA_FIRST + level
    }

    fn stone() -> u32 {
        generic::default_state("minecraft:stone").unwrap()
    }

    /// A world with a stone floor at y = 0
    fn flat_world() -> TestWorld {
        let mut world = TestWorld::new(2, 1, 2);
        for x in 0..32 {
            for z in 0..32 {
                world.set_block_raw(BlockPos::new(x, 0, z), stone());
            }
        }
        world
    }

    /// Runs the updates of the fluids until nothing changes anymore
    fn settle(world: &mut TestWorld, start: &[BlockPos]) {
        let mut queue: VecDeque<BlockPos> = start.iter().copied().collect();
        for _ in 0..100_000 {
            let Some(pos) = queue.pop_front() else {
                return;
            };
            let mut out = Vec::new();
            tick(world, pos, &mut out);
            for pos in out {
                if wake(world, pos).is_some() && !queue.contains(&pos) {
                    queue.push_back(pos);
                }
            }
        }
        panic!("the fluids never settled");
    }

    fn at(world: &TestWorld, x: i32, y: i32, z: i32) -> u32 {
        world.get_block_raw(BlockPos::new(x, y, z))
    }

    #[test]
    fn level_roundtrip() {
        for level in 0..16 {
            for kind in [Kind::Water, Kind::Lava] {
                let fluid = FluidState::from_level(kind, level);
                assert_eq!(fluid.level(), if level > 8 { 8 } else { level });
                assert_eq!(
                    fluid_at_id(fluid.block_id()),
                    Some(FluidState::from_level(kind, fluid.level()))
                );
            }
        }
        assert_eq!(FluidState::from_level(Kind::Water, 1).amount, 7);
        assert_eq!(FluidState::from_level(Kind::Water, 8).amount, 8);
        assert_eq!(fluid_at_id(stone()), None);
    }

    #[test]
    fn water_spreads_seven_blocks() {
        let mut world = flat_world();
        let source = BlockPos::new(16, 1, 16);
        world.set_block_raw(source, water(0));
        settle(&mut world, &[source]);

        assert_eq!(at(&world, 16, 1, 16), water(0));
        for distance in 1..=7 {
            assert_eq!(at(&world, 16 + distance, 1, 16), water(distance as u32));
            assert_eq!(at(&world, 16, 1, 16 - distance), water(distance as u32));
        }
        assert_eq!(at(&world, 16 + 8, 1, 16), 0);
        // Fluid flows around a corner, so the reach is the walking distance
        assert_eq!(at(&world, 16 + 4, 1, 16 + 3), water(7));
        assert_eq!(at(&world, 16 + 4, 1, 16 + 4), 0);
    }

    #[test]
    fn lava_spreads_three_blocks() {
        let mut world = flat_world();
        let source = BlockPos::new(16, 1, 16);
        world.set_block_raw(source, lava(0));
        settle(&mut world, &[source]);

        assert_eq!(at(&world, 17, 1, 16), lava(2));
        assert_eq!(at(&world, 18, 1, 16), lava(4));
        assert_eq!(at(&world, 19, 1, 16), lava(6));
        assert_eq!(at(&world, 20, 1, 16), 0);
    }

    #[test]
    fn water_falls_and_spreads_at_the_bottom() {
        let mut world = flat_world();
        // A shaft keeps the source from spreading sideways in the air
        for y in 2..=5 {
            for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                world.set_block_raw(BlockPos::new(16 + dx, y, 16 + dz), stone());
            }
        }
        let source = BlockPos::new(16, 5, 16);
        world.set_block_raw(source, water(0));
        settle(&mut world, &[source]);

        for y in 2..=4 {
            assert_eq!(at(&world, 16, y, 16), water(8));
        }
        // The column lands on the floor with full strength
        assert_eq!(at(&world, 16, 1, 16), water(8));
        assert_eq!(at(&world, 17, 1, 16), water(1));
    }

    #[test]
    fn two_sources_make_a_third() {
        let mut world = flat_world();
        let (a, b) = (BlockPos::new(10, 1, 10), BlockPos::new(12, 1, 10));
        world.set_block_raw(a, water(0));
        world.set_block_raw(b, water(0));
        settle(&mut world, &[a, b]);
        assert_eq!(at(&world, 11, 1, 10), water(0));

        // Lava doesn't do that
        let (a, b) = (BlockPos::new(10, 1, 20), BlockPos::new(12, 1, 20));
        world.set_block_raw(a, lava(0));
        world.set_block_raw(b, lava(0));
        settle(&mut world, &[a, b]);
        assert_ne!(at(&world, 11, 1, 20), lava(0));
    }

    #[test]
    fn flowing_water_dries_up_without_source() {
        let mut world = flat_world();
        let source = BlockPos::new(16, 1, 16);
        world.set_block_raw(source, water(0));
        settle(&mut world, &[source]);

        world.set_block_raw(source, 0);
        let start: Vec<BlockPos> = (-7..=7)
            .flat_map(|x| (-7..=7).map(move |z| BlockPos::new(16 + x, 1, 16 + z)))
            .collect();
        settle(&mut world, &start);
        for pos in start {
            assert_eq!(world.get_block_raw(pos), 0, "{pos} still has water");
        }
    }

    #[test]
    fn water_flows_towards_the_nearest_hole() {
        let mut world = flat_world();
        world.set_block_raw(BlockPos::new(19, 0, 16), 0);
        let source = BlockPos::new(16, 1, 16);
        world.set_block_raw(source, water(0));

        let mut out = Vec::new();
        tick(&mut world, source, &mut out);
        assert_eq!(at(&world, 17, 1, 16), water(1));
        assert_eq!(at(&world, 15, 1, 16), 0);
        assert_eq!(at(&world, 16, 1, 15), 0);
        assert_eq!(at(&world, 16, 1, 17), 0);
    }

    #[test]
    fn water_cannot_pass_solid_blocks() {
        let mut world = flat_world();
        world.set_block_raw(BlockPos::new(17, 1, 16), stone());
        let source = BlockPos::new(16, 1, 16);
        world.set_block_raw(source, water(0));
        settle(&mut world, &[source]);

        assert_eq!(at(&world, 17, 1, 16), stone());
        // It goes around the stone instead
        assert_eq!(at(&world, 18, 1, 16), water(4));
    }

    #[test]
    fn water_next_to_lava_makes_stone() {
        let mut world = flat_world();
        let obsidian = generic::default_state("minecraft:obsidian").unwrap();
        let cobblestone = generic::default_state("minecraft:cobblestone").unwrap();

        world.set_block_raw(BlockPos::new(10, 1, 10), lava(0));
        world.set_block_raw(BlockPos::new(11, 1, 10), water(0));
        assert_eq!(wake(&mut world, BlockPos::new(10, 1, 10)), None);
        assert_eq!(at(&world, 10, 1, 10), obsidian);

        world.set_block_raw(BlockPos::new(20, 1, 10), lava(3));
        world.set_block_raw(BlockPos::new(20, 2, 10), water(0));
        assert_eq!(wake(&mut world, BlockPos::new(20, 1, 10)), None);
        assert_eq!(at(&world, 20, 1, 10), cobblestone);

        // Water below doesn't count
        world.set_block_raw(BlockPos::new(25, 2, 10), lava(0));
        world.set_block_raw(BlockPos::new(25, 1, 10), water(0));
        assert!(wake(&mut world, BlockPos::new(25, 2, 10)).is_some());
    }

    #[test]
    fn lava_falling_onto_water_makes_stone() {
        let mut world = flat_world();
        world.set_block_raw(BlockPos::new(16, 1, 16), water(0));
        let source = BlockPos::new(16, 2, 16);
        world.set_block_raw(source, lava(0));
        let mut out = Vec::new();
        tick(&mut world, source, &mut out);
        assert_eq!(at(&world, 16, 1, 16), stone());
    }

    #[test]
    fn fluids_replace_plants_but_not_slabs() {
        let mut world = flat_world();
        let grass = generic::default_state("minecraft:short_grass").unwrap();
        world.set_block_raw(BlockPos::new(17, 1, 16), grass);
        let slab = generic::default_state("minecraft:oak_slab").unwrap();
        let bottom_slab = generic::with_property(slab, "type", "bottom");
        world.set_block_raw(BlockPos::new(15, 1, 16), bottom_slab);
        let source = BlockPos::new(16, 1, 16);
        world.set_block_raw(source, water(0));
        settle(&mut world, &[source]);

        assert_eq!(at(&world, 17, 1, 16), water(1));
        // The slab is waterlogged and spreads water as a source itself
        let waterlogged = generic::with_property(bottom_slab, "waterlogged", "true");
        assert_eq!(at(&world, 15, 1, 16), waterlogged);
        assert_eq!(at(&world, 14, 1, 16), water(1));
    }
}
