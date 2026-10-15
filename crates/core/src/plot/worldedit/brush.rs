//! Brushes that are bound to an item and used with a click. They follow FastAsyncWorldEdit:
//! `/brush <type> ...`, the options `/size`, `/range`, `/mask`, `/smask`, `/mat`, `/target`,
//! `/targetmask`, `/targetoffset` and the click bindings `/primary` and `/secondary`.

use super::job::{index_items, EditJob, WorkItem};
use super::ops;
use super::pattern::{FastRng, GlobalTransform, Mask, Pattern, PatternEnv};
use super::{shapes, SLICE_BUDGET};
use crate::player::PacketSender;
use crate::plot::Plot;
use crate::plot::PlotWorld;
use cinnabar_blocks::BlockPos;
use cinnabar_schematic::WorldEditClipboard;
use cinnabar_world::World;
use rustc_hash::FxHashSet;
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// How far away from the player a brush can be used when no range is set
pub const BRUSH_RANGE: f64 = 200.0;

/// The biggest size of a brush
pub const MAX_BRUSH_SIZE: u32 = 128;

/// The most blocks a recursive brush visits
const MAX_RECURSIVE_BLOCKS: usize = 200_000;

const NEIGHBOURS: [(i32, i32, i32); 6] = [
    (1, 0, 0),
    (-1, 0, 0),
    (0, 1, 0),
    (0, -1, 0),
    (0, 0, 1),
    (0, 0, -1),
];

#[derive(Clone, Debug, PartialEq)]
pub enum BrushKind {
    Sphere {
        hollow: bool,
        falling: bool,
    },
    Cylinder {
        height: u32,
        hollow: bool,
    },
    /// A flat disc that faces the player
    Circle,
    Smooth {
        iterations: u32,
        natural_only: bool,
    },
    Clipboard {
        ignore_air: bool,
        original_position: bool,
        skip: Option<Mask>,
    },
    /// Lets the blocks in the column fall
    Gravity {
        from_world_top: bool,
    },
    Extinguish,
    Splatter {
        seeds: u32,
        recursion: u32,
        solid: bool,
    },
    Scatter {
        points: u32,
        distance: u32,
        overlay: bool,
    },
    Layer {
        layers: Vec<Pattern>,
    },
    Surface,
    Recursive {
        depth_first: bool,
    },
    Erode,
    Line {
        hollow: bool,
        select: bool,
        flat: bool,
    },
    Catenary {
        length_factor: f64,
        hollow: bool,
        select: bool,
    },
    /// Runs commands at the target
    Command {
        commands: Vec<String>,
    },
    /// Runs commands at random points around the target
    ScatterCommand {
        points: u32,
        command_radius: u32,
        commands: Vec<String>,
    },
}

impl BrushKind {
    /// The name of the brush as FAWE prints it
    pub fn name(&self) -> &'static str {
        match self {
            BrushKind::Sphere { .. } => "sphere",
            BrushKind::Cylinder { .. } => "cylinder",
            BrushKind::Circle => "circle",
            BrushKind::Smooth { .. } => "smooth",
            BrushKind::Clipboard { .. } => "clipboard",
            BrushKind::Gravity { .. } => "gravity",
            BrushKind::Extinguish => "extinguish",
            BrushKind::Splatter { .. } => "splatter",
            BrushKind::Scatter { .. } => "scatter",
            BrushKind::Layer { .. } => "layer",
            BrushKind::Surface => "surface",
            BrushKind::Recursive { .. } => "recursive",
            BrushKind::Erode => "erode",
            BrushKind::Line { .. } => "line",
            BrushKind::Catenary { .. } => "catenary",
            BrushKind::Command { .. } => "command",
            BrushKind::ScatterCommand { .. } => "scmd",
        }
    }

    /// Whether the brush places the pattern of the brush
    fn uses_pattern(&self) -> bool {
        matches!(
            self,
            BrushKind::Sphere { .. }
                | BrushKind::Cylinder { .. }
                | BrushKind::Circle
                | BrushKind::Splatter { .. }
                | BrushKind::Scatter { .. }
                | BrushKind::Surface
                | BrushKind::Recursive { .. }
                | BrushKind::Line { .. }
                | BrushKind::Catenary { .. }
        )
    }
}

/// What the player looks at, for brushes that depend on the view direction
#[derive(Clone, Copy, Debug, Default)]
pub struct View {
    pub yaw: f64,
    pub pitch: f64,
}

#[derive(Clone, Debug)]
pub struct Brush {
    pub kind: BrushKind,
    pub pattern: Pattern,
    pub size: u32,
    /// Only blocks that match are changed (the destination mask)
    pub mask: Option<Mask>,
    pub mask_text: Option<String>,
    /// Only blocks that match can be placed (the source mask)
    pub smask: Option<Mask>,
    pub smask_text: Option<String>,
    /// How far away the brush can be used, the default is [`BRUSH_RANGE`]
    pub range: Option<u32>,
    /// `/target`: 1 the block you look at, 2 and 3 a point ahead of you, 4 the block in front of
    /// the block you look at
    pub target_mode: u8,
    /// Blocks the target trace stops at, by default every block that is not air
    pub target_mask: Option<Mask>,
    pub target_mask_text: Option<String>,
    /// `/targetoffset`: added to the height of the target
    pub target_offset: i32,
    /// `/transform`, only remembered
    pub transform_text: Option<String>,
    /// `/scroll`: what the scroll action of the brush changes
    pub scroll: Option<String>,
    /// The points that were clicked so far, for brushes that need two clicks
    pub points: Vec<BlockPos>,
}

