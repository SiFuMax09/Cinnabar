//! The commands for brushes and tools: `/brush`, the brush options and `/tool`.

use super::brush::{Brush, BrushKind, MAX_BRUSH_SIZE};
use super::tool::Tool;
use super::*;
use crate::player::PacketSender;
use crate::utils;
use cinnabar_blocks::items::Item;
use std::path::PathBuf;

/// The brushes FAWE has that this server cannot do
const UNSUPPORTED_BRUSHES: &[&str] = &[
    "copypaste",
    "populateschematic",
    "shatter",
    "pull",
    "stencil",
    "spline",
    "sweep",
    "sspl",
    "blendball",
    "rock",
    "height",
    "flatten",
    "cliff",
    "butcher",
    "kill",
];

pub const BRUSH_TYPES: &str = "sphere, cylinder, circle, clipboard, smooth, gravity, extinguish, \
    splatter, scatter, layer, surface, recursive, erode, line, catenary, command, scmd";

fn held_item(player: &Player) -> Option<Item> {
    player.inventory[player.selected_slot as usize + 36]
        .as_ref()
        .map(|item| item.item_type)
}

fn held_item_id(player: &Player) -> Option<u32> {
    held_item(player).map(|item| item.get_id())
}

fn number(text: &str, default: u32, min: u32, max: u32, what: &str) -> Result<u32, String> {
    if text.is_empty() {
        return Ok(default);
    }
    match text.parse::<u32>() {
        Ok(n) if (min..=max).contains(&n) => Ok(n),
        _ => Err(format!(
            "The {what} has to be a number from {min} to {max}."
        )),
    }
}

fn size_of(text: &str, default: u32) -> Result<u32, String> {
    number(text, default, 0, MAX_BRUSH_SIZE, "radius")
}

fn pattern_of(text: &str, usage: &str) -> Result<Pattern, String> {
    if text.is_empty() {
        return Err(format!("Too few arguments. Usage: {usage}"));
    }
    Pattern::parse(text).map_err(|err| err.to_string())
}

fn permission_of(kind: &str) -> &'static str {
    match kind {
        "sphere" | "circle" => "worldedit.brush.sphere",
        "cylinder" => "worldedit.brush.cylinder",
        "clipboard" => "worldedit.brush.clipboard",
        "smooth" => "worldedit.brush.smooth",
        "gravity" => "worldedit.brush.gravity",
        "extinguish" => "worldedit.brush.ex",
        "splatter" => "worldedit.brush.splatter",
        "scatter" => "worldedit.brush.scatter",
        "layer" => "worldedit.brush.layer",
        "surface" => "worldedit.brush.surface",
        "recursive" => "worldedit.brush.recursive",
        "erode" => "worldedit.brush.erode",
        "line" => "worldedit.brush.line",
        "catenary" => "worldedit.brush.spline",
        "command" => "worldedit.brush.command",
        "scmd" => "worldedit.brush.scattercommand",
        _ => "worldedit.brush.options",
    }
}

/// The name a brush type is known by, `None` for something that is not a brush
fn canonical(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "sphere" | "s" => "sphere",
        "cylinder" | "cyl" | "c" => "cylinder",
        "circle" => "circle",
        "clipboard" | "copy" => "clipboard",
        "smooth" => "smooth",
        "gravity" | "grav" => "gravity",
        "extinguish" | "ex" => "extinguish",
        "splatter" | "splat" => "splatter",
        "scatter" => "scatter",
        "layer" => "layer",
        "surface" => "surface",
        "recursive" | "recurse" => "recursive",
        "erode" => "erode",
        "line" => "line",
        "catenary" | "cat" => "catenary",
        "command" | "cmd" => "command",
        "scmd" | "scattercommand" => "scmd",
        _ => return None,
    })
}

/// Everything after the brush type: the arguments and the flags
struct BrushArgs<'a> {
    args: Vec<&'a str>,
    flags: Vec<char>,
    mask_flag: Option<&'a str>,
}

impl BrushArgs<'_> {
    fn arg(&self, i: usize) -> &str {
        self.args.get(i).copied().unwrap_or("")
    }

    fn has(&self, flag: char) -> bool {
        self.flags.contains(&flag)
    }

    fn rest(&self, from: usize) -> Vec<&str> {
        self.args.iter().skip(from).copied().collect()
    }
}

