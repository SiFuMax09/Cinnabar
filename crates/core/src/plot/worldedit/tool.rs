//! The tools of `/tool`: items that do something when they are clicked.

use super::brush::{trace, BRUSH_RANGE};
use super::ops;
use super::pattern::Pattern;
use super::SLICE_BUDGET;
use crate::player::PacketSender;
use crate::plot::{Plot, PlotWorld};
use cinnabar_blocks::blocks::Block;
use cinnabar_blocks::BlockPos;
use cinnabar_world::World;
use rustc_hash::FxHashSet;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// The most blocks a flood fill or the tree remover changes
const MAX_TOOL_BLOCKS: usize = 100_000;

#[derive(Clone, Debug)]
pub enum Tool {
    /// `/tool repl`: right click replaces the block, left click copies it
    Replacer { pattern: Pattern, text: String },
    /// `/tool info`
    Info,
    /// `/tool lrbuild`: left click places the first pattern, right click the second
    LongRangeBuild {
        primary: Pattern,
        secondary: Pattern,
    },
    /// `/tool farwand`: left click sets the first and right click the second position
    FarWand,
    /// `/tool floodfill`
    FloodFill { pattern: Pattern, range: u32 },
    /// `/tool deltree`
    DeleteTree,
    /// `/tool cycler`
    Cycler,
    /// `/tool inspect`
    Inspect,
}

impl Tool {
    /// The part of the message that is sent when the tool is bound
    pub fn bound_name(&self) -> &'static str {
        match self {
            Tool::Replacer { .. } => "Block replacer tool",
            Tool::Info => "Info tool",
            Tool::LongRangeBuild { .. } => "Long-range building tool",
            Tool::FarWand => "Far wand tool",
            Tool::FloodFill { .. } => "Flood fill tool",
            Tool::DeleteTree => "Floating tree remover tool",
            Tool::Cycler => "Block data cycler tool",
            Tool::Inspect => "Inspect tool",
        }
    }
}

fn is_tree_part(id: u32) -> bool {
    let block = Block::from_id(id);
    let name = block.get_name();
    let name = name.strip_prefix("minecraft:").unwrap_or(name);
    name.ends_with("_log")
        || name.ends_with("_wood")
        || name.ends_with("_leaves")
        || name.ends_with("_stem")
        || name.ends_with("_hyphae")
        || name.ends_with("vine")
        || name.ends_with("vines")
        || name.ends_with("_wart_block")
        || name == "shroomlight"
        || name == "mushroom_stem"
        || name.ends_with("_mushroom_block")
}

fn block_name(id: u32) -> &'static str {
    let name = Block::from_id(id).get_name();
    name.strip_prefix("minecraft:").unwrap_or(name)
}

impl Plot {
    /// Uses the tool that is bound to the item. `primary` is a left click. Returns false if
    /// there is no tool for the item.
    pub(crate) fn try_use_tool(
        &mut self,
        player_idx: usize,
        item_id: u32,
        clicked: Option<BlockPos>,
        primary: bool,
    ) -> bool {
        let player = &mut self.players[player_idx];
        let Some(tool) = player.worldedit_tools.get(&item_id).cloned() else {
            return false;
        };
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
        if !allowed {
            player.send_no_permission_message();
            return true;
        }
        // Tools that only react to one of the clicks
        let reacts = match &tool {
            Tool::Replacer { .. } | Tool::LongRangeBuild { .. } | Tool::FarWand | Tool::Cycler => {
                true
            }
            _ => !primary,
        };
        if !reacts {
            return true;
        }
        let traced = trace(
            &self.world,
            player.pos,
            player.pitch as f64,
            player.yaw as f64,
            BRUSH_RANGE,
            None,
        );
        let hit = clicked.or(traced.as_ref().map(|t| t.hit));
        let Some(hit) = hit else {
            player.send_error_message("No block in sight!");
            return true;
        };
        let before = match (&traced, clicked) {
            (Some(t), Some(c)) if t.hit == c => t.before,
            (Some(t), None) => t.before,
            _ => hit,
        };
        if !Plot::in_plot_bounds(self.world.x, self.world.z, hit.x, hit.z) {
            player.send_error_message("That block is outside of your plot.");
            return true;
        }
        let owner = player.uuid;
        let gmask = player.worldedit_mask.clone();
        let id = self.world.get_block_raw(hit);
        let single = |pos: BlockPos, pattern: Pattern| {
            ops::positions_job(owner, gmask.clone(), vec![pos], pattern, None, pos)
        };
        let mut job = None;
        match tool {
            Tool::Replacer { pattern, .. } => {
                if primary {
                    player.worldedit_tools.insert(
                        item_id,
                        Tool::Replacer {
                            pattern: Pattern::Single(id),
                            text: block_name(id).to_owned(),
                        },
                    );
                    player.send_worldedit_message(&format!(
                        "Replacer tool pattern set to {}.",
                        block_name(id)
                    ));
                } else {
                    job = Some(single(hit, pattern));
                }
            }
            Tool::Info => {
                let block = Block::from_id(id);
                player.send_worldedit_message(&format!(
                    "Block: minecraft:{} ({:?}) at {}, {}, {}",
                    block_name(id),
                    block,
                    hit.x,
                    hit.y,
                    hit.z
                ));
            }
            Tool::LongRangeBuild {
                primary: p,
                secondary,
            } => {
                let pattern = if primary { p } else { secondary };
                if traced.is_some() && before != hit {
                    job = Some(single(before, pattern));
                } else {
                    player.send_error_message("No block in sight!");
                }
            }
            Tool::FarWand => {
                if primary {
                    player.worldedit_set_first_position(hit);
                } else {
                    player.worldedit_set_second_position(hit);
                }
            }
            Tool::FloodFill { pattern, range } => {
                let positions = flood_positions(&self.world, hit, range as i32);
                job = Some(ops::positions_job(
                    owner,
                    gmask.clone(),
                    positions,
                    pattern,
                    None,
                    hit,
                ));
            }
            Tool::DeleteTree => {
                if !is_tree_part(id) {
                    player.send_error_message("That is not a tree.");
                } else {
                    let positions = tree_positions(&self.world, hit);
                    job = Some(ops::positions_job(
                        owner,
                        gmask.clone(),
                        positions,
                        Pattern::Single(0),
                        None,
                        hit,
                    ));
                }
            }
            Tool::Cycler => {
                let next = cycle_state(id, primary);
                if next == id {
                    player.send_error_message("That block has no data to cycle.");
                } else {
                    job = Some(single(hit, Pattern::Single(next)));
                }
            }
            Tool::Inspect => {
                player.send_error_message(
                    "There is no block history on this server, inspect cannot be used.",
                );
            }
        }
        if let Some(job) = job {
            self.reset_redpiler();
            self.edit_jobs.push_back(job.quiet());
            self.run_edit_jobs(SLICE_BUDGET);
        }
        true
    }
}