impl Brush {
    pub fn new(kind: BrushKind, pattern: Pattern, size: u32) -> Brush {
        Brush {
            kind,
            pattern,
            size,
            mask: None,
            mask_text: None,
            smask: None,
            smask_text: None,
            range: None,
            target_mode: 1,
            target_mask: None,
            target_mask_text: None,
            target_offset: 0,
            transform_text: None,
            scroll: None,
            points: Vec::new(),
        }
    }

    pub fn describe(&self) -> String {
        let mut text = format!("{}, size {}", self.kind.name(), self.size);
        if self.kind.uses_pattern() {
            text.push_str(&format!(", pattern {}", self.pattern));
        }
        if let Some(mask) = &self.mask_text {
            text.push_str(&format!(", mask {mask}"));
        }
        if let Some(mask) = &self.smask_text {
            text.push_str(&format!(", source mask {mask}"));
        }
        text
    }

    pub fn needs_clipboard(&self) -> bool {
        matches!(self.kind, BrushKind::Clipboard { .. })
            || self.pattern.needs_clipboard()
            || matches!(&self.kind, BrushKind::Layer { layers } if layers.iter().any(Pattern::needs_clipboard))
    }

    /// The job (or the commands) that apply the brush at `target`
    pub fn action(
        &mut self,
        world: &PlotWorld,
        owner: u128,
        gmask: Option<Mask>,
        target: BlockPos,
        clipboard: Option<&WorldEditClipboard>,
        view: View,
    ) -> Result<BrushAction, String> {
        if self.needs_clipboard() && clipboard.is_none() {
            return Err("Your clipboard is empty. Use //copy first.".to_owned());
        }
        let target = BlockPos::new(target.x, target.y + self.target_offset, target.z);
        let size = self.size;
        let r = size as i32;
        let mask = ops::combine_masks(gmask, self.mask.clone());
        let clipboard_copy = clipboard.cloned();
        let pattern_job = |positions: Vec<BlockPos>| {
            let edits = positions.into_iter().map(|pos| (pos, None)).collect();
            BrushAction::Job(placement_job(
                world,
                owner,
                mask.clone(),
                edits,
                self.pattern.clone(),
                self.smask.clone(),
                clipboard_copy.clone(),
                target,
            ))
        };
        let explicit_job = |edits: Vec<(BlockPos, u32)>| {
            let edits = edits.into_iter().map(|(p, id)| (p, Some(id))).collect();
            BrushAction::Job(placement_job(
                world,
                owner,
                mask.clone(),
                edits,
                Pattern::Single(0),
                self.smask.clone(),
                None,
                target,
            ))
        };
        Ok(match &self.kind {
            BrushKind::Sphere { hollow, falling } => {
                let mut positions = sphere_positions(target, size as f64, *hollow);
                if *falling {
                    positions = drop_positions(world, positions, size as i32 * 2);
                }
                pattern_job(positions)
            }
            BrushKind::Cylinder { height, hollow } => {
                let mut positions = Vec::new();
                for dy in 0..*height as i32 {
                    for dx in -r..=r {
                        for dz in -r..=r {
                            let inside = match hollow {
                                true => shapes::in_cylinder_shell(dx, dz, size as f64),
                                false => shapes::in_cylinder(dx, dz, size as f64),
                            };
                            if inside {
                                positions.push(BlockPos::new(
                                    target.x + dx,
                                    target.y + dy,
                                    target.z + dz,
                                ));
                            }
                        }
                    }
                }
                pattern_job(positions)
            }
            BrushKind::Circle => pattern_job(circle_positions(target, size as f64, view)),
            BrushKind::Smooth {
                iterations,
                natural_only,
            } => {
                let natural = natural_only
                    .then(|| Mask::parse(NATURAL_BLOCKS).expect("the natural blocks are valid"));
                BrushAction::Job(ops::smooth_job(
                    owner,
                    mask.clone(),
                    BlockPos::new(target.x - r, target.y - r, target.z - r),
                    BlockPos::new(target.x + r, target.y + r, target.z + r),
                    *iterations,
                    natural,
                ))
            }
            BrushKind::Clipboard {
                ignore_air,
                original_position,
                skip,
            } => {
                let mut cb = clipboard_copy.clone().unwrap();
                let mut ignore_air = *ignore_air;
                if let Some(skip) = skip {
                    // Blocks that match the mask are not pasted, they count as air
                    for id in cb.data.iter_mut() {
                        if skip.is_state_only() && skip.test_state(*id as u32) {
                            *id = 0;
                        }
                    }
                    ignore_air = true;
                }
                let base = if *original_position {
                    BlockPos::new(
                        target.x - cb.offset_x,
                        target.y - cb.offset_y,
                        target.z - cb.offset_z,
                    )
                } else {
                    BlockPos::new(
                        target.x - cb.size_x as i32 / 2,
                        target.y - cb.size_y as i32 / 2,
                        target.z - cb.size_z as i32 / 2,
                    )
                };
                BrushAction::Job(ops::paste_job(
                    world,
                    owner,
                    mask.clone(),
                    cb,
                    vec![base],
                    ignore_air,
                ))
            }
            BrushKind::Gravity { from_world_top } => {
                explicit_job(gravity_edits(world, target, size, *from_world_top))
            }
            BrushKind::Extinguish => {
                let fire = Mask::parse("fire,soul_fire").expect("fire is a block");
                let mut edits = Vec::new();
                for pos in sphere_positions(target, size as f64, false) {
                    let id = world.get_block_raw(pos);
                    if id != 0 && fire.test_state(id) {
                        edits.push((pos, 0));
                    }
                }
                explicit_job(edits)
            }
            BrushKind::Splatter {
                seeds,
                recursion,
                solid,
            } => pattern_job(splatter_positions(
                world, target, size, *seeds, *recursion, *solid,
            )),
            BrushKind::Scatter {
                points,
                distance,
                overlay,
            } => pattern_job(scatter_positions(
                world, target, size, *points, *distance, *overlay,
            )),
            BrushKind::Layer { layers } => {
                let mut edits = Vec::new();
                let mut env = PatternEnv::new(clipboard, target);
                for dx in -r..=r {
                    for dz in -r..=r {
                        if !shapes::in_cylinder(dx, dz, size as f64) {
                            continue;
                        }
                        let (x, z) = (target.x + dx, target.z + dz);
                        let Some(top) = column_top(world, x, z, target.y - r, target.y + r) else {
                            continue;
                        };
                        for (i, layer) in layers.iter().enumerate() {
                            let pos = BlockPos::new(x, top - i as i32, z);
                            if !in_world(world, pos) || world.get_block_raw(pos) == 0 {
                                break;
                            }
                            let old = world.get_block_raw(pos);
                            edits.push((pos, layer.pick(pos, old, &mut env)));
                        }
                    }
                }
                explicit_job(edits)
            }
            BrushKind::Surface => {
                let mut positions = Vec::new();
                for pos in sphere_positions(target, size as f64, false) {
                    if world.get_block_raw(pos) != 0
                        && NEIGHBOURS.iter().any(|(x, y, z)| {
                            let n = BlockPos::new(pos.x + x, pos.y + y, pos.z + z);
                            in_world(world, n) && world.get_block_raw(n) == 0
                        })
                    {
                        positions.push(pos);
                    }
                }
                pattern_job(positions)
            }
            BrushKind::Recursive { depth_first } => {
                let wanted = world.get_block_raw(target);
                let filter = self.mask.clone();
                let positions =
                    recursive_positions(world, target, size as f64, *depth_first, |_pos, id| {
                        match &filter {
                            Some(m) if m.is_state_only() => m.test_state(id),
                            Some(_) => id == wanted,
                            None => id == wanted,
                        }
                    });
                pattern_job(positions)
            }
            BrushKind::Erode => explicit_job(morph_edits(world, target, size)),
            BrushKind::Line {
                hollow,
                select: _,
                flat,
            } => {
                let Some(first) = self.points.pop() else {
                    self.points.push(target);
                    return Ok(BrushAction::Message(
                        "Click to set the end point of the line.".to_owned(),
                    ));
                };
                let positions = line_positions(first, target, size, *hollow, *flat);
                pattern_job(positions)
            }
            BrushKind::Catenary {
                length_factor,
                hollow,
                select: _,
            } => {
                let Some(first) = self.points.pop() else {
                    self.points.push(target);
                    return Ok(BrushAction::Message(
                        "Click to set the end point of the catenary.".to_owned(),
                    ));
                };
                let positions = catenary_positions(first, target, *length_factor, size, *hollow);
                pattern_job(positions)
            }
            BrushKind::Command { commands } => BrushAction::Commands(
                commands
                    .iter()
                    .map(|c| fill_placeholders(c, target, size))
                    .collect(),
            ),
            BrushKind::ScatterCommand {
                points,
                command_radius,
                commands,
            } => {
                let mut rng = FastRng::new();
                let mut result = Vec::new();
                for _ in 0..*points {
                    let p = random_in_sphere(&mut rng, target, size as f64);
                    for c in commands {
                        result.push(fill_placeholders(c, p, *command_radius));
                    }
                }
                BrushAction::Commands(result)
            }
        })
    }

