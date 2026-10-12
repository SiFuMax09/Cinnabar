//! State of the item transport blocks (hoppers, droppers and dispensers) that is not part of the
//! block entities: hopper cooldowns, which blocks have to be ticked and the items that move
//! between the redstone logic and the entities of the plot.

use mchprs_blocks::block_entities::InventoryEntry;
use mchprs_blocks::blocks::{Block, HopperFacing};
use mchprs_blocks::{generic, BlockPos};
use rustc_hash::{FxHashMap, FxHashSet};
use std::ops::Range;
use std::sync::OnceLock;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TransportKind {
    Hopper,
    Dropper,
    Dispenser,
}

fn kind_ranges() -> &'static [(Range<u32>, TransportKind)] {
    static RANGES: OnceLock<Vec<(Range<u32>, TransportKind)>> = OnceLock::new();
    RANGES.get_or_init(|| {
        let hopper_ids: Vec<u32> = [true, false]
            .into_iter()
            .flat_map(|enabled| {
                [
                    HopperFacing::Down,
                    HopperFacing::North,
                    HopperFacing::South,
                    HopperFacing::West,
                    HopperFacing::East,
                ]
                .into_iter()
                .map(move |facing| Block::Hopper { facing, enabled }.get_id())
            })
            .collect();
        vec![
            (
                *hopper_ids.iter().min().unwrap()..*hopper_ids.iter().max().unwrap() + 1,
                TransportKind::Hopper,
            ),
            (
                generic::state_range("minecraft:dropper").unwrap(),
                TransportKind::Dropper,
            ),
            (
                generic::state_range("minecraft:dispenser").unwrap(),
                TransportKind::Dispenser,
            ),
        ]
    })
}

impl TransportKind {
    pub fn of_state(id: u32) -> Option<TransportKind> {
        kind_ranges()
            .iter()
            .find(|(range, _)| range.contains(&id))
            .map(|(_, kind)| *kind)
    }
}

/// The state id ranges of the blocks that hold their items in a block entity without a variant of
/// their own
fn container_ranges() -> &'static [Range<u32>] {
    static RANGES: OnceLock<Vec<Range<u32>>> = OnceLock::new();
    RANGES.get_or_init(|| {
        const COLORS: [&str; 16] = [
            "white",
            "orange",
            "magenta",
            "light_blue",
            "yellow",
            "lime",
            "pink",
            "gray",
            "light_gray",
            "cyan",
            "purple",
            "blue",
            "brown",
            "green",
            "red",
            "black",
        ];
        let mut names = vec![
            "minecraft:dropper".to_owned(),
            "minecraft:dispenser".to_owned(),
            "minecraft:chest".to_owned(),
            "minecraft:trapped_chest".to_owned(),
            "minecraft:shulker_box".to_owned(),
        ];
        names.extend(
            COLORS
                .iter()
                .map(|color| format!("minecraft:{color}_shulker_box")),
        );
        let mut ranges: Vec<_> = names
            .iter()
            .filter_map(|name| generic::state_range(name))
            .collect();
        ranges.push(kind_ranges()[0].0.clone());
        ranges
    })
}

/// Identifies the block a container state belongs to, ignoring properties like the facing.
/// Containers of the same family keep their block entity when one state is replaced by another.
pub fn container_family(id: u32) -> Option<u32> {
    container_ranges()
        .iter()
        .find(|range| range.contains(&id))
        .map(|range| range.start)
}

/// The part of a hopper that is not saved with its items
#[derive(Clone, Copy, Debug)]
pub struct HopperState {
    /// Game ticks until the hopper may move items again, `-1` when it never moved any
    pub cooldown: i32,
    /// The last game tick the hopper was ticked in
    pub ticked_game_time: i64,
}

impl Default for HopperState {
    fn default() -> Self {
        HopperState {
            cooldown: -1,
            ticked_game_time: i64::MIN,
        }
    }
}

/// An item entity as the redstone logic sees it
#[derive(Clone, Debug)]
pub struct WorldItem {
    pub entity_id: i32,
    pub pos: (f64, f64, f64),
    pub entry: InventoryEntry,
}

/// An item that a dropper or dispenser wants to spawn as an entity
#[derive(Clone, Debug)]
pub struct SpawnedItem {
    pub pos: (f64, f64, f64),
    pub vel: (f64, f64, f64),
    pub entry: InventoryEntry,
}

/// Something other than a plain item that a dispenser launches
#[derive(Clone, Debug)]
pub enum SpawnRequest {
    PrimedTnt {
        pos: (f64, f64, f64),
    },
    EnderPearl {
        pos: (f64, f64, f64),
        vel: (f64, f64, f64),
    },
}

#[derive(Default, Debug, Clone)]
pub struct ItemTransport {
    /// The number of game ticks that were ticked
    pub game_time: i64,
    /// The hoppers in the order they are ticked, which is the order they were placed in
    hopper_order: Vec<BlockPos>,
    hoppers: FxHashMap<BlockPos, HopperState>,
    other_blocks: FxHashSet<BlockPos>,
    /// The item entities the hoppers can pick up from, refreshed by the plot before every tick
    pub items: Vec<WorldItem>,
    /// The entities in `items` that were changed or consumed by a hopper
    pub changed_items: FxHashSet<i32>,
    pub spawned: Vec<SpawnedItem>,
    pub spawn_requests: Vec<SpawnRequest>,
}

impl ItemTransport {
    /// Keeps track of the blocks that need ticks. Called whenever a block was set.
    pub fn on_block_set(&mut self, pos: BlockPos, new_id: u32) {
        let kind = TransportKind::of_state(new_id);
        if kind == Some(TransportKind::Hopper) {
            self.other_blocks.remove(&pos);
            if !self.hoppers.contains_key(&pos) {
                self.hoppers.insert(pos, HopperState::default());
                self.hopper_order.push(pos);
            }
            return;
        }
        if self.hoppers.remove(&pos).is_some() {
            self.hopper_order.retain(|p| *p != pos);
        }
        if kind.is_some() {
            self.other_blocks.insert(pos);
        } else {
            self.other_blocks.remove(&pos);
        }
    }

    /// Whether the world has no hoppers, droppers or dispensers
    pub fn is_empty(&self) -> bool {
        self.hoppers.is_empty() && self.other_blocks.is_empty()
    }

    pub fn hopper_positions(&self) -> Vec<BlockPos> {
        self.hopper_order.clone()
    }

    pub fn hopper(&mut self, pos: BlockPos) -> Option<&mut HopperState> {
        self.hoppers.get_mut(&pos)
    }
}
