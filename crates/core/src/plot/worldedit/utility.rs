//! Utility commands of the FAWE command overview: history, options (`//fast`, `//toggleplace`,
//! `//gsmask`, `//tips`), the help pages for masks/patterns/transforms and the small area
//! commands around the player (`//drain`, `//fixlava`, `//fixwater`, `//snow`, `//thaw`,
//! `//green`, `//ex`).

use super::ops;
use super::pattern::{Mask, Pattern};
use super::*;
use crate::plot::PLOT_BLOCK_HEIGHT;
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Mutex;

/// Options of a player that are not stored in the player itself
#[derive(Default, Clone)]
pub(super) struct PlayerOptions {
    pub fast: bool,
    pub toggle_place: bool,
    pub tips: bool,
    pub source_mask: Option<Mask>,
}

static OPTIONS: LazyLock<Mutex<HashMap<u128, PlayerOptions>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub(super) fn with_options<R>(uuid: u128, f: impl FnOnce(&mut PlayerOptions) -> R) -> R {
    let mut map = OPTIONS.lock().unwrap_or_else(|e| e.into_inner());
    f(map.entry(uuid).or_default())
}

/// Parses `on|off` like FAWE's boolean arguments. `None` when the text is empty (toggle).
fn parse_toggle(text: &str) -> Result<Option<bool>, String> {
    match text.to_ascii_lowercase().as_str() {
        "" => Ok(None),
        "on" | "true" | "yes" | "enable" | "enabled" => Ok(Some(true)),
        "off" | "false" | "no" | "disable" | "disabled" => Ok(Some(false)),
        _ => Err(format!("Invalid boolean: {text} (expected on or off)")),
    }
}

fn check_player_arg(ctx: &CommandExecuteContext<'_>, index: usize) -> bool {
    let name = ctx.arguments[index].unwrap_string();
    if name.is_empty() || name.eq_ignore_ascii_case(&ctx.player.username) {
        return true;
    }
    ctx.player.send_error_message(&format!(
        "Player not found: {name} (only your own history can be used)"
    ));
    false
}

fn history_command(ctx: CommandExecuteContext<'_>, undo: bool) {
    if !check_player_arg(&ctx, 1) {
        return;
    }
    let times = ctx.arguments[0].unwrap_uint().clamp(1, 1000);
    let available = if undo {
        ctx.player.worldedit_history.undo_len()
    } else {
        ctx.player.worldedit_history.redo_len()
    } as u32;
    if available == 0 {
        ctx.player.send_error_message(if undo {
            "Nothing left to undo."
        } else {
            "Nothing left to redo."
        });
        return;
    }
    ctx.jobs.push_back(ops::history_job(
        ctx.player.uuid,
        times.min(available),
        undo,
    ));
}

pub(super) fn execute_undo(ctx: CommandExecuteContext<'_>) {
    history_command(ctx, true);
}

pub(super) fn execute_redo(ctx: CommandExecuteContext<'_>) {
    history_command(ctx, false);
}

pub(super) fn execute_clear_history(ctx: CommandExecuteContext<'_>) {
    ctx.player.worldedit_history.clear();
    ctx.player.send_worldedit_message("History cleared.");
}

pub(super) fn execute_fast(ctx: CommandExecuteContext<'_>) {
    let requested = match parse_toggle(ctx.arguments[0].unwrap_string()) {
        Ok(requested) => requested,
        Err(message) => return ctx.player.send_error_message(&message),
    };
    let enabled = with_options(ctx.player.uuid, |o| {
        o.fast = requested.unwrap_or(!o.fast);
        o.fast
    });
    ctx.player.send_worldedit_message(if enabled {
        "Fast mode enabled. Lighting in the affected chunks may be wrong and/or you may need to rejoin to see changes."
    } else {
        "Fast mode disabled."
    });
}

pub(super) fn execute_toggle_place(ctx: CommandExecuteContext<'_>) {
    let pos1 = with_options(ctx.player.uuid, |o| {
        o.toggle_place = !o.toggle_place;
        o.toggle_place
    });
    ctx.player.send_worldedit_message(if pos1 {
        "Now placing at pos #1."
    } else {
        "Now placing at the block you stand in."
    });
}

pub(super) fn execute_tips(ctx: CommandExecuteContext<'_>) {
    let requested = match parse_toggle(ctx.arguments[0].unwrap_string()) {
        Ok(requested) => requested,
        Err(message) => return ctx.player.send_error_message(&message),
    };
    let enabled = with_options(ctx.player.uuid, |o| {
        o.tips = requested.unwrap_or(!o.tips);
        o.tips
    });
    ctx.player.send_worldedit_message(if enabled {
        "Tips enabled."
    } else {
        "Tips disabled."
    });
}

