//! Builders for the edit jobs of the WorldEdit commands and brushes.

use super::history::HistoryEntry;
use super::job::*;
use super::pattern::{Mask, Pattern, PatternEnv};
use super::session::{apply_entities, apply_section_changes, clamp_to_plot, read_box, EditSession};
use super::shapes;
use crate::plot::PlotWorld;
use cinnabar_blocks::BlockPos;
use cinnabar_schematic::WorldEditClipboard;
use cinnabar_world::World;
use rustc_hash::FxHashSet;
use std::rc::Rc;

/// Both masks have to match, if there are any
pub fn combine_masks(a: Option<Mask>, b: Option<Mask>) -> Option<Mask> {
    match (a, b) {
        (Some(a), Some(b)) => Some(Mask::and(vec![a, b])),
        (a, b) => a.or(b),
    }
}

/// Only the blocks for which `shape` is true are placed
type Shape = Box<dyn Fn(BlockPos) -> bool>;

/// Places the blocks of a pattern in the boxes of the items
fn pattern_run(
    pattern: Pattern,
    clipboard: Option<WorldEditClipboard>,
    origin: BlockPos,
    shape: Option<Shape>,
) -> RunFn {
    Box::new(move |session, item| {
        let WorkItem::Box(min, max) = item else {
            return 0;
        };
        let mut env = PatternEnv::new(clipboard.as_ref(), origin);
        if let Some(id) = pattern.as_single() {
            match &shape {
                Some(shape) => session.edit_box(*min, *max, |pos, _| shape(pos).then_some(id)),
                None => session.edit_box(*min, *max, |_, _| Some(id)),
            }
        } else {
            session.edit_box(*min, *max, |pos, old| {
                if let Some(shape) = &shape
                    && !shape(pos)
                {
                    return None;
                }
                Some(pattern.pick(pos, old, &mut env))
            })
        }
    })
}

/// //set, //replace and everything that places a pattern in an area. `filter` makes it //replace.
pub fn region_job(
    world: &PlotWorld,
    owner: u128,
    gmask: Option<Mask>,
    filter: Option<Mask>,
    min: BlockPos,
    max: BlockPos,
    pattern: Pattern,
    clipboard: Option<WorldEditClipboard>,
    shape: Option<Shape>,
) -> EditJob {
    let items = section_items(world, min, max);
    EditJob::single(
        owner,
        combine_masks(gmask, filter),
        items,
        pattern_run(pattern, clipboard, min, shape),
    )
}

pub fn sphere_job(
    world: &PlotWorld,
    owner: u128,
    gmask: Option<Mask>,
    filter: Option<Mask>,
    center: BlockPos,
    radius: u32,
    hollow: bool,
    pattern: Pattern,
    clipboard: Option<WorldEditClipboard>,
) -> EditJob {
    let r = radius as i32;
    let radius = radius as f64;
    let shape: Shape = Box::new(move |p| {
        let (dx, dy, dz) = (p.x - center.x, p.y - center.y, p.z - center.z);
        if hollow {
            shapes::in_sphere_shell(dx, dy, dz, radius)
        } else {
            shapes::in_sphere(dx, dy, dz, radius)
        }
    });
    region_job(
        world,
        owner,
        gmask,
        filter,
        BlockPos::new(center.x - r, center.y - r, center.z - r),
        BlockPos::new(center.x + r, center.y + r, center.z + r),
        pattern,
        clipboard,
        Some(shape),
    )
}

/// A cylinder that stands on `base` and goes up `height` blocks (down if negative)
pub fn cylinder_job(
    world: &PlotWorld,
    owner: u128,
    gmask: Option<Mask>,
    filter: Option<Mask>,
    base: BlockPos,
    radius: u32,
    height: i32,
    hollow: bool,
    pattern: Pattern,
    clipboard: Option<WorldEditClipboard>,
) -> EditJob {
    let r = radius as i32;
    let radius = radius as f64;
    let shape: Shape = Box::new(move |p| {
        let (dx, dz) = (p.x - base.x, p.z - base.z);
        if hollow {
            shapes::in_cylinder_shell(dx, dz, radius)
        } else {
            shapes::in_cylinder(dx, dz, radius)
        }
    });
    let (y0, y1) = if height >= 0 {
        (base.y, base.y + height.max(1) - 1)
    } else {
        (base.y + height + 1, base.y)
    };
    region_job(
        world,
        owner,
        gmask,
        filter,
        BlockPos::new(base.x - r, y0, base.z - r),
        BlockPos::new(base.x + r, y1, base.z + r),
        pattern,
        clipboard,
        Some(shape),
    )
}

