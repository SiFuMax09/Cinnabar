use super::brush::{Brush, BrushKind, MAX_BRUSH_SIZE};
use super::job::*;
use super::ops;
use super::pattern::FastRng;
use super::session::{create_clipboard, read_box};
use super::shapes;
use super::transform::{flip_clipboard, rotate_clipboard, Axis};
use super::*;
use crate::config::CONFIG;
use crate::player::PacketSender;
use crate::plot::PLOT_BLOCK_HEIGHT;
use crate::utils::{self, HyphenatedUUID};
use mchprs_blocks::block_entities::{BlockEntity, InventoryEntry};
use mchprs_blocks::blocks::{Block, HopperFacing};
use mchprs_blocks::items::{Item, ItemStack};
use mchprs_blocks::{BlockDirection, BlockFace, BlockFacing, BlockPos};
use mchprs_network::packets::clientbound::*;
use mchprs_schematic::{load_schematic, save_schematic, WorldEditClipboard};
use mchprs_text::{ColorCode, TextComponentBuilder};
use regex::Regex;
use std::path::PathBuf;
use std::sync::LazyLock;
use std::time::Instant;
use tracing::error;

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
}

static SCHEMATI_VALIDATE_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[a-zA-Z0-9_.]+\.schem(atic)?").unwrap());

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
    );
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
    );
    ctx.jobs.push_back(job);
}

pub(super) fn execute_replace_near(ctx: CommandExecuteContext<'_>) {
    let size = ctx.arguments[0].unwrap_uint().min(256) as i32;
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
    );
    ctx.jobs.push_back(job);
}

pub(super) fn execute_count(ctx: CommandExecuteContext<'_>) {
    let start_time = Instant::now();

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

    ctx.player.send_worldedit_message(&format!(
        "Counted {} block(s) ({:?})",
        counted,
        start_time.elapsed()
    ));
}

pub(super) fn execute_copy(ctx: CommandExecuteContext<'_>) {
    let start_time = Instant::now();

    let origin = ctx.player.pos.block_pos();
    let clipboard = create_clipboard(
        ctx.plot,
        origin,
        ctx.player.first_position.unwrap(),
        ctx.player.second_position.unwrap(),
    );
    ctx.player.worldedit_clipboard = Some(clipboard);

    ctx.player.send_worldedit_message(&format!(
        "Your selection was copied. ({:?})",
        start_time.elapsed()
    ));
}

pub(super) fn execute_cut(ctx: CommandExecuteContext<'_>) {
    let (min, max) = selection(&ctx);

    let origin = ctx.player.pos.block_pos();
    ctx.player.worldedit_clipboard = Some(create_clipboard(ctx.plot, origin, min, max));
    let job = ops::region_job(
        ctx.plot,
        ctx.player.uuid,
        ctx.player.worldedit_mask.clone(),
        None,
        min,
        max,
        Pattern::Single(0),
        None,
        None,
    )
    .on_finish(|player, stats| {
        player.send_worldedit_message(&format!("Your selection was cut. ({:?})", stats.elapsed));
    });
    ctx.jobs.push_back(job);
}

pub(super) fn execute_move(mut ctx: CommandExecuteContext<'_>) {
    let move_amt = ctx.arguments[0].unwrap_uint();
    let direction = ctx.arguments[1].unwrap_direction();
    let replace = ctx.arguments[2].unwrap_pattern().clone();

    let (min, max) = selection(&ctx);
    let offset = direction.offset_pos(BlockPos::zero(), move_amt as i32);
    let clipboard = create_clipboard(ctx.plot, min, min, max);

    let job = EditJob::new(ctx.player.uuid, ctx.player.worldedit_mask.clone())
        .stage(ops::clear_stage(ctx.plot, min, max, replace))
        .stage(
            ops::paste_stage(ctx.plot, clipboard, vec![min + offset], ctx.has_flag('a'))
                .commit_to(HistoryDest::Undo),
        )
        .on_finish(|player, stats| {
            player.send_worldedit_message(&format!(
                "Your selection was moved. ({:?})",
                stats.elapsed
            ));
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

/// Lets the blocks of the area update their neighbours, as a stage that comes after the edit
fn update_stage(world: &PlotWorld, min: BlockPos, max: BlockPos) -> Stage {
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
                            mchprs_redstone::update(block, session.world, pos);
                        }
                    }
                }
            }
            0
        }),
    )
}