    /// Whether the brush has to be clicked twice
    pub fn needs_two_points(&self) -> bool {
        matches!(
            self.kind,
            BrushKind::Line { .. } | BrushKind::Catenary { .. }
        )
    }
}

/// What using a brush does
pub enum BrushAction {
    Job(EditJob),
    /// Commands to run as the player, without the leading slash
    Commands(Vec<String>),
    /// Only tells the player something
    Message(String),
}

/// The blocks the `-n` flag of the smooth brush treats as ground
const NATURAL_BLOCKS: &str = "stone,granite,diorite,andesite,deepslate,dirt,grass_block,\
    coarse_dirt,podzol,mycelium,sand,red_sand,gravel,clay,sandstone,red_sandstone,snow_block,\
    snow,ice,packed_ice,terracotta,netherrack,bedrock,coal_ore,iron_ore,gold_ore,diamond_ore,\
    copper_ore,redstone_ore,lapis_ore,emerald_ore,water,lava";

fn in_world(world: &PlotWorld, pos: BlockPos) -> bool {
    let (min, max) = world.get_corners();
    pos.x >= min.x
        && pos.y >= min.y
        && pos.z >= min.z
        && pos.x <= max.x
        && pos.y <= max.y
        && pos.z <= max.z
}

fn fill_placeholders(command: &str, pos: BlockPos, size: u32) -> String {
    command
        .replace("{x}", &pos.x.to_string())
        .replace("{y}", &pos.y.to_string())
        .replace("{z}", &pos.z.to_string())
        .replace("{world}", "world")
        .replace("{size}", &size.to_string())
}

