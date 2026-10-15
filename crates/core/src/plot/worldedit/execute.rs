use super::job::*;
use super::ops;
use super::pattern::FastRng;
use super::session::{create_clipboard, read_box};
use super::shapes;
use super::transform::{flip_clipboard, rotate_clipboard, Axis};
use super::*;
use crate::player::PacketSender;
use crate::plot::PLOT_BLOCK_HEIGHT;
use crate::utils;
use cinnabar_blocks::block_entities::{BlockEntity, InventoryEntry};
use cinnabar_blocks::blocks::{Block, HopperFacing};
use cinnabar_blocks::items::{Item, ItemStack};
use cinnabar_blocks::{BlockDirection, BlockFacing, BlockPos};
use cinnabar_network::packets::clientbound::*;
use cinnabar_schematic::WorldEditClipboard;
use cinnabar_text::{ColorCode, TextComponentBuilder};
use std::time::Instant;

pub(super) fn execute_wand(ctx: CommandExecuteContext<'_>) {
    let item = ItemStack {
        count: 1,
        item_type: Item::WoodenAxe,
        nbt: None,
    };
    ctx.player.inventory[(ctx.player.selected_slot + 36) as usize] = Some(item);
    let entity_equipment = CSetEquipment {
        entity_id: ctx.player.entity_id as i32,
        equipment: vec![CSetEquipmentEquipment {
            slot: 0,
            item: ctx.player.inventory[(ctx.player.selected_slot + 36) as usize]
                .as_ref()
                .map(utils::encode_slot_data),
        }],
    }
    .encode();
    for player in &mut ctx.plot.packet_senders {
        player.send_packet(&entity_equipment);
    }
    ctx.player
        .send_worldedit_message("Left click: select pos #1; Right click: select pos #2");
}

/// The corners of the selection with the smaller coordinates first
fn selection(ctx: &CommandExecuteContext<'_>) -> (BlockPos, BlockPos) {
    let first = ctx.player.first_position.unwrap();
    let second = ctx.player.second_position.unwrap();
    (first.min(second), first.max(second))
}

/// The clipboard of the player if the pattern needs it. Tells the player when there is none.
fn clipboard_for(
    ctx: &CommandExecuteContext<'_>,
    pattern: &Pattern,
) -> Result<Option<WorldEditClipboard>, ()> {
    if !pattern.needs_clipboard() {
        return Ok(None);
    }
    match &ctx.player.worldedit_clipboard {
        Some(clipboard) => Ok(Some(clipboard.clone())),
        None => {
            ctx.player
                .send_error_message("Your clipboard is empty. Use //copy first.");
            Err(())
        }
    }
}

fn held_item_id(player: &Player) -> Option<u32> {
    player.inventory[player.selected_slot as usize + 36]
        .as_ref()
        .map(|item| item.item_type.get_id())
}

pub(super) fn execute_set(ctx: CommandExecuteContext<'_>) {
    let pattern = ctx.arguments[0].unwrap_pattern().clone();
    let Ok(clipboard) = clipboard_for(&ctx, &pattern) else {
        return;
    };
    let (min, max) = selection(&ctx);
    let job = ops::region_job(
        ctx.plot,
        ctx.player.uuid,
        ctx.player.worldedit_mask.clone(),
        None,
        min,
        max,
        pattern,
        clipboard,
        None,
    )
    .on_finish(|player, stats| {
        // FAWE stays quiet when nothing changed
        if stats.blocks != 0 {
            player.send_worldedit_message(&format!("Operation completed ({}).", stats.blocks));
        }
    });
    ctx.jobs.push_back(job);
}

pub(super) fn execute_replace(ctx: CommandExecuteContext<'_>) {
    let filter = ctx.arguments[0].unwrap_mask().clone();
    let pattern = ctx.arguments[1].unwrap_pattern().clone();
    let Ok(clipboard) = clipboard_for(&ctx, &pattern) else {
        return;
    };
    let (min, max) = selection(&ctx);
    let job = ops::region_job(
        ctx.plot,
        ctx.player.uuid,
        ctx.player.worldedit_mask.clone(),
        Some(filter),
        min,
        max,
        pattern,
        clipboard,
        None,
    )
    .on_finish(|player, stats| {
        player.send_worldedit_message(&format!("{} blocks have been replaced.", stats.blocks));
    });
    ctx.jobs.push_back(job);
}

pub(super) fn execute_replace_near(ctx: CommandExecuteContext<'_>) {
    let size = ctx.arguments[0].unwrap_uint().clamp(1, 256) as i32;
    let filter = ctx.arguments[1].unwrap_mask().clone();
    let pattern = ctx.arguments[2].unwrap_pattern().clone();
    let Ok(clipboard) = clipboard_for(&ctx, &pattern) else {
        return;
    };
    let center = ctx.player.pos.block_pos();
    let job = ops::region_job(
        ctx.plot,
        ctx.player.uuid,
        ctx.player.worldedit_mask.clone(),
        Some(filter),
        BlockPos::new(center.x - size, center.y - size, center.z - size),
        BlockPos::new(center.x + size, center.y + size, center.z + size),
        pattern,
        clipboard,
        None,
    )
    .on_finish(|player, stats| {
        player.send_worldedit_message(&format!("{} blocks have been replaced.", stats.blocks));
    });
    ctx.jobs.push_back(job);
}

pub(super) fn execute_count(ctx: CommandExecuteContext<'_>) {
    let filter = ctx.arguments[0].unwrap_mask();
    let (min, max) = selection(&ctx);
    let data = read_box(ctx.plot, min, max);
    let counted = if filter.is_state_only() {
        data.iter()
            .filter(|id| filter.test_state(**id as u32))
            .count()
    } else {
        let mut rng = FastRng::new();
        let mut counted = 0;
        let mut i = 0;
        for y in min.y..=max.y {
            for z in min.z..=max.z {
                for x in min.x..=max.x {
                    let pos = BlockPos::new(x, y, z);
                    counted += filter.test(&*ctx.plot, pos, data[i] as u32, &mut rng) as usize;
                    i += 1;
                }
            }
        }
        counted
    };

    ctx.player
        .send_worldedit_message(&format!("Counted: {}", counted));
}

/// The include mask of `-m` and `-a`: blocks that do not match are not copied. Returns the
/// clipboard without them (as air, so pasting with `ignore_air` skips them) and how many blocks
/// are left.
fn filter_clipboard(
    world: &PlotWorld,
    mut clipboard: WorldEditClipboard,
    min: BlockPos,
    mask: Option<&Mask>,
    ignore_air: bool,
) -> (WorldEditClipboard, usize) {
    if mask.is_none() && !ignore_air {
        let volume = clipboard.volume();
        return (clipboard, volume);
    }
    let mut rng = FastRng::new();
    let mut kept = 0;
    let (sx, sy, sz) = (clipboard.size_x, clipboard.size_y, clipboard.size_z);
    for y in 0..sy {
        for z in 0..sz {
            for x in 0..sx {
                let i = clipboard.index(x, y, z);
                let id = clipboard.data[i] as u32;
                let pos = min + BlockPos::new(x as i32, y as i32, z as i32);
                let keep = (!ignore_air || id != 0)
                    && mask.is_none_or(|mask| mask.test(world, pos, id, &mut rng));
                if keep {
                    kept += 1;
                } else {
                    clipboard.data[i] = 0;
                    clipboard
                        .block_entities
                        .remove(&BlockPos::new(x as i32, y as i32, z as i32));
                }
            }
        }
    }
    (clipboard, kept)
}

/// The mask given with `-m`, or an error message for the player
fn mask_flag(ctx: &CommandExecuteContext<'_>) -> Result<Option<Mask>, ()> {
    match ctx.flag_arg('m') {
        Some(text) => match Mask::parse(text) {
            Ok(mask) => Ok(Some(mask)),
            Err(err) => {
                ctx.player.send_error_message(&err.to_string());
                Err(())
            }
        },
        None => Ok(None),
    }
}