pub(super) fn execute_paste(ctx: CommandExecuteContext<'_>) {
    let clipboard = ctx.player.worldedit_clipboard.clone().unwrap();
    let pos = ctx.player.pos.block_pos();
    let first_pos = BlockPos::new(
        pos.x - clipboard.offset_x,
        pos.y - clipboard.offset_y,
        pos.z - clipboard.offset_z,
    );
    let second_pos = BlockPos::new(
        first_pos.x + clipboard.size_x as i32 - 1,
        first_pos.y + clipboard.size_y as i32 - 1,
        first_pos.z + clipboard.size_z as i32 - 1,
    );
    let mut job = EditJob::new(ctx.player.uuid, ctx.player.worldedit_mask.clone()).stage(
        ops::paste_stage(ctx.plot, clipboard, vec![first_pos], ctx.has_flag('a'))
            .commit_to(HistoryDest::Undo),
    );
    if ctx.has_flag('u') {
        job = job.stage(update_stage(ctx.plot, first_pos, second_pos));
    }
    ctx.jobs.push_back(job.on_finish(|player, stats| {
        player.send_worldedit_message(&format!("Your clipboard was pasted. ({:?})", stats.elapsed));
    }));
    if ctx.has_flag('s') {
        ctx.player.worldedit_set_first_position(first_pos);
        ctx.player.worldedit_set_second_position(second_pos);
    }
}