fn commands_of(words: &[&str]) -> Result<Vec<String>, String> {
    let commands: Vec<String> = words
        .join(" ")
        .split(';')
        .map(|c| c.trim().to_owned())
        .filter(|c| !c.is_empty())
        .collect();
    if commands.is_empty() {
        return Err("Too few arguments. A command is missing.".to_owned());
    }
    Ok(commands)
}

fn create_brush(kind: &str, a: &BrushArgs<'_>) -> Result<Brush, String> {
    let air = Pattern::Single(0);
    let hollow = a.has('h');
    Ok(match kind {
        "sphere" => {
            let pattern = pattern_of(a.arg(0), "/brush sphere [-hf] <pattern> [radius=2]")?;
            Brush::new(
                BrushKind::Sphere {
                    hollow,
                    falling: a.has('f'),
                },
                pattern,
                size_of(a.arg(1), 2)?,
            )
        }
        "cylinder" => {
            let pattern = pattern_of(
                a.arg(0),
                "/brush cylinder [-h] <pattern> [radius=2] [height=1]",
            )?;
            let height = number(a.arg(2), 1, 1, MAX_BRUSH_SIZE, "height")?;
            Brush::new(
                BrushKind::Cylinder { height, hollow },
                pattern,
                size_of(a.arg(1), 2)?,
            )
        }
        "circle" => Brush::new(
            BrushKind::Circle,
            pattern_of(a.arg(0), "/brush circle <pattern> [radius=5]")?,
            size_of(a.arg(1), 5)?,
        ),
        "clipboard" => {
            let skip = match a.mask_flag {
                Some(text) => Some(Mask::parse(text).map_err(|e| e.to_string())?),
                None => None,
            };
            Brush::new(
                BrushKind::Clipboard {
                    ignore_air: a.has('a'),
                    original_position: a.has('o'),
                    skip,
                },
                air,
                1,
            )
        }
        "smooth" => {
            let size = size_of(a.arg(0), 2)?;
            let iterations = number(a.arg(1), 4, 1, 100, "iterations")?;
            Brush::new(
                BrushKind::Smooth {
                    iterations,
                    natural_only: a.has('n'),
                },
                air,
                size,
            )
        }
        "gravity" => Brush::new(
            BrushKind::Gravity {
                from_world_top: a.has('h'),
            },
            air,
            size_of(a.arg(0), 5)?,
        ),
        "extinguish" => Brush::new(BrushKind::Extinguish, air, size_of(a.arg(0), 5)?),
        "splatter" => {
            let pattern = pattern_of(
                a.arg(0),
                "/brush splatter <pattern> [radius=5] [seeds=1] [recursion=5] [solid=true]",
            )?;
            let solid = match a.arg(4) {
                "" => true,
                text => {
                    shapes::parse_bool(text).ok_or("The solid argument has to be true or false.")?
                }
            };
            Brush::new(
                BrushKind::Splatter {
                    seeds: number(a.arg(2), 1, 1, 1000, "seeds")?,
                    recursion: number(a.arg(3), 5, 0, 100, "recursion")?,
                    solid,
                },
                pattern,
                size_of(a.arg(1), 5)?,
            )
        }
        "scatter" => {
            let pattern = pattern_of(
                a.arg(0),
                "/brush scatter [-o] <pattern> [radius=5] [points=5] [distance=1]",
            )?;
            Brush::new(
                BrushKind::Scatter {
                    points: number(a.arg(2), 5, 1, 10_000, "points")?,
                    distance: number(a.arg(3), 1, 0, MAX_BRUSH_SIZE, "distance")?,
                    overlay: a.has('o'),
                },
                pattern,
                size_of(a.arg(1), 5)?,
            )
        }
        "layer" => {
            if a.args.len() < 2 {
                return Err(
                    "Too few arguments. Usage: /brush layer <radius> <pattern> [pattern...]"
                        .to_owned(),
                );
            }
            let size = size_of(a.arg(0), 5)?;
            let layers = a
                .rest(1)
                .into_iter()
                .map(|p| Pattern::parse(p).map_err(|e| e.to_string()))
                .collect::<Result<Vec<_>, _>>()?;
            Brush::new(BrushKind::Layer { layers }, air, size)
        }
        "surface" => Brush::new(
            BrushKind::Surface,
            pattern_of(a.arg(0), "/brush surface <pattern> [radius=5]")?,
            size_of(a.arg(1), 5)?,
        ),
        "recursive" => Brush::new(
            BrushKind::Recursive {
                depth_first: a.has('d'),
            },
            pattern_of(a.arg(0), "/brush recursive [-d] <pattern-to> [radius=5]")?,
            size_of(a.arg(1), 5)?,
        ),
        "erode" => Brush::new(BrushKind::Erode, air, size_of(a.arg(0), 5)?),
        "line" => Brush::new(
            BrushKind::Line {
                hollow,
                select: a.has('s'),
                flat: a.has('f'),
            },
            pattern_of(a.arg(0), "/brush line [-hsf] <pattern> [radius=0]")?,
            size_of(a.arg(1), 0)?,
        ),
        "catenary" => {
            let pattern = pattern_of(
                a.arg(0),
                "/brush catenary [-hs] <pattern> [lengthFactor=1.2] [size=0]",
            )?;
            let length_factor = match a.arg(1) {
                "" => 1.2,
                text => text
                    .parse::<f64>()
                    .ok()
                    .filter(|f| *f >= 1.0 && *f <= 100.0)
                    .ok_or("The length factor has to be a number from 1 to 100.")?,
            };
            Brush::new(
                BrushKind::Catenary {
                    length_factor,
                    hollow,
                    select: a.has('s'),
                },
                pattern,
                size_of(a.arg(2), 0)?,
            )
        }
        "command" => {
            if a.args.len() < 2 {
                return Err(
                    "Too few arguments. Usage: /brush command <radius> <command>[;command...]"
                        .to_owned(),
                );
            }
            let size = size_of(a.arg(0), 5)?;
            Brush::new(
                BrushKind::Command {
                    commands: commands_of(&a.rest(1))?,
                },
                air,
                size,
            )
        }
        "scmd" => {
            if a.args.len() < 4 {
                return Err("Too few arguments. Usage: /brush scmd <scatter-radius> <points> <cmd-radius=1> <command>[;command...]".to_owned());
            }
            let size = size_of(a.arg(0), 5)?;
            let points = number(a.arg(1), 5, 1, 10_000, "points")?;
            let command_radius = number(a.arg(2), 1, 0, MAX_BRUSH_SIZE, "command radius")?;
            Brush::new(
                BrushKind::ScatterCommand {
                    points,
                    command_radius,
                    commands: commands_of(&a.rest(3))?,
                },
                air,
                size,
            )
        }
        _ => unreachable!("every canonical brush type is handled"),
    })
}