pub(super) fn execute_move(mut ctx: CommandExecuteContext<'_>) {
    let count = ctx.arguments[0].unwrap_int();
    let direction = ctx.arguments[1].unwrap_direction_vec();
    let replace = ctx.arguments[2].unwrap_pattern().clone();
    if count < 1 {
        ctx.player.send_error_message("Multiplier must be >= 1");
        return;
    }
    let Ok(mask) = mask_flag(&ctx) else { return };

    let (min, max) = selection(&ctx);
    let offset = direction * count;
    let (clipboard, moved) = filter_clipboard(
        ctx.plot,
        create_clipboard(ctx.plot, min, min, max),
        min,
        mask.as_ref(),
        ctx.has_flag('a'),
    );

    let job = EditJob::new(ctx.player.uuid, ctx.player.worldedit_mask.clone())
        .stage(ops::clear_stage(ctx.plot, min, max, replace))
        .stage(
            ops::paste_stage(ctx.plot, clipboard, vec![min + offset], true)
                .commit_to(HistoryDest::Undo),
        )
        .on_finish(move |player, _| {
            player.send_worldedit_message(&format!("{moved} blocks moved."));
        });
    ctx.jobs.push_back(job);

    if ctx.has_flag('s') {
        let first_pos = ctx.player.first_position.unwrap() + offset;
        let second_pos = ctx.player.second_position.unwrap() + offset;
        let player = &mut ctx.player;
        player.worldedit_set_first_position(first_pos);
        player.worldedit_set_second_position(second_pos);
    }
}

/// //fall: the blocks of every column fall down until they rest on something
pub(super) fn execute_fall(ctx: CommandExecuteContext<'_>) {
    let replace = ctx.arguments[0].unwrap_pattern().clone();
    let Some(replace_id) = replace.as_single() else {
        ctx.player
            .send_error_message("The block to leave behind has to be a single block.");
        return;
    };
    let (min, max) = selection(&ctx);
    // Without -m the blocks fall down to the bottom of the plot
    let check_min = if ctx.has_flag('m') { min.y } else { 0 };
    let items = column_items(
        ctx.plot,
        BlockPos::new(min.x, check_min, min.z),
        BlockPos::new(max.x, max.y, max.z),
    );
    let (start_y, end_y) = (min.y, max.y);
    let run: RunFn = Box::new(move |session, item| {
        let WorkItem::Box(lo, hi) = item else {
            return 0;
        };
        let mut moved = 0;
        for x in lo.x..=hi.x {
            for z in lo.z..=hi.z {
                let mut free = check_min;
                for y in check_min..=end_y {
                    let pos = BlockPos::new(x, y, z);
                    let id = session.get(pos);
                    if y < start_y {
                        if id != replace_id {
                            free = y + 1;
                        }
                        continue;
                    }
                    if id != replace_id {
                        if free != y {
                            moved += session.set(BlockPos::new(x, free, z), id) as usize;
                            session.set(pos, replace_id);
                        }
                        free += 1;
                    }
                }
            }
        }
        moved
    });
    let job = EditJob::single(
        ctx.player.uuid,
        ctx.player.worldedit_mask.clone(),
        items,
        run,
    )
    .on_finish(|player, stats| {
        player.send_worldedit_message(&format!("{} blocks affected", stats.blocks));
    });
    ctx.jobs.push_back(job);
}

/// Lets the blocks of the area update their neighbours, as a stage that comes after the edit
pub(super) fn update_stage(world: &PlotWorld, min: BlockPos, max: BlockPos) -> Stage {
    Stage::new(
        section_items(world, min, max),
        Box::new(|session, item| {
            let WorkItem::Box(lo, hi) = item else {
                return 0;
            };
            for y in lo.y..=hi.y {
                for z in lo.z..=hi.z {
                    for x in lo.x..=hi.x {
                        let pos = BlockPos::new(x, y, z);
                        if session.get(pos) != 0 {
                            let block = session.world.get_block(pos);
                            cinnabar_redstone::update(block, session.world, pos);
                        }
                    }
                }
            }
            0
        }),
    )
}

pub(super) fn execute_stack(ctx: CommandExecuteContext<'_>) {
    let count = ctx.arguments[0].unwrap_int();
    let offset = ctx.arguments[1].unwrap_direction_vec();
    if count < 1 {
        ctx.player.send_error_message("Count must be >= 1");
        return;
    }
    let Ok(mask) = mask_flag(&ctx) else { return };
    // More copies than fit in the plot would be placed outside of it anyway
    let count = count.min(100_000);
    let (min, max) = selection(&ctx);
    let (clipboard, _) = filter_clipboard(
        ctx.plot,
        create_clipboard(ctx.plot, min, min, max),
        min,
        mask.as_ref(),
        ctx.has_flag('a'),
    );
    // The offset is in region sizes, with -r in blocks
    let step = if ctx.has_flag('r') {
        offset
    } else {
        BlockPos::new(
            offset.x * clipboard.size_x as i32,
            offset.y * clipboard.size_y as i32,
            offset.z * clipboard.size_z as i32,
        )
    };
    let bases = (1..=count).map(|i| min + step * i).collect();
    let job = ops::paste_job(
        ctx.plot,
        ctx.player.uuid,
        ctx.player.worldedit_mask.clone(),
        clipboard,
        bases,
        true,
    )
    .on_finish(|player, stats| {
        player.send_worldedit_message(&format!(
            "{} blocks changed. Undo with //undo",
            stats.blocks
        ));
    });
    ctx.jobs.push_back(job);

    if ctx.has_flag('s') {
        let shift = step * count;
        let first_pos = ctx.player.first_position.unwrap() + shift;
        let second_pos = ctx.player.second_position.unwrap() + shift;
        ctx.player.worldedit_set_first_position(first_pos);
        ctx.player.worldedit_set_second_position(second_pos);
    }
}

pub(super) fn execute_rstack(ctx: CommandExecuteContext<'_>) {
    let stack_amt = ctx.arguments[0].unwrap_uint();
    let stack_spacing = ctx.arguments[1].unwrap_uint();
    let direction = ctx.arguments[2].unwrap_direction_vec();
    let (min, max) = selection(&ctx);
    let clipboard = create_clipboard(ctx.plot, min, min, max);
    let bases = (1..=stack_amt.min(10_000))
        .map(|i| min + direction * (i * stack_spacing) as i32)
        .collect();
    let job = ops::paste_job(
        ctx.plot,
        ctx.player.uuid,
        ctx.player.worldedit_mask.clone(),
        clipboard,
        bases,
        !ctx.has_flag('a'),
    )
    .on_finish(|player, stats| {
        player.send_worldedit_message(&format!(
            "Your selection was stacked successfully. ({:?})",
            stats.elapsed
        ));
    });
    ctx.jobs.push_back(job);

    if ctx.has_flag('e') {
        expand_selection(
            ctx.player,
            direction * (stack_amt * stack_spacing) as i32,
            false,
        );
    }
}

pub(super) fn execute_flip(ctx: CommandExecuteContext<'_>) {
    let direction = ctx.arguments[0].unwrap_direction();
    let axis = match direction {
        BlockFacing::East | BlockFacing::West => Axis::X,
        BlockFacing::North | BlockFacing::South => Axis::Z,
        BlockFacing::Up | BlockFacing::Down => Axis::Y,
    };
    let clipboard = ctx.player.worldedit_clipboard.as_ref().unwrap();
    let flipped = flip_clipboard(clipboard, axis);

    ctx.player.worldedit_clipboard = Some(flipped);
    ctx.player
        .send_worldedit_message("The clipboard copy has been flipped.");
}