pub(super) fn execute_stack(ctx: CommandExecuteContext<'_>) {
    // More copies than fit in the plot would be placed outside of it anyway
    let stack_amt = ctx.arguments[0].unwrap_uint().min(1024);
    let direction = ctx.arguments[1].unwrap_direction();
    let (min, max) = selection(&ctx);
    let clipboard = create_clipboard(ctx.plot, min, min, max);
    let stack_offset = match direction {
        BlockFacing::North | BlockFacing::South => clipboard.size_z,
        BlockFacing::East | BlockFacing::West => clipboard.size_x,
        BlockFacing::Up | BlockFacing::Down => clipboard.size_y,
    };
    let bases = (1..=stack_amt)
        .map(|i| direction.offset_pos(min, (i * stack_offset) as i32))
        .collect();
    let job = ops::paste_job(
        ctx.plot,
        ctx.player.uuid,
        ctx.player.worldedit_mask.clone(),
        clipboard,
        bases,
        ctx.has_flag('a'),
    )
    .on_finish(|player, stats| {
        player.send_worldedit_message(&format!(
            "Your selection was stacked. ({:?})",
            stats.elapsed
        ));
    });
    ctx.jobs.push_back(job);
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

pub(super) fn execute_undo(ctx: CommandExecuteContext<'_>) {
    let times = ctx.arguments[0].unwrap_uint().clamp(1, 1000);
    ctx.jobs
        .push_back(ops::history_job(ctx.player.uuid, times, true));
}

pub(super) fn execute_redo(ctx: CommandExecuteContext<'_>) {
    let times = ctx.arguments[0].unwrap_uint().clamp(1, 1000);
    ctx.jobs
        .push_back(ops::history_job(ctx.player.uuid, times, false));
}

pub(super) fn execute_flip(ctx: CommandExecuteContext<'_>) {
    let start_time = Instant::now();

    let direction = ctx.arguments[0].unwrap_direction();
    let axis = match direction {
        BlockFacing::East | BlockFacing::West => Axis::X,
        BlockFacing::North | BlockFacing::South => Axis::Z,
        BlockFacing::Up | BlockFacing::Down => Axis::Y,
    };
    let clipboard = ctx.player.worldedit_clipboard.as_ref().unwrap();
    let flipped = flip_clipboard(clipboard, axis);

    ctx.player.worldedit_clipboard = Some(flipped);
    ctx.player.send_worldedit_message(&format!(
        "The clipboard copy has been flipped. ({:?})",
        start_time.elapsed()
    ));
}

pub(super) fn execute_rotate(ctx: CommandExecuteContext<'_>) {
    let start_time = Instant::now();

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
    ctx.player.send_worldedit_message(&format!(
        "The clipboard copy has been rotated. ({:?})",
        start_time.elapsed()
    ));
}

fn sphere(ctx: CommandExecuteContext<'_>, hollow: bool) {
    let pattern = ctx.arguments[0].unwrap_pattern().clone();
    let radius = ctx.arguments[1].unwrap_uint().min(512);
    let Ok(clipboard) = clipboard_for(&ctx, &pattern) else {
        return;
    };
    let mut center = ctx.player.pos.block_pos();
    if ctx.has_flag('r') {
        center.y += radius as i32;
    }
    let job = ops::sphere_job(
        ctx.plot,
        ctx.player.uuid,
        ctx.player.worldedit_mask.clone(),
        None,
        center,
        radius,
        hollow,
        pattern,
        clipboard,
    );
    ctx.jobs.push_back(job);
}

pub(super) fn execute_sphere(ctx: CommandExecuteContext<'_>) {
    sphere(ctx, false);
}

pub(super) fn execute_hsphere(ctx: CommandExecuteContext<'_>) {
    sphere(ctx, true);
}

fn cylinder(ctx: CommandExecuteContext<'_>, hollow: bool) {
    let pattern = ctx.arguments[0].unwrap_pattern().clone();
    let radius = ctx.arguments[1].unwrap_uint().min(512);
    let height = ctx.arguments[2].unwrap_uint().min(PLOT_BLOCK_HEIGHT as u32);
    let Ok(clipboard) = clipboard_for(&ctx, &pattern) else {
        return;
    };
    let job = ops::cylinder_job(
        ctx.plot,
        ctx.player.uuid,
        ctx.player.worldedit_mask.clone(),
        None,
        ctx.player.pos.block_pos(),
        radius,
        height as i32,
        hollow,
        pattern,
        clipboard,
    );
    ctx.jobs.push_back(job);
}

pub(super) fn execute_cyl(ctx: CommandExecuteContext<'_>) {
    cylinder(ctx, false);
}

pub(super) fn execute_hcyl(ctx: CommandExecuteContext<'_>) {
    cylinder(ctx, true);
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
    ctx.jobs.push_back(job);
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
    ctx.jobs.push_back(job);
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
    ctx.jobs.push_back(job);
}

pub(super) fn execute_smooth(ctx: CommandExecuteContext<'_>) {
    let iterations = ctx.arguments[0].unwrap_uint().min(100);
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
        None,
    );
    ctx.jobs.push_back(job);
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
        pattern,
        clipboard,
    );
    ctx.jobs.push_back(job);
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
    );
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
    let pos = ctx.player.pos.block_pos();
    let (x, z) = (pos.x & !0xF, pos.z & !0xF);
    ctx.player
        .worldedit_set_first_position(BlockPos::new(x, 0, z));
    ctx.player
        .worldedit_set_second_position(BlockPos::new(x + 15, PLOT_BLOCK_HEIGHT - 1, z + 15));
}

fn parse_size(text: &str, default: u32) -> Result<u32, String> {
    if text.is_empty() {
        return Ok(default);
    }
    match text.parse::<u32>() {
        Ok(size) if (1..=MAX_BRUSH_SIZE).contains(&size) => Ok(size),
        _ => Err(format!(
            "The size has to be a number from 1 to {MAX_BRUSH_SIZE}."
        )),
    }
}

