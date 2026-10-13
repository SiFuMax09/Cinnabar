//! Brushes that are bound to an item and used with a right click.

use super::job::EditJob;
use super::ops;
use super::pattern::{Mask, Pattern};
use super::{ray_trace_block, SLICE_BUDGET};
use crate::player::PacketSender;
use crate::plot::Plot;
use crate::plot::PlotWorld;
use cinnabar_blocks::BlockPos;
use cinnabar_schematic::WorldEditClipboard;
use std::time::{Duration, Instant};

/// How far away from the player a brush can be used
const BRUSH_RANGE: f64 = 200.0;

/// The biggest size of a brush
pub const MAX_BRUSH_SIZE: u32 = 128;

#[derive(Clone, Debug, PartialEq)]
pub enum BrushKind {
    Sphere { hollow: bool },
    Cylinder { height: u32, hollow: bool },
    Smooth { iterations: u32 },
    Clipboard { ignore_air: bool },
}

#[derive(Clone, Debug)]
pub struct Brush {
    pub kind: BrushKind,
    pub pattern: Pattern,
    pub size: u32,
    /// Only blocks that match are changed
    pub mask: Option<Mask>,
    pub mask_text: Option<String>,
}

impl Brush {
    pub fn describe(&self) -> String {
        let kind = match &self.kind {
            BrushKind::Sphere { hollow } => {
                format!("{}sphere", if *hollow { "hollow " } else { "" })
            }
            BrushKind::Cylinder { height, hollow } => format!(
                "{}cylinder (height {})",
                if *hollow { "hollow " } else { "" },
                height
            ),
            BrushKind::Smooth { iterations } => format!("smooth ({} iterations)", iterations),
            BrushKind::Clipboard { ignore_air } => format!(
                "clipboard{}",
                if *ignore_air { " (without air)" } else { "" }
            ),
        };
        let mut text = format!("{kind}, size {}", self.size);
        if !matches!(
            self.kind,
            BrushKind::Smooth { .. } | BrushKind::Clipboard { .. }
        ) {
            text.push_str(&format!(", pattern {}", self.pattern));
        }
        if let Some(mask) = &self.mask_text {
            text.push_str(&format!(", mask {mask}"));
        }
        text
    }

    pub fn needs_clipboard(&self) -> bool {
        matches!(self.kind, BrushKind::Clipboard { .. }) || self.pattern.needs_clipboard()
    }

    /// The job that applies the brush at `target`
    pub fn job(
        &self,
        world: &PlotWorld,
        owner: u128,
        gmask: Option<Mask>,
        target: BlockPos,
        clipboard: Option<&WorldEditClipboard>,
    ) -> Result<EditJob, String> {
        let r = self.size as i32;
        let needs = self.needs_clipboard();
        let clipboard_copy = clipboard.cloned();
        if needs && clipboard_copy.is_none() {
            return Err("Your clipboard is empty. Use //copy first.".to_owned());
        }
        Ok(match &self.kind {
            BrushKind::Sphere { hollow } => ops::sphere_job(
                world,
                owner,
                gmask,
                self.mask.clone(),
                target,
                self.size,
                *hollow,
                self.pattern.clone(),
                clipboard_copy,
            ),
            BrushKind::Cylinder { height, hollow } => ops::cylinder_job(
                world,
                owner,
                gmask,
                self.mask.clone(),
                target,
                self.size,
                *height as i32,
                *hollow,
                self.pattern.clone(),
                clipboard_copy,
            ),
            BrushKind::Smooth { iterations } => ops::smooth_job(
                owner,
                ops::combine_masks(gmask, self.mask.clone()),
                BlockPos::new(target.x - r, target.y - r, target.z - r),
                BlockPos::new(target.x + r, target.y + r, target.z + r),
                *iterations,
                None,
            ),
            BrushKind::Clipboard { ignore_air } => {
                let cb = clipboard_copy.unwrap();
                let base = BlockPos::new(
                    target.x - cb.offset_x,
                    target.y - cb.offset_y,
                    target.z - cb.offset_z,
                );
                ops::paste_job(
                    world,
                    owner,
                    ops::combine_masks(gmask, self.mask.clone()),
                    cb,
                    vec![base],
                    *ignore_air,
                )
            }
        })
    }
}

impl Plot {
    /// Uses the brush that is bound to the item `item_id` in the hand of a player. `clicked` is the
    /// block that was clicked, without it the block the player looks at is used. Returns false if
    /// there is no brush for the item.
    pub(crate) fn try_use_brush(
        &mut self,
        player_idx: usize,
        item_id: u32,
        clicked: Option<BlockPos>,
    ) -> bool {
        let player = &mut self.players[player_idx];
        let Some(brush) = player.worldedit_brushes.get(&item_id).cloned() else {
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
        let target = clicked.or_else(|| {
            ray_trace_block(
                &self.world,
                player.pos,
                player.pitch as f64,
                player.yaw as f64,
                BRUSH_RANGE,
            )
        });
        let Some(target) = target else {
            player.send_error_message("No block in sight!");
            return true;
        };
        if !Plot::in_plot_bounds(self.world.x, self.world.z, target.x, target.z) {
            player.send_error_message("That block is outside of your plot.");
            return true;
        }
        match brush.job(
            &self.world,
            player.uuid,
            player.worldedit_mask.clone(),
            target,
            player.worldedit_clipboard.as_ref(),
        ) {
            Ok(job) => {
                self.reset_redpiler();
                self.edit_jobs.push_back(job.quiet());
                self.run_edit_jobs(SLICE_BUDGET);
            }
            Err(message) => player.send_error_message(&message),
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cinnabar_world::World;

    fn sphere(size: u32) -> Brush {
        Brush {
            kind: BrushKind::Sphere { hollow: false },
            pattern: Pattern::parse("stone").unwrap(),
            size,
            mask: None,
            mask_text: None,
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
        let mut job = brush.job(&world, 0, None, target, None).unwrap();
        let entry = job.run_for_test(&mut world);
        assert!(entry.blocks > 100);
        assert_eq!(world.get_block_raw(target), 1);

        // With a mask only matching blocks change
        brush.pattern = Pattern::parse("dirt").unwrap();
        brush.mask = Some(Mask::parse("air").unwrap());
        let before = world.get_block_raw(target);
        let mut job = brush
            .job(&world, 0, None, BlockPos::new(30, 30, 34), None)
            .unwrap();
        job.run_for_test(&mut world);
        assert_eq!(world.get_block_raw(target), before);
        assert_ne!(world.get_block_raw(BlockPos::new(30, 30, 37)), 0);
    }

    #[test]
    fn clipboard_brush_needs_a_clipboard() {
        let world = PlotWorld::empty_for_test(0, 0);
        let mut brush = sphere(1);
        brush.kind = BrushKind::Clipboard { ignore_air: false };
        assert!(brush.job(&world, 0, None, BlockPos::zero(), None).is_err());
    }
}