pub(super) fn execute_rotate(ctx: CommandExecuteContext<'_>) {
    let mut rotations = Vec::new();
    for (axis, arg) in [Axis::Y, Axis::X, Axis::Z].into_iter().zip(&ctx.arguments) {
        let degrees = arg.unwrap_int();
        if degrees % 90 != 0 {
            ctx.player
                .send_error_message("Rotate amount must be a multiple of 90.");
            return;
        }
        rotations.push((axis, degrees / 90));
    }
    let mut clipboard = ctx.player.worldedit_clipboard.take().unwrap();
    for (axis, turns) in rotations {
        clipboard = rotate_clipboard(&clipboard, axis, turns);
    }
    ctx.player.worldedit_clipboard = Some(clipboard);
    ctx.player
        .send_worldedit_message("The clipboard copy has been rotated.");
}

fn count_message(job: EditJob, message: impl Fn(usize) -> String + 'static) -> EditJob {
    job.on_finish(move |player, stats| player.send_worldedit_message(&message(stats.blocks)))
}

fn created(job: EditJob) -> EditJob {
    count_message(job, |n| format!("{n} blocks have been created."))
}

/// The radii of a shape command, tells the player when they are wrong
fn radii_arg(
    ctx: &CommandExecuteContext<'_>,
    index: usize,
    allow_y: bool,
) -> Option<(f64, f64, f64)> {
    match shapes::parse_radii(ctx.arguments[index].unwrap_string(), allow_y) {
        Ok((x, y, z)) => {
            let limit = 512.0;
            Some((x.min(limit), y.min(limit), z.min(limit)))
        }
        Err(message) => {
            ctx.player.send_error_message(&message);
            None
        }
    }
}

fn sphere(ctx: CommandExecuteContext<'_>, hollow: bool) {
    let pattern = ctx.arguments[0].unwrap_pattern().clone();
    let Some(radii) = radii_arg(&ctx, 1, true) else {
        return;
    };
    let raised_text = ctx.arguments[2].unwrap_string();
    let raised = match raised_text.as_str() {
        "" => false,
        text => match shapes::parse_bool(text) {
            Some(raised) => raised,
            None => {
                ctx.player
                    .send_error_message(&format!("Expected true or false but got '{text}'."));
                return;
            }
        },
    };
    let hollow = hollow || ctx.has_flag('h');
    let Ok(clipboard) = clipboard_for(&ctx, &pattern) else {
        return;
    };
    let mut center = ctx.player.pos.block_pos();
    if raised {
        center.y += radii.1 as i32;
    }
    let job = ops::ellipsoid_job(
        ctx.plot,
        ctx.player.uuid,
        ctx.player.worldedit_mask.clone(),
        center,
        radii,
        hollow,
        pattern,
        clipboard,
    );
    ctx.jobs.push_back(created(job));
}

pub(super) fn execute_sphere(ctx: CommandExecuteContext<'_>) {
    sphere(ctx, false);
}

pub(super) fn execute_hsphere(ctx: CommandExecuteContext<'_>) {
    sphere(ctx, true);
}

fn cylinder(ctx: CommandExecuteContext<'_>, hollow: bool) {
    let pattern = ctx.arguments[0].unwrap_pattern().clone();
    let Some((rx, _, rz)) = radii_arg(&ctx, 1, false) else {
        return;
    };
    let height = ctx.arguments[2]
        .unwrap_uint()
        .clamp(1, PLOT_BLOCK_HEIGHT as u32);
    let hollow = hollow || ctx.has_flag('h');
    let Ok(clipboard) = clipboard_for(&ctx, &pattern) else {
        return;
    };
    let job = ops::elliptic_cylinder_job(
        ctx.plot,
        ctx.player.uuid,
        ctx.player.worldedit_mask.clone(),
        ctx.player.pos.block_pos(),
        (rx, rz),
        height as i32,
        hollow,
        pattern,
        clipboard,
    );
    ctx.jobs.push_back(created(job));
}

pub(super) fn execute_cyl(ctx: CommandExecuteContext<'_>) {
    cylinder(ctx, false);
}

pub(super) fn execute_hcyl(ctx: CommandExecuteContext<'_>) {
    cylinder(ctx, true);
}

fn pyramid(ctx: CommandExecuteContext<'_>, hollow: bool) {
    let pattern = ctx.arguments[0].unwrap_pattern().clone();
    let size = ctx.arguments[1].unwrap_uint().clamp(1, 512) as i32;
    let hollow = hollow || ctx.has_flag('h');
    let Ok(clipboard) = clipboard_for(&ctx, &pattern) else {
        return;
    };
    let job = ops::pyramid_job(
        ctx.plot,
        ctx.player.uuid,
        ctx.player.worldedit_mask.clone(),
        ctx.player.pos.block_pos(),
        size,
        hollow,
        pattern,
        clipboard,
    );
    ctx.jobs.push_back(created(job));
}

pub(super) fn execute_pyramid(ctx: CommandExecuteContext<'_>) {
    pyramid(ctx, false);
}

pub(super) fn execute_hpyramid(ctx: CommandExecuteContext<'_>) {
    pyramid(ctx, true);
}

pub(super) fn execute_lay(ctx: CommandExecuteContext<'_>) {
    let pattern = ctx.arguments[0].unwrap_pattern().clone();
    let Ok(clipboard) = clipboard_for(&ctx, &pattern) else {
        return;
    };
    let (min, max) = selection(&ctx);
    let job = ops::lay_job(
        ctx.plot,
        ctx.player.uuid,
        ctx.player.worldedit_mask.clone(),
        min,
        max,
        pattern,
        clipboard,
    );
    ctx.jobs.push_back(count_message(job, |n| {
        format!("{n} blocks have been changed.")
    }));
}

pub(super) fn execute_naturalize(ctx: CommandExecuteContext<'_>) {
    let (min, max) = selection(&ctx);
    let job = ops::naturalize_job(
        ctx.plot,
        ctx.player.uuid,
        ctx.player.worldedit_mask.clone(),
        min,
        max,
    );
    ctx.jobs.push_back(count_message(job, |n| {
        format!("{n} block(s) have been made to look more natural.")
    }));
}

fn shell(ctx: CommandExecuteContext<'_>, faces: bool) {
    let pattern = ctx.arguments[0].unwrap_pattern().clone();
    let Ok(clipboard) = clipboard_for(&ctx, &pattern) else {
        return;
    };
    let (min, max) = selection(&ctx);
    let job = ops::shell_job(
        ctx.plot,
        ctx.player.uuid,
        ctx.player.worldedit_mask.clone(),
        min,
        max,
        faces,
        pattern,
        clipboard,
    );
    ctx.jobs.push_back(count_message(job, |n| {
        format!("{n} blocks have been changed.")
    }));
}

pub(super) fn execute_walls(ctx: CommandExecuteContext<'_>) {
    shell(ctx, false);
}

pub(super) fn execute_faces(ctx: CommandExecuteContext<'_>) {
    shell(ctx, true);
}

pub(super) fn execute_overlay(ctx: CommandExecuteContext<'_>) {
    let pattern = ctx.arguments[0].unwrap_pattern().clone();
    let Ok(clipboard) = clipboard_for(&ctx, &pattern) else {
        return;
    };
    let (min, max) = selection(&ctx);
    let job = ops::overlay_job(
        ctx.plot,
        ctx.player.uuid,
        ctx.player.worldedit_mask.clone(),
        min,
        max,
        pattern,
        clipboard,
    );
    ctx.jobs.push_back(count_message(job, |n| {
        format!("{n} blocks have been overlaid.")
    }));
}

/// The biggest area the commands that have to look at all of it work on
const MAX_ANALYSED_VOLUME: i64 = 16 * 1024 * 1024;

fn volume(min: BlockPos, max: BlockPos) -> i64 {
    (max.x - min.x + 1) as i64 * (max.y - min.y + 1) as i64 * (max.z - min.z + 1) as i64
}