fn create_brush(ctx: &CommandExecuteContext<'_>, kind: &str) -> Result<Brush, String> {
    let arg = |i: usize| ctx.arguments[i].unwrap_string().as_str();
    let (a1, a2, a3) = (arg(1), arg(2), arg(3));
    let hollow = ctx.has_flag('h');
    let pattern = |text: &str| {
        if text.is_empty() {
            return Err("A pattern is missing, use for example //brush sphere stone 5".to_owned());
        }
        Pattern::parse(text).map_err(|err| err.to_string())
    };
    let air = Pattern::Single(0);
    let (kind, pattern, size) = match kind {
        "sphere" | "s" => (
            BrushKind::Sphere { hollow },
            pattern(a1)?,
            parse_size(a2, 3)?,
        ),
        "cylinder" | "cyl" | "c" => {
            let height = parse_size(a3, 1)?;
            (
                BrushKind::Cylinder { height, hollow },
                pattern(a1)?,
                parse_size(a2, 3)?,
            )
        }
        "smooth" => {
            let iterations = match a2.is_empty() {
                true => 4,
                false => a2
                    .parse::<u32>()
                    .ok()
                    .filter(|n| (1..=100).contains(n))
                    .ok_or("The iterations have to be a number from 1 to 100.")?,
            };
            (BrushKind::Smooth { iterations }, air, parse_size(a1, 3)?)
        }
        "clipboard" | "clip" => (
            BrushKind::Clipboard {
                ignore_air: ctx.has_flag('a'),
            },
            air,
            1,
        ),
        _ => {
            return Err(
                "Unknown brush, use sphere, cylinder, smooth, clipboard or none.".to_owned(),
            )
        }
    };
    Ok(Brush {
        kind,
        pattern,
        size,
        mask: None,
        mask_text: None,
    })
}

pub(super) fn execute_brush(ctx: CommandExecuteContext<'_>) {
    let kind = ctx.arguments[0].unwrap_string().to_lowercase();
    let Some(item_id) = held_item_id(ctx.player) else {
        ctx.player
            .send_error_message("Hold the item you want to bind the brush to.");
        return;
    };
    if ctx.player.inventory[ctx.player.selected_slot as usize + 36]
        .as_ref()
        .is_some_and(|item| item.item_type == Item::WoodenAxe)
    {
        ctx.player
            .send_error_message("The wooden axe is the selection wand, use another item.");
        return;
    }
    if matches!(kind.as_str(), "none" | "off") {
        ctx.player.worldedit_brushes.remove(&item_id);
        ctx.player
            .send_worldedit_message("The brush of this item was removed.");
        return;
    }
    match create_brush(&ctx, &kind) {
        Ok(brush) => {
            ctx.player.send_worldedit_message(&format!(
                "Brush bound to the item in your hand: {}",
                brush.describe()
            ));
            ctx.player.worldedit_brushes.insert(item_id, brush);
        }
        Err(message) => ctx.player.send_error_message(&message),
    }
}

/// Changes the brush in the hand of the player
fn with_held_brush(
    ctx: &mut CommandExecuteContext<'_>,
    change: impl FnOnce(&mut Brush) -> Result<(), String>,
) {
    let Some(item_id) = held_item_id(ctx.player) else {
        ctx.player
            .send_error_message("Hold the item with the brush you want to change.");
        return;
    };
    let Some(brush) = ctx.player.worldedit_brushes.get_mut(&item_id) else {
        ctx.player
            .send_error_message("There is no brush bound to the item in your hand.");
        return;
    };
    match change(brush) {
        Ok(()) => {
            let text = brush.describe();
            ctx.player
                .send_worldedit_message(&format!("Brush changed: {text}"));
        }
        Err(message) => ctx.player.send_error_message(&message),
    }
}

pub(super) fn execute_size(mut ctx: CommandExecuteContext<'_>) {
    let size = ctx.arguments[0].unwrap_uint();
    with_held_brush(&mut ctx, |brush| {
        brush.size = parse_size(&size.to_string(), 1)?;
        Ok(())
    });
}

pub(super) fn execute_mat(mut ctx: CommandExecuteContext<'_>) {
    let pattern = ctx.arguments[0].unwrap_pattern().clone();
    with_held_brush(&mut ctx, |brush| {
        brush.pattern = pattern;
        Ok(())
    });
}