pub(super) fn execute_gsmask(ctx: CommandExecuteContext<'_>) {
    let text = ctx.arguments[0].unwrap_string();
    let mask = if text.is_empty() || matches!(text.as_str(), "none" | "null" | "off") {
        None
    } else {
        match Mask::parse(text) {
            Ok(mask) => Some(mask),
            Err(err) => return ctx.player.send_error_message(&err.to_string()),
        }
    };
    let set = mask.is_some();
    with_options(ctx.player.uuid, |o| o.source_mask = mask);
    ctx.player.send_worldedit_message(if set {
        "Global source mask set."
    } else {
        "Global source mask disabled."
    });
}

const PAGE_SIZE: usize = 8;

fn send_page(player: &Player, title: &str, command: &str, lines: &[&str], page: u32) {
    let pages = lines.len().div_ceil(PAGE_SIZE).max(1);
    let page = page.max(1) as usize;
    if page > pages {
        return player.send_error_message(&format!("Page {page} does not exist (1-{pages})."));
    }
    player.send_worldedit_message(&format!("{title} (page {page}/{pages})"));
    for line in lines.iter().skip((page - 1) * PAGE_SIZE).take(PAGE_SIZE) {
        player.send_worldedit_message(line);
    }
    if page < pages {
        player.send_worldedit_message(&format!("Use {command} {} to see the next page.", page + 1));
    }
}

const MASK_HELP: &[&str] = &[
    "<block>,<block>: any of the blocks (also with [properties] and * wildcards)",
    "<mask>&<mask>: all of the masks",
    "!<mask>: not the mask",
    ">/<<mask>: the block above/below matches the mask",
    "#existing, #air, #solid: blocks that are not air, air, solid blocks",
    "##<tag>: a block tag, for example ##wool",
    "%<percent>: matches with the given chance",
    "=<id>: a raw block state id",
];

const PATTERN_HELP: &[&str] = &[
    "<block>: a single block, also with [properties]",
    "<percent>%<pattern>,...: a random choice, the weights default to 1",
    "^<block>: place the block but keep the properties of the old block",
    "#clipboard: the blocks of your clipboard, repeated over the area",
    "=<id>: a raw block state id",
];

const TRANSFORM_HELP: &[&str] = &[
    "#offset[dx][dy][dz][transform]: moves the blocks",
    "#rotate[rx][ry][rz][transform]: rotates around your position (multiples of 90)",
    "#scale[sx][sy][sz][transform]: scales by whole numbers",
    "#pattern[pattern][transform]: replaces the blocks with the pattern",
    "#linear[t1,t2,...], #linear3d[t1,t2,...]: uses the transforms one after the other",
    "#spread[dx][dy][dz][transform]: moves the blocks by a random amount",
    "Use //gtransform <transform> (or /transform for a brush), none removes it.",
];

pub(super) fn execute_gtransform(ctx: CommandExecuteContext<'_>) {
    let text = ctx.arguments[0].unwrap_string();
    if text.is_empty() || matches!(text.as_str(), "none" | "null" | "off") {
        super::pattern::set_global_transform(ctx.player.uuid, None);
        return ctx.player.send_worldedit_message("Transform cleared.");
    }
    match super::pattern::GlobalTransform::parse(text, ctx.player.pos.block_pos()) {
        Ok(transform) => {
            super::pattern::set_global_transform(ctx.player.uuid, Some(transform));
            ctx.player.send_worldedit_message("Transform set.");
        }
        Err(err) => ctx.player.send_error_message(&err.to_string()),
    }
}

pub(super) fn execute_masks(ctx: CommandExecuteContext<'_>) {
    send_page(
        ctx.player,
        "Masks",
        "//masks",
        MASK_HELP,
        ctx.arguments[0].unwrap_uint(),
    );
}

pub(super) fn execute_patterns(ctx: CommandExecuteContext<'_>) {
    send_page(
        ctx.player,
        "Patterns",
        "//patterns",
        PATTERN_HELP,
        ctx.arguments[0].unwrap_uint(),
    );
}

pub(super) fn execute_transforms(ctx: CommandExecuteContext<'_>) {
    send_page(
        ctx.player,
        "Transforms",
        "//transforms",
        TRANSFORM_HELP,
        ctx.arguments[0].unwrap_uint(),
    );
}

/// An edit of the blocks in a circle around the player over the whole height of the plot
fn area_job(
    ctx: &CommandExecuteContext<'_>,
    radius: u32,
    filter: &str,
    pattern: &str,
) -> Result<EditJob, String> {
    let radius = radius.clamp(1, 256) as i32;
    let filter = Mask::parse(filter).map_err(|e| e.to_string())?;
    let pattern = Pattern::parse(pattern).map_err(|e| e.to_string())?;
    let center = ctx.player.pos.block_pos();
    let (cx, cz) = (center.x, center.z);
    let r2 = (radius as i64) * (radius as i64) + radius as i64;
    Ok(ops::region_job(
        ctx.plot,
        ctx.player.uuid,
        ctx.player.worldedit_mask.clone(),
        Some(filter),
        BlockPos::new(cx - radius, 0, cz - radius),
        BlockPos::new(cx + radius, PLOT_BLOCK_HEIGHT - 1, cz + radius),
        pattern,
        None,
        Some(Box::new(move |p: BlockPos| {
            let (dx, dz) = ((p.x - cx) as i64, (p.z - cz) as i64);
            dx * dx + dz * dz <= r2
        })),
    ))
}