fn equipped_message(brush: &Brush) -> String {
    let size = brush.size;
    match &brush.kind {
        BrushKind::Sphere { .. } => format!("Sphere brush shape equipped ({size})."),
        BrushKind::Cylinder { height, .. } => {
            format!("Cylinder brush shape equipped ({size} by {height}).")
        }
        BrushKind::Clipboard { .. } => "Clipboard brush shape equipped.".to_owned(),
        BrushKind::Smooth { iterations, .. } => {
            format!("Smooth brush equipped ({size} x {iterations}x using all blocks).")
        }
        BrushKind::Gravity { .. } => format!("Gravity brush equipped ({size})."),
        BrushKind::Extinguish => format!("Extinguisher equipped ({size})."),
        other => {
            let name = other.name();
            let mut text = name[..1].to_uppercase();
            text.push_str(&name[1..]);
            format!("{text} brush equipped ({size}).")
        }
    }
}

/// The text a brush is saved and loaded as: the command that makes it
fn spec_text(kind: &str, args: &[&str], flags: &[char], mask_flag: Option<&str>) -> String {
    let mut text = kind.to_owned();
    if !flags.is_empty() {
        text.push_str(" -");
        text.extend(flags.iter().filter(|f| **f != 'm'));
    }
    if let Some(mask) = mask_flag {
        text.push_str(&format!(" -m {mask}"));
    }
    for arg in args {
        text.push(' ');
        text.push_str(arg);
    }
    text
}

fn collect_args<'a>(ctx: &'a CommandExecuteContext<'_>) -> Vec<&'a str> {
    ctx.arguments[1..]
        .iter()
        .map(|a| a.unwrap_string().as_str())
        .take_while(|a| !a.is_empty())
        .collect()
}

#[derive(Clone, Copy, PartialEq)]
enum Click {
    Both,
    Primary,
    Secondary,
}

