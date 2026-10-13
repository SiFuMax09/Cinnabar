use super::{DIRECTIONS, MAX_LIGHT, OPPOSITE, UP};
use crate::storage::Chunk;
use crate::World;
use cinnabar_blocks::generic::{light_emission, light_occluding_faces, light_opacity};
use cinnabar_blocks::BlockPos;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(super) struct Node {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

impl Node {
    pub fn offset(self, dir: usize) -> Node {
        let (dx, dy, dz) = DIRECTIONS[dir];
        Node {
            x: self.x + dx,
            y: self.y + dy,
            z: self.z + dz,
        }
    }

    fn local(self) -> (u32, u32, u32) {
        ((self.x & 15) as u32, self.y as u32, (self.z & 15) as u32)
    }
}

const ALL_DIRS: u8 = 0b11_1111;

/// An entry of a queue: a block whose light level went down from, or up to, `level` and the
/// directions the change has to be passed on to
#[derive(Clone, Copy)]
pub(super) struct Entry {
    pub node: Node,
    pub level: u8,
    pub dirs: u8,
    /// The entry raises the light level of the block itself first, for light sources
    pub from_emission: bool,
}

impl Entry {
    pub fn new(node: Node, level: u8, dirs: u8) -> Entry {
        Entry {
            node,
            level,
            dirs,
            from_emission: false,
        }
    }
}

fn all_but(dir: usize) -> u8 {
    ALL_DIRS & !(1 << dir)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Sky,
    Block,
}

#[derive(Default)]
pub(super) struct Queues {
    pub decrease: VecDeque<Entry>,
    pub increase: VecDeque<Entry>,
}

/// Spreads changes of blocks through the light of the chunks of a world
#[derive(Default)]
pub struct LightEngine {
    /// Blocks whose light properties changed
    checks: Vec<Node>,
    pub(super) queues: [Queues; 2],
}

fn queue_index(kind: Kind) -> usize {
    kind as usize
}

fn state_at<W: World>(world: &W, node: Node) -> Option<u32> {
    let chunk = world.get_chunk(node.x >> 4, node.z >> 4)?;
    if node.y < 0 || node.y >= chunk.light.num_sections() as i32 * 16 {
        return None;
    }
    let (x, y, z) = node.local();
    Some(chunk.get_block(x, y, z))
}

fn level_at<W: World>(world: &W, kind: Kind, node: Node) -> Option<u8> {
    let chunk = world.get_chunk(node.x >> 4, node.z >> 4)?;
    if node.y < 0 || node.y >= chunk.light.num_sections() as i32 * 16 {
        return None;
    }
    let (x, y, z) = node.local();
    Some(match kind {
        Kind::Sky => chunk.light.sky_light(x, y, z),
        Kind::Block => chunk.light.block_light(x, y, z),
    })
}

fn set_level_at<W: World>(world: &mut W, kind: Kind, node: Node, level: u8) {
    if let Some(chunk) = world.get_chunk_mut(node.x >> 4, node.z >> 4) {
        let (x, y, z) = node.local();
        match kind {
            Kind::Sky => chunk.light.set_sky_light(x, y, z, level),
            Kind::Block => chunk.light.set_block_light(x, y, z, level),
        }
    }
}

fn source_y_at<W: World>(world: &W, x: i32, z: i32) -> Option<i32> {
    let chunk = world.get_chunk(x >> 4, z >> 4)?;
    Some(chunk.light.source_y((x & 15) as u32, (z & 15) as u32))
}

fn world_height<W: World>(world: &W, node: Node) -> Option<i32> {
    let chunk = world.get_chunk(node.x >> 4, node.z >> 4)?;
    Some(chunk.light.num_sections() as i32 * 16)
}

/// Whether a face that completely covers the block keeps light from passing between the blocks.
/// Vanilla also lets two partially covering faces occlude together, that is not handled.
fn shape_occludes(from: u32, to: u32, dir: usize) -> bool {
    light_occluding_faces(from) & 1 << dir != 0
        || light_occluding_faces(to) & 1 << OPPOSITE[dir] != 0
}

fn emission_of(kind: Kind, state: u32) -> u8 {
    match kind {
        Kind::Sky => 0,
        Kind::Block => light_emission(state),
    }
}

impl LightEngine {
    pub fn new() -> LightEngine {
        LightEngine::default()
    }

    /// Takes the changed positions out of the chunk
    pub fn collect_pending(&mut self, chunk: &mut Chunk) {
        if !chunk.light.has_pending() {
            return;
        }
        for packed in chunk.light.take_pending() {
            self.checks.push(Node {
                x: chunk.x * 16 + (packed & 15) as i32,
                y: (packed >> 8) as i32,
                z: chunk.z * 16 + (packed >> 4 & 15) as i32,
            });
        }
    }

    /// Queues a block to be checked, as if it was changed
    pub fn check_block(&mut self, pos: BlockPos) {
        self.checks.push(Node {
            x: pos.x,
            y: pos.y,
            z: pos.z,
        });
    }

    pub fn has_work(&self) -> bool {
        !self.checks.is_empty()
            || self
                .queues
                .iter()
                .any(|q| !q.decrease.is_empty() || !q.increase.is_empty())
    }

    /// Updates the light for all changes made. Stops after the given time and returns false if
    /// there is still work left, which will be continued by the next call.
    pub fn run<W: World>(&mut self, world: &mut W, budget: Option<Duration>) -> bool {
        let deadline = budget.map(|budget| Instant::now() + budget);
        let mut steps = 0u32;
        let mut expired = || {
            steps += 1;
            steps % 256 == 0 && deadline.is_some_and(|deadline| Instant::now() >= deadline)
        };

        if !self.checks.is_empty() {
            self.checks.sort_unstable();
            self.checks.dedup();
            while let Some(node) = self.checks.pop() {
                self.check_node(world, Kind::Block, node);
                self.check_node(world, Kind::Sky, node);
                if expired() {
                    return false;
                }
            }
        }

        for kind in [Kind::Sky, Kind::Block] {
            let queue = queue_index(kind);
            while let Some(entry) = self.queues[queue].decrease.pop_front() {
                self.propagate_decrease(world, kind, entry);
                if expired() {
                    return false;
                }
            }
            while let Some(entry) = self.queues[queue].increase.pop_front() {
                self.propagate_increase(world, kind, entry);
                if expired() {
                    return false;
                }
            }
        }
        true
    }

    fn enqueue_decrease(&mut self, kind: Kind, entry: Entry) {
        self.queues[queue_index(kind)].decrease.push_back(entry);
    }

    pub(super) fn enqueue_increase(&mut self, kind: Kind, entry: Entry) {
        self.queues[queue_index(kind)].increase.push_back(entry);
    }

    fn check_node<W: World>(&mut self, world: &mut W, kind: Kind, node: Node) {
        let Some(old_level) = level_at(world, kind, node) else {
            return;
        };
        match kind {
            Kind::Block => {
                let emission = state_at(world, node).map_or(0, light_emission);
                if emission < old_level {
                    set_level_at(world, kind, node, 0);
                    self.enqueue_decrease(kind, Entry::new(node, old_level, ALL_DIRS));
                } else {
                    // Pull in the light of the neighbors, the block might have become transparent
                    self.enqueue_decrease(kind, Entry::new(node, 1, ALL_DIRS));
                }
                if emission > 0 {
                    let mut entry = Entry::new(node, emission, ALL_DIRS);
                    entry.from_emission = true;
                    self.enqueue_increase(kind, entry);
                }
            }
            Kind::Sky => {
                let Some(lowest) = source_y_at(world, node.x, node.z) else {
                    return;
                };
                self.update_sources_in_column(world, node.x, node.z, lowest);
                if node.y >= lowest {
                    // Rebuild the light around the source
                    let dirs = all_but(UP);
                    self.enqueue_decrease(kind, Entry::new(node, MAX_LIGHT, dirs));
                    self.enqueue_increase(kind, Entry::new(node, MAX_LIGHT, dirs));
                } else if old_level > 0 {
                    set_level_at(world, kind, node, 0);
                    self.enqueue_decrease(kind, Entry::new(node, old_level, ALL_DIRS));
                } else {
                    self.enqueue_decrease(kind, Entry::new(node, 1, ALL_DIRS));
                }
            }
        }
    }

    /// Makes the sky light in a column match the lowest source of the column
    fn update_sources_in_column<W: World>(&mut self, world: &mut W, x: i32, z: i32, lowest: i32) {
        let top = Node { x, y: 0, z };
        let Some(height) = world_height(world, top) else {
            return;
        };
        // Cells below the source that are still fully lit lose their light
        let mut y = lowest.min(height) - 1;
        while y >= 0 {
            let node = Node { x, y, z };
            if level_at(world, Kind::Sky, node) != Some(MAX_LIGHT) {
                break;
            }
            set_level_at(world, Kind::Sky, node, 0);
            let dirs = if y == lowest - 1 {
                ALL_DIRS
            } else {
                all_but(UP)
            };
            self.enqueue_decrease(Kind::Sky, Entry::new(node, MAX_LIGHT, dirs));
            y -= 1;
        }

        // Cells from the source up are fully lit
        let neighbor_lowest = [(-1, 0), (1, 0), (0, -1), (0, 1)]
            .into_iter()
            .map(|(dx, dz)| source_y_at(world, x + dx, z + dz).unwrap_or(i32::MIN))
            .max()
            .unwrap();
        for y in lowest.max(0)..height {
            let node = Node { x, y, z };
            if level_at(world, Kind::Sky, node) == Some(MAX_LIGHT) {
                break;
            }
            set_level_at(world, Kind::Sky, node, MAX_LIGHT);
            if y < neighbor_lowest || y == lowest {
                self.enqueue_increase(Kind::Sky, Entry::new(node, MAX_LIGHT, all_but(UP)));
            }
        }
    }

    fn propagate_increase<W: World>(&mut self, world: &mut W, kind: Kind, entry: Entry) {
        let from = entry.node;
        let Some(mut from_level) = level_at(world, kind, from) else {
            return;
        };
        if entry.from_emission && from_level < entry.level {
            set_level_at(world, kind, from, entry.level);
            from_level = entry.level;
        }
        // The level changed again since the entry was queued, a newer entry takes care of it
        if from_level != entry.level {
            return;
        }

        let mut from_state = None;
        for dir in 0..6 {
            if entry.dirs >> dir & 1 == 0 {
                continue;
            }
            let to = from.offset(dir);
            let Some(to_level) = level_at(world, kind, to) else {
                continue;
            };
            if from_level - 1 <= to_level {
                continue;
            }
            let Some(to_state) = state_at(world, to) else {
                continue;
            };
            let opacity = light_opacity(to_state).max(1);
            let new_level = from_level.saturating_sub(opacity);
            if new_level <= to_level {
                continue;
            }
            let from_state = *from_state.get_or_insert_with(|| state_at(world, from).unwrap_or(0));
            if shape_occludes(from_state, to_state, dir) {
                continue;
            }
            set_level_at(world, kind, to, new_level);
            if new_level > 1 {
                self.enqueue_increase(kind, Entry::new(to, new_level, all_but(OPPOSITE[dir])));
            }
        }
    }

    fn propagate_decrease<W: World>(&mut self, world: &mut W, kind: Kind, entry: Entry) {
        let from = entry.node;
        for dir in 0..6 {
            if entry.dirs >> dir & 1 == 0 {
                continue;
            }
            let to = from.offset(dir);
            let Some(to_level) = level_at(world, kind, to) else {
                continue;
            };
            if to_level == 0 {
                continue;
            }
            if to_level < entry.level {
                // The light of the neighbor came from the block that went dark
                let emission = match kind {
                    Kind::Sky => 0,
                    Kind::Block => state_at(world, to).map_or(0, |state| emission_of(kind, state)),
                };
                set_level_at(world, kind, to, 0);
                if emission < to_level {
                    self.enqueue_decrease(kind, Entry::new(to, to_level, all_but(OPPOSITE[dir])));
                }
                if emission > 0 {
                    let mut entry = Entry::new(to, emission, ALL_DIRS);
                    entry.from_emission = true;
                    self.enqueue_increase(kind, entry);
                }
            } else {
                // The neighbor is lit by something else and lights the block back up
                self.enqueue_increase(kind, Entry::new(to, to_level, 1 << OPPOSITE[dir]));
            }
        }
    }
}