pub(super) fn execute_hollow(ctx: CommandExecuteContext<'_>) {
    let thickness = ctx.arguments[0].unwrap_uint();
    let pattern = ctx.arguments[1].unwrap_pattern().clone();
    let Ok(clipboard) = clipboard_for(&ctx, &pattern) else {
        return;
    };
    let (min, max) = selection(&ctx);
    if volume(min, max) > MAX_ANALYSED_VOLUME {
        ctx.player
            .send_error_message("The selection is too big to hollow out.");
        return;
    }
    let job = ops::hollow_job(
        ctx.player.uuid,
        ctx.player.worldedit_mask.clone(),
        min,
        max,
        thickness,
        pattern,
        clipboard,
    );
    ctx.jobs.push_back(count_message(job, |n| {
        format!("{n} blocks have been changed.")
    }));
}

pub(super) fn execute_smooth(ctx: CommandExecuteContext<'_>) {
    let iterations = ctx.arguments[0].unwrap_uint().min(100);
    let mask_text = ctx.arguments[1].unwrap_string();
    let surface = if mask_text.is_empty() {
        None
    } else {
        match Mask::parse(mask_text) {
            Ok(mask) => Some(mask),
            Err(err) => {
                ctx.player.send_error_message(&err.to_string());
                return;
            }
        }
    };
    let (min, max) = selection(&ctx);
    if volume(min, max) > MAX_ANALYSED_VOLUME {
        ctx.player
            .send_error_message("The selection is too big to smooth.");
        return;
    }
    let job = ops::smooth_job(
        ctx.player.uuid,
        ctx.player.worldedit_mask.clone(),
        min,
        max,
        iterations,
        surface,
    );
    ctx.jobs.push_back(count_message(job, |n| {
        format!("Terrain's height map smoothed. {n} blocks changed.")
    }));
}

pub(super) fn execute_line(ctx: CommandExecuteContext<'_>) {
    let pattern = ctx.arguments[0].unwrap_pattern().clone();
    let thickness = ctx.arguments[1].unwrap_uint().min(16);
    let Ok(clipboard) = clipboard_for(&ctx, &pattern) else {
        return;
    };
    let job = ops::line_job(
        ctx.player.uuid,
        ctx.player.worldedit_mask.clone(),
        ctx.player.first_position.unwrap(),
        ctx.player.second_position.unwrap(),
        thickness,
        ctx.has_flag('h'),
        pattern,
        clipboard,
    );
    ctx.jobs.push_back(count_message(job, |n| {
        format!("{n} block(s) have been changed.")
    }));
}

pub(super) fn execute_center(ctx: CommandExecuteContext<'_>) {
    let pattern = ctx.arguments[0].unwrap_pattern().clone();
    let Ok(clipboard) = clipboard_for(&ctx, &pattern) else {
        return;
    };
    let (min, max) = selection(&ctx);
    let (lo, hi) = shapes::center_boxes(min, max);
    let job = ops::region_job(
        ctx.plot,
        ctx.player.uuid,
        ctx.player.worldedit_mask.clone(),
        None,
        lo,
        hi,
        pattern,
        clipboard,
        None,
    )
    .on_finish(|player, stats| {
        player.send_worldedit_message(&format!("Center set. ({} blocks changed)", stats.blocks));
    });
    ctx.jobs.push_back(job);
}

fn fill(ctx: CommandExecuteContext<'_>, depth: u32) {
    let pattern = ctx.arguments[0].unwrap_pattern().clone();
    let radius = ctx.arguments[1].unwrap_uint().min(256);
    let depth = depth.clamp(1, PLOT_BLOCK_HEIGHT as u32);
    let Ok(clipboard) = clipboard_for(&ctx, &pattern) else {
        return;
    };
    if (2 * radius as i64 + 1).pow(2) * depth as i64 > MAX_ANALYSED_VOLUME {
        ctx.player
            .send_error_message("That area is too big to fill, use a smaller radius or depth.");
        return;
    }
    let job = ops::fill_job(
        ctx.player.uuid,
        ctx.player.worldedit_mask.clone(),
        ctx.player.pos.block_pos(),
        radius,
        depth,
        pattern,
        clipboard,
    );
    ctx.jobs.push_back(job);
}

pub(super) fn execute_fill(ctx: CommandExecuteContext<'_>) {
    let depth = ctx.arguments[2].unwrap_uint();
    fill(ctx, depth);
}

pub(super) fn execute_fillr(ctx: CommandExecuteContext<'_>) {
    let depth = ctx.arguments[2].unwrap_uint();
    fill(ctx, depth);
}

pub(super) fn execute_chunk(ctx: CommandExecuteContext<'_>) {
    let coordinates = ctx.arguments[0].unwrap_string().clone();
    let by_chunk = ctx.has_flag('c');
    let selected = ctx.has_flag('s');
    let player = ctx.player;
    let top = PLOT_BLOCK_HEIGHT - 1;

    if selected {
        // All chunks the selection touches
        let (Some(first), Some(second)) = (player.first_position, player.second_position) else {
            player.send_error_message("Make a region selection first.");
            return;
        };
        let (min, max) = (first.min(second), first.max(second));
        let (lo, hi) = (
            BlockPos::new(min.x & !0xF, 0, min.z & !0xF),
            BlockPos::new(max.x | 0xF, top, max.z | 0xF),
        );
        player.worldedit_set_first_position(lo);
        player.worldedit_set_second_position(hi);
        player.send_worldedit_message(&format!(
            "Chunks selected: ({}, {}) - ({}, {})",
            lo.x >> 4,
            lo.z >> 4,
            hi.x >> 4,
            hi.z >> 4
        ));
        return;
    }

    let (chunk_x, chunk_z) = if coordinates.is_empty() {
        let pos = player.pos.block_pos();
        (pos.x >> 4, pos.z >> 4)
    } else {
        let parts: Vec<&str> = coordinates.split(',').map(str::trim).collect();
        let parsed: Option<Vec<i32>> = parts.iter().map(|part| part.parse().ok()).collect();
        match parsed.as_deref() {
            Some([x, z]) if by_chunk => (*x, *z),
            Some([x, z]) => (x >> 4, z >> 4),
            _ => {
                player.send_error_message(&format!("Invalid coordinates {}", coordinates));
                return;
            }
        }
    };
    let (x, z) = (chunk_x << 4, chunk_z << 4);
    player.worldedit_set_first_position(BlockPos::new(x, 0, z));
    player.worldedit_set_second_position(BlockPos::new(x + 15, top, z + 15));
    player.send_worldedit_message(&format!("Chunk selected: {}, {}", chunk_x, chunk_z));
}

fn parse_optional_mask(text: &str) -> Result<Option<Mask>, String> {
    if text.is_empty() || matches!(text, "none" | "off" | "clear") {
        return Ok(None);
    }
    Mask::parse(text).map(Some).map_err(|err| err.to_string())
}

pub(super) fn execute_gmask(ctx: CommandExecuteContext<'_>) {
    match parse_optional_mask(ctx.arguments[0].unwrap_string()) {
        Ok(mask) => {
            let message = match mask {
                Some(_) => "Global mask set.",
                None => "Global mask disabled.",
            };
            ctx.player.worldedit_mask = mask;
            ctx.player.send_worldedit_message(message);
        }
        Err(message) => ctx.player.send_error_message(&message),
    }
}

pub(super) fn execute_sel(ctx: CommandExecuteContext<'_>) {
    let selector = ctx.arguments[0].unwrap_string().to_ascii_lowercase();
    let player = ctx.player;
    match selector.as_str() {
        "" => {
            if !ctx.flags.contains(&'d') {
                player.first_position = None;
                player.second_position = None;
                player.worldedit_send_cui("s|cuboid");
            }
            player.send_worldedit_message("Selection cleared.");
        }
        "cuboid" => {
            player.first_position = None;
            player.second_position = None;
            player.worldedit_send_cui("s|cuboid");
            player
                .send_worldedit_message("Cuboid: left click for point 1, right click for point 2");
        }
        other => {
            player.send_error_message(&format!(
                "Only the cuboid selector is supported, not '{}'.",
                other
            ));
        }
    }
}