fn bind_brush(ctx: &mut CommandExecuteContext<'_>, click: Click) {
    let kind = ctx.arguments[0].unwrap_string().to_lowercase();
    let Some(item_id) = held_item_id(ctx.player) else {
        ctx.player
            .send_error_message("Hold the item you want to bind the brush to.");
        return;
    };
    if held_item(ctx.player) == Some(Item::WoodenAxe) {
        ctx.player
            .send_error_message("The wooden axe is the selection wand, use another item.");
        return;
    }
    if matches!(kind.as_str(), "none" | "unbind" | "off") {
        remove_binding(ctx.player, item_id);
        ctx.player
            .send_worldedit_message("Tool unbound from your current item.");
        return;
    }
    let Some(name) = canonical(&kind) else {
        if UNSUPPORTED_BRUSHES.contains(&kind.as_str()) {
            ctx.player.send_error_message(&format!(
                "The {kind} brush is not available on this server."
            ));
        } else {
            ctx.player.send_error_message(&format!(
                "Unknown brush: {kind}. Available brushes: {BRUSH_TYPES}."
            ));
        }
        return;
    };
    if !ctx.player.has_permission(permission_of(name)) {
        ctx.player.send_no_permission_message();
        return;
    }
    let args = collect_args(ctx);
    let mut flags = ctx.flags.clone();
    flags.dedup();
    let mask_flag = ctx.flag_arg('m');
    let parsed = BrushArgs {
        args: args.clone(),
        flags: flags.clone(),
        mask_flag,
    };
    match create_brush(name, &parsed) {
        Ok(mut brush) => {
            let spec = spec_text(name, &args, &flags, mask_flag);
            brush.scroll = None;
            let message = equipped_message(&brush);
            let player = &mut *ctx.player;
            player.worldedit_tools.remove(&item_id);
            if click != Click::Primary {
                player.worldedit_brushes.insert(item_id, brush.clone());
            } else {
                player.worldedit_brushes.remove(&item_id);
            }
            if click != Click::Secondary {
                player.worldedit_brushes_primary.insert(item_id, brush);
            } else {
                player.worldedit_brushes_primary.remove(&item_id);
            }
            player.worldedit_brush_specs.insert(item_id, spec);
            player.send_worldedit_message(&message);
        }
        Err(message) => ctx.player.send_error_message(&message),
    }
}

fn remove_binding(player: &mut Player, item_id: u32) {
    player.worldedit_brushes.remove(&item_id);
    player.worldedit_brushes_primary.remove(&item_id);
    player.worldedit_tools.remove(&item_id);
    player.worldedit_brush_specs.remove(&item_id);
}

pub(super) fn execute_brush(mut ctx: CommandExecuteContext<'_>) {
    bind_brush(&mut ctx, Click::Both);
}

pub(super) fn execute_primary(mut ctx: CommandExecuteContext<'_>) {
    bind_brush(&mut ctx, Click::Primary);
}

pub(super) fn execute_secondary(mut ctx: CommandExecuteContext<'_>) {
    bind_brush(&mut ctx, Click::Secondary);
}

pub(super) fn execute_none(ctx: CommandExecuteContext<'_>) {
    if let Some(item_id) = held_item_id(ctx.player) {
        remove_binding(ctx.player, item_id);
    }
    ctx.player
        .send_worldedit_message("Tool unbound from your current item.");
}

/// Changes the brushes of the item in the hand of the player
fn with_held_brush(
    ctx: &mut CommandExecuteContext<'_>,
    message: &str,
    change: impl Fn(&mut Brush) -> Result<(), String>,
) {
    let Some(item_id) = held_item_id(ctx.player) else {
        ctx.player
            .send_error_message("You are not holding a brush.");
        return;
    };
    let player = &mut *ctx.player;
    let mut found = false;
    let mut failure = None;
    for map in [
        &mut player.worldedit_brushes,
        &mut player.worldedit_brushes_primary,
    ] {
        if let Some(brush) = map.get_mut(&item_id) {
            found = true;
            if let Err(error) = change(brush) {
                failure = Some(error);
                break;
            }
        }
    }
    if let Some(error) = failure {
        player.send_error_message(&error);
        return;
    }
    if !found {
        player.send_error_message("Your current item is not a brush. Use /brush first.");
        return;
    }
    player.send_worldedit_message(message);
}

pub(super) fn execute_size(mut ctx: CommandExecuteContext<'_>) {
    let size = ctx.arguments[0].unwrap_uint();
    if size > MAX_BRUSH_SIZE {
        ctx.player
            .send_error_message(&format!("The maximum brush size is {MAX_BRUSH_SIZE}."));
        return;
    }
    with_held_brush(&mut ctx, &format!("Brush size set to {size}."), move |b| {
        b.size = size;
        Ok(())
    });
}

pub(super) fn execute_range(mut ctx: CommandExecuteContext<'_>) {
    let range = ctx.arguments[0].unwrap_uint();
    with_held_brush(
        &mut ctx,
        &format!("Brush range set to {range}."),
        move |b| {
            b.range = Some(range);
            Ok(())
        },
    );
}