/// //walls and //faces
pub fn shell_job(
    world: &PlotWorld,
    owner: u128,
    gmask: Option<Mask>,
    min: BlockPos,
    max: BlockPos,
    faces: bool,
    pattern: Pattern,
    clipboard: Option<WorldEditClipboard>,
) -> EditJob {
    let items = section_items_of(world, &shapes::shell_boxes(min, max, faces));
    EditJob::single(
        owner,
        gmask,
        items,
        pattern_run(pattern, clipboard, min, None),
    )
}

/// Sets single positions
fn positions_run(
    positions: Rc<Vec<BlockPos>>,
    pattern: Pattern,
    clipboard: Option<WorldEditClipboard>,
    origin: BlockPos,
) -> RunFn {
    Box::new(move |session, item| {
        let WorkItem::Index(i) = item else { return 0 };
        let mut env = PatternEnv::new(clipboard.as_ref(), origin);
        let start = i * POSITIONS_PER_ITEM;
        let end = (start + POSITIONS_PER_ITEM).min(positions.len());
        let mut count = 0;
        for pos in &positions[start..end] {
            let old = session.get(*pos);
            let new = pattern.pick(*pos, old, &mut env);
            count += session.set(*pos, new) as usize;
        }
        count
    })
}

pub fn positions_job(
    owner: u128,
    gmask: Option<Mask>,
    positions: Vec<BlockPos>,
    pattern: Pattern,
    clipboard: Option<WorldEditClipboard>,
    origin: BlockPos,
) -> EditJob {
    let items = index_items(positions.len(), POSITIONS_PER_ITEM);
    EditJob::single(
        owner,
        gmask,
        items,
        positions_run(Rc::new(positions), pattern, clipboard, origin),
    )
}

/// //line
pub fn line_job(
    owner: u128,
    gmask: Option<Mask>,
    a: BlockPos,
    b: BlockPos,
    thickness: u32,
    pattern: Pattern,
    clipboard: Option<WorldEditClipboard>,
) -> EditJob {
    let mut positions = Vec::new();
    if thickness == 0 {
        positions = shapes::line_points(a, b);
    } else {
        let r = thickness as i32;
        let mut seen = FxHashSet::default();
        for point in shapes::line_points(a, b) {
            for dx in -r..=r {
                for dy in -r..=r {
                    for dz in -r..=r {
                        if shapes::in_sphere(dx, dy, dz, thickness as f64 - 0.5) {
                            let p = BlockPos::new(point.x + dx, point.y + dy, point.z + dz);
                            if seen.insert(p) {
                                positions.push(p);
                            }
                        }
                    }
                }
            }
        }
    }
    positions_job(owner, gmask, positions, pattern, clipboard, a.min(b))
}

/// //overlay: places the pattern on top of the highest block of every column
pub fn overlay_job(
    world: &PlotWorld,
    owner: u128,
    gmask: Option<Mask>,
    min: BlockPos,
    max: BlockPos,
    pattern: Pattern,
    clipboard: Option<WorldEditClipboard>,
) -> EditJob {
    let items = column_items(world, min, max);
    let run: RunFn = Box::new(move |session, item| {
        let WorkItem::Box(lo, hi) = item else {
            return 0;
        };
        let mut env = PatternEnv::new(clipboard.as_ref(), min);
        let mut count = 0;
        for x in lo.x..=hi.x {
            for z in lo.z..=hi.z {
                for y in (lo.y..=hi.y).rev() {
                    if session.get(BlockPos::new(x, y, z)) != 0 {
                        let above = BlockPos::new(x, y + 1, z);
                        let old = session.get(above);
                        let new = pattern.pick(above, old, &mut env);
                        count += session.set(above, new) as usize;
                        break;
                    }
                }
            }
        }
        count
    });
    EditJob::single(owner, gmask, items, run)
}

