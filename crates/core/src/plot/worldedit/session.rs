//! The fast paths for editing and reading the world in bulk.
//!
//! An [`EditSession`] writes blocks straight into chunk sections, records what it changed for
//! //undo and applies the global mask. Nothing is sent to the players or updated here, the
//! caller flushes the world once it is done.

use super::history::{push_change, HistoryEntry, Recorder};
use super::pattern::{FastRng, GlobalTransform, Mask, PatternEnv};
use crate::plot::{PlotWorld, PLOT_BLOCK_HEIGHT};
use cinnabar_blocks::block_entities::BlockEntity;
use cinnabar_blocks::blocks::Block;
use cinnabar_blocks::BlockPos;
use cinnabar_schematic::WorldEditClipboard;
use cinnabar_world::World;
use rustc_hash::FxHashMap;

pub struct EditSession<'a> {
    pub world: &'a mut PlotWorld,
    pub recorder: &'a mut Recorder,
    /// Only blocks that match are replaced
    pub mask: Option<&'a Mask>,
    pub rng: FastRng,
    /// Everything that is placed goes through the transform
    transform: Option<&'a GlobalTransform>,
    transform_env: PatternEnv<'static>,
}

/// Clamps a box to the blocks of the plot. Returns `None` if nothing is left.
pub fn clamp_to_plot(
    world: &PlotWorld,
    min: BlockPos,
    max: BlockPos,
) -> Option<(BlockPos, BlockPos)> {
    let (c_min, c_max) = world.get_corners();
    let min = min.max(c_min);
    let max = max.min(c_max);
    (min.x <= max.x && min.y <= max.y && min.z <= max.z).then_some((min, max))
}