pub(super) fn execute_mat(mut ctx: CommandExecuteContext<'_>) {
    let pattern = ctx.arguments[0].unwrap_pattern().clone();
    with_held_brush(&mut ctx, "Brush material set.", move |b| {
        b.pattern = pattern.clone();
        Ok(())
    });
}

fn optional_mask(text: &str) -> Result<Option<Mask>, String> {
    if text.is_empty() || matches!(text, "none" | "off" | "clear") {
        return Ok(None);
    }
    Mask::parse(text).map(Some).map_err(|err| err.to_string())
}

pub(super) fn execute_mask(mut ctx: CommandExecuteContext<'_>) {
    let text = ctx.arguments[0].unwrap_string().clone();
    let mask = match optional_mask(&text) {
        Ok(mask) => mask,
        Err(error) => return ctx.player.send_error_message(&error),
    };
    let message = match mask {
        Some(_) => "Brush mask set.",
        None => "Brush mask disabled.",
    };
    with_held_brush(&mut ctx, message, move |b| {
        b.mask = mask.clone();
        b.mask_text = mask.is_some().then(|| text.clone());
        Ok(())
    });
}

pub(super) fn execute_smask(mut ctx: CommandExecuteContext<'_>) {
    let text = ctx.arguments[0].unwrap_string().clone();
    let mask = match optional_mask(&text) {
        Ok(mask) => mask,
        Err(error) => return ctx.player.send_error_message(&error),
    };
    let message = match mask {
        Some(_) => "Brush source mask set.",
        None => "Brush source mask disabled.",
    };
    with_held_brush(&mut ctx, message, move |b| {
        b.smask = mask.clone();
        b.smask_text = mask.is_some().then(|| text.clone());
        Ok(())
    });
}

pub(super) fn execute_targetmask(mut ctx: CommandExecuteContext<'_>) {
    let text = ctx.arguments[0].unwrap_string().clone();
    let mask = match optional_mask(&text) {
        Ok(mask) => mask,
        Err(error) => return ctx.player.send_error_message(&error),
    };
    let message = match mask {
        Some(_) => "Brush target mask set.",
        None => "Brush target mask disabled.",
    };
    with_held_brush(&mut ctx, message, move |b| {
        b.target_mask = mask.clone();
        b.target_mask_text = mask.is_some().then(|| text.clone());
        Ok(())
    });
}

pub(super) fn execute_targetoffset(mut ctx: CommandExecuteContext<'_>) {
    let offset = match &ctx.arguments[0] {
        Argument::Integer(v) => *v,
        _ => 0,
    };
    with_held_brush(
        &mut ctx,
        &format!("Brush target offset set to {offset}."),
        move |b| {
            b.target_offset = offset;
            Ok(())
        },
    );
}

pub(super) fn execute_target(mut ctx: CommandExecuteContext<'_>) {
    let mode = ctx.arguments[0].unwrap_uint();
    if !(1..=4).contains(&mode) {
        ctx.player
            .send_error_message("The target mode has to be a number from 1 to 4.");
        return;
    }
    with_held_brush(
        &mut ctx,
        &format!("Brush target mode set to {mode}."),
        move |b| {
            b.target_mode = mode as u8;
            Ok(())
        },
    );
}

pub(super) fn execute_transform(mut ctx: CommandExecuteContext<'_>) {
    let text = ctx.arguments[0].unwrap_string().clone();
    let cleared = text.is_empty() || text == "none";
    if !cleared && let Err(err) = super::pattern::Transform::parse(&text) {
        return ctx.player.send_error_message(&err.to_string());
    }
    let message = match cleared {
        true => "Brush transform disabled.",
        false => "Brush transform set.",
    };
    with_held_brush(&mut ctx, message, move |b| {
        b.transform_text = (!cleared).then(|| text.clone());
        Ok(())
    });
}

pub(super) fn execute_scroll(mut ctx: CommandExecuteContext<'_>) {
    let mode = ctx.arguments[0].unwrap_string().to_lowercase();
    if !matches!(
        mode.as_str(),
        "none" | "clipboard" | "mask" | "pattern" | "range" | "size" | "visual" | "target"
    ) {
        ctx.player.send_error_message(
            "Unknown scroll action. Use none, clipboard, mask, pattern, range, size, visual or target.",
        );
        return;
    }
    let message = format!("Scroll action set to {mode}.");
    with_held_brush(&mut ctx, &message, move |b| {
        b.scroll = (mode != "none").then(|| mode.clone());
        Ok(())
    });
}