pub(super) fn execute_visualize(ctx: CommandExecuteContext<'_>) {
    let player = ctx.player;
    player.selection_viz.enabled = !player.selection_viz.enabled;
    let state = if player.selection_viz.enabled {
        "enabled"
    } else {
        "disabled"
    };
    let mut message = format!("Selection visualization {state}.");
    if player.selection_viz.enabled && player.selection_viz.cui {
        message.push_str(" It is hidden while WorldEditCUI is in use.");
    }
    player.send_worldedit_message(&message);
}

/// Reads `x,y,z` (each part may be relative to the player with a leading `~`). An empty text
/// means the block the player is standing in.
fn parse_coordinates(player: &Player, text: &str) -> Result<BlockPos, ()> {
    let here = player.pos.block_pos();
    if text.is_empty() {
        return Ok(here);
    }
    let parts: Vec<&str> = text.split(',').map(str::trim).collect();
    if parts.len() != 3 {
        return Err(());
    }
    let mut values = [0i32; 3];
    for (i, (part, base)) in parts.iter().zip([here.x, here.y, here.z]).enumerate() {
        values[i] = if let Some(offset) = part.strip_prefix('~') {
            base + if offset.is_empty() {
                0
            } else {
                offset.parse::<i32>().map_err(|_| ())?
            }
        } else {
            part.parse::<i32>().map_err(|_| ())?
        };
    }
    Ok(BlockPos::new(values[0], values[1], values[2]))
}

pub(super) fn execute_pos1(ctx: CommandExecuteContext<'_>) {
    let text = ctx.arguments[0].unwrap_string();
    match parse_coordinates(ctx.player, text) {
        Ok(pos) => ctx.player.worldedit_set_first_position(pos),
        Err(()) => ctx
            .player
            .send_error_message(&format!("Invalid coordinates {}", text)),
    }
}

pub(super) fn execute_pos2(ctx: CommandExecuteContext<'_>) {
    let text = ctx.arguments[0].unwrap_string();
    match parse_coordinates(ctx.player, text) {
        Ok(pos) => ctx.player.worldedit_set_second_position(pos),
        Err(()) => ctx
            .player
            .send_error_message(&format!("Invalid coordinates {}", text)),
    }
}

pub(super) fn execute_hpos1(mut ctx: CommandExecuteContext<'_>) {
    let player = &mut ctx.player;
    let pitch = player.pitch as f64;
    let yaw = player.yaw as f64;

    let result = ray_trace_block(ctx.plot, player.pos, pitch, yaw, 300.0);

    let player = ctx.player;
    match result {
        Some(pos) => player.worldedit_set_first_position(pos),
        None => player.send_error_message("No block in sight!"),
    }
}

pub(super) fn execute_hpos2(mut ctx: CommandExecuteContext<'_>) {
    let player = &mut ctx.player;
    let pitch = player.pitch as f64;
    let yaw = player.yaw as f64;

    let result = ray_trace_block(ctx.plot, player.pos, pitch, yaw, 300.0);

    let player = &mut ctx.player;
    match result {
        Some(pos) => player.worldedit_set_second_position(pos),
        None => player.send_error_message("No block in sight!"),
    }
}

fn vector_of(direction: BlockFacing, amount: i32) -> BlockPos {
    direction.offset_pos(BlockPos::zero(), amount)
}

/// The direction and amounts of `//expand` and `//contract`: `<amount> [reverseAmount] [direction]`
fn amounts_and_direction(
    player: &Player,
    amount: i64,
    second: &str,
    third: &str,
) -> Result<(i64, i64, BlockFacing), String> {
    let player_facing = player.get_facing();
    let (reverse, direction) = if third.is_empty() {
        match second.parse::<i64>() {
            Ok(reverse) if !second.is_empty() => (reverse, "me"),
            _ => (0, if second.is_empty() { "me" } else { second }),
        }
    } else {
        let reverse = second
            .parse::<i64>()
            .map_err(|_| format!("Invalid reverse amount {}", second))?;
        (reverse, third)
    };
    match parse_facing_name(direction, player_facing) {
        Some(facing) => Ok((amount, reverse, facing)),
        None => Err(format!("Unknown direction {}", direction)),
    }
}

fn clamp_height(mut pos: BlockPos) -> BlockPos {
    pos.y = pos.y.clamp(0, PLOT_BLOCK_HEIGHT - 1);
    pos
}

pub(super) fn execute_expand(ctx: CommandExecuteContext<'_>) {
    let amount_text = ctx.arguments[0].unwrap_string().clone();
    let second = ctx.arguments[1].unwrap_string().clone();
    let third = ctx.arguments[2].unwrap_string().clone();
    let player = ctx.player;
    let first_pos = player.first_position.unwrap();
    let second_pos = player.second_position.unwrap();
    let old_volume = volume(first_pos.min(second_pos), first_pos.max(second_pos));

    let (p1, p2, vertical) = if amount_text.eq_ignore_ascii_case("vert")
        || amount_text.eq_ignore_ascii_case("vertical")
    {
        let mut p1 = first_pos;
        let mut p2 = second_pos;
        if p1.y <= p2.y {
            p1.y = 0;
            p2.y = PLOT_BLOCK_HEIGHT - 1;
        } else {
            p2.y = 0;
            p1.y = PLOT_BLOCK_HEIGHT - 1;
        }
        (p1, p2, true)
    } else {
        let Ok(amount) = amount_text.parse::<i64>() else {
            player.send_error_message(&format!("Invalid amount {}", amount_text));
            return;
        };
        let (amount, reverse, direction) =
            match amounts_and_direction(player, amount, &second, &third) {
                Ok(parsed) => parsed,
                Err(err) => {
                    player.send_error_message(&err);
                    return;
                }
            };
        let (mut p1, mut p2) = adjusted_corners(
            first_pos,
            second_pos,
            vector_of(direction, amount as i32),
            false,
        );
        if reverse != 0 {
            (p1, p2) = adjusted_corners(p1, p2, vector_of(direction, -(reverse as i32)), false);
        }
        (p1, p2, false)
    };
    let (p1, p2) = (clamp_height(p1), clamp_height(p2));
    set_corners(player, p1, p2);
    let new_volume = volume(p1.min(p2), p1.max(p2));
    let message = if vertical {
        format!(
            "Region expanded {} blocks (top-to-bottom).",
            new_volume - old_volume
        )
    } else {
        format!("Region expanded {} blocks", new_volume - old_volume)
    };
    player.send_worldedit_message(&message);
}

pub(super) fn execute_contract(ctx: CommandExecuteContext<'_>) {
    let amount = ctx.arguments[0].unwrap_uint() as i64;
    let second = ctx.arguments[1].unwrap_string().clone();
    let third = ctx.arguments[2].unwrap_string().clone();
    let player = ctx.player;
    let first_pos = player.first_position.unwrap();
    let second_pos = player.second_position.unwrap();
    let old_volume = volume(first_pos.min(second_pos), first_pos.max(second_pos));

    let (amount, reverse, direction) = match amounts_and_direction(player, amount, &second, &third)
    {
        Ok(parsed) => parsed,
        Err(err) => {
            player.send_error_message(&err);
            return;
        }
    };
    let (mut p1, mut p2) = adjusted_corners(
        first_pos,
        second_pos,
        vector_of(direction, amount as i32),
        true,
    );
    if reverse != 0 {
        (p1, p2) = adjusted_corners(p1, p2, vector_of(direction, -(reverse as i32)), true);
    }
    set_corners(player, p1, p2);
    let new_volume = volume(p1.min(p2), p1.max(p2));
    player.send_worldedit_message(&format!(
        "Region contracted {} blocks.",
        old_volume - new_volume
    ));
}

