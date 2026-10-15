//! `//copy`, `//cut`, `//paste` and the commands around the clipboard of FastAsyncWorldEdit:
//! `//lazycopy`, `//lazycut`, `//place`, `/clearclipboard`, `/asset` and `/download`.

use super::job::HistoryDest;
use super::pattern::FastRng;
use super::session::create_clipboard;
use super::*;
use cinnabar_schematic::WorldEditClipboard;
use std::sync::Mutex;

/// Where each player copied the clipboard from (the smaller corner of the region), for
/// `//paste -o`
static ORIGINS: LazyLock<Mutex<HashMap<u128, BlockPos>>> = LazyLock::new(Default::default);

pub(super) fn set_origin(uuid: u128, origin: Option<BlockPos>) {
    if let Ok(mut origins) = ORIGINS.lock() {
        match origin {
            Some(pos) => origins.insert(uuid, pos),
            None => origins.remove(&uuid),
        };
    }
}

fn origin_of(uuid: u128) -> Option<BlockPos> {
    ORIGINS.lock().ok()?.get(&uuid).copied()
}

const ENTITIES_FLAG: FlagDescription = FlagDescription {
    letter: 'e',
    argument_type: None,
    description: "Also include entities",
};
const BIOMES_FLAG: FlagDescription = FlagDescription {
    letter: 'b',
    argument_type: None,
    description: "Also include biomes",
};
const MASK_FLAG: FlagDescription = FlagDescription {
    letter: 'm',
    argument_type: Some(ArgumentType::Mask),
    description: "Set the include mask, non-matching blocks become air",
};

pub(super) const COPY_FLAGS: &[FlagDescription] = &[ENTITIES_FLAG, BIOMES_FLAG, MASK_FLAG];
pub(super) const PASTE_FLAGS: &[FlagDescription] = &[
    FlagDescription {
        letter: 'a',
        argument_type: None,
        description: "Skip air blocks",
    },
    FlagDescription {
        letter: 'o',
        argument_type: None,
        description: "Paste at the original position",
    },
    FlagDescription {
        letter: 's',
        argument_type: None,
        description: "Select the region after pasting",
    },
    FlagDescription {
        letter: 'n',
        argument_type: None,
        description: "No paste, select only",
    },
    FlagDescription {
        letter: 'u',
        argument_type: None,
        description: "Also update all affected blocks",
    },
    ENTITIES_FLAG,
    BIOMES_FLAG,
    MASK_FLAG,
];

/// The mask of the `-m` flag. The error is the message for the player.
fn flag_mask(ctx: &CommandExecuteContext<'_>) -> Result<Option<Mask>, String> {
    match ctx.flag_arg('m') {
        Some(text) => Mask::parse(text)
            .map(Some)
            .map_err(|err| format!("Error parsing argument of type Mask: {err}")),
        None => Ok(None),
    }
}

/// Turns the blocks of the clipboard that do not match the mask into air. `base` is the position
/// of the first block of the clipboard in the world the mask looks at.
fn apply_include_mask(
    world: &PlotWorld,
    clipboard: &mut WorldEditClipboard,
    mask: &Mask,
    base: BlockPos,
) {
    let mut rng = FastRng::new();
    for y in 0..clipboard.size_y {
        for z in 0..clipboard.size_z {
            for x in 0..clipboard.size_x {
                let index = clipboard.index(x, y, z);
                let id = clipboard.data[index] as u32;
                let pos = base + BlockPos::new(x as i32, y as i32, z as i32);
                if !mask.test(world, pos, id, &mut rng) {
                    clipboard.data[index] = 0;
                    clipboard
                        .block_entities
                        .remove(&BlockPos::new(x as i32, y as i32, z as i32));
                }
            }
        }
    }
}

fn selection_corners(ctx: &CommandExecuteContext<'_>) -> (BlockPos, BlockPos) {
    let first = ctx.player.first_position.unwrap();
    let second = ctx.player.second_position.unwrap();
    (first.min(second), first.max(second))
}

/// `//copy` and `//lazycopy`
pub(super) fn execute_copy(ctx: CommandExecuteContext<'_>) {
    let mask = match flag_mask(&ctx) {
        Ok(mask) => mask,
        Err(message) => {
            ctx.player.send_error_message(&message);
            return;
        }
    };
    let (min, max) = selection_corners(&ctx);
    let origin = ctx.player.pos.block_pos();
    let mut clipboard = create_clipboard(ctx.plot, origin, min, max);
    if let Some(mask) = &mask {
        apply_include_mask(ctx.plot, &mut clipboard, mask, min);
    }
    let volume = clipboard.volume();
    ctx.player.worldedit_clipboard = Some(clipboard);
    set_origin(ctx.player.uuid, Some(min));
    super::schematic::forget_loaded(ctx.player.uuid);
    ctx.player
        .send_worldedit_message(&format!("{volume} block(s) were copied."));
}