fn brush_dir(player: &Player, global: bool) -> PathBuf {
    let name = match global {
        true => "global".to_owned(),
        false => utils::HyphenatedUUID(player.uuid).to_string(),
    };
    PathBuf::from("./brushes").join(name)
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

pub(super) fn execute_savebrush(ctx: CommandExecuteContext<'_>) {
    let name = ctx.arguments[0].unwrap_string().clone();
    if !valid_name(&name) {
        ctx.player.send_error_message("The brush name is invalid.");
        return;
    }
    let Some(item_id) = held_item_id(ctx.player) else {
        ctx.player
            .send_error_message("You are not holding a brush.");
        return;
    };
    let (Some(brush), Some(spec)) = (
        ctx.player.worldedit_brushes.get(&item_id),
        ctx.player.worldedit_brush_specs.get(&item_id),
    ) else {
        ctx.player
            .send_error_message("Your current item is not a brush.");
        return;
    };
    let mut text = format!("brush {spec}\nsize {}\n", brush.size);
    if let Some(range) = brush.range {
        text.push_str(&format!("range {range}\n"));
    }
    text.push_str(&format!("target {}\n", brush.target_mode));
    text.push_str(&format!("targetoffset {}\n", brush.target_offset));
    if !matches!(
        brush.kind,
        BrushKind::Clipboard { .. } | BrushKind::Layer { .. }
    ) {
        text.push_str(&format!("mat {}\n", brush.pattern));
    }
    for (key, value) in [
        ("mask", &brush.mask_text),
        ("smask", &brush.smask_text),
        ("targetmask", &brush.target_mask_text),
        ("transform", &brush.transform_text),
    ] {
        if let Some(value) = value {
            text.push_str(&format!("{key} {value}\n"));
        }
    }
    let dir = brush_dir(ctx.player, ctx.has_flag('g'));
    let result = std::fs::create_dir_all(&dir)
        .and_then(|_| std::fs::write(dir.join(format!("{name}.brush")), text));
    match result {
        Ok(()) => ctx
            .player
            .send_worldedit_message(&format!("Brush saved as {name}.")),
        Err(error) => {
            tracing::error!("could not save a brush: {error}");
            ctx.player
                .send_error_message("The brush could not be saved.");
        }
    }
}

pub(super) fn execute_loadbrush(ctx: CommandExecuteContext<'_>) {
    let name = ctx.arguments[0].unwrap_string().clone();
    if !valid_name(&name) {
        ctx.player.send_error_message("The brush name is invalid.");
        return;
    }
    let Some(item_id) = held_item_id(ctx.player) else {
        ctx.player
            .send_error_message("Hold the item you want to bind the brush to.");
        return;
    };
    let file = format!("{name}.brush");
    let text = std::fs::read_to_string(brush_dir(ctx.player, false).join(&file))
        .or_else(|_| std::fs::read_to_string(brush_dir(ctx.player, true).join(&file)));
    let Ok(text) = text else {
        ctx.player
            .send_error_message(&format!("The brush {name} does not exist."));
        return;
    };
    match load_brush(&text) {
        Ok((brush, spec)) => {
            let player = &mut *ctx.player;
            player.worldedit_tools.remove(&item_id);
            player.worldedit_brushes.insert(item_id, brush.clone());
            player.worldedit_brushes_primary.insert(item_id, brush);
            player.worldedit_brush_specs.insert(item_id, spec);
            player.send_worldedit_message(&format!("Brush {name} loaded."));
        }
        Err(error) => ctx.player.send_error_message(&error),
    }
}

fn load_brush(text: &str) -> Result<(Brush, String), String> {
    let mut lines = text.lines();
    let first = lines
        .next()
        .and_then(|l| l.strip_prefix("brush "))
        .ok_or("The brush file is broken.")?;
    let spec = first.to_owned();
    let mut words: Vec<&str> = spec.split(' ').collect();
    let kind = words.remove(0);
    let mut flags = Vec::new();
    let mut mask_flag = None;
    let mut args = Vec::new();
    let mut iter = words.into_iter();
    while let Some(word) = iter.next() {
        if word.starts_with('-') && word.len() > 1 && args.is_empty() {
            for flag in word[1..].chars() {
                flags.push(flag);
            }
            if word == "-m" {
                mask_flag = iter.next();
            }
        } else {
            args.push(word);
        }
    }
    let mut brush = create_brush(
        kind,
        &BrushArgs {
            args,
            flags,
            mask_flag,
        },
    )?;
    for line in lines {
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        match key {
            "size" => brush.size = value.parse().map_err(|_| "The brush file is broken.")?,
            "range" => brush.range = value.parse().ok(),
            "target" => brush.target_mode = value.parse().unwrap_or(1),
            "targetoffset" => brush.target_offset = value.parse().unwrap_or(0),
            "mat" => brush.pattern = Pattern::parse(value).map_err(|e| e.to_string())?,
            "mask" => {
                brush.mask = Some(Mask::parse(value).map_err(|e| e.to_string())?);
                brush.mask_text = Some(value.to_owned());
            }
            "smask" => {
                brush.smask = Some(Mask::parse(value).map_err(|e| e.to_string())?);
                brush.smask_text = Some(value.to_owned());
            }
            "targetmask" => {
                brush.target_mask = Some(Mask::parse(value).map_err(|e| e.to_string())?);
                brush.target_mask_text = Some(value.to_owned());
            }
            "transform" => brush.transform_text = Some(value.to_owned()),
            _ => {}
        }
    }
    Ok((brush, spec))
}

pub(super) fn execute_listbrush(ctx: CommandExecuteContext<'_>) {
    let scope = ctx.arguments[0].unwrap_string().to_lowercase();
    let page = ctx.arguments[1].unwrap_uint().max(1) as usize;
    let mut names: Vec<String> = Vec::new();
    let mut dirs = Vec::new();
    if scope != "global" {
        dirs.push(brush_dir(ctx.player, false));
    }
    if scope != "mine" {
        dirs.push(brush_dir(ctx.player, true));
    }
    for dir in dirs {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                if let Some(name) = entry.file_name().to_string_lossy().strip_suffix(".brush") {
                    names.push(name.to_owned());
                }
            }
        }
    }
    names.sort();
    names.dedup();
    if names.is_empty() {
        ctx.player.send_error_message("No brushes found.");
        return;
    }
    const PER_PAGE: usize = 10;
    let pages = names.len().div_ceil(PER_PAGE);
    if page > pages {
        ctx.player
            .send_error_message(&format!("There are only {pages} pages."));
        return;
    }
    ctx.player
        .send_worldedit_message(&format!("Brush list (page {page}/{pages}):"));
    for name in names.iter().skip((page - 1) * PER_PAGE).take(PER_PAGE) {
        ctx.player.send_worldedit_message(&format!("- {name}"));
    }
}