impl<'a> EditSession<'a> {
    pub fn new(
        world: &'a mut PlotWorld,
        recorder: &'a mut Recorder,
        mask: Option<&'a Mask>,
    ) -> EditSession<'a> {
        EditSession {
            world,
            recorder,
            mask,
            rng: FastRng::new(),
            transform: None,
            transform_env: PatternEnv::new(None, BlockPos::new(0, 0, 0)),
        }
    }

    pub fn with_transform(mut self, transform: Option<&'a GlobalTransform>) -> EditSession<'a> {
        self.transform = transform;
        self
    }

    pub fn get(&self, pos: BlockPos) -> u32 {
        self.world.get_block_raw(pos)
    }

    /// Whether the mask lets the block at `pos` be replaced
    fn allowed(&mut self, pos: BlockPos, old: u32) -> bool {
        match self.mask {
            Some(mask) => mask.test(&*self.world, pos, old, &mut self.rng),
            None => true,
        }
    }

    /// Sets a single block. Slower than [`EditSession::edit_box`], use it when blocks depend on
    /// their neighbours.
    pub fn set(&mut self, pos: BlockPos, new: u32) -> bool {
        let Some(transform) = self.transform else {
            return self.set_plain(pos, new);
        };
        let mut placements = Vec::new();
        transform.transform.apply(
            pos,
            new,
            transform.origin,
            &mut self.transform_env,
            &mut placements,
        );
        let mut changed = false;
        for (pos, new) in placements {
            changed |= self.set_plain(pos, new);
        }
        changed
    }

    fn set_plain(&mut self, pos: BlockPos, new: u32) -> bool {
        let old = self.world.get_block_raw(pos);
        if old == new || !self.allowed(pos, old) {
            return false;
        }
        self.set_unchecked(pos, old, new)
    }

    fn set_unchecked(&mut self, pos: BlockPos, old: u32, new: u32) -> bool {
        if !self.world.set_block_raw(pos, new) {
            return false;
        }
        self.recorder.record(
            pos.x >> 4,
            pos.z >> 4,
            (pos.y >> 4) as u16,
            local_index(pos),
            old as u16,
        );
        self.drop_block_entity(pos, old, new);
        true
    }

    /// Removes the block entity of a block that was replaced by a block that can't keep it
    fn drop_block_entity(&mut self, pos: BlockPos, old: u32, new: u32) {
        let Some(chunk) = self.world.get_chunk(pos.x >> 4, pos.z >> 4) else {
            return;
        };
        if chunk.block_entities.is_empty() {
            return;
        }
        if chunk
            .get_block_entity(BlockPos::new(pos.x & 0xF, pos.y, pos.z & 0xF))
            .is_none()
        {
            return;
        }
        if Block::from_id(old).has_block_entity() && Block::from_id(new).has_block_entity() {
            return;
        }
        let entity = self.world.get_block_entity(pos).cloned();
        self.recorder.block_entities.push((pos, entity));
        self.world.delete_block_entity(pos);
    }

    /// Sets a block entity and remembers the old one
    pub fn set_block_entity(&mut self, pos: BlockPos, entity: BlockEntity) {
        if clamp_to_plot(self.world, pos, pos).is_none() {
            return;
        }
        let old = self.world.get_block_entity(pos).cloned();
        self.recorder.block_entities.push((pos, old));
        self.world.set_block_entity(pos, entity);
    }

    /// Restores a block entity (or none) without recording
    pub fn restore_block_entity(&mut self, pos: BlockPos, entity: Option<BlockEntity>) {
        let current = self.world.get_block_entity(pos).cloned();
        self.recorder.block_entities.push((pos, current));
        match entity {
            Some(entity) => self.world.set_block_entity(pos, entity),
            None => self.world.delete_block_entity(pos),
        }
    }

    /// Calls `f` with the position and the old block of every block in the box (clamped to the
    /// plot) and sets the block it returns. `f` can't look at the world, the whole section is
    /// borrowed. Returns how many blocks were changed.
    pub fn edit_box<F>(&mut self, min: BlockPos, max: BlockPos, mut f: F) -> usize
    where
        F: FnMut(BlockPos, u32) -> Option<u32>,
    {
        let Some((min, max)) = clamp_to_plot(self.world, min, max) else {
            return 0;
        };
        let mask_is_complex =
            self.transform.is_some() || self.mask.is_some_and(|mask| !mask.is_state_only());
        if mask_is_complex {
            return self.edit_box_slow(min, max, f);
        }
        let mask = self.mask;
        let mut changed = 0;
        for chunk_x in (min.x >> 4)..=(max.x >> 4) {
            for chunk_z in (min.z >> 4)..=(max.z >> 4) {
                let Some(chunk_idx) = self
                    .world
                    .get_chunk_index_for_block(chunk_x << 4, chunk_z << 4)
                else {
                    continue;
                };
                let x0 = min.x.max(chunk_x << 4);
                let x1 = max.x.min((chunk_x << 4) + 15);
                let z0 = min.z.max(chunk_z << 4);
                let z1 = max.z.min((chunk_z << 4) + 15);
                for section_y in (min.y >> 4)..=(max.y >> 4) {
                    let y0 = min.y.max(section_y << 4);
                    let y1 = max.y.min((section_y << 4) + 15);
                    let chunk = &mut self.world.chunks[chunk_idx];
                    let has_entities = !chunk.block_entities.is_empty();
                    let section = &mut chunk.sections[section_y as usize];
                    let mut runs = None;
                    let mut dropped: Vec<(BlockPos, u32, u32)> = Vec::new();
                    for y in y0..=y1 {
                        for z in z0..=z1 {
                            for x in x0..=x1 {
                                let (lx, ly, lz) =
                                    ((x & 0xF) as u32, (y & 0xF) as u32, (z & 0xF) as u32);
                                let old = section.get_block(lx, ly, lz);
                                let pos = BlockPos::new(x, y, z);
                                let Some(new) = f(pos, old) else { continue };
                                if new == old {
                                    continue;
                                }
                                if let Some(mask) = mask
                                    && !mask.test_state(old)
                                {
                                    continue;
                                }
                                section.set_block(lx, ly, lz, new);
                                if runs.is_none() {
                                    runs = Some(self.recorder.section_runs(
                                        chunk_x,
                                        chunk_z,
                                        section_y as u16,
                                    ));
                                }
                                push_change(
                                    runs.as_mut().unwrap(),
                                    (ly << 8 | lz << 4 | lx) as u16,
                                    old as u16,
                                );
                                changed += 1;
                                if has_entities {
                                    dropped.push((pos, old, new));
                                }
                            }
                        }
                    }
                    for (pos, old, new) in dropped {
                        self.drop_block_entity(pos, old, new);
                    }
                }
            }
        }
        self.recorder.blocks += changed;
        changed
    }

    fn edit_box_slow<F>(&mut self, min: BlockPos, max: BlockPos, mut f: F) -> usize
    where
        F: FnMut(BlockPos, u32) -> Option<u32>,
    {
        let mut changed = 0;
        for y in min.y..=max.y {
            for z in min.z..=max.z {
                for x in min.x..=max.x {
                    let pos = BlockPos::new(x, y, z);
                    let old = self.world.get_block_raw(pos);
                    if let Some(new) = f(pos, old)
                        && self.set(pos, new)
                    {
                        changed += 1;
                    }
                }
            }
        }
        changed
    }

    /// Reads a box into the clipboard layout, positions outside of the plot are air
    pub fn read_box(&self, min: BlockPos, max: BlockPos) -> Vec<u16> {
        read_box(self.world, min, max)
    }
}