fn parse_optional_mask(text: &str) -> Result<Option<Mask>, String> {
    if text.is_empty() || matches!(text, "none" | "off" | "clear") {
        return Ok(None);
    }
    Mask::parse(text).map(Some).map_err(|err| err.to_string())
}

pub(super) fn execute_mask(mut ctx: CommandExecuteContext<'_>) {
    let text = ctx.arguments[0].unwrap_string().clone();
    with_held_brush(&mut ctx, |brush| {
        brush.mask = parse_optional_mask(&text)?;
        brush.mask_text = brush.mask.is_some().then_some(text);
        Ok(())
    });
}

pub(super) fn execute_gmask(ctx: CommandExecuteContext<'_>) {
    match parse_optional_mask(ctx.arguments[0].unwrap_string()) {
        Ok(mask) => {
            let message = match mask {
                Some(_) => "Your global mask was set.",
                None => "Your global mask was removed.",
            };
            ctx.player.worldedit_mask = mask;
            ctx.player.send_worldedit_message(message);
        }
        Err(message) => ctx.player.send_error_message(&message),
    }
}

pub(super) fn execute_none(ctx: CommandExecuteContext<'_>) {
    if let Some(item_id) = held_item_id(ctx.player) {
        ctx.player.worldedit_brushes.remove(&item_id);
    }
    ctx.player
        .send_worldedit_message("The brush of this item was removed.");
}

pub(super) fn execute_load(ctx: CommandExecuteContext<'_>) {
    let start_time = Instant::now();

    let mut file_name = ctx.arguments[0].unwrap_string().clone();
    if !SCHEMATI_VALIDATE_REGEX.is_match(&file_name) {
        ctx.player.send_error_message("Filename is invalid");
        return;
    }

    if CONFIG.schemati {
        let prefix = HyphenatedUUID(ctx.player.uuid).to_string() + "/";
        file_name.insert_str(0, &prefix);
    }

    let path = PathBuf::from("./schems").join(file_name);
    let clipboard = load_schematic(&path);
    match clipboard {
        Ok(cb) => {
            ctx.player.worldedit_clipboard = Some(cb);
            ctx.player.send_worldedit_message(&format!(
                "The schematic was loaded to your clipboard. Do //paste to birth it into the world. ({:?})",
                start_time.elapsed()
            ));
        }
        Err(e) => {
            if let Some(e) = e.downcast_ref::<std::io::Error>()
                && e.kind() == std::io::ErrorKind::NotFound
            {
                let msg = "The specified schematic file could not be found.";
                ctx.player.send_error_message(msg);
                return;
            }
            error!("There was an error loading a schematic:");
            error!("{}", e);
            ctx.player.send_error_message(
                "There was an error loading the schematic. Check console for more details.",
            );
        }
    }
}

pub(super) fn execute_save(ctx: CommandExecuteContext<'_>) {
    let start_time = Instant::now();

    let mut file_name = ctx.arguments[0].unwrap_string().clone();
    if !SCHEMATI_VALIDATE_REGEX.is_match(&file_name) {
        ctx.player.send_error_message("Filename is invalid");
        return;
    }

    if CONFIG.schemati {
        let prefix = HyphenatedUUID(ctx.player.uuid).to_string() + "/";
        file_name.insert_str(0, &prefix);
    }

    let path = PathBuf::from("./schems").join(file_name);
    let clipboard = ctx.player.worldedit_clipboard.as_ref().unwrap();
    match save_schematic(&path, clipboard) {
        Ok(_) => {
            ctx.player.send_worldedit_message(&format!(
                "The schematic was saved successfully. ({:?})",
                start_time.elapsed()
            ));
        }
        Err(err) => {
            error!("There was an error saving a schematic: ");
            error!("{:?}", err);
            ctx.player
                .send_error_message("There was an error saving the schematic.");
        }
    }
}