const TOOL_TYPES: &str =
    "tree, repl, info, lrbuild, inspect, farwand, floodfill, cycler, deltree, none";

fn tool_permission(name: &str) -> &'static str {
    match name {
        "repl" => "worldedit.tool.replacer",
        "info" => "worldedit.tool.info",
        "lrbuild" => "worldedit.tool.lrbuild",
        "inspect" => "worldedit.tool.inspect",
        "farwand" => "worldedit.tool.farwand",
        "floodfill" => "worldedit.tool.flood-fill",
        "cycler" => "worldedit.tool.data-cycler",
        "deltree" => "worldedit.tool.deltree",
        _ => "worldedit.tool.tree",
    }
}

pub(super) fn execute_tool(ctx: CommandExecuteContext<'_>) {
    let name = ctx.arguments[0].unwrap_string().to_lowercase();
    let args: Vec<&str> = ctx.arguments[1..]
        .iter()
        .map(|a| a.unwrap_string().as_str())
        .take_while(|a| !a.is_empty())
        .collect();
    let Some(item) = held_item(ctx.player) else {
        ctx.player
            .send_error_message("Hold the item you want to bind the tool to.");
        return;
    };
    let item_id = item.get_id();
    if matches!(name.as_str(), "none" | "unbind") {
        remove_binding(ctx.player, item_id);
        ctx.player
            .send_worldedit_message("Tool unbound from your current item.");
        return;
    }
    let name = match name.as_str() {
        "flood" => "floodfill".to_owned(),
        "replacer" => "repl".to_owned(),
        _ => name,
    };
    if name == "tree" {
        ctx.player
            .send_error_message("The tree tool is not available on this server.");
        return;
    }
    if !TOOL_TYPES.split(", ").any(|t| t == name) {
        ctx.player.send_error_message(&format!(
            "Unknown tool: {name}. Available tools: {TOOL_TYPES}."
        ));
        return;
    }
    if !ctx.player.has_permission(tool_permission(&name)) {
        ctx.player.send_no_permission_message();
        return;
    }
    if item == Item::WoodenAxe {
        ctx.player
            .send_error_message("The wooden axe is the selection wand, use another item.");
        return;
    }
    let pattern = |text: &str, usage: &str| pattern_of(text, usage);
    let tool = match name.as_str() {
        "repl" => pattern(args.first().copied().unwrap_or(""), "/tool repl <pattern>").map(|p| {
            Tool::Replacer {
                pattern: p,
                text: args[0].to_owned(),
            }
        }),
        "info" => Ok(Tool::Info),
        "inspect" => Ok(Tool::Inspect),
        "farwand" => Ok(Tool::FarWand),
        "cycler" => Ok(Tool::Cycler),
        "deltree" => Ok(Tool::DeleteTree),
        "lrbuild" => {
            let usage = "/tool lrbuild <leftclick block> <rightclick block>";
            pattern(args.first().copied().unwrap_or(""), usage).and_then(|primary| {
                pattern(args.get(1).copied().unwrap_or(""), usage)
                    .map(|secondary| Tool::LongRangeBuild { primary, secondary })
            })
        }
        "floodfill" => {
            let usage = "/tool floodfill <pattern> <range>";
            pattern(args.first().copied().unwrap_or(""), usage).and_then(|p| {
                number(args.get(1).copied().unwrap_or(""), 0, 1, 128, "range")
                    .map_err(|e| e.to_owned())
                    .and_then(|range| match args.get(1) {
                        Some(_) => Ok(Tool::FloodFill { pattern: p, range }),
                        None => Err(format!("Too few arguments. Usage: {usage}")),
                    })
            })
        }
        _ => unreachable!(),
    };
    match tool {
        Ok(tool) => {
            let message = format!("{} bound to {}.", tool.bound_name(), item.get_name());
            let player = &mut *ctx.player;
            player.worldedit_brushes.remove(&item_id);
            player.worldedit_brushes_primary.remove(&item_id);
            player.worldedit_brush_specs.remove(&item_id);
            player.worldedit_tools.insert(item_id, tool);
            player.send_worldedit_message(&message);
        }
        Err(message) => ctx.player.send_error_message(&message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args<'a>(args: &[&'a str], flags: &[char]) -> BrushArgs<'a> {
        BrushArgs {
            args: args.to_vec(),
            flags: flags.to_vec(),
            mask_flag: None,
        }
    }

    #[test]
    fn brush_defaults_follow_fawe() {
        let sphere = create_brush("sphere", &args(&["stone"], &[])).unwrap();
        assert_eq!(sphere.size, 2);
        let cyl = create_brush("cylinder", &args(&["stone"], &['h'])).unwrap();
        assert_eq!(cyl.size, 2);
        assert_eq!(
            cyl.kind,
            BrushKind::Cylinder {
                height: 1,
                hollow: true
            }
        );
        let smooth = create_brush("smooth", &args(&[], &['n'])).unwrap();
        assert_eq!(smooth.size, 2);
        assert_eq!(
            smooth.kind,
            BrushKind::Smooth {
                iterations: 4,
                natural_only: true
            }
        );
        assert_eq!(create_brush("gravity", &args(&[], &[])).unwrap().size, 5);
        assert_eq!(create_brush("extinguish", &args(&[], &[])).unwrap().size, 5);
    }

    #[test]
    fn brush_arguments_are_checked() {
        assert!(create_brush("sphere", &args(&[], &[])).is_err());
        assert!(create_brush("sphere", &args(&["stone", "9999"], &[])).is_err());
        assert!(create_brush("layer", &args(&["5"], &[])).is_err());
        assert!(create_brush("command", &args(&["5", "/set", "stone;/up", "2"], &[])).is_ok());
        assert_eq!(canonical("grav"), Some("gravity"));
        assert_eq!(canonical("ex"), Some("extinguish"));
        assert_eq!(canonical("splat"), Some("splatter"));
        assert_eq!(canonical("nonsense"), None);
    }

    #[test]
    fn saved_brushes_load_again() {
        let text = "brush sphere -h stone 4\nsize 6\nmask dirt\nrange 30\n";
        let (brush, spec) = load_brush(text).unwrap();
        assert_eq!(spec, "sphere -h stone 4");
        assert_eq!(brush.size, 6);
        assert_eq!(brush.range, Some(30));
        assert_eq!(brush.mask_text.as_deref(), Some("dirt"));
        assert!(matches!(brush.kind, BrushKind::Sphere { hollow: true, .. }));
    }
}