fn random_in_sphere(rng: &mut FastRng, center: BlockPos, radius: f64) -> BlockPos {
    loop {
        let r = radius.ceil() as i32;
        let (dx, dy, dz) = (
            rng.below(2 * r as u32 + 1) as i32 - r,
            rng.below(2 * r as u32 + 1) as i32 - r,
            rng.below(2 * r as u32 + 1) as i32 - r,
        );
        if shapes::in_sphere(dx, dy, dz, radius) {
            return BlockPos::new(center.x + dx, center.y + dy, center.z + dz);
        }
    }
}

pub fn sphere_positions(center: BlockPos, radius: f64, hollow: bool) -> Vec<BlockPos> {
    let r = radius.ceil() as i32;
    let mut positions = Vec::new();
    for dx in -r..=r {
        for dy in -r..=r {
            for dz in -r..=r {
                let inside = match hollow {
                    true => shapes::in_sphere_shell(dx, dy, dz, radius),
                    false => shapes::in_sphere(dx, dy, dz, radius),
                };
                if inside {
                    positions.push(BlockPos::new(center.x + dx, center.y + dy, center.z + dz));
                }
            }
        }
    }
    positions
}

/// A flat disc that is perpendicular to the main axis of the view direction
fn circle_positions(center: BlockPos, radius: f64, view: View) -> Vec<BlockPos> {
    let r = radius.ceil() as i32;
    // The direction the player looks in
    let yaw = view.yaw.to_radians();
    let pitch = view.pitch.to_radians();
    let (dir_x, dir_y, dir_z) = (
        -yaw.sin() * pitch.cos(),
        -pitch.sin(),
        yaw.cos() * pitch.cos(),
    );
    let (ax, ay, az) = (dir_x.abs(), dir_y.abs(), dir_z.abs());
    let mut positions = Vec::new();
    for a in -r..=r {
        for b in -r..=r {
            if !shapes::in_sphere(a, b, 0, radius) {
                continue;
            }
            let (dx, dy, dz) = if ay >= ax && ay >= az {
                (a, 0, b)
            } else if ax >= az {
                (0, a, b)
            } else {
                (a, b, 0)
            };
            positions.push(BlockPos::new(center.x + dx, center.y + dy, center.z + dz));
        }
    }
    positions
}

/// Lets the positions fall down onto the ground, column by column. The shape keeps its number of
/// blocks per column.
fn drop_positions(world: &PlotWorld, positions: Vec<BlockPos>, max_fall: i32) -> Vec<BlockPos> {
    let mut columns: std::collections::BTreeMap<(i32, i32), Vec<i32>> = Default::default();
    for p in positions {
        columns.entry((p.x, p.z)).or_default().push(p.y);
    }
    let mut result = Vec::new();
    for ((x, z), ys) in columns {
        let lowest = *ys.iter().min().unwrap();
        let mut y = lowest;
        while y > lowest - max_fall
            && in_world(world, BlockPos::new(x, y - 1, z))
            && world.get_block_raw(BlockPos::new(x, y - 1, z)) == 0
        {
            y -= 1;
        }
        let shift = lowest - y;
        result.extend(ys.into_iter().map(|py| BlockPos::new(x, py - shift, z)));
    }
    result
}

/// The height of the highest block of the column between `low` and `high`
fn column_top(world: &PlotWorld, x: i32, z: i32, low: i32, high: i32) -> Option<i32> {
    (low..=high).rev().find(|&y| {
        let pos = BlockPos::new(x, y, z);
        in_world(world, pos) && world.get_block_raw(pos) != 0
    })
}

fn gravity_edits(
    world: &PlotWorld,
    target: BlockPos,
    radius: u32,
    from_world_top: bool,
) -> Vec<(BlockPos, u32)> {
    let r = radius as i32;
    let (min, max) = world.get_corners();
    let low = (target.y - r).max(min.y);
    let high = if from_world_top {
        max.y
    } else {
        (target.y + r).min(max.y)
    };
    let mut edits = Vec::new();
    for dx in -r..=r {
        for dz in -r..=r {
            if !shapes::in_cylinder(dx, dz, radius as f64) {
                continue;
            }
            let (x, z) = (target.x + dx, target.z + dz);
            let blocks: Vec<u32> = (low..=high)
                .map(|y| world.get_block_raw(BlockPos::new(x, y, z)))
                .collect();
            let mut fallen: Vec<u32> = blocks.iter().copied().filter(|&id| id != 0).collect();
            fallen.resize(blocks.len(), 0);
            for (i, (old, new)) in blocks.iter().zip(&fallen).enumerate() {
                if old != new {
                    edits.push((BlockPos::new(x, low + i as i32, z), *new));
                }
            }
        }
    }
    edits
}