pub(super) fn execute_sel(ctx: CommandExecuteContext<'_>) {
    let player = ctx.player;
    player.first_position = None;
    player.second_position = None;
    player.send_worldedit_message("Selection cleared.");
    player.worldedit_send_cui("s|cuboid");
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

pub(super) fn execute_pos1(ctx: CommandExecuteContext<'_>) {
    let pos = ctx.player.pos.block_pos();
    ctx.player.worldedit_set_first_position(pos);
}

pub(super) fn execute_pos2(ctx: CommandExecuteContext<'_>) {
    let pos = ctx.player.pos.block_pos();
    ctx.player.worldedit_set_second_position(pos);
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

pub(super) fn execute_expand(ctx: CommandExecuteContext<'_>) {
    let amount = ctx.arguments[0].unwrap_uint();
    let direction = ctx.arguments[1].unwrap_direction();
    let player = ctx.player;

    expand_selection(
        player,
        direction.offset_pos(BlockPos::zero(), amount as i32),
        false,
    );

    player.send_worldedit_message(&format!("Region expanded {} block(s).", amount));
}

pub(super) fn execute_contract(ctx: CommandExecuteContext<'_>) {
    let amount = ctx.arguments[0].unwrap_uint();
    let direction = ctx.arguments[1].unwrap_direction();
    let player = ctx.player;

    expand_selection(
        player,
        direction.offset_pos(BlockPos::zero(), amount as i32),
        true,
    );

    player.send_worldedit_message(&format!("Region contracted {} block(s).", amount));
}

pub(super) fn execute_shift(ctx: CommandExecuteContext<'_>) {
    let amount = ctx.arguments[0].unwrap_uint();
    let direction = ctx.arguments[1].unwrap_direction();
    let player = ctx.player;
    let first_pos = player.first_position.unwrap();
    let second_pos = player.second_position.unwrap();

    let mut move_both_points = |x, y, z| {
        player.worldedit_set_first_position(BlockPos::new(
            first_pos.x + x,
            first_pos.y + y,
            first_pos.z + z,
        ));
        player.worldedit_set_second_position(BlockPos::new(
            second_pos.x + x,
            second_pos.y + y,
            second_pos.z + z,
        ));
    };

    match direction {
        BlockFacing::Up => move_both_points(0, amount as i32, 0),
        BlockFacing::Down => move_both_points(0, -(amount as i32), 0),
        BlockFacing::East => move_both_points(amount as i32, 0, 0),
        BlockFacing::West => move_both_points(-(amount as i32), 0, 0),
        BlockFacing::South => move_both_points(0, 0, amount as i32),
        BlockFacing::North => move_both_points(0, 0, -(amount as i32)),
    }

    player.send_worldedit_message(&format!("Region shifted {} block(s).", amount));
}

pub(super) fn execute_help(mut ctx: CommandExecuteContext<'_>) {
    let command_name = ctx.arguments[0].unwrap_string().clone();
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

pub(super) fn execute_up(ctx: CommandExecuteContext<'_>) {
    let distance = ctx.arguments[0].unwrap_uint();
    let player = ctx.player;

    let mut pos = player.pos;
    pos.y += distance as f64;
    let block_pos = pos.block_pos();

    let platform_pos = block_pos.offset(BlockFace::Bottom);
    if matches!(ctx.plot.get_block(platform_pos), Block::Air) {
        ctx.plot.set_block(platform_pos, Block::Glass {});
    }

    player.teleport(pos);
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
        player.send_worldedit_message(&format!("Ascended {} levels.", initial_levels - levels));
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
        player.send_worldedit_message(&format!("Descended {} levels.", initial_levels - levels));
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
            enabled: false,
            facing: HopperFacing::Down,
        },
    };
    let slots = to.num_slots() as u32;

    let (min, max) = selection(&ctx);
    for x in min.x..=max.x {
        for y in min.y..=max.y {
            for z in min.z..=max.z {
                let pos = BlockPos::new(x, y, z);
                let block = ctx.plot.get_block(pos);

                if !matches!(
                    block,
                    Block::Furnace { .. } | Block::Barrel { .. } | Block::Hopper { .. }
                ) {
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