/// The blocks that are connected to `start` and of the same type, at most `range` blocks away
fn flood_positions(world: &PlotWorld, start: BlockPos, range: i32) -> Vec<BlockPos> {
    let wanted = world.get_block_raw(start);
    let mut seen: FxHashSet<BlockPos> = FxHashSet::default();
    let mut queue = VecDeque::new();
    let mut result = Vec::new();
    seen.insert(start);
    queue.push_back(start);
    while let Some(pos) = queue.pop_front() {
        if result.len() >= MAX_TOOL_BLOCKS {
            break;
        }
        result.push(pos);
        for (x, y, z) in [
            (1, 0, 0),
            (-1, 0, 0),
            (0, 1, 0),
            (0, -1, 0),
            (0, 0, 1),
            (0, 0, -1),
        ] {
            let next = BlockPos::new(pos.x + x, pos.y + y, pos.z + z);
            if (next.x - start.x).abs() > range
                || (next.y - start.y).abs() > range
                || (next.z - start.z).abs() > range
            {
                continue;
            }
            let (min, max) = world.get_corners();
            if next.y < min.y || next.y > max.y || next.x < min.x || next.z < min.z {
                continue;
            }
            if next.x > max.x || next.z > max.z {
                continue;
            }
            if world.get_block_raw(next) == wanted && seen.insert(next) {
                queue.push_back(next);
            }
        }
    }
    result
}

/// The logs, leaves and vines that hang together with `start`
fn tree_positions(world: &PlotWorld, start: BlockPos) -> Vec<BlockPos> {
    let mut seen: FxHashSet<BlockPos> = FxHashSet::default();
    let mut queue = VecDeque::new();
    let mut result = Vec::new();
    seen.insert(start);
    queue.push_back(start);
    let (min, max) = world.get_corners();
    while let Some(pos) = queue.pop_front() {
        if result.len() >= MAX_TOOL_BLOCKS {
            break;
        }
        result.push(pos);
        for x in -1..=1 {
            for y in -1..=1 {
                for z in -1..=1 {
                    let next = BlockPos::new(pos.x + x, pos.y + y, pos.z + z);
                    if next.x < min.x
                        || next.y < min.y
                        || next.z < min.z
                        || next.x > max.x
                        || next.y > max.y
                        || next.z > max.z
                    {
                        continue;
                    }
                    if (next.x - start.x).abs() > 64 || (next.z - start.z).abs() > 64 {
                        continue;
                    }
                    if is_tree_part(world.get_block_raw(next)) && seen.insert(next) {
                        queue.push_back(next);
                    }
                }
            }
        }
    }
    result
}

/// The next (or previous) state of the same block
fn cycle_state(id: u32, forward: bool) -> u32 {
    let name = block_name(id);
    let mut first = id;
    while first > 0 && block_name(first - 1) == name {
        first -= 1;
    }
    let mut last = id;
    while block_name(last + 1) == name && last < u32::MAX - 1 {
        last += 1;
        if last > id + 4096 {
            break;
        }
    }
    if forward {
        if id >= last {
            first
        } else {
            id + 1
        }
    } else if id <= first {
        last
    } else {
        id - 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cycling_stays_in_the_same_block() {
        let lever = super::super::pattern::Pattern::parse("lever")
            .unwrap()
            .as_single()
            .unwrap();
        let next = cycle_state(lever, true);
        assert_ne!(next, lever);
        assert_eq!(block_name(next), block_name(lever));
        assert_eq!(cycle_state(next, false), lever);
    }

    #[test]
    fn trees_are_recognised() {
        let log = super::super::pattern::Pattern::parse("oak_log")
            .unwrap()
            .as_single()
            .unwrap();
        let stone = super::super::pattern::Pattern::parse("stone")
            .unwrap()
            .as_single()
            .unwrap();
        assert!(is_tree_part(log));
        assert!(!is_tree_part(stone));
    }
}