/// //hollow: removes everything that is deeper than `thickness` blocks below the surface
pub fn hollow_job(
    owner: u128,
    gmask: Option<Mask>,
    min: BlockPos,
    max: BlockPos,
    thickness: u32,
    pattern: Pattern,
    clipboard: Option<WorldEditClipboard>,
) -> EditJob {
    let stage = Stage::lazy(move |env| {
        let size = (
            (max.x - min.x + 1) as usize,
            (max.y - min.y + 1) as usize,
            (max.z - min.z + 1) as usize,
        );
        let solid: Vec<bool> = read_box(env.world, min, max)
            .iter()
            .map(|id| *id != 0)
            .collect();
        let max_distance = thickness.min(250) as u8 + 1;
        let dist = surface_distances_for(&solid, size, max_distance);
        let thickness = thickness.min(250) as u8;
        let items = section_items(env.world, min, max);
        let run: RunFn = Box::new(move |session, item| {
            let WorkItem::Box(lo, hi) = item else {
                return 0;
            };
            let mut pick_env = PatternEnv::new(clipboard.as_ref(), min);
            session.edit_box(*lo, *hi, |pos, old| {
                let i = (((pos.y - min.y) as usize * size.2 + (pos.z - min.z) as usize) * size.0)
                    + (pos.x - min.x) as usize;
                let d = dist[i];
                (d != u8::MAX && d > thickness).then(|| pattern.pick(pos, old, &mut pick_env))
            })
        });
        Ok(StageBody { items, run })
    })
    .commit_to(HistoryDest::Undo);
    EditJob::new(owner, gmask).stage(stage)
}

fn surface_distances_for(solid: &[bool], size: (usize, usize, usize), max_distance: u8) -> Vec<u8> {
    shapes::surface_distances(solid, size, max_distance)
}

/// //fill and //fillr
pub fn fill_job(
    owner: u128,
    gmask: Option<Mask>,
    origin: BlockPos,
    radius: u32,
    depth: u32,
    pattern: Pattern,
    clipboard: Option<WorldEditClipboard>,
) -> EditJob {
    let stage = Stage::lazy(move |env| {
        let world = env.world;
        let positions = shapes::flood_fill(origin, radius as i32, depth as i32, |p| {
            clamp_to_plot(world, p, p).is_some() && world.get_block_raw(p) == 0
        });
        let items = index_items(positions.len(), POSITIONS_PER_ITEM);
        let run = positions_run(Rc::new(positions), pattern, clipboard, origin);
        Ok(StageBody { items, run })
    })
    .commit_to(HistoryDest::Undo);
    EditJob::new(owner, gmask).stage(stage)
}

/// //smooth and the smooth brush: smooths the terrain surface in the box
pub fn smooth_job(
    owner: u128,
    gmask: Option<Mask>,
    min: BlockPos,
    max: BlockPos,
    iterations: u32,
    surface: Option<Mask>,
) -> EditJob {
    let stage = Stage::lazy(move |env| {
        let Some((min, max)) = clamp_to_plot(env.world, min, max) else {
            return Err("The area is outside of the plot.".to_owned());
        };
        let width = (max.x - min.x + 1) as usize;
        let depth = (max.z - min.z + 1) as usize;
        let mut heights = vec![-1i32; width * depth];
        let mut tops = vec![0u16; width * depth];
        for z in 0..depth {
            for x in 0..width {
                for y in (min.y..=max.y).rev() {
                    let pos = BlockPos::new(min.x + x as i32, y, min.z + z as i32);
                    let id = env.world.get_block_raw(pos);
                    let ground = match &surface {
                        Some(mask) if mask.is_state_only() => mask.test_state(id),
                        _ => id != 0,
                    };
                    if ground {
                        heights[z * width + x] = y;
                        tops[z * width + x] = id as u16;
                        break;
                    }
                }
            }
        }
        let smoothed = shapes::smooth_heights(&heights, width, iterations);
        let items = column_items(env.world, min, max);
        let run: RunFn = Box::new(move |session, item| {
            let WorkItem::Box(lo, hi) = item else {
                return 0;
            };
            let mut count = 0;
            for z in lo.z..=hi.z {
                for x in lo.x..=hi.x {
                    let i = (z - min.z) as usize * width + (x - min.x) as usize;
                    let (old, new) = (heights[i], smoothed[i]);
                    if old < 0 || old == new {
                        continue;
                    }
                    if new > old {
                        let top = tops[i] as u32;
                        for y in old + 1..=new.min(max.y) {
                            count += session.set(BlockPos::new(x, y, z), top) as usize;
                        }
                    } else {
                        for y in new + 1..=old {
                            count += session.set(BlockPos::new(x, y, z), 0) as usize;
                        }
                    }
                }
            }
            count
        });
        Ok(StageBody { items, run })
    })
    .commit_to(HistoryDest::Undo);
    EditJob::new(owner, gmask).stage(stage)
}