pub(super) fn execute_shift(ctx: CommandExecuteContext<'_>) {
    let amount = ctx.arguments[0].unwrap_uint();
    let direction = ctx.arguments[1].unwrap_direction();
    let player = ctx.player;
    let first_pos = player.first_position.unwrap();
    let second_pos = player.second_position.unwrap();
    let offset = vector_of(direction, amount as i32);

    player.worldedit_set_first_position(first_pos + offset);
    player.worldedit_set_second_position(second_pos + offset);
    player.send_worldedit_message("Region shifted.");
}

/// The offsets `//outset` and `//inset` apply: both directions of every axis the flags allow
fn all_directions(ctx: &CommandExecuteContext<'_>, amount: i32) -> [BlockPos; 2] {
    let horizontal = !ctx.has_flag('v') || ctx.has_flag('h');
    let vertical = !ctx.has_flag('h') || ctx.has_flag('v');
    let (x, z) = if horizontal { (amount, amount) } else { (0, 0) };
    let y = if vertical { amount } else { 0 };
    [BlockPos::new(x, y, z), BlockPos::new(-x, -y, -z)]
}

pub(super) fn execute_outset(ctx: CommandExecuteContext<'_>) {
    let amount = ctx.arguments[0].unwrap_uint() as i32;
    let [up, down] = all_directions(&ctx, amount);
    let player = ctx.player;
    let (p1, p2) = adjusted_corners(
        player.first_position.unwrap(),
        player.second_position.unwrap(),
        up,
        false,
    );
    let (p1, p2) = adjusted_corners(p1, p2, down, false);
    set_corners(player, clamp_height(p1), clamp_height(p2));
    player.send_worldedit_message("Region outset.");
}

pub(super) fn execute_inset(ctx: CommandExecuteContext<'_>) {
    let amount = ctx.arguments[0].unwrap_uint() as i32;
    let [up, down] = all_directions(&ctx, amount);
    let player = ctx.player;
    let (p1, p2) = adjusted_corners(
        player.first_position.unwrap(),
        player.second_position.unwrap(),
        up,
        true,
    );
    let (p1, p2) = adjusted_corners(p1, p2, down, true);
    set_corners(player, p1, p2);
    player.send_worldedit_message("Region inset.");
}

pub(super) fn execute_selection_size(ctx: CommandExecuteContext<'_>) {
    let player = ctx.player;
    if ctx.flags.contains(&'c') {
        let Some(clipboard) = &player.worldedit_clipboard else {
            player.send_error_message("Your clipboard is empty. Use //copy first.");
            return;
        };
        let size = (clipboard.size_x, clipboard.size_y, clipboard.size_z);
        let offset = (clipboard.offset_x, clipboard.offset_y, clipboard.offset_z);
        let volume = clipboard.volume();
        player.send_worldedit_message(&format!(
            "Offset: ({}, {}, {})",
            offset.0, offset.1, offset.2
        ));
        player.send_worldedit_message(&format!("Size: ({}, {}, {})", size.0, size.1, size.2));
        player.send_worldedit_message(&format!(
            "Cuboid distance: ({}, {}, {})",
            size.0 - 1,
            size.1 - 1,
            size.2 - 1
        ));
        player.send_worldedit_message(&format!("# of blocks: {}", volume));
        return;
    }
    let (Some(first), Some(second)) = (player.first_position, player.second_position) else {
        player.send_error_message("Make a region selection first.");
        return;
    };
    let (min, max) = (first.min(second), first.max(second));
    player.send_worldedit_message("Type: CuboidRegionSelector");
    player.send_worldedit_message(&format!(
        "Size: ({}, {}, {})",
        max.x - min.x + 1,
        max.y - min.y + 1,
        max.z - min.z + 1
    ));
    player.send_worldedit_message(&format!(
        "Cuboid distance: ({}, {}, {})",
        max.x - min.x,
        max.y - min.y,
        max.z - min.z
    ));
    player.send_worldedit_message(&format!("# of blocks: {}", volume(min, max)));
}

pub(super) fn execute_distr(ctx: CommandExecuteContext<'_>) {
    const PAGE_SIZE: usize = 8;
    let by_state = ctx.has_flag('d');
    let data: Vec<u16> = if ctx.has_flag('c') {
        match &ctx.player.worldedit_clipboard {
            Some(clipboard) => clipboard.data.clone(),
            None => {
                ctx.player
                    .send_error_message("Your clipboard is empty. Use //copy first.");
                return;
            }
        }
    } else {
        let (Some(first), Some(second)) = (ctx.player.first_position, ctx.player.second_position)
        else {
            ctx.player
                .send_error_message("Make a region selection first.");
            return;
        };
        read_box(ctx.plot, first.min(second), first.max(second))
    };

    let mut counts: HashMap<u32, usize> = HashMap::new();
    for id in &data {
        let key = if by_state {
            *id as u32
        } else {
            // Without the states every block of a type is counted as its default state
            cinnabar_blocks::generic::default_state(cinnabar_blocks::generic::state_name(
                *id as u32,
            ))
            .unwrap_or(*id as u32)
        };
        *counts.entry(key).or_default() += 1;
    }
    let total = data.len();
    if total == 0 {
        ctx.player.send_worldedit_message("No blocks counted.");
        return;
    }
    let mut sorted: Vec<(u32, usize)> = counts.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

    let pages = sorted.len().div_ceil(PAGE_SIZE);
    let page = ctx
        .flag_arg('p')
        .and_then(|page| page.parse::<usize>().ok())
        .unwrap_or(1)
        .clamp(1, pages.max(1));
    ctx.player
        .send_worldedit_message(&format!("Total Block Count: {}", total));
    for (id, count) in sorted.iter().skip((page - 1) * PAGE_SIZE).take(PAGE_SIZE) {
        let mut name = cinnabar_blocks::generic::state_name(*id).to_string();
        if by_state {
            let props = cinnabar_blocks::generic::properties(*id);
            if !props.is_empty() {
                let props: Vec<String> =
                    props.iter().map(|(k, v)| format!("{}={}", k, v)).collect();
                name.push_str(&format!("[{}]", props.join(",")));
            }
        }
        ctx.player.send_worldedit_message(&format!(
            "{:<7} ({:.3}%) {}",
            count,
            *count as f64 * 100.0 / total as f64,
            name
        ));
    }
    if pages > 1 {
        ctx.player
            .send_worldedit_message(&format!("Page {} of {} (use -p <page>)", page, pages));
    }
}