/// The index of a block in its chunk section
pub fn local_index(pos: BlockPos) -> u16 {
    (((pos.y & 0xF) << 8) | ((pos.z & 0xF) << 4) | (pos.x & 0xF)) as u16
}

/// Reads the blocks of a box in the order the clipboard uses (x, then z, then y)
pub fn read_box(world: &PlotWorld, min: BlockPos, max: BlockPos) -> Vec<u16> {
    let size_x = (max.x - min.x + 1) as usize;
    let size_y = (max.y - min.y + 1) as usize;
    let size_z = (max.z - min.z + 1) as usize;
    let mut out = vec![0u16; size_x * size_y * size_z];
    let Some((cmin, cmax)) = clamp_to_plot(world, min, max) else {
        return out;
    };
    for chunk_x in (cmin.x >> 4)..=(cmax.x >> 4) {
        for chunk_z in (cmin.z >> 4)..=(cmax.z >> 4) {
            let Some(chunk_idx) = world.get_chunk_index_for_block(chunk_x << 4, chunk_z << 4)
            else {
                continue;
            };
            let chunk = &world.chunks[chunk_idx];
            let x0 = cmin.x.max(chunk_x << 4);
            let x1 = cmax.x.min((chunk_x << 4) + 15);
            let z0 = cmin.z.max(chunk_z << 4);
            let z1 = cmax.z.min((chunk_z << 4) + 15);
            for section_y in (cmin.y >> 4)..=(cmax.y >> 4) {
                let section = &chunk.sections[section_y as usize];
                if section.block_count() == 0 {
                    continue;
                }
                let y0 = cmin.y.max(section_y << 4);
                let y1 = cmax.y.min((section_y << 4) + 15);
                for y in y0..=y1 {
                    for z in z0..=z1 {
                        let row = ((y - min.y) as usize * size_z + (z - min.z) as usize) * size_x;
                        for x in x0..=x1 {
                            out[row + (x - min.x) as usize] = section.get_block(
                                (x & 0xF) as u32,
                                (y & 0xF) as u32,
                                (z & 0xF) as u32,
                            ) as u16;
                        }
                    }
                }
            }
        }
    }
    out
}

/// The block entities in the box, with positions relative to `min`
pub fn read_block_entities(
    world: &PlotWorld,
    min: BlockPos,
    max: BlockPos,
) -> FxHashMap<BlockPos, BlockEntity> {
    let mut result = FxHashMap::default();
    let Some((cmin, cmax)) = clamp_to_plot(world, min, max) else {
        return result;
    };
    for chunk_x in (cmin.x >> 4)..=(cmax.x >> 4) {
        for chunk_z in (cmin.z >> 4)..=(cmax.z >> 4) {
            let Some(chunk) = world.get_chunk(chunk_x, chunk_z) else {
                continue;
            };
            for (local, entity) in &chunk.block_entities {
                let pos =
                    BlockPos::new((chunk_x << 4) + local.x, local.y, (chunk_z << 4) + local.z);
                if pos.x >= min.x
                    && pos.x <= max.x
                    && pos.y >= min.y
                    && pos.y <= max.y
                    && pos.z >= min.z
                    && pos.z <= max.z
                {
                    result.insert(pos - min, entity.clone());
                }
            }
        }
    }
    result
}

/// Copies a region of the world into a clipboard whose origin is `origin`
pub fn create_clipboard(
    world: &PlotWorld,
    origin: BlockPos,
    first: BlockPos,
    second: BlockPos,
) -> WorldEditClipboard {
    let min = first.min(second);
    let max = first.max(second);
    let offset = origin - min;
    WorldEditClipboard {
        offset_x: offset.x,
        offset_y: offset.y,
        offset_z: offset.z,
        size_x: (max.x - min.x + 1) as u32,
        size_y: (max.y - min.y + 1) as u32,
        size_z: (max.z - min.z + 1) as u32,
        data: read_box(world, min, max),
        block_entities: read_block_entities(world, min, max),
    }
}