fn splatter_positions(
    world: &PlotWorld,
    target: BlockPos,
    radius: u32,
    seeds: u32,
    recursion: u32,
    solid: bool,
) -> Vec<BlockPos> {
    let mut rng = FastRng::new();
    let mut seen: FxHashSet<BlockPos> = FxHashSet::default();
    let mut result = Vec::new();
    let radius = radius as f64;
    for _ in 0..seeds.max(1) {
        let start = random_in_sphere(&mut rng, target, radius);
        let mut queue = VecDeque::new();
        queue.push_back((start, 0u32));
        while let Some((pos, depth)) = queue.pop_front() {
            if !in_world(world, pos) || !seen.insert(pos) {
                continue;
            }
            let (dx, dy, dz) = (pos.x - target.x, pos.y - target.y, pos.z - target.z);
            if !shapes::in_sphere(dx, dy, dz, radius) {
                continue;
            }
            if solid && world.get_block_raw(pos) == 0 {
                continue;
            }
            result.push(pos);
            if depth < recursion {
                for (x, y, z) in NEIGHBOURS {
                    if rng.below(2) == 0 {
                        queue
                            .push_back((BlockPos::new(pos.x + x, pos.y + y, pos.z + z), depth + 1));
                    }
                }
            }
        }
    }
    result
}

fn scatter_positions(
    world: &PlotWorld,
    target: BlockPos,
    radius: u32,
    points: u32,
    distance: u32,
    overlay: bool,
) -> Vec<BlockPos> {
    let mut rng = FastRng::new();
    let mut result: Vec<BlockPos> = Vec::new();
    let min_distance = (distance as i64).pow(2);
    for _ in 0..points {
        for _attempt in 0..16 {
            let mut p = random_in_sphere(&mut rng, target, radius as f64);
            if overlay {
                let r = radius as i32;
                match column_top(world, p.x, p.z, target.y - r, target.y + r) {
                    Some(top) => p = BlockPos::new(p.x, top + 1, p.z),
                    None => continue,
                }
            }
            if !in_world(world, p) {
                continue;
            }
            let too_close = distance > 1
                && result.iter().any(|q| {
                    let (dx, dy, dz) = ((p.x - q.x) as i64, (p.y - q.y) as i64, (p.z - q.z) as i64);
                    dx * dx + dy * dy + dz * dz < min_distance
                });
            if !too_close {
                result.push(p);
                break;
            }
        }
    }
    result
}

fn recursive_positions(
    world: &PlotWorld,
    start: BlockPos,
    radius: f64,
    depth_first: bool,
    matches: impl Fn(BlockPos, u32) -> bool,
) -> Vec<BlockPos> {
    let mut seen: FxHashSet<BlockPos> = FxHashSet::default();
    let mut result = Vec::new();
    let mut queue = VecDeque::new();
    queue.push_back(start);
    seen.insert(start);
    while let Some(pos) = if depth_first {
        queue.pop_back()
    } else {
        queue.pop_front()
    } {
        if result.len() >= MAX_RECURSIVE_BLOCKS {
            break;
        }
        let id = world.get_block_raw(pos);
        if !matches(pos, id) {
            continue;
        }
        result.push(pos);
        for (x, y, z) in NEIGHBOURS {
            let next = BlockPos::new(pos.x + x, pos.y + y, pos.z + z);
            let (dx, dy, dz) = (next.x - start.x, next.y - start.y, next.z - start.z);
            if in_world(world, next) && shapes::in_sphere(dx, dy, dz, radius) && seen.insert(next) {
                queue.push_back(next);
            }
        }
    }
    result
}

/// Erodes the blocks with many open sides and then fills the holes with many closed sides
fn morph_edits(world: &PlotWorld, center: BlockPos, radius: u32) -> Vec<(BlockPos, u32)> {
    const ERODE_FACES: usize = 2;
    const FILL_FACES: usize = 5;
    let positions = sphere_positions(center, radius as f64, false);
    let get = |p: BlockPos| {
        if in_world(world, p) {
            world.get_block_raw(p)
        } else {
            1
        }
    };
    let mut edits: Vec<(BlockPos, u32)> = Vec::new();
    // Erode
    let mut eroded: FxHashSet<BlockPos> = FxHashSet::default();
    for &pos in &positions {
        if get(pos) == 0 {
            continue;
        }
        let open = NEIGHBOURS
            .iter()
            .filter(|(x, y, z)| get(BlockPos::new(pos.x + x, pos.y + y, pos.z + z)) == 0)
            .count();
        if open >= ERODE_FACES + 1 || (open >= ERODE_FACES && radius <= 1) {
            eroded.insert(pos);
            edits.push((pos, 0));
        }
    }
    // Fill
    for &pos in &positions {
        if get(pos) != 0 && !eroded.contains(&pos) {
            continue;
        }
        if get(pos) != 0 {
            continue;
        }
        let mut counts: Vec<(u32, usize)> = Vec::new();
        for (x, y, z) in NEIGHBOURS {
            let n = BlockPos::new(pos.x + x, pos.y + y, pos.z + z);
            let id = get(n);
            if id == 0 || eroded.contains(&n) {
                continue;
            }
            match counts.iter_mut().find(|(i, _)| *i == id) {
                Some((_, c)) => *c += 1,
                None => counts.push((id, 1)),
            }
        }
        let solid: usize = counts.iter().map(|(_, c)| c).sum();
        if solid >= FILL_FACES {
            let best = counts.iter().max_by_key(|(_, c)| *c).unwrap().0;
            edits.push((pos, best));
        }
    }
    edits
}