pub(super) fn execute_help(mut ctx: CommandExecuteContext<'_>) {
    let mut command_name = ctx.arguments[0].unwrap_string().clone();
    let mut page = ctx.arguments[1].unwrap_uint();
    if let Ok(number) = command_name.parse::<u32>() {
        page = number;
        command_name.clear();
    }
    if command_name.is_empty() {
        const PER_PAGE: usize = 10;
        let mut names: Vec<_> = COMMANDS
            .iter()
            .filter(|(_, c)| {
                c.permission_node.is_empty() || ctx.player.has_permission(c.permission_node)
            })
            .map(|(name, c)| (name.trim_start_matches('/'), c.description))
            .collect();
        names.sort();
        let pages = names.len().div_ceil(PER_PAGE).max(1);
        let page = page.max(1) as usize;
        if page > pages {
            ctx.player
                .send_error_message(&format!("Page {page} does not exist (1-{pages})."));
            return;
        }
        ctx.player
            .send_worldedit_message(&format!("Help: page {page}/{pages}"));
        for (name, desc) in names.iter().skip((page - 1) * PER_PAGE).take(PER_PAGE) {
            ctx.player
                .send_worldedit_message(&format!("//{name}: {desc}"));
        }
        ctx.player
            .send_worldedit_message("Use //help <command> for details about a command.");
        return;
    }
    let command_name = command_name.trim_start_matches('/').to_owned();
    let command_name = if COMMANDS.contains_key(command_name.as_str()) {
        command_name
    } else {
        ALIASES
            .get(command_name.as_str())
            .or_else(|| ALIASES.get(format!("/{command_name}").as_str()))
            .map(|t| t.split(' ').next().unwrap().to_owned())
            .unwrap_or(command_name)
    };
    let slash_command_name = "/".to_owned() + &command_name;
    let player = &mut ctx.player;

    let maybe_command = COMMANDS
        .get(command_name.as_str())
        .map(|c| (command_name.as_str(), c))
        .or_else(|| {
            COMMANDS
                .get(slash_command_name.as_str())
                .map(|c| (slash_command_name.as_str(), c))
        });
    let (command_name, command) = match maybe_command {
        Some(command) => command,
        None => {
            player.send_error_message(&format!("Unknown command: {}", command_name));
            return;
        }
    };

    let mut message = vec![
        TextComponentBuilder::new("--------------".to_owned())
            .color_code(ColorCode::Yellow)
            .strikethrough()
            .finish(),
        TextComponentBuilder::new(format!(" Help for /{} ", command_name)).finish(),
        TextComponentBuilder::new("--------------\n".to_owned())
            .color_code(ColorCode::Yellow)
            .strikethrough()
            .finish(),
        TextComponentBuilder::new(command.description.to_owned())
            .color_code(ColorCode::Gray)
            .finish(),
        TextComponentBuilder::new("\nUsage: ".to_owned())
            .color_code(ColorCode::Gray)
            .finish(),
        TextComponentBuilder::new(format!("/{}", command_name))
            .color_code(ColorCode::Gold)
            .finish(),
    ];

    for arg in command.arguments {
        message.append(&mut vec![
            TextComponentBuilder::new(" [".to_owned())
                .color_code(ColorCode::Yellow)
                .finish(),
            TextComponentBuilder::new(arg.name.to_owned())
                .color_code(ColorCode::Gold)
                .finish(),
            TextComponentBuilder::new("]".to_owned())
                .color_code(ColorCode::Yellow)
                .finish(),
        ]);
    }

    if !command.arguments.is_empty() {
        message.push(
            TextComponentBuilder::new("\nArguments:".to_owned())
                .color_code(ColorCode::Gray)
                .finish(),
        );
    }

    for arg in command.arguments {
        message.append(&mut vec![
            TextComponentBuilder::new("\n  [".to_owned())
                .color_code(ColorCode::Yellow)
                .finish(),
            TextComponentBuilder::new(arg.name.to_owned())
                .color_code(ColorCode::Gold)
                .finish(),
            TextComponentBuilder::new("]".to_owned())
                .color_code(ColorCode::Yellow)
                .finish(),
        ]);

        let default = if let Some(arg) = &arg.default {
            match arg {
                Argument::UnsignedInteger(int) => Some(int.to_string()),
                _ => None,
            }
        } else {
            match arg.argument_type {
                ArgumentType::Direction | ArgumentType::DirectionVector => Some("me".to_string()),
                ArgumentType::UnsignedInteger => Some("1".to_string()),
                _ => None,
            }
        };
        if let Some(default) = default {
            message.push(
                TextComponentBuilder::new(format!(" (defaults to {})", default))
                    .color_code(ColorCode::Gray)
                    .finish(),
            );
        }

        message.push(
            TextComponentBuilder::new(format!(": {}", arg.description))
                .color_code(ColorCode::Gray)
                .finish(),
        );
    }

    if !command.flags.is_empty() {
        message.push(
            TextComponentBuilder::new("\nFlags:".to_owned())
                .color_code(ColorCode::Gray)
                .finish(),
        );
    }

    for flag in command.flags {
        message.append(&mut vec![
            TextComponentBuilder::new(format!("\n  -{}", flag.letter))
                .color_code(ColorCode::Gold)
                .finish(),
            TextComponentBuilder::new(format!(": {}", flag.description))
                .color_code(ColorCode::Gray)
                .finish(),
        ]);
    }

    player.send_chat_message(&message);
}

fn is_free(world: &PlotWorld, pos: BlockPos) -> bool {
    pos.y < 0 || pos.y >= PLOT_BLOCK_HEIGHT || matches!(world.get_block(pos), Block::Air)
}

/// Puts a glass block below the feet at `pos` if there is air (or when forced)
fn place_platform(world: &mut PlotWorld, pos: BlockPos, force: bool) {
    let below = BlockPos::new(pos.x, pos.y - 1, pos.z);
    if below.y >= 0 && (force || matches!(world.get_block(below), Block::Air)) {
        world.set_block(below, Block::Glass {});
    }
}

fn teleport_to_feet(player: &mut Player, pos: BlockPos) {
    let mut target = player.pos;
    target.y = pos.y as f64;
    player.teleport(target);
}

pub(super) fn execute_up(ctx: CommandExecuteContext<'_>) {
    let distance = ctx.arguments[0].unwrap_uint() as i32;
    let force_glass = ctx.has_flag('g');
    let player = ctx.player;
    let feet = player.pos.block_pos();
    let target = feet.y + distance;
    if target >= PLOT_BLOCK_HEIGHT {
        player.send_error_message("You would hit something above you.");
        return;
    }
    // Everything the player passes (head height up to the head at the target) has to be free
    for y in (feet.y + 2)..=(target + 1) {
        if !is_free(ctx.plot, BlockPos::new(feet.x, y, feet.z)) {
            player.send_error_message("You would hit something above you.");
            return;
        }
    }
    let destination = BlockPos::new(feet.x, target, feet.z);
    place_platform(ctx.plot, destination, force_glass);
    teleport_to_feet(player, destination);
    player.send_worldedit_message("Whoosh!");
}

pub(super) fn execute_ceil(ctx: CommandExecuteContext<'_>) {
    let clearance = ctx.arguments[0].unwrap_uint() as i32;
    let force_glass = ctx.has_flag('g');
    let player = ctx.player;
    let feet = player.pos.block_pos();
    // The first solid block above the head
    let ceiling = ((feet.y + 2)..PLOT_BLOCK_HEIGHT)
        .find(|y| !is_free(ctx.plot, BlockPos::new(feet.x, *y, feet.z)));
    let Some(ceiling) = ceiling else {
        player.send_error_message("No free spot above you found.");
        return;
    };
    let target = ceiling - 2 - clearance;
    if target <= feet.y {
        player.send_error_message("No free spot above you found.");
        return;
    }
    let destination = BlockPos::new(feet.x, target, feet.z);
    place_platform(ctx.plot, destination, force_glass);
    teleport_to_feet(player, destination);
    player.send_worldedit_message("Whoosh!");
}

/// The blocks along the line of sight, starting at the player's body, each only once
fn sight_cells(player: &Player, max_distance: f64) -> Vec<BlockPos> {
    let yaw = (player.yaw as f64 + 90.0).to_radians();
    let pitch = (-(player.pitch as f64)).to_radians();
    let step = 0.2;
    let (dx, dy, dz) = (
        step * pitch.cos() * yaw.cos(),
        step * pitch.sin(),
        step * pitch.cos() * yaw.sin(),
    );
    let mut pos = player.pos;
    pos.y += 1.0;
    let mut cells: Vec<BlockPos> = Vec::new();
    let mut travelled = 0.0;
    while travelled < max_distance {
        let cell = pos.block_pos();
        if cells.last() != Some(&cell) {
            cells.push(cell);
        }
        pos.x += dx;
        pos.y += dy;
        pos.z += dz;
        travelled += step;
    }
    cells
}

pub(super) fn execute_thru(ctx: CommandExecuteContext<'_>) {
    let player = ctx.player;
    let cells = sight_cells(player, 20.0);
    // Through the wall first, then the first spot where the player fits
    let mut inside_wall = false;
    let mut wall_start = 0usize;
    for (i, cell) in cells.iter().enumerate() {
        let free = is_free(ctx.plot, *cell);
        if !inside_wall {
            if !free {
                inside_wall = true;
                wall_start = i;
            }
            continue;
        }
        let head = BlockPos::new(cell.x, cell.y + 1, cell.z);
        let feet = BlockPos::new(cell.x, cell.y - 1, cell.z);
        if free && is_free(ctx.plot, head) && is_free(ctx.plot, feet) && i - wall_start <= 30 {
            let mut target = player.pos;
            target.x = cell.x as f64 + 0.5;
            target.y = feet.y as f64;
            target.z = cell.z as f64 + 0.5;
            if Plot::in_plot_bounds(ctx.plot.x, ctx.plot.z, cell.x, cell.z) {
                player.teleport(target);
                player.send_worldedit_message("Whoosh!");
                return;
            }
            break;
        }
    }
    player.send_error_message("No free spot ahead of you found.");
}