/// Applies a history entry (which restores the old blocks). The changes this does are recorded in
/// the session, which makes the inverse entry.
pub fn apply_section_changes(
    session: &mut EditSession<'_>,
    entry: &HistoryEntry,
    section: usize,
) -> usize {
    let changes = &entry.sections[section];
    let base_x = changes.chunk_x << 4;
    let base_z = changes.chunk_z << 4;
    let base_y = (changes.section_y as i32) << 4;
    if base_y >= PLOT_BLOCK_HEIGHT {
        return 0;
    }
    let mut count = 0;
    for run in changes.runs.iter().rev() {
        for idx in (run.start..run.start + run.len).rev() {
            let pos = BlockPos::new(
                base_x + (idx & 0xF) as i32,
                base_y + (idx >> 8) as i32,
                base_z + ((idx >> 4) & 0xF) as i32,
            );
            let old = session.world.get_block_raw(pos);
            if old != run.old as u32 && session.set_unchecked(pos, old, run.old as u32) {
                count += 1;
            }
        }
    }
    count
}

pub fn apply_entities(session: &mut EditSession<'_>, entry: &HistoryEntry) {
    for (pos, entity) in entry.block_entities.iter().rev() {
        session.restore_block_entity(*pos, entity.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn world() -> PlotWorld {
        PlotWorld::empty_for_test(0, 0)
    }

    #[test]
    fn edit_box_sets_and_records() {
        let mut world = world();
        let mut recorder = Recorder::new();
        let mut session = EditSession::new(&mut world, &mut recorder, None);
        let n = session.edit_box(BlockPos::new(0, 0, 0), BlockPos::new(31, 15, 31), |_, _| {
            Some(1)
        });
        assert_eq!(n, 32 * 16 * 32);
        assert_eq!(world.get_block_raw(BlockPos::new(31, 15, 31)), 1);
        assert_eq!(world.get_block_raw(BlockPos::new(32, 15, 31)), 0);
        let entry = recorder.take_entry(0, 0);
        assert_eq!(entry.blocks, 32 * 16 * 32);
        assert_eq!(entry.sections.len(), 4);
    }

    #[test]
    fn edit_box_clamps_to_plot() {
        let mut world = world();
        let mut recorder = Recorder::new();
        let mut session = EditSession::new(&mut world, &mut recorder, None);
        let n = session.edit_box(BlockPos::new(-5, -5, -5), BlockPos::new(1, 1, 1), |_, _| {
            Some(1)
        });
        assert_eq!(n, 2 * 2 * 2);
        let n = session.edit_box(
            BlockPos::new(-100, 0, 0),
            BlockPos::new(-50, 5, 5),
            |_, _| Some(1),
        );
        assert_eq!(n, 0);
    }

    #[test]
    fn undo_restores_everything() {
        let mut world = world();
        let min = BlockPos::new(3, 4, 5);
        let max = BlockPos::new(40, 30, 20);
        {
            let mut rec = Recorder::new();
            let mut s = EditSession::new(&mut world, &mut rec, None);
            s.edit_box(min, max, |p, _| Some(((p.x + p.y + p.z) % 5 + 1) as u32));
        }
        let before = read_box(&world, min, max);
        let mut recorder = Recorder::new();
        {
            let mut s = EditSession::new(&mut world, &mut recorder, None);
            s.edit_box(min, max, |_, _| Some(9));
            // and a second time over the same blocks
            s.edit_box(min, max, |p, _| Some(10 + (p.x & 1) as u32));
        }
        let entry = recorder.take_entry(0, 0);
        let mut redo = Recorder::new();
        {
            let mut s = EditSession::new(&mut world, &mut redo, None);
            for i in 0..entry.sections.len() {
                apply_section_changes(&mut s, &entry, i);
            }
        }
        assert_eq!(read_box(&world, min, max), before);
        // Undoing the undo brings the edit back
        let redo_entry = redo.take_entry(0, 0);
        let mut again = Recorder::new();
        let mut s = EditSession::new(&mut world, &mut again, None);
        for i in 0..redo_entry.sections.len() {
            apply_section_changes(&mut s, &redo_entry, i);
        }
        assert_eq!(world.get_block_raw(BlockPos::new(4, 5, 6)), 10);
        assert_eq!(world.get_block_raw(BlockPos::new(5, 5, 6)), 11);
    }

    #[test]
    fn mask_limits_the_edit() {
        let mut world = world();
        world.set_block_raw(BlockPos::new(1, 1, 1), 1);
        let mask = Mask::parse("stone").unwrap();
        let mut recorder = Recorder::new();
        let mut session = EditSession::new(&mut world, &mut recorder, Some(&mask));
        let n = session.edit_box(BlockPos::new(0, 0, 0), BlockPos::new(2, 2, 2), |_, _| {
            Some(2)
        });
        assert_eq!(n, 1);
        assert_eq!(world.get_block_raw(BlockPos::new(1, 1, 1)), 2);
        assert_eq!(world.get_block_raw(BlockPos::new(0, 0, 0)), 0);
    }

    #[test]
    fn complex_mask_uses_neighbours() {
        let mut world = world();
        world.set_block_raw(BlockPos::new(1, 0, 1), 1);
        let mask = Mask::parse("#air&<stone").unwrap();
        let mut recorder = Recorder::new();
        let mut session = EditSession::new(&mut world, &mut recorder, Some(&mask));
        let n = session.edit_box(BlockPos::new(0, 0, 0), BlockPos::new(2, 2, 2), |_, _| {
            Some(2)
        });
        assert_eq!(n, 1);
        assert_eq!(world.get_block_raw(BlockPos::new(1, 1, 1)), 2);
    }

    #[test]
    fn clipboard_roundtrip_through_world() {
        let mut world = world();
        world.set_block_raw(BlockPos::new(10, 3, 10), 5);
        world.set_block_raw(BlockPos::new(12, 4, 11), 6);
        let cb = create_clipboard(
            &world,
            BlockPos::new(10, 3, 10),
            BlockPos::new(12, 4, 11),
            BlockPos::new(10, 3, 10),
        );
        assert_eq!((cb.size_x, cb.size_y, cb.size_z), (3, 2, 2));
        assert_eq!(cb.get(0, 0, 0), 5);
        assert_eq!(cb.get(2, 1, 1), 6);
        assert_eq!((cb.offset_x, cb.offset_y, cb.offset_z), (0, 0, 0));
    }

    #[test]
    fn replacing_a_block_entity_block_drops_the_entity() {
        let mut world = world();
        let pos = BlockPos::new(5, 5, 5);
        let furnace = Block::Furnace {
            facing: cinnabar_blocks::BlockDirection::North,
            lit: false,
        }
        .get_id();
        world.set_block_raw(pos, furnace);
        world.set_block_entity(
            pos,
            BlockEntity::Container {
                comparator_override: 0,
                inventory: Vec::new(),
                ty: cinnabar_blocks::block_entities::ContainerType::Furnace,
            },
        );
        let mut recorder = Recorder::new();
        let mut session = EditSession::new(&mut world, &mut recorder, None);
        session.edit_box(pos, pos, |_, _| Some(1));
        assert!(world.get_block_entity(pos).is_none());
        let entry = recorder.take_entry(0, 0);
        assert_eq!(entry.block_entities.len(), 1);
        let mut rec2 = Recorder::new();
        let mut s = EditSession::new(&mut world, &mut rec2, None);
        apply_section_changes(&mut s, &entry, 0);
        apply_entities(&mut s, &entry);
        assert!(world.get_block_entity(pos).is_some());
        assert_eq!(world.get_block_raw(pos), furnace);
    }

    #[test]
    fn transform_moves_the_edits() {
        let mut world = world();
        let mut recorder = Recorder::new();
        let transform = GlobalTransform::parse("#offset[0][5][0]", BlockPos::new(0, 0, 0)).unwrap();
        let mut session =
            EditSession::new(&mut world, &mut recorder, None).with_transform(Some(&transform));
        let n = session.edit_box(BlockPos::new(1, 1, 1), BlockPos::new(2, 1, 1), |_, _| {
            Some(1)
        });
        assert_eq!(n, 2);
        session.set(BlockPos::new(4, 1, 4), 1);
        assert_eq!(world.get_block_raw(BlockPos::new(1, 6, 1)), 1);
        assert_eq!(world.get_block_raw(BlockPos::new(1, 1, 1)), 0);
        assert_eq!(world.get_block_raw(BlockPos::new(4, 6, 4)), 1);
    }
}