/// Pastes the clipboard at all the `bases` (the world position of the corner of the clipboard)
/// one after the other
pub fn paste_stage(
    world: &PlotWorld,
    clipboard: WorldEditClipboard,
    bases: Vec<BlockPos>,
    ignore_air: bool,
) -> Stage {
    let mut items = Vec::new();
    let size = BlockPos::new(
        clipboard.size_x as i32 - 1,
        clipboard.size_y as i32 - 1,
        clipboard.size_z as i32 - 1,
    );
    for (i, base) in bases.iter().enumerate() {
        for item in section_items(world, *base, *base + size) {
            if let WorkItem::Box(min, max) = item {
                items.push(WorkItem::Copy(i, min, max));
            }
        }
        if !clipboard.block_entities.is_empty() {
            items.push(WorkItem::Index(i));
        }
    }
    let run: RunFn = Box::new(move |session, item| match item {
        WorkItem::Copy(i, min, max) => {
            let base = bases[*i];
            session.edit_box(*min, *max, |pos, _| {
                let id = clipboard.get(
                    (pos.x - base.x) as u32,
                    (pos.y - base.y) as u32,
                    (pos.z - base.z) as u32,
                );
                (!(ignore_air && id == 0)).then_some(id)
            })
        }
        WorkItem::Index(i) => {
            let base = bases[*i];
            for (rel, entity) in &clipboard.block_entities {
                session.set_block_entity(base + *rel, entity.clone());
            }
            0
        }
        WorkItem::Box(..) => 0,
    });
    Stage::new(items, run)
}

pub fn paste_job(
    world: &PlotWorld,
    owner: u128,
    gmask: Option<Mask>,
    clipboard: WorldEditClipboard,
    bases: Vec<BlockPos>,
    ignore_air: bool,
) -> EditJob {
    EditJob::new(owner, gmask)
        .stage(paste_stage(world, clipboard, bases, ignore_air).commit_to(HistoryDest::Undo))
}

/// Sets all blocks of a box to a pattern as a stage without history, to combine with others
pub fn clear_stage(world: &PlotWorld, min: BlockPos, max: BlockPos, pattern: Pattern) -> Stage {
    Stage::new(
        section_items(world, min, max),
        pattern_run(pattern, None, min, None),
    )
}

/// One stage that undoes (or redoes) the oldest `count` steps, one history entry at a time.
/// `undo` is true for //undo.
pub fn history_job(owner: u128, count: u32, undo: bool) -> EditJob {
    let mut job = EditJob::new(owner, None);
    let (plot_what, dest) = if undo {
        ("undo", HistoryDest::Redo)
    } else {
        ("redo", HistoryDest::UndoKeepRedo)
    };
    for _ in 0..count {
        let stage = Stage::lazy(move |env| {
            let player = env
                .player
                .as_deref_mut()
                .ok_or_else(|| "The player left.".to_owned())?;
            let entry = if undo {
                player.worldedit_history.pop_undo()
            } else {
                player.worldedit_history.pop_redo()
            }
            .ok_or_else(|| format!("There is nothing left to {plot_what}."))?;
            if entry.plot_x != env.world.x || entry.plot_z != env.world.z {
                // Put it back, it belongs to another plot
                if undo {
                    player.worldedit_history.push_undo(entry, false);
                } else {
                    player.worldedit_history.push_redo(entry);
                }
                return Err(format!("Cannot {plot_what} outside of your current plot."));
            }
            Ok(entry_body(entry))
        })
        .commit_to(dest);
        job = job.stage(stage);
    }
    job.on_finish(move |player, stats| {
        player.send_worldedit_message(&format!(
            "{} complete: {} block(s) restored ({:?})",
            if undo { "Undo" } else { "Redo" },
            stats.blocks,
            stats.elapsed
        ));
    })
}