pub(super) fn execute_jumpto(ctx: CommandExecuteContext<'_>) {
    let player = ctx.player;
    let Some(hit) = ray_trace_block(
        ctx.plot,
        player.pos,
        player.pitch as f64,
        player.yaw as f64,
        300.0,
    ) else {
        player.send_error_message("No block in sight (or too far)!");
        return;
    };
    // The first spot above the block where the player fits
    let spot = (hit.y + 1..PLOT_BLOCK_HEIGHT).find(|y| {
        is_free(ctx.plot, BlockPos::new(hit.x, *y, hit.z))
            && is_free(ctx.plot, BlockPos::new(hit.x, *y + 1, hit.z))
    });
    let Some(y) = spot else {
        player.send_error_message("No block in sight (or too far)!");
        return;
    };
    let mut target = player.pos;
    target.x = hit.x as f64 + 0.5;
    target.y = y as f64;
    target.z = hit.z as f64 + 0.5;
    player.teleport(target);
    player.send_worldedit_message("Poof!");
}

pub(super) fn execute_unstuck(ctx: CommandExecuteContext<'_>) {
    let player = ctx.player;
    let feet = player.pos.block_pos();
    let fits = |y: i32| {
        is_free(ctx.plot, BlockPos::new(feet.x, y, feet.z))
            && is_free(ctx.plot, BlockPos::new(feet.x, y + 1, feet.z))
    };
    let spot = (feet.y..PLOT_BLOCK_HEIGHT)
        .find(|y| fits(*y))
        .or_else(|| (0..feet.y).rev().find(|y| fits(*y)));
    match spot {
        Some(y) => {
            if y != feet.y {
                teleport_to_feet(player, BlockPos::new(feet.x, y, feet.z));
            }
            player.send_worldedit_message("There you go!");
        }
        None => player.send_error_message("No free spot found."),
    }
}

pub(super) fn execute_ascend(ctx: CommandExecuteContext<'_>) {
    let initial_levels = ctx.arguments[0].unwrap_uint();
    let mut levels = initial_levels;

    let player = ctx.player;
    let player_pos = player.pos.block_pos();
    let mut player_y = player_pos.y;

    for (y, _) in (player_y..=PLOT_BLOCK_HEIGHT).enumerate() {
        if levels == 0 {
            break;
        }
        let y = y as i32 + 1;

        let floor_pos = player_pos + BlockPos::new(0, y - 1, 0);
        let pos = player_pos + BlockPos::new(0, y, 0);
        let high_pos = player_pos + BlockPos::new(0, y + 1, 0);
        if ctx.plot.get_block(floor_pos) != Block::Air
            && ctx.plot.get_block(pos) == Block::Air
            && ctx.plot.get_block(high_pos) == Block::Air
        {
            player_y = pos.y;
            levels -= 1;
        }
    }

    if player_y == player_pos.y {
        player.send_error_message("No free spot above you found.");
    } else {
        let mut pos = player.pos;
        pos.y = player_y as f64;
        player.teleport(pos);
        player.send_worldedit_message(&format!("Ascended {} level(s).", initial_levels - levels));
    }
}

pub(super) fn execute_descend(ctx: CommandExecuteContext<'_>) {
    let initial_levels = ctx.arguments[0].unwrap_uint();
    let mut levels = initial_levels;

    let player = ctx.player;
    let player_pos = player.pos.block_pos();
    let mut player_y = player_pos.y;

    for (y, _) in (1..player_y).enumerate() {
        if levels == 0 {
            break;
        }
        let y = -(y as i32 + 1);

        let floor_pos = player_pos + BlockPos::new(0, y - 1, 0);
        let pos = player_pos + BlockPos::new(0, y, 0);
        let high_pos = player_pos + BlockPos::new(0, y + 1, 0);
        if ctx.plot.get_block(floor_pos) != Block::Air
            && ctx.plot.get_block(pos) == Block::Air
            && ctx.plot.get_block(high_pos) == Block::Air
        {
            player_y = pos.y;
            levels -= 1;
        }
    }

    if player_y == player_pos.y {
        player.send_error_message("No free spot below you found.");
    } else {
        let mut pos = player.pos;
        pos.y = player_y as f64;
        player.teleport(pos);
        player.send_worldedit_message(&format!("Descended {} level(s).", initial_levels - levels));
    }
}

pub(super) fn execute_update(ctx: CommandExecuteContext<'_>) {
    let start_time = Instant::now();

    let (first_pos, second_pos) = if ctx.has_flag('p') {
        ctx.plot.get_corners()
    } else if let (Some(first_pos), Some(second_pos)) =
        (ctx.player.first_position, ctx.player.second_position)
    {
        (first_pos, second_pos)
    } else {
        ctx.player
            .send_error_message("Your selection is incomplete.");
        return;
    };

    update(ctx.plot, first_pos, second_pos);

    ctx.player.send_worldedit_message(&format!(
        "Your selection was updated successfully. ({:?})",
        start_time.elapsed()
    ));
}

pub(super) fn execute_replace_container(ctx: CommandExecuteContext<'_>) {
    let start_time = Instant::now();

    let from = ctx.arguments[0].unwrap_container_type();
    let to = ctx.arguments[1].unwrap_container_type();

    let new_block = match to {
        ContainerType::Furnace => Block::Furnace {
            facing: BlockDirection::North,
            lit: false,
        },
        ContainerType::Barrel => Block::Barrel {
            open: false,
            facing: BlockFacing::Up,
        },
        ContainerType::Hopper => Block::Hopper {
            enabled: true,
            facing: HopperFacing::Down,
        },
        other => {
            Block::Generic(cinnabar_blocks::generic::default_state(&other.to_string()).unwrap())
        }
    };
    let slots = to.num_slots() as u32;

    let (min, max) = selection(&ctx);
    for x in min.x..=max.x {
        for y in min.y..=max.y {
            for z in min.z..=max.z {
                let pos = BlockPos::new(x, y, z);
                let block = ctx.plot.get_block(pos);

                if ContainerType::for_block(block).is_none() {
                    continue;
                }
                let block_entity = ctx.plot.get_block_entity(pos);
                if let Some(BlockEntity::Container {
                    comparator_override,
                    ty,
                    ..
                }) = block_entity
                {
                    if *ty != from {
                        continue;
                    }
                    let ss = *comparator_override;

                    let items_needed = match ss {
                        0 => 0,
                        15 => slots * 64,
                        _ => ((32 * slots * ss as u32) as f32 / 7.0 - 1.0).ceil() as u32,
                    } as usize;
                    let mut inventory = Vec::new();
                    for (slot, items_added) in (0..items_needed).step_by(64).enumerate() {
                        let count = (items_needed - items_added).min(64);
                        inventory.push(InventoryEntry {
                            id: Item::Redstone {}.get_id(),
                            slot: slot as i8,
                            count: count as i8,
                            nbt: None,
                        });
                    }

                    let new_entity = BlockEntity::Container {
                        comparator_override: ss,
                        inventory,
                        ty: to,
                    };
                    ctx.plot.set_block_entity(pos, new_entity);
                    ctx.plot.set_block(pos, new_block);
                }
            }
        }
    }

    ctx.player.send_worldedit_message(&format!(
        "Your selection was replaced successfully. ({:?})",
        start_time.elapsed()
    ));
}

pub(super) fn execute_unimplemented(_ctx: CommandExecuteContext<'_>) {
    unimplemented!("Unimplimented worldedit command");
}