fn run_area(
    ctx: CommandExecuteContext<'_>,
    radius: u32,
    filter: &str,
    pattern: &str,
    message: &'static str,
) {
    match area_job(&ctx, radius, filter, pattern) {
        Ok(job) => ctx.jobs.push_back(job.on_finish(move |player, stats| {
            player.send_worldedit_message(&message.replace("{}", &stats.blocks.to_string()));
        })),
        Err(message) => ctx.player.send_error_message(&message),
    }
}

pub(super) fn execute_drain(ctx: CommandExecuteContext<'_>) {
    let radius = ctx.arguments[0].unwrap_uint();
    run_area(
        ctx,
        radius,
        "water,lava",
        "air",
        "{} blocks have been drained.",
    );
}

pub(super) fn execute_fix_lava(ctx: CommandExecuteContext<'_>) {
    let radius = ctx.arguments[0].unwrap_uint();
    run_area(
        ctx,
        radius,
        "lava[level=1],lava[level=2],lava[level=3],lava[level=4],lava[level=5],lava[level=6],lava[level=7]",
        "lava[level=0]",
        "{} lava blocks have been fixed.",
    );
}

pub(super) fn execute_fix_water(ctx: CommandExecuteContext<'_>) {
    let radius = ctx.arguments[0].unwrap_uint();
    run_area(
        ctx,
        radius,
        "water[level=1],water[level=2],water[level=3],water[level=4],water[level=5],water[level=6],water[level=7]",
        "water[level=0]",
        "{} water blocks have been fixed.",
    );
}

pub(super) fn execute_extinguish(ctx: CommandExecuteContext<'_>) {
    let radius = ctx.arguments[0].unwrap_uint();
    run_area(
        ctx,
        radius,
        "fire,soul_fire",
        "air",
        "{} fires have been extinguished.",
    );
}

pub(super) fn execute_snow(ctx: CommandExecuteContext<'_>) {
    let radius = ctx.arguments[0].unwrap_uint();
    run_area(
        ctx,
        radius,
        "#air&<#solid",
        "snow",
        "{} surfaces covered. Let it snow!",
    );
}

pub(super) fn execute_green(ctx: CommandExecuteContext<'_>) {
    let radius = ctx.arguments[0].unwrap_uint();
    let filter = if ctx.has_flag('f') {
        "dirt&>#air,coarse_dirt&>#air"
    } else {
        "dirt&>#air"
    };
    run_area(ctx, radius, filter, "grass_block", "{} surfaces greened.");
}

pub(super) fn execute_thaw(ctx: CommandExecuteContext<'_>) {
    let radius = ctx.arguments[0].unwrap_uint();
    let total = Rc::new(Cell::new(0usize));
    let first = area_job(&ctx, radius, "snow", "air");
    let second = area_job(&ctx, radius, "ice", "water");
    let (Ok(first), Ok(second)) = (first, second) else {
        return ctx
            .player
            .send_error_message("Could not build the thaw edit.");
    };
    let t1 = total.clone();
    ctx.jobs
        .push_back(first.on_finish(move |_, stats| t1.set(t1.get() + stats.blocks)));
    ctx.jobs.push_back(second.on_finish(move |player, stats| {
        player.send_worldedit_message(&format!("{} surfaces thawed.", total.get() + stats.blocks));
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggles() {
        assert_eq!(parse_toggle("").unwrap(), None);
        assert_eq!(parse_toggle("ON").unwrap(), Some(true));
        assert_eq!(parse_toggle("off").unwrap(), Some(false));
        assert!(parse_toggle("maybe").is_err());
    }

    #[test]
    fn area_masks_and_patterns_parse() {
        for (mask, pattern) in [
            ("water,lava", "air"),
            ("lava[level=1],lava[level=7]", "lava[level=0]"),
            ("water[level=3]", "water[level=0]"),
            ("fire,soul_fire", "air"),
            ("#air&<#solid", "snow"),
            ("dirt&>#air,coarse_dirt&>#air", "grass_block"),
            ("snow", "air"),
            ("ice", "water"),
        ] {
            Mask::parse(mask).unwrap_or_else(|e| panic!("{mask}: {e}"));
            Pattern::parse(pattern).unwrap_or_else(|e| panic!("{pattern}: {e}"));
        }
    }

    #[test]
    fn options_are_per_player() {
        with_options(9_000_001, |o| o.fast = true);
        assert!(with_options(9_000_001, |o| o.fast));
        assert!(!with_options(9_000_002, |o| o.fast));
    }
}