/// `//cut [leave-id]` and `//lazycut [leave-id]`
pub(super) fn execute_cut(ctx: CommandExecuteContext<'_>) {
    let mask = match flag_mask(&ctx) {
        Ok(mask) => mask,
        Err(message) => {
            ctx.player.send_error_message(&message);
            return;
        }
    };
    let leave = ctx.arguments[0].unwrap_pattern().clone();
    let (min, max) = selection_corners(&ctx);
    let origin = ctx.player.pos.block_pos();
    let mut clipboard = create_clipboard(ctx.plot, origin, min, max);
    if let Some(mask) = &mask {
        apply_include_mask(ctx.plot, &mut clipboard, mask, min);
    }
    let volume = clipboard.volume();
    let pattern_clipboard = leave.needs_clipboard().then(|| clipboard.clone());
    ctx.player.worldedit_clipboard = Some(clipboard);
    set_origin(ctx.player.uuid, Some(min));
    super::schematic::forget_loaded(ctx.player.uuid);
    let job = ops::region_job(
        ctx.plot,
        ctx.player.uuid,
        ctx.player.worldedit_mask.clone(),
        mask,
        min,
        max,
        leave,
        pattern_clipboard,
        None,
    )
    .on_finish(move |player, _stats| {
        player.send_worldedit_message(&format!("{volume} block(s) were cut."));
    });
    ctx.jobs.push_back(job);
}

/// `//paste`, `//place`
pub(super) fn execute_paste(ctx: CommandExecuteContext<'_>) {
    let mask = match flag_mask(&ctx) {
        Ok(mask) => mask,
        Err(message) => {
            ctx.player.send_error_message(&message);
            return;
        }
    };
    let mut clipboard = ctx.player.worldedit_clipboard.clone().unwrap();
    let pos = ctx.player.pos.block_pos();
    let first_pos = match origin_of(ctx.player.uuid) {
        Some(original) if ctx.has_flag('o') => original,
        _ => BlockPos::new(
            pos.x - clipboard.offset_x,
            pos.y - clipboard.offset_y,
            pos.z - clipboard.offset_z,
        ),
    };
    let second_pos = BlockPos::new(
        first_pos.x + clipboard.size_x as i32 - 1,
        first_pos.y + clipboard.size_y as i32 - 1,
        first_pos.z + clipboard.size_z as i32 - 1,
    );
    if ctx.has_flag('s') || ctx.has_flag('n') {
        ctx.player.worldedit_set_first_position(first_pos);
        ctx.player.worldedit_set_second_position(second_pos);
    }
    if ctx.has_flag('n') {
        return;
    }
    if let Some(mask) = &mask {
        apply_include_mask(ctx.plot, &mut clipboard, mask, first_pos);
    }
    let placed_at =
        first_pos + BlockPos::new(clipboard.offset_x, clipboard.offset_y, clipboard.offset_z);
    let mut job = EditJob::new(ctx.player.uuid, ctx.player.worldedit_mask.clone()).stage(
        ops::paste_stage(ctx.plot, clipboard, vec![first_pos], ctx.has_flag('a'))
            .commit_to(HistoryDest::Undo),
    );
    if ctx.has_flag('u') {
        job = job.stage(update_stage(ctx.plot, first_pos, second_pos));
    }
    ctx.jobs.push_back(job.on_finish(move |player, _stats| {
        player.send_worldedit_message(&format!(
            "The clipboard has been pasted at ({}, {}, {})",
            placed_at.x, placed_at.y, placed_at.z
        ));
    }));
}

/// `/clearclipboard`
pub(super) fn execute_clearclipboard(ctx: CommandExecuteContext<'_>) {
    ctx.player.worldedit_clipboard = None;
    set_origin(ctx.player.uuid, None);
    super::schematic::forget_loaded(ctx.player.uuid);
    ctx.player.send_worldedit_message("Clipboard cleared.");
}

/// `/download` and `/asset` need the web interface of FastAsyncWorldEdit, which does not exist
/// here
pub(super) fn execute_web_stub(ctx: CommandExecuteContext<'_>) {
    ctx.player
        .send_error_message("The web interface is not available on this server.");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins_are_stored_per_player() {
        set_origin(1, Some(BlockPos::new(1, 2, 3)));
        set_origin(2, Some(BlockPos::new(4, 5, 6)));
        assert_eq!(origin_of(1), Some(BlockPos::new(1, 2, 3)));
        set_origin(1, None);
        assert_eq!(origin_of(1), None);
        assert_eq!(origin_of(2), Some(BlockPos::new(4, 5, 6)));
        set_origin(2, None);
    }

    #[test]
    fn copy_flags_are_distinct() {
        for flags in [COPY_FLAGS, PASTE_FLAGS] {
            for (i, a) in flags.iter().enumerate() {
                assert!(flags[i + 1..].iter().all(|b| b.letter != a.letter));
            }
        }
    }
}