fn thicken(points: Vec<BlockPos>, radius: u32, hollow: bool, flat: bool) -> Vec<BlockPos> {
    if radius == 0 {
        return points;
    }
    let r = radius as i32;
    let mut set: FxHashSet<BlockPos> = FxHashSet::default();
    let mut ordered = Vec::new();
    for point in points {
        for dx in -r..=r {
            for dy in if flat { 0..=0 } else { -r..=r } {
                for dz in -r..=r {
                    if shapes::in_sphere(dx, dy, dz, radius as f64) {
                        let p = BlockPos::new(point.x + dx, point.y + dy, point.z + dz);
                        if set.insert(p) {
                            ordered.push(p);
                        }
                    }
                }
            }
        }
    }
    if hollow {
        ordered.retain(|p| {
            NEIGHBOURS
                .iter()
                .any(|(x, y, z)| !set.contains(&BlockPos::new(p.x + x, p.y + y, p.z + z)))
        });
    }
    ordered
}

fn line_positions(
    a: BlockPos,
    b: BlockPos,
    radius: u32,
    hollow: bool,
    flat: bool,
) -> Vec<BlockPos> {
    thicken(shapes::line_points(a, b), radius, hollow, flat)
}

/// A chain that hangs between two points, `length_factor` times as long as the distance
fn catenary_positions(
    a: BlockPos,
    b: BlockPos,
    length_factor: f64,
    radius: u32,
    hollow: bool,
) -> Vec<BlockPos> {
    let (dx, dy, dz) = ((b.x - a.x) as f64, (b.y - a.y) as f64, (b.z - a.z) as f64);
    let horizontal = (dx * dx + dz * dz).sqrt();
    let length = (horizontal * horizontal + dy * dy).sqrt() * length_factor.max(1.0);
    if horizontal < 1.0 || length <= dy.abs() + 1e-6 {
        return thicken(shapes::line_points(a, b), radius, hollow, false);
    }
    // Solve sqrt(L^2 - dy^2) = 2 a sinh(h / 2a) for a
    let wanted = (length * length - dy * dy).sqrt();
    let f = |a: f64| 2.0 * a * (horizontal / (2.0 * a)).sinh() - wanted;
    let (mut lo, mut hi) = (1e-3, 1e5);
    if f(hi) > 0.0 {
        return thicken(shapes::line_points(a, b), radius, hollow, false);
    }
    for _ in 0..200 {
        let mid = (lo + hi) / 2.0;
        if f(mid) > 0.0 {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let k = (lo + hi) / 2.0;
    // y(t) = k cosh((t - t0) / k) + c with y(0) = 0, y(h) = dy
    let t0 = horizontal / 2.0 - k * ((dy) / (2.0 * k * (horizontal / (2.0 * k)).sinh())).asinh();
    let c = -k * (t0 / k).cosh();
    let steps = (horizontal.max(dy.abs()) * 3.0).ceil() as usize + 1;
    let mut points = Vec::new();
    let mut seen: FxHashSet<BlockPos> = FxHashSet::default();
    for i in 0..=steps {
        let t = horizontal * i as f64 / steps as f64;
        let y = k * ((t - t0) / k).cosh() + c;
        let f = t / horizontal;
        let p = BlockPos::new(
            (a.x as f64 + dx * f).round() as i32,
            (a.y as f64 + y).round() as i32,
            (a.z as f64 + dz * f).round() as i32,
        );
        if seen.insert(p) {
            points.push(p);
        }
    }
    thicken(points, radius, hollow, false)
}

const PLACEMENTS_PER_ITEM: usize = 4096;

/// Places blocks at single positions: either the given block or one picked from the pattern. A
/// source mask keeps blocks that do not match from being placed.
fn placement_job(
    world: &PlotWorld,
    owner: u128,
    mask: Option<Mask>,
    mut edits: Vec<(BlockPos, Option<u32>)>,
    pattern: Pattern,
    smask: Option<Mask>,
    clipboard: Option<WorldEditClipboard>,
    origin: BlockPos,
) -> EditJob {
    edits.retain(|(pos, _)| in_world(world, *pos));
    let items = index_items(edits.len(), PLACEMENTS_PER_ITEM);
    let edits = Rc::new(edits);
    EditJob::single(
        owner,
        mask,
        items,
        Box::new(move |session, item| {
            let WorkItem::Index(i) = item else { return 0 };
            let mut env = PatternEnv::new(clipboard.as_ref(), origin);
            let start = i * PLACEMENTS_PER_ITEM;
            let end = (start + PLACEMENTS_PER_ITEM).min(edits.len());
            let mut count = 0;
            for (pos, id) in &edits[start..end] {
                let old = session.get(*pos);
                let new = match id {
                    Some(id) => *id,
                    None => pattern.pick(*pos, old, &mut env),
                };
                if let Some(smask) = &smask
                    && smask.is_state_only()
                    && !smask.test_state(new)
                {
                    continue;
                }
                count += session.set(*pos, new) as usize;
            }
            count
        }),
    )
}

/// Where a brush trace ends
pub struct Trace {
    /// The block that was hit
    pub hit: BlockPos,
    /// The last block before the hit
    pub before: BlockPos,
}

/// Follows the view of the player until a block that is not air, or one that matches the target
/// mask, is hit
pub fn trace(
    world: &PlotWorld,
    mut pos: crate::player::PlayerPos,
    pitch: f64,
    yaw: f64,
    max_distance: f64,
    target_mask: Option<&Mask>,
) -> Option<Trace> {
    let step = 0.1;
    pos.y += 1.65;
    let rot_x = (yaw + 90.0) % 360.0;
    let rot_y = -pitch;
    let h = step * rot_y.to_radians().cos();
    let (ox, oy, oz) = (
        h * rot_x.to_radians().cos(),
        step * rot_y.to_radians().sin(),
        h * rot_x.to_radians().sin(),
    );
    let mut rng = FastRng::new();
    let mut before = pos.block_pos();
    let mut distance = 0.0;
    while distance < max_distance {
        let block_pos = pos.block_pos();
        if in_world(world, block_pos) {
            let id = world.get_block_raw(block_pos);
            let hit = match target_mask {
                Some(mask) => mask.test(world, block_pos, id, &mut rng),
                None => id != 0,
            };
            if hit {
                return Some(Trace {
                    hit: block_pos,
                    before,
                });
            }
        }
        if block_pos != before {
            before = block_pos;
        }
        pos.x += ox;
        pos.y += oy;
        pos.z += oz;
        distance += step;
    }
    None
}

impl Plot {
    /// Uses the brush that is bound to the item `item_id` in the hand of a player. `clicked` is the
    /// block that was clicked, without it the block the player looks at is used. `primary` is a
    /// left click. Returns false if there is no brush for the item.
    pub(crate) fn use_brush(
        &mut self,
        player_idx: usize,
        item_id: u32,
        clicked: Option<BlockPos>,
        primary: bool,
    ) -> bool {
        let player = &mut self.players[player_idx];
        let brushes = match primary {
            true => &player.worldedit_brushes_primary,
            false => &player.worldedit_brushes,
        };
        let Some(mut brush) = brushes.get(&item_id).cloned() else {
            return false;
        };
        // Clicking a block with an item that does nothing makes the client send two packets
        let now = Instant::now();
        if player
            .worldedit_last_brush
            .is_some_and(|last| now.duration_since(last) < Duration::from_millis(120))
        {
            return true;
        }
        player.worldedit_last_brush = Some(now);

        let allowed =
            player.has_permission("plots.worldedit.bypass") || self.owner == Some(player.uuid);
        if !allowed || !player.has_permission("worldedit.brush.use") {
            player.send_no_permission_message();
            return true;
        }
        let range = brush.range.map_or(BRUSH_RANGE, |r| r as f64);
        let traced = || {
            trace(
                &self.world,
                player.pos,
                player.pitch as f64,
                player.yaw as f64,
                range,
                brush.target_mask.as_ref(),
            )
        };
        let target = match brush.target_mode {
            2 | 3 => {
                // A point ahead of the player
                let yaw = (player.yaw as f64).to_radians();
                let pitch = (player.pitch as f64).to_radians();
                let distance = range.min(brush.size as f64 * 2.0 + 4.0);
                let eye = (player.pos.x, player.pos.y + 1.65, player.pos.z);
                Some(BlockPos::new(
                    (eye.0 - yaw.sin() * pitch.cos() * distance).floor() as i32,
                    (eye.1 - pitch.sin() * distance).floor() as i32,
                    (eye.2 + yaw.cos() * pitch.cos() * distance).floor() as i32,
                ))
            }
            4 => traced().map(|t| t.before),
            _ => match (&brush.target_mask, clicked) {
                (None, Some(clicked)) => Some(clicked),
                _ => traced().map(|t| t.hit),
            },
        };
        let Some(target) = target else {
            player.send_error_message("No block in sight!");
            return true;
        };
        if !Plot::in_plot_bounds(self.world.x, self.world.z, target.x, target.z) {
            player.send_error_message("That block is outside of your plot.");
            return true;
        }
        let view = View {
            yaw: player.yaw as f64,
            pitch: player.pitch as f64,
        };
        let result = brush.action(
            &self.world,
            player.uuid,
            player.worldedit_mask.clone(),
            target,
            player.worldedit_clipboard.as_ref(),
            view,
        );
        let select = match &brush.kind {
            BrushKind::Line { select, .. } | BrushKind::Catenary { select, .. } => *select,
            _ => false,
        };
        if select {
            player.worldedit_set_first_position(target);
        }
        // Brushes that remember clicked points
        if brush.needs_two_points() {
            let slot = match primary {
                true => player.worldedit_brushes_primary.get_mut(&item_id),
                false => player.worldedit_brushes.get_mut(&item_id),
            };
            if let Some(slot) = slot {
                slot.points = brush.points.clone();
            }
        }
        match result {
            Ok(BrushAction::Job(job)) => {
                let transform = brush
                    .transform_text
                    .as_deref()
                    .and_then(|text| GlobalTransform::parse(text, target).ok());
                let job = job.with_transform(transform);
                self.reset_redpiler();
                self.edit_jobs.push_back(job.quiet());
                self.run_edit_jobs(SLICE_BUDGET);
            }
            Ok(BrushAction::Commands(commands)) => {
                for command in commands {
                    // `//set` is typed with two slashes, one of them is the command prefix
                    let command = command.strip_prefix('/').unwrap_or(&command).to_owned();
                    self.run_brush_command(player_idx, &command);
                }
            }
            Ok(BrushAction::Message(message)) => {
                self.players[player_idx].send_worldedit_message(&message)
            }
            Err(message) => self.players[player_idx].send_error_message(&message),
        }
        true
    }

    pub(crate) fn try_use_brush(
        &mut self,
        player_idx: usize,
        item_id: u32,
        clicked: Option<BlockPos>,
    ) -> bool {
        self.use_brush(player_idx, item_id, clicked, false)
    }

    pub(crate) fn try_use_brush_primary(
        &mut self,
        player_idx: usize,
        item_id: u32,
        clicked: Option<BlockPos>,
    ) -> bool {
        self.use_brush(player_idx, item_id, clicked, true)
    }

    fn run_brush_command(&mut self, player_idx: usize, command: &str) {
        let mut args: Vec<&str> = command.split(' ').collect();
        let name = args.remove(0);
        self.handle_command(player_idx, name, args);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cinnabar_world::World;

    fn sphere(size: u32) -> Brush {
        Brush::new(
            BrushKind::Sphere {
                hollow: false,
                falling: false,
            },
            Pattern::parse("stone").unwrap(),
            size,
        )
    }

    fn run(brush: &mut Brush, world: &mut PlotWorld, target: BlockPos) -> usize {
        match brush
            .action(world, 0, None, target, None, View::default())
            .unwrap()
        {
            BrushAction::Job(mut job) => job.run_for_test(world).blocks,
            _ => panic!("no job"),
        }
    }

    #[test]
    fn describe_mentions_the_settings() {
        let mut brush = sphere(5);
        brush.mask = Some(Mask::parse("dirt").unwrap());
        brush.mask_text = Some("dirt".to_owned());
        let text = brush.describe();
        assert!(text.contains("sphere") && text.contains("size 5"));
        assert!(text.contains("stone") && text.contains("mask dirt"));
    }

    #[test]
    fn brush_job_applies_at_the_target_and_respects_the_mask() {
        let mut world = PlotWorld::empty_for_test(0, 0);
        let target = BlockPos::new(30, 30, 30);
        let mut brush = sphere(3);
        assert!(run(&mut brush, &mut world, target) > 100);
        assert_eq!(world.get_block_raw(target), 1);

        // With a mask only matching blocks change
        brush.pattern = Pattern::parse("dirt").unwrap();
        brush.mask = Some(Mask::parse("air").unwrap());
        let before = world.get_block_raw(target);
        run(&mut brush, &mut world, BlockPos::new(30, 30, 34));
        assert_eq!(world.get_block_raw(target), before);
        assert_ne!(world.get_block_raw(BlockPos::new(30, 30, 37)), 0);
    }

    #[test]
    fn source_mask_limits_what_is_placed() {
        let mut world = PlotWorld::empty_for_test(0, 0);
        let mut brush = sphere(2);
        brush.smask = Some(Mask::parse("dirt").unwrap());
        assert_eq!(run(&mut brush, &mut world, BlockPos::new(30, 30, 30)), 0);
        brush.smask = Some(Mask::parse("stone").unwrap());
        assert!(run(&mut brush, &mut world, BlockPos::new(30, 30, 30)) > 0);
    }

    #[test]
    fn clipboard_brush_needs_a_clipboard() {
        let world = PlotWorld::empty_for_test(0, 0);
        let mut brush = sphere(1);
        brush.kind = BrushKind::Clipboard {
            ignore_air: false,
            original_position: false,
            skip: None,
        };
        assert!(brush
            .action(&world, 0, None, BlockPos::zero(), None, View::default())
            .is_err());
    }

    #[test]
    fn gravity_drops_blocks_and_extinguish_removes_fire() {
        let mut world = PlotWorld::empty_for_test(0, 0);
        let x = 30;
        world.set_block_raw(BlockPos::new(x, 33, x), 1);
        let mut brush = sphere(3);
        brush.kind = BrushKind::Gravity {
            from_world_top: false,
        };
        run(&mut brush, &mut world, BlockPos::new(x, 33, x));
        assert_eq!(world.get_block_raw(BlockPos::new(x, 33, x)), 0);
        assert_eq!(world.get_block_raw(BlockPos::new(x, 30, x)), 1);
    }

    #[test]
    fn line_brush_needs_two_clicks() {
        let mut world = PlotWorld::empty_for_test(0, 0);
        let mut brush = sphere(0);
        brush.kind = BrushKind::Line {
            hollow: false,
            select: false,
            flat: false,
        };
        let first = brush
            .action(
                &world,
                0,
                None,
                BlockPos::new(30, 30, 30),
                None,
                View::default(),
            )
            .unwrap();
        assert!(matches!(first, BrushAction::Message(_)));
        let BrushAction::Job(mut job) = brush
            .action(
                &world,
                0,
                None,
                BlockPos::new(30, 30, 40),
                None,
                View::default(),
            )
            .unwrap()
        else {
            panic!("expected a job");
        };
        assert_eq!(job.run_for_test(&mut world).blocks, 11);
    }

    #[test]
    fn falling_sphere_rests_on_the_ground() {
        let world = PlotWorld::empty_for_test(0, 0);
        let positions = drop_positions(
            &world,
            sphere_positions(BlockPos::new(30, 40, 30), 2.0, false),
            20,
        );
        assert_eq!(
            positions.len(),
            sphere_positions(BlockPos::new(30, 40, 30), 2.0, false).len()
        );
        assert!(positions.iter().map(|p| p.y).min().unwrap() < 38);
    }
}