fn entry_body(entry: HistoryEntry) -> StageBody {
    let mut items: Vec<WorkItem> = (0..entry.sections.len()).map(WorkItem::Index).collect();
    items.push(WorkItem::Index(entry.sections.len()));
    let run: RunFn = Box::new(move |session: &mut EditSession<'_>, item| match item {
        WorkItem::Index(i) if *i < entry.sections.len() => {
            apply_section_changes(session, &entry, *i)
        }
        WorkItem::Index(_) => {
            apply_entities(session, &entry);
            0
        }
        _ => 0,
    });
    StageBody { items, run }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plot::worldedit::history::Recorder;

    macro_rules! run_job {
        ($world:ident, $job:expr $(,)?) => {{
            let job = $job;
            run_all(&mut $world, job)
        }};
    }

    #[test]
    fn combine_masks_requires_both() {
        let a = Mask::parse("stone,dirt").unwrap();
        let b = Mask::parse("!dirt").unwrap();
        let both = combine_masks(Some(a.clone()), Some(b)).unwrap();
        assert!(both.test_state(1));
        assert!(!both.test_state(10));
        assert_eq!(combine_masks(Some(a.clone()), None), Some(a));
        assert_eq!(combine_masks(None, None), None);
    }

    #[test]
    fn set_replace_and_undo_jobs() {
        let mut world = PlotWorld::empty_for_test(0, 0);
        let (min, max) = (BlockPos::new(2, 2, 2), BlockPos::new(40, 20, 33));
        let stone = Pattern::parse("stone").unwrap();
        let entry = run_job!(
            world,
            region_job(&world, 0, None, None, min, max, stone, None, None),
        );
        assert_eq!(entry.blocks, 39 * 19 * 32);
        assert_eq!(world.get_block_raw(BlockPos::new(40, 20, 33)), 1);
        assert_eq!(world.get_block_raw(BlockPos::new(41, 20, 33)), 0);

        let dirt = Pattern::parse("dirt").unwrap();
        let filter = Mask::parse("stone").unwrap();
        let replace = region_job(&world, 0, None, Some(filter), min, max, dirt, None, None);
        run_job!(world, replace);
        assert_ne!(world.get_block_raw(BlockPos::new(5, 5, 5)), 1);
    }

    #[test]
    fn sphere_job_places_a_sphere() {
        let mut world = PlotWorld::empty_for_test(0, 0);
        let center = BlockPos::new(20, 20, 20);
        let job = sphere_job(
            &world,
            0,
            None,
            None,
            center,
            4,
            false,
            Pattern::parse("stone").unwrap(),
            None,
        );
        let entry = run_job!(world, job);
        assert!(entry.blocks > 250 && entry.blocks < 400);
        assert_ne!(world.get_block_raw(center), 0);
        assert_eq!(world.get_block_raw(BlockPos::new(25, 20, 20)), 0);
        let hollow = sphere_job(
            &world,
            0,
            None,
            None,
            center,
            4,
            true,
            Pattern::parse("air").unwrap(),
            None,
        );
        run_job!(world, hollow);
        // Only the shell was replaced
        assert_ne!(world.get_block_raw(center), 0);
        assert_eq!(world.get_block_raw(BlockPos::new(24, 20, 20)), 0);
    }

    #[test]
    fn walls_overlay_and_line() {
        let mut world = PlotWorld::empty_for_test(0, 0);
        let (min, max) = (BlockPos::new(0, 0, 0), BlockPos::new(9, 4, 9));
        let stone = Pattern::parse("stone").unwrap();
        run_job!(
            world,
            shell_job(&world, 0, None, min, max, false, stone, None),
        );
        assert_ne!(world.get_block_raw(BlockPos::new(0, 2, 5)), 0);
        assert_eq!(world.get_block_raw(BlockPos::new(5, 2, 5)), 0);
        assert_eq!(world.get_block_raw(BlockPos::new(5, 0, 5)), 0);

        let glass = Pattern::parse("glass").unwrap();
        run_job!(world, overlay_job(&world, 0, None, min, max, glass, None),);
        // Above the top of the walls
        assert_eq!(
            world.get_block_raw(BlockPos::new(0, 5, 5)),
            Block::Glass.get_id()
        );
        assert_eq!(world.get_block_raw(BlockPos::new(5, 5, 5)), 0);

        let line = line_job(
            0,
            None,
            BlockPos::new(1, 20, 1),
            BlockPos::new(8, 20, 1),
            0,
            Pattern::parse("dirt").unwrap(),
            None,
        );
        let entry = run_job!(world, line);
        assert_eq!(entry.blocks, 8);
    }

    #[test]
    fn hollow_job_keeps_the_shell() {
        let mut world = PlotWorld::empty_for_test(0, 0);
        let (min, max) = (BlockPos::new(0, 0, 0), BlockPos::new(6, 6, 6));
        run_job!(
            world,
            region_job(
                &world,
                0,
                None,
                None,
                min,
                max,
                Pattern::parse("stone").unwrap(),
                None,
                None,
            ),
        );
        let job = hollow_job(0, None, min, max, 0, Pattern::parse("air").unwrap(), None);
        run_job!(world, job);
        assert_ne!(world.get_block_raw(BlockPos::new(0, 3, 3)), 0);
        assert_eq!(world.get_block_raw(BlockPos::new(3, 3, 3)), 0);
        assert_eq!(world.get_block_raw(BlockPos::new(1, 3, 3)), 0);
    }

    #[test]
    fn fill_job_fills_a_pit() {
        let mut world = PlotWorld::empty_for_test(0, 0);
        // Ground at y<10 with a 3x3 pit 2 deep
        run_job!(
            world,
            region_job(
                &world,
                0,
                None,
                None,
                BlockPos::new(0, 0, 0),
                BlockPos::new(30, 9, 30),
                Pattern::parse("stone").unwrap(),
                None,
                None,
            ),
        );
        run_job!(
            world,
            region_job(
                &world,
                0,
                None,
                None,
                BlockPos::new(10, 8, 10),
                BlockPos::new(12, 9, 12),
                Pattern::parse("air").unwrap(),
                None,
                None,
            ),
        );
        let job = fill_job(
            0,
            None,
            BlockPos::new(11, 9, 11),
            5,
            5,
            Pattern::parse("dirt").unwrap(),
            None,
        );
        let entry = run_job!(world, job);
        assert_eq!(entry.blocks, 18);
        assert_ne!(world.get_block_raw(BlockPos::new(11, 8, 11)), 0);
    }

    #[test]
    fn paste_and_stack() {
        let mut world = PlotWorld::empty_for_test(0, 0);
        let (min, max) = (BlockPos::new(0, 0, 0), BlockPos::new(2, 2, 2));
        run_job!(
            world,
            region_job(
                &world,
                0,
                None,
                None,
                min,
                max,
                Pattern::parse("stone").unwrap(),
                None,
                None,
            ),
        );
        let cb = super::super::session::create_clipboard(&world, min, min, max);
        let bases = vec![
            BlockPos::new(0, 0, 3),
            BlockPos::new(0, 0, 6),
            BlockPos::new(0, 0, 9),
        ];
        let entry = run_job!(world, paste_job(&world, 0, None, cb, bases, false));
        assert_eq!(entry.blocks, 27 * 3);
        assert_eq!(world.get_block_raw(BlockPos::new(2, 2, 11)), 1);
        assert_eq!(world.get_block_raw(BlockPos::new(2, 2, 12)), 0);
    }

    /// Runs a job to the end and returns its history entry
    fn run_all(world: &mut PlotWorld, mut job: EditJob) -> HistoryEntry {
        let mut recorder = Recorder::new();
        let _ = &mut recorder;
        job.run_for_test(world)
    }

    use cinnabar_blocks::blocks::Block;
}
