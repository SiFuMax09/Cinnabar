//! [Worldedit](https://github.com/EngineHub/WorldEdit) and [RedstoneTools](https://github.com/paulikauro/RedstoneTools) implementation

#[cfg(test)]
mod bench;
mod brush;
mod brush_commands;
mod clipboard_cmds;
#[cfg(test)]
mod command_tests;
mod execute;
mod history;
mod job;
mod misc;
mod ops;
mod pattern;
mod schematic;
mod session;
mod shapes;
mod tool;
mod transform;
mod utility;

use super::commands::CommandFlags;
use super::{Plot, PlotWorld};
use crate::player::{PacketSender, Player, PlayerPos};
use brush_commands::*;
use cinnabar_blocks::block_entities::ContainerType;
use cinnabar_blocks::blocks::Block;
use cinnabar_blocks::{BlockFacing, BlockPos};
use cinnabar_network::packets::clientbound::{CCommandsNode, CDeclareCommandsNodeParser};
use cinnabar_utils::map;
use cinnabar_world::{for_each_block_mut_optimized, World};
use execute::*;
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::LazyLock;

pub use brush::{Brush, BrushKind};
pub use history::History;
pub use job::{EditJob, SLICE_BUDGET};
pub use misc::super_pickaxe_click;
pub use pattern::{Mask, Pattern, PatternParseError};
pub use tool::Tool;

// Attempts to execute a worldedit command. Returns true of the command was handled.
// The command is not handled if it is not found in the worldedit commands and alias lists.
pub fn execute_command(
    plot: &mut Plot,
    player_idx: usize,
    command: &str,
    args: &mut Vec<&str>,
) -> bool {
    let player = &mut plot.players[player_idx];
    let command = if let Some(command) = COMMANDS.get(command) {
        command
    } else if let Some(command) = ALIASES.get(command) {
        let mut alias: Vec<&str> = command.split(' ').collect();
        let command = alias.remove(0);
        args.append(&mut alias);
        &COMMANDS[command]
    } else {
        return false;
    };

    let wea = player.has_permission("plots.worldedit.bypass") || misc::is_bypassing(player.uuid);
    if !wea {
        if let Some(owner) = plot.owner {
            if owner != player.uuid {
                // tried to worldedit on plot that wasn't theirs
                player.send_no_permission_message();
                return true;
            }
        } else {
            // tried to worldedit on unclaimed plot
            player.send_no_permission_message();
            return true;
        }
    }

    if !command.permission_node.is_empty() && !player.has_permission(command.permission_node) {
        player.send_no_permission_message();
        return true;
    }

    if command.requires_positions {
        let plot_x = plot.world.x;
        let plot_z = plot.world.z;
        if player.first_position.is_none() || player.second_position.is_none() {
            player.send_error_message("Make a region selection first.");
            return true;
        }
        let first_pos = player.first_position.unwrap();
        let second_pos = player.second_position.unwrap();
        if !Plot::in_plot_bounds(plot_x, plot_z, first_pos.x, first_pos.z) {
            player.send_system_message("First position is outside plot bounds!");
            return true;
        }
        if !Plot::in_plot_bounds(plot_x, plot_z, second_pos.x, second_pos.z) {
            player.send_system_message("Second position is outside plot bounds!");
            return true;
        }
    }

    if command.requires_clipboard && player.worldedit_clipboard.is_none() {
        player.send_error_message("Your clipboard is empty. Use //copy first.");
        return true;
    }

    let flag_descs = command.flags;

    let mut ctx_flags = Vec::new();
    let mut ctx_flag_args: Vec<(char, String)> = Vec::new();

    let total_arg_count = args.len();
    let mut arg_iter = args.iter().copied().enumerate();
    let mut args = Vec::new();

    while let Some((i, arg)) = arg_iter.next() {
        // `-5` is a number, not a flag
        if !command.raw_args
            && arg.starts_with('-')
            && arg[1..].starts_with(|c: char| c.is_ascii_alphabetic())
        {
            let mut with_argument = false;
            let flags = arg.chars();
            for flag in flags.skip(1) {
                if with_argument {
                    player.send_error_message("Flag with argument must be last in grouping");
                    return true;
                }
                let flag_desc = if let Some(desc) = flag_descs.iter().find(|d| d.letter == flag) {
                    desc
                } else {
                    player.send_error_message(&format!("Unknown flag: {}", flag));
                    return true;
                };
                if let Some(argument_type) = flag_desc.argument_type {
                    if (i + 1) >= total_arg_count {
                        player.send_error_message(&format!(
                            "Missing {:?} argument for {} flag",
                            argument_type, flag
                        ));
                        return true;
                    }
                    if let Some((_, value)) = arg_iter.next() {
                        ctx_flag_args.push((flag, value.to_owned()));
                    }
                    with_argument = true;
                }
                ctx_flags.push(flag);
            }
        } else {
            args.push(arg);
        }
    }

    let arg_descs = command.arguments;

    // `//replace <to>` replaces everything that is not air
    if std::ptr::eq(command, &COMMANDS["/replace"]) && args.len() == 1 {
        args.insert(0, "#existing");
    }
    // `replacenear <size> <to>` replaces everything that is not air
    if std::ptr::eq(command, &COMMANDS["replacenear"]) && args.len() == 2 {
        args.insert(1, "#existing");
    }

    if args.len() > arg_descs.len() {
        player.send_error_message("Too many arguments.");
        return true;
    }

    let mut arguments = Vec::new();
    for (i, arg_desc) in arg_descs.iter().enumerate() {
        let arg = args.get(i).copied();
        match Argument::parse(player, arg_desc, arg) {
            Ok(default_arg) => arguments.push(default_arg),
            Err(err) => {
                player.send_error_message(&err.to_string());
                return true;
            }
        }
    }
    if let Some(plot_fn) = command.plot_fn {
        plot_fn(plot, player_idx, &arguments, &ctx_flags);
        plot.run_edit_jobs(SLICE_BUDGET);
        return true;
    }
    if command.reads_world && !plot.edit_jobs.is_empty() {
        // The command looks at the world, so the queued edits have to be done
        plot.finish_edit_jobs();
    }
    if command.mutates_world {
        plot.reset_redpiler();
    }
    let ctx = CommandExecuteContext {
        plot: &mut plot.world,
        player: &mut plot.players[player_idx],
        arguments,
        flags: ctx_flags,
        flag_args: ctx_flag_args,
        jobs: &mut plot.edit_jobs,
    };
    (command.execute_fn)(ctx);
    // Run what fits in the time budget right away, the rest follows in the next updates
    plot.run_edit_jobs(SLICE_BUDGET);
    if command.mutates_world {
        plot.publish_world();
    }
    true
}

#[derive(Debug)]
struct ArgumentParseError {
    arg_type: ArgumentType,
    reason: String,
}

impl ArgumentParseError {
    fn new(arg_type: ArgumentType, reason: &str) -> ArgumentParseError {
        ArgumentParseError {
            arg_type,
            reason: String::from(reason),
        }
    }
}

impl fmt::Display for ArgumentParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Error parsing argument of type {:?}: {}",
            self.arg_type, self.reason
        )
    }
}

impl std::error::Error for ArgumentParseError {}

type ArgumentParseResult = Result<Argument, ArgumentParseError>;

#[derive(Copy, Clone, Debug)]
enum ArgumentType {
    UnsignedInteger,
    Integer,
    Direction,
    /// Used for diag directions in redstonetools commands
    DirectionVector,
    Mask,
    Pattern,
    String,
    ContainerType,
}

#[derive(Debug, Clone)]
enum Argument {
    UnsignedInteger(u32),
    Integer(i32),
    Direction(BlockFacing),
    DirectionVector(BlockPos),
    Pattern(Pattern),
    Mask(Mask),
    String(String),
    ContainerType(ContainerType),
}

impl Argument {
    fn unwrap_uint(&self) -> u32 {
        match self {
            Argument::UnsignedInteger(val) => *val,
            _ => panic!("Argument was not an UnsignedInteger"),
        }
    }

    fn unwrap_int(&self) -> i32 {
        match self {
            Argument::Integer(val) => *val,
            _ => panic!("Argument was not an Integer"),
        }
    }

    fn unwrap_direction(&self) -> BlockFacing {
        match self {
            Argument::Direction(val) => *val,
            _ => panic!("Argument was not an Direction"),
        }
    }

    fn unwrap_direction_vec(&self) -> BlockPos {
        match self {
            Argument::DirectionVector(val) => *val,
            _ => panic!("Argument was not an DirectionVector"),
        }
    }

    fn unwrap_pattern(&self) -> &Pattern {
        match self {
            Argument::Pattern(val) => val,
            _ => panic!("Argument was not a Pattern"),
        }
    }

    fn unwrap_mask(&self) -> &Mask {
        match self {
            Argument::Mask(val) => val,
            _ => panic!("Argument was not a Mask"),
        }
    }

    fn unwrap_string(&self) -> &String {
        match self {
            Argument::String(val) => val,
            _ => panic!("Argument was not a String"),
        }
    }

    fn unwrap_container_type(&self) -> ContainerType {
        match self {
            Argument::ContainerType(val) => *val,
            _ => panic!("Container type must be one of [barrel, furnace, hopper]"),
        }
    }

    fn get_default(player: &Player, desc: &ArgumentDescription) -> ArgumentParseResult {
        if let Some(default) = &desc.default {
            return Ok(default.clone());
        }
        if let Some(default) = desc.default_str {
            return Argument::parse(player, desc, Some(default));
        }

        let arg_type = desc.argument_type;
        match arg_type {
            ArgumentType::Direction | ArgumentType::DirectionVector => {
                Argument::parse(player, desc, Some("me"))
            }
            ArgumentType::UnsignedInteger => Ok(Argument::UnsignedInteger(1)),
            _ => Err(ArgumentParseError::new(
                arg_type,
                "argument can't be inferred",
            )),
        }
    }

    fn parse(
        player: &Player,
        desc: &ArgumentDescription,
        arg: Option<&str>,
    ) -> ArgumentParseResult {
        pattern::set_parse_context(pattern::ParseContext {
            origin: Some(player.pos.block_pos()),
            region: player.first_position.zip(player.second_position),
        });
        if arg.is_none() {
            return Argument::get_default(player, desc);
        }
        let arg = arg.unwrap();
        let arg_type = desc.argument_type;
        match arg_type {
            ArgumentType::Direction => {
                let player_facing = player.get_facing();
                Ok(Argument::Direction(
                    match parse_facing_name(arg, player_facing) {
                        Some(facing) => facing,
                        None => return Err(ArgumentParseError::new(arg_type, "unknown direction")),
                    },
                ))
            }
            ArgumentType::UnsignedInteger => match arg.parse::<u32>() {
                Ok(num) => Ok(Argument::UnsignedInteger(num)),
                Err(_) => Err(ArgumentParseError::new(arg_type, "error parsing uint")),
            },
            ArgumentType::Integer => match arg.parse::<i32>() {
                Ok(num) => Ok(Argument::Integer(num)),
                Err(_) => Err(ArgumentParseError::new(arg_type, "error parsing integer")),
            },
            ArgumentType::Pattern => match Pattern::parse(arg) {
                Ok(pattern) => Ok(Argument::Pattern(pattern)),
                Err(err) => Err(ArgumentParseError::new(arg_type, &err.to_string())),
            },
            ArgumentType::Mask => match Mask::parse(arg) {
                Ok(mask) => Ok(Argument::Mask(mask)),
                Err(err) => Err(ArgumentParseError::new(arg_type, &err.to_string())),
            },
            ArgumentType::String => Ok(Argument::String(arg.to_owned())),
            ArgumentType::DirectionVector => {
                let mut vec = BlockPos::new(0, 0, 0);
                let player_facing = player.get_facing();
                if arg == "me" {
                    vec = player_facing.offset_pos(vec, 1);
                    if !matches!(player_facing, BlockFacing::Down | BlockFacing::Up) {
                        let pitch = player.pitch;
                        if pitch > 22.5 {
                            vec.y -= 1;
                        } else if pitch < -22.5 {
                            vec.y += 1;
                        }
                    }
                    return Ok(Argument::DirectionVector(vec));
                }

                // `x,y,z` is an explicit offset
                let parts: Vec<&str> = arg.split(',').collect();
                if parts.len() == 3 {
                    let parsed: Vec<Option<i32>> =
                        parts.iter().map(|part| part.trim().parse().ok()).collect();
                    if let [Some(x), Some(y), Some(z)] = parsed[..] {
                        return Ok(Argument::DirectionVector(BlockPos::new(x, y, z)));
                    }
                    return Err(ArgumentParseError::new(arg_type, "unknown direction"));
                }

                let mut base_dir = arg;
                if parse_facing_name(arg, player_facing).is_some() {
                    // A whole name like `forward` or `down`, not a name with a suffix
                    let facing = parse_facing_name(arg, player_facing).unwrap();
                    return Ok(Argument::DirectionVector(facing.offset_pos(vec, 1)));
                }
                if arg.len() > 1 && matches!(arg.chars().last(), Some('u' | 'd')) {
                    match arg.chars().last().unwrap() {
                        'u' => vec.y += 1,
                        'd' => vec.y -= 1,
                        _ => unreachable!(),
                    }
                    base_dir = &arg[..1];
                }

                let facing = match parse_facing_name(base_dir, player_facing) {
                    Some(facing) => facing,
                    None => return Err(ArgumentParseError::new(arg_type, "unknown direction")),
                };
                let vec = facing.offset_pos(vec, 1);

                Ok(Argument::DirectionVector(vec))
            }
            ArgumentType::ContainerType => match arg.parse::<ContainerType>() {
                Ok(ty) => Ok(Argument::ContainerType(ty)),
                Err(_) => Err(ArgumentParseError::new(
                    arg_type,
                    "error parsing container type",
                )),
            },
        }
    }
}

/// The direction a name stands for, like WorldEdit's direction argument: `me`, `u`/`up`,
/// `d`/`down`, `l`/`left`, `r`/`right`, `f`/`forward`, `b`/`back` (relative to where the player
/// looks) and the compass directions `n`, `e`, `s`, `w` or their full names.
fn parse_facing_name(name: &str, player_facing: BlockFacing) -> Option<BlockFacing> {
    let opposite = |facing: BlockFacing| match facing {
        BlockFacing::Up => BlockFacing::Down,
        BlockFacing::Down => BlockFacing::Up,
        other => other.rotate().rotate(),
    };
    Some(match name.to_ascii_lowercase().as_str() {
        "me" | "f" | "forward" => player_facing,
        "b" | "back" => opposite(player_facing),
        "u" | "up" => BlockFacing::Up,
        "d" | "down" => BlockFacing::Down,
        "l" | "left" => player_facing.rotate_ccw(),
        "r" | "right" => player_facing.rotate(),
        "n" | "north" => BlockFacing::North,
        "e" | "east" => BlockFacing::East,
        "s" | "south" => BlockFacing::South,
        "w" | "west" => BlockFacing::West,
        _ => return None,
    })
}

struct ArgumentDescription {
    name: &'static str,
    argument_type: ArgumentType,
    description: &'static str,
    default: Option<Argument>,
    /// The default as the player would write it
    default_str: Option<&'static str>,
}

macro_rules! argument {
    ($name:literal, $type:ident, $desc:literal) => {
        ArgumentDescription {
            name: $name,
            argument_type: ArgumentType::$type,
            description: $desc,
            default: None,
            default_str: None,
        }
    };
    ($name:literal, $type:ident, $desc:literal, $default:literal) => {
        ArgumentDescription {
            name: $name,
            argument_type: ArgumentType::$type,
            description: $desc,
            default: Some(Argument::$type($default)),
            default_str: None,
        }
    };
}

macro_rules! argument_str {
    ($name:literal, $type:ident, $desc:literal, $default:literal) => {
        ArgumentDescription {
            name: $name,
            argument_type: ArgumentType::$type,
            description: $desc,
            default: None,
            default_str: Some($default),
        }
    };
}

struct FlagDescription {
    letter: char,
    argument_type: Option<ArgumentType>,
    description: &'static str,
}

macro_rules! flag {
    ($name:literal, $type:expr, $desc:literal) => {
        FlagDescription {
            letter: $name,
            argument_type: $type,
            description: $desc,
        }
    };
}

struct CommandExecuteContext<'a> {
    plot: &'a mut PlotWorld,
    player: &'a mut Player,
    arguments: Vec<Argument>,
    flags: Vec<char>,
    /// The values of the flags that take an argument, like the mask of `-m`
    flag_args: Vec<(char, String)>,
    jobs: &'a mut VecDeque<EditJob>,
}

impl CommandExecuteContext<'_> {
    fn has_flag(&self, c: char) -> bool {
        self.flags.contains(&c)
    }

    fn flag_arg(&self, c: char) -> Option<&str> {
        self.flag_args
            .iter()
            .find(|(flag, _)| *flag == c)
            .map(|(_, value)| value.as_str())
    }
}

struct WorldeditCommand {
    arguments: &'static [ArgumentDescription],
    flags: &'static [FlagDescription],
    requires_positions: bool,
    requires_clipboard: bool,
    /// Hands every argument, flags included, to the command as a plain string
    raw_args: bool,
    execute_fn: fn(CommandExecuteContext<'_>),
    description: &'static str,
    permission_node: &'static str,
    mutates_world: bool,
    /// Whether the command looks at the blocks of the world when it is run. Then the edits that
    /// are still queued have to be finished first.
    reads_world: bool,
    normal_completions: bool,
    /// Commands that need the whole plot (entities, queued edits) instead of the world
    plot_fn: Option<fn(&mut Plot, usize, &[Argument], &[char])>,
}

impl Default for WorldeditCommand {
    fn default() -> Self {
        Self {
            arguments: &[],
            flags: &[],
            execute_fn: execute_unimplemented,
            description: "",
            requires_clipboard: false,
            raw_args: false,
            requires_positions: false,
            permission_node: "",
            mutates_world: true,
            reads_world: false,
            normal_completions: true,
            plot_fn: None,
        }
    }
}

static COMMANDS: LazyLock<HashMap<&'static str, WorldeditCommand>> = LazyLock::new(|| {
    let mut commands = map! {
        "/undo" => WorldeditCommand {
            arguments: &[
                argument!("times", UnsignedInteger, "Number of edits to undo"),
                argument_str!("player", String, "The player whose history to undo", "")
            ],
            execute_fn: utility::execute_undo,
            description: "Undoes the last actions (from history)",
            permission_node: "worldedit.history.undo",
            ..Default::default()
        },
        "/clearhistory" => WorldeditCommand {
            arguments: &[],
            execute_fn: utility::execute_clear_history,
            description: "Clear your history",
            permission_node: "worldedit.history.clear",
            mutates_world: false,
            ..Default::default()
        },
        "/fast" => WorldeditCommand {
            arguments: &[argument_str!("mode", String, "on or off, toggles when left out", "")],
            execute_fn: utility::execute_fast,
            description: "Toggle fast mode",
            permission_node: "worldedit.fast",
            mutates_world: false,
            ..Default::default()
        },
        "/toggleplace" => WorldeditCommand {
            arguments: &[],
            execute_fn: utility::execute_toggle_place,
            description: "Switch between your position and pos1 for placement",
            permission_node: "worldedit.toggleplace",
            mutates_world: false,
            ..Default::default()
        },
        "/tips" => WorldeditCommand {
            arguments: &[argument_str!("mode", String, "on or off, toggles when left out", "")],
            execute_fn: utility::execute_tips,
            description: "Toggle FAWE tips",
            permission_node: "worldedit.tips",
            mutates_world: false,
            ..Default::default()
        },
        "/gsmask" => WorldeditCommand {
            arguments: &[argument_str!("mask", String, "The mask, none removes it", "none")],
            execute_fn: utility::execute_gsmask,
            description: "Set the global source mask",
            permission_node: "worldedit.global-mask",
            mutates_world: false,
            ..Default::default()
        },
        "/masks" => WorldeditCommand {
            arguments: &[argument!("page", UnsignedInteger, "The page to show")],
            execute_fn: utility::execute_masks,
            description: "Mask help",
            permission_node: "worldedit.help",
            mutates_world: false,
            ..Default::default()
        },
        "/patterns" => WorldeditCommand {
            arguments: &[argument!("page", UnsignedInteger, "The page to show")],
            execute_fn: utility::execute_patterns,
            description: "Pattern help",
            permission_node: "worldedit.help",
            mutates_world: false,
            ..Default::default()
        },
        "/transforms" => WorldeditCommand {
            arguments: &[argument!("page", UnsignedInteger, "The page to show")],
            execute_fn: utility::execute_transforms,
            description: "Transform help",
            permission_node: "worldedit.help",
            mutates_world: false,
            ..Default::default()
        },
        "/drain" => WorldeditCommand {
            arguments: &[argument!("radius", UnsignedInteger, "The radius of the area", 10)],
            execute_fn: utility::execute_drain,
            description: "Drain water and lava pools around you",
            permission_node: "worldedit.drain",
            ..Default::default()
        },
        "/fixlava" => WorldeditCommand {
            arguments: &[argument!("radius", UnsignedInteger, "The radius of the area", 10)],
            execute_fn: utility::execute_fix_lava,
            description: "Fix lava to be still",
            permission_node: "worldedit.fixlava",
            ..Default::default()
        },
        "/fixwater" => WorldeditCommand {
            arguments: &[argument!("radius", UnsignedInteger, "The radius of the area", 10)],
            execute_fn: utility::execute_fix_water,
            description: "Fix water to be still",
            permission_node: "worldedit.fixwater",
            ..Default::default()
        },
        "/ex" => WorldeditCommand {
            arguments: &[argument!("radius", UnsignedInteger, "The radius of the area", 40)],
            execute_fn: utility::execute_extinguish,
            description: "Extinguish nearby fire",
            permission_node: "worldedit.extinguish",
            ..Default::default()
        },
        "/snow" => WorldeditCommand {
            arguments: &[argument!("radius", UnsignedInteger, "The radius of the area", 10)],
            execute_fn: utility::execute_snow,
            description: "Simulate snow",
            permission_node: "worldedit.snow",
            ..Default::default()
        },
        "/thaw" => WorldeditCommand {
            arguments: &[argument!("radius", UnsignedInteger, "The radius of the area", 10)],
            execute_fn: utility::execute_thaw,
            description: "Thaw the area",
            permission_node: "worldedit.thaw",
            ..Default::default()
        },
        "/green" => WorldeditCommand {
            arguments: &[argument!("radius", UnsignedInteger, "The radius of the area", 10)],
            execute_fn: utility::execute_green,
            description: "Turn dirt into grass",
            permission_node: "worldedit.green",
            flags: &[flag!('f', None, "Also convert coarse dirt")],
            ..Default::default()
        },
        "/redo" => WorldeditCommand {
            arguments: &[
                argument!("times", UnsignedInteger, "Number of edits to redo"),
                argument_str!("player", String, "The player whose history to redo", "")
            ],
            execute_fn: utility::execute_redo,
            description: "Redoes the last undone actions (from history)",
            permission_node: "worldedit.history.redo",
            ..Default::default()
        },
        "/move" => WorldeditCommand {
            arguments: &[
                argument!("count", Integer, "# of blocks to move", 1),
                argument!("direction", DirectionVector, "The direction to move"),
                argument_str!("replace", Pattern, "The pattern of blocks to leave", "air")
            ],
            requires_positions: true,
            reads_world: true,
            execute_fn: execute_move,
            description: "Move the contents of the selection",
            flags: &[
                flag!('s', None, "Shift the selection to the target location"),
                flag!('a', None, "Ignore air blocks"),
                flag!('e', None, "Also copy entities"),
                flag!('b', None, "Also copy biomes"),
                flag!('m', Some(ArgumentType::Mask), "Set the include mask, non-matching blocks become air")
            ],
            permission_node: "worldedit.region.move",
            ..Default::default()
        },
        "/rotate" => WorldeditCommand {
            arguments: &[
                argument!("rotateY", Integer, "Degrees to rotate around the y-axis", 0),
                argument!("rotateX", Integer, "Degrees to rotate around the x-axis", 0),
                argument!("rotateZ", Integer, "Degrees to rotate around the z-axis", 0),
            ],
            requires_clipboard: true,
            execute_fn: execute_rotate,
            description: "Rotate the contents of the clipboard in steps of 90 degrees",
            mutates_world: false,
            ..Default::default()
        },
        "replacenear" => WorldeditCommand {
            arguments: &[
                argument!("radius", UnsignedInteger, "The radius of the square to remove in"),
                argument!("from", Mask, "The mask matching blocks to remove"),
                argument!("to", Pattern, "The pattern of blocks to replace with")
            ],
            execute_fn: execute_replace_near,
            description: "Replace nearby blocks",
            permission_node: "worldedit.replacenear",
            ..Default::default()
        },
        "/sphere" => WorldeditCommand {
            arguments: &[
                argument!("pattern", Pattern, "The pattern of blocks to place"),
                argument!("radius", String, "The radius of the sphere, or x,y,z for an ellipsoid"),
                argument_str!("raised", String, "Whether to raise the sphere so it rests on the ground", "")
            ],
            flags: &[flag!('h', None, "Create a hollow sphere")],
            execute_fn: execute_sphere,
            description: "Generate a filled sphere",
            permission_node: "worldedit.generation.sphere",
            ..Default::default()
        },
        "/hsphere" => WorldeditCommand {
            arguments: &[
                argument!("pattern", Pattern, "The pattern of blocks to place"),
                argument!("radius", String, "The radius of the sphere, or x,y,z for an ellipsoid"),
                argument_str!("raised", String, "Whether to raise the sphere so it rests on the ground", "")
            ],
            flags: &[flag!('h', None, "Create a hollow sphere")],
            execute_fn: execute_hsphere,
            description: "Generate a hollow sphere",
            permission_node: "worldedit.generation.sphere",
            ..Default::default()
        },
        "/cyl" => WorldeditCommand {
            arguments: &[
                argument!("pattern", Pattern, "The pattern of blocks to place"),
                argument!("radius", String, "The radius of the cylinder, or x,z for an ellipse"),
                argument!("height", UnsignedInteger, "The height of the cylinder", 1)
            ],
            flags: &[flag!('h', None, "Create a hollow cylinder")],
            execute_fn: execute_cyl,
            description: "Generate a filled cylinder that goes up from your feet",
            permission_node: "worldedit.generation.cylinder",
            ..Default::default()
        },
        "/hcyl" => WorldeditCommand {
            arguments: &[
                argument!("pattern", Pattern, "The pattern of blocks to place"),
                argument!("radius", String, "The radius of the cylinder, or x,z for an ellipse"),
                argument!("height", UnsignedInteger, "The height of the cylinder", 1)
            ],
            flags: &[flag!('h', None, "Create a hollow cylinder")],
            execute_fn: execute_hcyl,
            description: "Generate a hollow cylinder that goes up from your feet",
            permission_node: "worldedit.generation.cylinder",
            ..Default::default()
        },
        "/pyramid" => WorldeditCommand {
            arguments: &[
                argument!("pattern", Pattern, "The pattern of blocks to place"),
                argument!("size", UnsignedInteger, "The size of the pyramid")
            ],
            flags: &[flag!('h', None, "Make a hollow pyramid")],
            execute_fn: execute_pyramid,
            description: "Generate a filled pyramid",
            permission_node: "worldedit.generation.pyramid",
            ..Default::default()
        },
        "/hpyramid" => WorldeditCommand {
            arguments: &[
                argument!("pattern", Pattern, "The pattern of blocks to place"),
                argument!("size", UnsignedInteger, "The size of the pyramid")
            ],
            flags: &[flag!('h', None, "Make a hollow pyramid")],
            execute_fn: execute_hpyramid,
            description: "Generate a hollow pyramid",
            permission_node: "worldedit.generation.pyramid",
            ..Default::default()
        },
        "/lay" => WorldeditCommand {
            arguments: &[
                argument!("pattern", Pattern, "The pattern of blocks to place")
            ],
            requires_positions: true,
            execute_fn: execute_lay,
            description: "Set the top block in the region",
            permission_node: "worldedit.region.lay",
            ..Default::default()
        },
        "/naturalize" => WorldeditCommand {
            requires_positions: true,
            execute_fn: execute_naturalize,
            description: "3 layers of dirt on top then rock below",
            permission_node: "worldedit.region.naturalize",
            ..Default::default()
        },
        "/walls" => WorldeditCommand {
            arguments: &[
                argument!("pattern", Pattern, "The pattern of blocks to place")
            ],
            requires_positions: true,
            execute_fn: execute_walls,
            description: "Build the four sides of the selection",
            permission_node: "worldedit.region.walls",
            ..Default::default()
        },
        "/faces" => WorldeditCommand {
            arguments: &[
                argument!("pattern", Pattern, "The pattern of blocks to place")
            ],
            requires_positions: true,
            execute_fn: execute_faces,
            description: "Build the walls, ceiling and floor of the selection",
            permission_node: "worldedit.region.faces",
            ..Default::default()
        },
        "/overlay" => WorldeditCommand {
            arguments: &[
                argument!("pattern", Pattern, "The pattern of blocks to place")
            ],
            requires_positions: true,
            execute_fn: execute_overlay,
            description: "Set a block on top of the blocks in the selection",
            permission_node: "worldedit.region.overlay",
            ..Default::default()
        },
        "/hollow" => WorldeditCommand {
            arguments: &[
                argument!("thickness", UnsignedInteger, "The thickness of the walls", 0),
                argument_str!("pattern", Pattern, "The pattern of blocks to fill the inside with", "air")
            ],
            requires_positions: true,
            execute_fn: execute_hollow,
            description: "Hollow out the objects in the selection",
            permission_node: "worldedit.region.hollow",
            ..Default::default()
        },
        "/smooth" => WorldeditCommand {
            arguments: &[
                argument!("iterations", UnsignedInteger, "The number of smoothing passes", 1),
                argument_str!("mask", String, "The mask of blocks to consider as the surface", "")
            ],
            requires_positions: true,
            reads_world: true,
            execute_fn: execute_smooth,
            description: "Smooth the elevation of the terrain in the selection",
            permission_node: "worldedit.region.smooth",
            ..Default::default()
        },
        "/line" => WorldeditCommand {
            arguments: &[
                argument!("pattern", Pattern, "The pattern of blocks to place"),
                argument!("thickness", UnsignedInteger, "The thickness of the line", 0)
            ],
            flags: &[flag!('h', None, "Generate only a shell")],
            requires_positions: true,
            execute_fn: execute_line,
            description: "Draw a line between the two positions",
            permission_node: "worldedit.generation.line",
            ..Default::default()
        },
        "/center" => WorldeditCommand {
            arguments: &[
                argument!("pattern", Pattern, "The pattern of blocks to place")
            ],
            requires_positions: true,
            execute_fn: execute_center,
            description: "Set the center block(s) of the selection",
            permission_node: "worldedit.region.center",
            ..Default::default()
        },
        "/fill" => WorldeditCommand {
            arguments: &[
                argument!("pattern", Pattern, "The pattern of blocks to fill with"),
                argument!("radius", UnsignedInteger, "The radius to fill in"),
                argument!("depth", UnsignedInteger, "The depth to fill")
            ],
            reads_world: true,
            execute_fn: execute_fill,
            description: "Fill a hole",
            permission_node: "worldedit.fill",
            ..Default::default()
        },
        "/fillr" => WorldeditCommand {
            arguments: &[
                argument!("pattern", Pattern, "The pattern of blocks to fill with"),
                argument!("radius", UnsignedInteger, "The radius to fill in"),
                argument!("depth", UnsignedInteger, "The depth to fill", 384)
            ],
            reads_world: true,
            execute_fn: execute_fillr,
            description: "Fill a hole recursively, all the way down",
            permission_node: "worldedit.fill.recursive",
            ..Default::default()
        },
        "/fall" => WorldeditCommand {
            arguments: &[
                argument_str!("replace", Pattern, "BlockStateHolder", "air")
            ],
            flags: &[flag!('m', None, "Only fall within the vertical selection")],
            requires_positions: true,
            reads_world: true,
            execute_fn: execute_fall,
            description: "Have the blocks in the selection fall",
            permission_node: "worldedit.region.fall",
            ..Default::default()
        },
        "/chunk" => WorldeditCommand {
            execute_fn: execute_chunk,
            description: "Select the chunk you are in",
            permission_node: "worldedit.selection.chunk",
            mutates_world: false,
            ..Default::default()
        },
        "brush" => WorldeditCommand {
            arguments: &[
                argument!("type", String, "The brush: sphere, cylinder, circle, clipboard, smooth, gravity, ex, splatter, scatter, layer, surface, recursive, erode, line, catenary, command, scmd or none"),
                argument_str!("arg1", String, "Arguments of the brush", ""),
                argument_str!("arg2", String, "Arguments of the brush", ""),
                argument_str!("arg3", String, "Arguments of the brush", ""),
                argument_str!("arg4", String, "Arguments of the brush", ""),
                argument_str!("arg5", String, "Arguments of the brush", ""),
                argument_str!("arg6", String, "Arguments of the brush", ""),
                argument_str!("arg7", String, "Arguments of the brush", ""),
                argument_str!("arg8", String, "Arguments of the brush", ""),
                argument_str!("arg9", String, "Arguments of the brush", ""),
                argument_str!("arg10", String, "Arguments of the brush", ""),
                argument_str!("arg11", String, "Arguments of the brush", ""),
                argument_str!("arg12", String, "Arguments of the brush", "")
            ],
            flags: &[
                flag!('a', None, "Skip air blocks (clipboard)"),
                flag!('b', None, "Include biomes (clipboard)"),
                flag!('d', None, "Depth-first order (recursive)"),
                flag!('e', None, "Include entities (clipboard)"),
                flag!('f', None, "Falling sphere, flat line"),
                flag!('g', None, "Butcher: golems"),
                flag!('h', None, "Hollow, shell only"),
                flag!('l', None, "Snow layers"),
                flag!('m', Some(ArgumentType::String), "Skip clipboard blocks that match the mask"),
                flag!('n', None, "Only naturally occurring blocks (smooth)"),
                flag!('o', None, "Original position (clipboard), overlay (scatter)"),
                flag!('p', None, "Butcher: pets"),
                flag!('r', None, "Random rotation"),
                flag!('s', None, "Select the clicked point (line, catenary)"),
                flag!('t', None, "Butcher: name-tagged mobs"),
                flag!('w', None, "Max saturation only")
            ],
            execute_fn: execute_brush,
            description: "Bind a brush to the item in your hand (left and right click)",
            permission_node: "",
            mutates_world: false,
            ..Default::default()
        },
        "primary" => WorldeditCommand {
            arguments: &[
                argument!("type", String, "The brush: sphere, cylinder, circle, clipboard, smooth, gravity, ex, splatter, scatter, layer, surface, recursive, erode, line, catenary, command, scmd or none"),
                argument_str!("arg1", String, "Arguments of the brush", ""),
                argument_str!("arg2", String, "Arguments of the brush", ""),
                argument_str!("arg3", String, "Arguments of the brush", ""),
                argument_str!("arg4", String, "Arguments of the brush", ""),
                argument_str!("arg5", String, "Arguments of the brush", ""),
                argument_str!("arg6", String, "Arguments of the brush", ""),
                argument_str!("arg7", String, "Arguments of the brush", ""),
                argument_str!("arg8", String, "Arguments of the brush", ""),
                argument_str!("arg9", String, "Arguments of the brush", ""),
                argument_str!("arg10", String, "Arguments of the brush", ""),
                argument_str!("arg11", String, "Arguments of the brush", ""),
                argument_str!("arg12", String, "Arguments of the brush", "")
            ],
            flags: &[
                flag!('a', None, "Skip air blocks (clipboard)"),
                flag!('b', None, "Include biomes (clipboard)"),
                flag!('d', None, "Depth-first order (recursive)"),
                flag!('e', None, "Include entities (clipboard)"),
                flag!('f', None, "Falling sphere, flat line"),
                flag!('g', None, "Butcher: golems"),
                flag!('h', None, "Hollow, shell only"),
                flag!('l', None, "Snow layers"),
                flag!('m', Some(ArgumentType::String), "Skip clipboard blocks that match the mask"),
                flag!('n', None, "Only naturally occurring blocks (smooth)"),
                flag!('o', None, "Original position (clipboard), overlay (scatter)"),
                flag!('p', None, "Butcher: pets"),
                flag!('r', None, "Random rotation"),
                flag!('s', None, "Select the clicked point (line, catenary)"),
                flag!('t', None, "Butcher: name-tagged mobs"),
                flag!('w', None, "Max saturation only")
            ],
            execute_fn: execute_primary,
            description: "Bind a brush to the left click of the item in your hand",
            permission_node: "worldedit.brush.primary",
            mutates_world: false,
            ..Default::default()
        },
        "secondary" => WorldeditCommand {
            arguments: &[
                argument!("type", String, "The brush: sphere, cylinder, circle, clipboard, smooth, gravity, ex, splatter, scatter, layer, surface, recursive, erode, line, catenary, command, scmd or none"),
                argument_str!("arg1", String, "Arguments of the brush", ""),
                argument_str!("arg2", String, "Arguments of the brush", ""),
                argument_str!("arg3", String, "Arguments of the brush", ""),
                argument_str!("arg4", String, "Arguments of the brush", ""),
                argument_str!("arg5", String, "Arguments of the brush", ""),
                argument_str!("arg6", String, "Arguments of the brush", ""),
                argument_str!("arg7", String, "Arguments of the brush", ""),
                argument_str!("arg8", String, "Arguments of the brush", ""),
                argument_str!("arg9", String, "Arguments of the brush", ""),
                argument_str!("arg10", String, "Arguments of the brush", ""),
                argument_str!("arg11", String, "Arguments of the brush", ""),
                argument_str!("arg12", String, "Arguments of the brush", "")
            ],
            flags: &[
                flag!('a', None, "Skip air blocks (clipboard)"),
                flag!('b', None, "Include biomes (clipboard)"),
                flag!('d', None, "Depth-first order (recursive)"),
                flag!('e', None, "Include entities (clipboard)"),
                flag!('f', None, "Falling sphere, flat line"),
                flag!('g', None, "Butcher: golems"),
                flag!('h', None, "Hollow, shell only"),
                flag!('l', None, "Snow layers"),
                flag!('m', Some(ArgumentType::String), "Skip clipboard blocks that match the mask"),
                flag!('n', None, "Only naturally occurring blocks (smooth)"),
                flag!('o', None, "Original position (clipboard), overlay (scatter)"),
                flag!('p', None, "Butcher: pets"),
                flag!('r', None, "Random rotation"),
                flag!('s', None, "Select the clicked point (line, catenary)"),
                flag!('t', None, "Butcher: name-tagged mobs"),
                flag!('w', None, "Max saturation only")
            ],
            execute_fn: execute_secondary,
            description: "Bind a brush to the right click of the item in your hand",
            permission_node: "worldedit.brush.secondary",
            mutates_world: false,
            ..Default::default()
        },
        "size" => WorldeditCommand {
            arguments: &[
                argument!("size", UnsignedInteger, "The new size of the brush in your hand")
            ],
            execute_fn: execute_size,
            description: "Set the size of the brush in your hand",
            permission_node: "worldedit.brush.options.size",
            mutates_world: false,
            ..Default::default()
        },
        "range" => WorldeditCommand {
            arguments: &[
                argument!("range", UnsignedInteger, "How far away the brush can be used")
            ],
            execute_fn: execute_range,
            description: "Set the range of the brush in your hand",
            permission_node: "worldedit.brush.options.range",
            mutates_world: false,
            ..Default::default()
        },
        "mat" => WorldeditCommand {
            arguments: &[
                argument!("pattern", Pattern, "The new pattern of the brush in your hand")
            ],
            execute_fn: execute_mat,
            description: "Set the material (pattern) of the brush in your hand",
            permission_node: "worldedit.brush.options.material",
            mutates_world: false,
            ..Default::default()
        },
        "mask" => WorldeditCommand {
            arguments: &[
                argument_str!("mask", String, "The mask, none removes it", "none")
            ],
            execute_fn: execute_mask,
            description: "Set the destination mask of the brush in your hand",
            permission_node: "worldedit.brush.options.mask",
            mutates_world: false,
            ..Default::default()
        },
        "smask" => WorldeditCommand {
            arguments: &[
                argument_str!("mask", String, "The mask, none removes it", "none")
            ],
            execute_fn: execute_smask,
            description: "Set the source mask of the brush in your hand",
            permission_node: "worldedit.brush.options.mask",
            mutates_world: false,
            ..Default::default()
        },
        "transform" => WorldeditCommand {
            arguments: &[
                argument_str!("transform", String, "The transform, none removes it", "none")
            ],
            execute_fn: execute_transform,
            description: "Set the transform of the brush in your hand",
            permission_node: "worldedit.brush.options.transform",
            mutates_world: false,
            ..Default::default()
        },
        "targetmask" => WorldeditCommand {
            arguments: &[
                argument_str!("mask", String, "The mask, none removes it", "none")
            ],
            execute_fn: execute_targetmask,
            description: "Set the mask the target of the brush stops at",
            permission_node: "worldedit.brush.targetmask",
            mutates_world: false,
            ..Default::default()
        },
        "targetoffset" => WorldeditCommand {
            arguments: &[
                argument!("offset", Integer, "The height that is added to the target", 0)
            ],
            execute_fn: execute_targetoffset,
            description: "Set the target offset of the brush in your hand",
            permission_node: "worldedit.brush.targetoffset",
            mutates_world: false,
            ..Default::default()
        },
        "target" => WorldeditCommand {
            arguments: &[
                argument!("mode", UnsignedInteger, "The target mode, 1 to 4", 1)
            ],
            execute_fn: execute_target,
            description: "Set the target mode of the brush in your hand",
            permission_node: "worldedit.brush.options.target",
            mutates_world: false,
            ..Default::default()
        },
        "scroll" => WorldeditCommand {
            arguments: &[
                argument_str!("action", String, "none, clipboard, mask, pattern, range, size, visual or target", "none")
            ],
            execute_fn: execute_scroll,
            description: "Set the scroll action of the brush in your hand",
            permission_node: "worldedit.brush.scroll",
            mutates_world: false,
            ..Default::default()
        },
        "savebrush" => WorldeditCommand {
            arguments: &[
                argument!("name", String, "The name of the brush")
            ],
            flags: &[flag!('g', None, "Save the brush for everyone")],
            execute_fn: execute_savebrush,
            description: "Save the brush in your hand",
            permission_node: "worldedit.brush.save",
            mutates_world: false,
            ..Default::default()
        },
        "loadbrush" => WorldeditCommand {
            arguments: &[
                argument!("name", String, "The name of the brush")
            ],
            execute_fn: execute_loadbrush,
            description: "Load a saved brush",
            permission_node: "worldedit.brush.load",
            mutates_world: false,
            ..Default::default()
        },
        "listbrush" => WorldeditCommand {
            arguments: &[
                argument_str!("scope", String, "mine, global or all", "all"),
                argument!("page", UnsignedInteger, "The page", 1)
            ],
            flags: &[flag!('d', None, "Details"), flag!('n', None, "Sort by name"), flag!('p', None, "Print the page")],
            execute_fn: execute_listbrush,
            description: "List the saved brushes",
            permission_node: "worldedit.brush.list",
            mutates_world: false,
            ..Default::default()
        },
        "tool" => WorldeditCommand {
            arguments: &[
                argument!("tool", String, "tree, repl, info, lrbuild, inspect, farwand, floodfill, cycler, deltree or none"),
                argument_str!("arg1", String, "Arguments of the tool", ""),
                argument_str!("arg2", String, "Arguments of the tool", "")
            ],
            execute_fn: execute_tool,
            description: "Bind a tool to the item in your hand",
            permission_node: "",
            mutates_world: false,
            ..Default::default()
        },
        "/gtransform" => WorldeditCommand {
            arguments: &[argument_str!(
                "transform",
                String,
                "The transform, none removes it",
                "none"
            )],
            execute_fn: utility::execute_gtransform,
            description: "Set the global transform, every edit goes through it",
            permission_node: "worldedit.global-transform",
            mutates_world: false,
            ..Default::default()
        },
        "/gmask" => WorldeditCommand {
            arguments: &[
                argument_str!("mask", String, "The mask, none removes it", "none")
            ],
            execute_fn: execute_gmask,
            description: "Set the global mask, only blocks that match are changed by any edit",
            permission_node: "worldedit.global-mask",
            mutates_world: false,
            ..Default::default()
        },
        "none" => WorldeditCommand {
            execute_fn: execute_none,
            description: "Unbind the tool or brush from the item in your hand",
            permission_node: "",
            mutates_world: false,
            ..Default::default()
        },
        "up" => WorldeditCommand {
            execute_fn: execute_up,
            reads_world: true,
            description: "Go upwards some distance",
            arguments: &[
                argument!("distance", UnsignedInteger, "Distance to go upwards")
            ],
            flags: &[
                flag!('f', None, "Force flight even if not flying"),
                flag!('g', None, "Force a glass platform")
            ],
            permission_node: "worldedit.navigation.up",
            ..Default::default()
        },
        "ceil" => WorldeditCommand {
            execute_fn: execute_ceil,
            reads_world: true,
            description: "Go to the ceiling",
            arguments: &[
                argument!("clearance", UnsignedInteger, "# of blocks to leave above you", 0)
            ],
            flags: &[
                flag!('f', None, "Force flight even if not flying"),
                flag!('g', None, "Force a glass platform")
            ],
            permission_node: "worldedit.navigation.ceiling",
            ..Default::default()
        },
        "thru" => WorldeditCommand {
            execute_fn: execute_thru,
            reads_world: true,
            description: "Passthrough walls",
            permission_node: "worldedit.navigation.thru.command",
            mutates_world: false,
            ..Default::default()
        },
        "jumpto" => WorldeditCommand {
            execute_fn: execute_jumpto,
            reads_world: true,
            description: "Teleport to a location",
            permission_node: "worldedit.navigation.jumpto.command",
            mutates_world: false,
            ..Default::default()
        },
        "unstuck" => WorldeditCommand {
            execute_fn: execute_unstuck,
            reads_world: true,
            description: "Escape from being stuck inside a block",
            permission_node: "worldedit.navigation.unstuck",
            mutates_world: false,
            ..Default::default()
        },
        "ascend" => WorldeditCommand {
            execute_fn: execute_ascend,
            reads_world: true,
            description: "Go up a floor",
            arguments: &[
                argument!("levels", UnsignedInteger, "# of levels to ascend")
            ],
            permission_node: "worldedit.navigation.ascend",
            mutates_world: false,
            ..Default::default()
        },
        "descend" => WorldeditCommand {
            execute_fn: execute_descend,
            reads_world: true,
            description: "Go down a floor",
            arguments: &[
                argument!("levels", UnsignedInteger, "# of levels to descend")
            ],
            permission_node: "worldedit.navigation.descend",
            mutates_world: false,
            ..Default::default()
        },
        "/pos1" => WorldeditCommand {
            arguments: &[
                argument_str!("coordinates", String, "Coordinates to set the position to (x,y,z)", "")
            ],
            execute_fn: execute_pos1,
            description: "Set position 1",
            permission_node: "worldedit.selection.pos",
            mutates_world: false,
            ..Default::default()
        },
        "/pos2" => WorldeditCommand {
            arguments: &[
                argument_str!("coordinates", String, "Coordinates to set the position to (x,y,z)", "")
            ],
            execute_fn: execute_pos2,
            description: "Set position 2",
            permission_node: "worldedit.selection.pos",
            mutates_world: false,
            ..Default::default()
        },
        "/hpos1" => WorldeditCommand {
            execute_fn: execute_hpos1,
            reads_world: true,
            description: "Set position 1 to targeted block",
            permission_node: "worldedit.selection.hpos",
            mutates_world: false,
            ..Default::default()
        },
        "/hpos2" => WorldeditCommand {
            execute_fn: execute_hpos2,
            reads_world: true,
            description: "Set position 2 to targeted block",
            permission_node: "worldedit.selection.hpos",
            mutates_world: false,
            ..Default::default()
        },
        "/visualize" => WorldeditCommand {
            execute_fn: execute_visualize,
            description: "Toggle the particle outline of your selection",
            mutates_world: false,
            ..Default::default()
        },
        "/sel" => WorldeditCommand {
            arguments: &[
                argument_str!("selector", String, "The selector to choose (cuboid)", ""),
                argument_str!("mode", String, "The expand mode of the selector", "")
            ],
            flags: &[
                flag!('d', None, "Set the default selector without changing the current one")
            ],
            execute_fn: execute_sel,
            description: "Choose a region selector",
            permission_node: "worldedit.analysis.sel",
            mutates_world: false,
            ..Default::default()
        },
        "/set" => WorldeditCommand {
            arguments: &[
                argument!("pattern", Pattern, "The pattern of blocks to set")
            ],
            requires_positions: true,
            execute_fn: execute_set,
            description: "Sets all the blocks in the region",
            permission_node: "worldedit.region.set",
            ..Default::default()
        },
        "/replace" => WorldeditCommand {
            arguments: &[
                argument!("from", Mask, "The mask representing blocks to replace"),
                argument!("to", Pattern, "The pattern of blocks to replace with")
            ],
            flags: &[flag!('f', None, "Perform the replacement without checking the history limits")],
            requires_positions: true,
            execute_fn: execute_replace,
            description: "Replace all blocks in the selection with another",
            permission_node: "worldedit.region.replace",
            ..Default::default()
        },
        "/copy" => WorldeditCommand {
            requires_positions: true,
            flags: clipboard_cmds::COPY_FLAGS,
            execute_fn: clipboard_cmds::execute_copy,
            reads_world: true,
            description: "Copy the selection to the clipboard",
            permission_node: "worldedit.clipboard.copy",
            mutates_world: false,
            ..Default::default()
        },
        "/cut" => WorldeditCommand {
            arguments: &[
                argument_str!("leave-id", Pattern, "Pattern to leave in place of the cut blocks", "air")
            ],
            requires_positions: true,
            flags: clipboard_cmds::COPY_FLAGS,
            execute_fn: clipboard_cmds::execute_cut,
            reads_world: true,
            description: "Cut the selection to the clipboard",
            permission_node: "worldedit.clipboard.cut",
            ..Default::default()
        },
        "/paste" => WorldeditCommand {
            requires_clipboard: true,
            execute_fn: clipboard_cmds::execute_paste,
            description: "Paste the clipboard's contents",
            flags: clipboard_cmds::PASTE_FLAGS,
            permission_node: "worldedit.clipboard.paste",
            ..Default::default()
        },
        "/place" => WorldeditCommand {
            requires_clipboard: true,
            execute_fn: clipboard_cmds::execute_paste,
            description: "Place the clipboard's contents without applying transformations (e.g. rotate)",
            flags: clipboard_cmds::PASTE_FLAGS,
            permission_node: "worldedit.clipboard.place",
            ..Default::default()
        },
        "/lazycopy" => WorldeditCommand {
            requires_positions: true,
            flags: clipboard_cmds::COPY_FLAGS,
            execute_fn: clipboard_cmds::execute_copy,
            reads_world: true,
            description: "Lazily copy the selection to the clipboard",
            permission_node: "worldedit.clipboard.lazycopy",
            mutates_world: false,
            ..Default::default()
        },
        "/lazycut" => WorldeditCommand {
            arguments: &[
                argument_str!("leave-id", Pattern, "Pattern to leave in place of the cut blocks", "air")
            ],
            requires_positions: true,
            flags: clipboard_cmds::COPY_FLAGS,
            execute_fn: clipboard_cmds::execute_cut,
            reads_world: true,
            description: "Lazily cut the selection to the clipboard",
            permission_node: "worldedit.clipboard.lazycut",
            ..Default::default()
        },
        "clearclipboard" => WorldeditCommand {
            execute_fn: clipboard_cmds::execute_clearclipboard,
            description: "Clear your clipboard",
            permission_node: "worldedit.clipboard.clear",
            mutates_world: false,
            ..Default::default()
        },
        "download" => WorldeditCommand {
            execute_fn: clipboard_cmds::execute_web_stub,
            description: "Download your clipboard through the web interface",
            permission_node: "worldedit.clipboard.download",
            mutates_world: false,
            ..Default::default()
        },
        "asset" => WorldeditCommand {
            execute_fn: clipboard_cmds::execute_web_stub,
            description: "Manage assets through the web interface",
            permission_node: "worldedit.asset",
            mutates_world: false,
            ..Default::default()
        },
        "schematic" => WorldeditCommand {
            arguments: &[
                argument_str!("subcommand", String, "clear, load, save, delete, list, unload, loadall, move, formats or show", ""),
                argument_str!("arg1", String, "Format, file name or filter", ""),
                argument_str!("arg2", String, "File name or page", ""),
                argument_str!("arg3", String, "Further arguments and flags", ""),
                argument_str!("arg4", String, "Further arguments and flags", ""),
                argument_str!("arg5", String, "Further arguments and flags", ""),
            ],
            raw_args: true,
            execute_fn: schematic::execute_schematic,
            description: "Schematic commands for saving/loading areas",
            mutates_world: false,
            normal_completions: true,
            ..Default::default()
        },
        "/stack" => WorldeditCommand {
            arguments: &[
                argument!("count", Integer, "# of copies to stack", 1),
                argument!("offset", DirectionVector, "How far to move the contents each stack")
            ],
            requires_positions: true,
            execute_fn: execute_stack,
            reads_world: true,
            description: "Repeat the contents of the selection",
            flags: &[
                flag!('s', None, "Shift the selection to the last stacked copy"),
                flag!('a', None, "Ignore air blocks"),
                flag!('e', None, "Also copy entities"),
                flag!('b', None, "Also copy biomes"),
                flag!('r', None, "Use block units"),
                flag!('m', Some(ArgumentType::Mask), "Set the include mask, non-matching blocks become air")
            ],
            permission_node: "worldedit.region.stack",
            ..Default::default()
        },
        "/count" => WorldeditCommand {
            arguments: &[
                argument!("mask", Mask, "The mask of blocks to match")
            ],
            requires_positions: true,
            execute_fn: execute_count,
            reads_world: true,
            description: "Counts the number of blocks matching a mask",
            permission_node: "worldedit.analysis.count",
            mutates_world: false,
            ..Default::default()
        },
        "/load" => WorldeditCommand {
            arguments: &[
                argument!("name", String, "The file name of the schematic to load")
            ],
            execute_fn: schematic::execute_load,
            description: "Loads a schematic file into the clipboard",
            permission_node: "worldedit.clipboard.load",
            mutates_world: false,
            normal_completions: false,
            ..Default::default()
        },
        "/save" => WorldeditCommand {
            arguments: &[
                argument!("name", String, "The file name of the schematic to save")
            ],
            flags: &[flag!('f', None, "Overwrite an existing file")],
            requires_clipboard: true,
            execute_fn: schematic::execute_save,
            description: "Save a schematic file from the clipboard",
            permission_node: "worldedit.clipboard.save",
            mutates_world: false,
            ..Default::default()
        },
        "/expand" => WorldeditCommand {
            arguments: &[
                argument!("amount", String, "Amount to expand the selection by, or `vert`"),
                argument_str!("reverseAmount", String, "Amount to expand the selection by in the other direction", ""),
                argument_str!("direction", String, "Direction to expand", "")
            ],
            requires_positions: true,
            execute_fn: execute_expand,
            description: "Expand the selection area",
            permission_node: "worldedit.selection.expand",
            mutates_world: false,
            ..Default::default()
        },
        "/contract" => WorldeditCommand {
            arguments: &[
                argument!("amount", UnsignedInteger, "Amount to contract the selection by"),
                argument_str!("reverseAmount", String, "Amount to contract the selection by in the other direction", ""),
                argument_str!("direction", String, "Direction to contract", "")
            ],
            requires_positions: true,
            execute_fn: execute_contract,
            description: "Contract the selection area",
            permission_node: "worldedit.selection.contract",
            mutates_world: false,
            ..Default::default()
        },
        "/shift" => WorldeditCommand {
            arguments: &[
                argument!("amount", UnsignedInteger, "Amount to shift the selection by"),
                argument!("direction", Direction, "Direction to shift")
            ],
            requires_positions: true,
            execute_fn: execute_shift,
            description: "Shift the selection area",
            permission_node: "worldedit.selection.shift",
            mutates_world: false,
            ..Default::default()
        },
        "/outset" => WorldeditCommand {
            arguments: &[
                argument!("amount", UnsignedInteger, "Amount to expand the selection by in all directions")
            ],
            flags: &[
                flag!('h', None, "Only expand horizontally"),
                flag!('v', None, "Only expand vertically")
            ],
            requires_positions: true,
            execute_fn: execute_outset,
            description: "Outset the selection area",
            permission_node: "worldedit.selection.outset",
            mutates_world: false,
            ..Default::default()
        },
        "/inset" => WorldeditCommand {
            arguments: &[
                argument!("amount", UnsignedInteger, "Amount to contract the selection by in all directions")
            ],
            flags: &[
                flag!('h', None, "Only contract horizontally"),
                flag!('v', None, "Only contract vertically")
            ],
            requires_positions: true,
            execute_fn: execute_inset,
            description: "Inset the selection area",
            permission_node: "worldedit.selection.inset",
            mutates_world: false,
            ..Default::default()
        },
        "/size" => WorldeditCommand {
            flags: &[
                flag!('c', None, "Get the size of the clipboard instead")
            ],
            execute_fn: execute_selection_size,
            description: "Get information about the selection",
            permission_node: "worldedit.selection.size",
            mutates_world: false,
            ..Default::default()
        },
        "/distr" => WorldeditCommand {
            flags: &[
                flag!('c', None, "Get the distribution of the clipboard instead"),
                flag!('d', None, "Separate blocks by state"),
                flag!('p', Some(ArgumentType::UnsignedInteger), "Gets page from a previous distribution")
            ],
            execute_fn: execute_distr,
            reads_world: true,
            description: "Get the distribution of blocks in the selection",
            permission_node: "worldedit.analysis.distr",
            mutates_world: false,
            ..Default::default()
        },
        "/flip" => WorldeditCommand {
            arguments: &[
                argument!("direction", Direction, "The direction to flip, defaults to look direction"),
            ],
            requires_clipboard: true,
            execute_fn: execute_flip,
            description: "Flip the contents of the clipboard across the origin",
            mutates_world: false,
            ..Default::default()
        },
        "/rstack" => WorldeditCommand {
            arguments: &[
                argument!("count", UnsignedInteger, "# of copies to stack"),
                argument!("spacing", UnsignedInteger, "The spacing between each selection", 2),
                argument!("direction", DirectionVector, "The direction to stack")
            ],
            requires_positions: true,
            flags: &[
                flag!('a', None, "Include air blocks"),
                flag!('e', None, "Expand selection")
            ],
            execute_fn: execute_rstack,
            reads_world: true,
            description: "Like //stack but allows the stacked copies to overlap, supports more directions, and more flags",
            permission_node: "redstonetools.rstack",
            ..Default::default()
        },
        "/update" => WorldeditCommand {
            execute_fn: execute_update,
            reads_world: true,
            description: "Updates all blocks in the selection",
            permission_node: "cinnabar.we.update",
            requires_positions: false,
            flags: &[
                flag!('p', None, "Update the entire plot"),
            ],
            ..Default::default()
        },
        "/help" => WorldeditCommand {
            arguments: &[
                argument_str!("command", String, "Command to retrieve help for, or the page", ""),
                argument!("page", UnsignedInteger, "The page to show"),
            ],
            execute_fn: execute_help,
            description: "Displays help for WorldEdit commands",
            permission_node: "worldedit.help",
            mutates_world: false,
            ..Default::default()
        },
        "/wand" => WorldeditCommand {
           execute_fn: execute_wand,
           description: "Gives a WorldEdit wand",
           permission_node: "worldedit.wand",
            mutates_world: false,
           ..Default::default()
        },
        "/replacecontainer" => WorldeditCommand {
            arguments: &[
                argument!("from", ContainerType, "The container type to replace"),
                argument!("to", ContainerType, "The container type to replace with"),
            ],
           execute_fn: execute_replace_container,
            reads_world: true,
           description: "Replaces all container types in the selection",
           permission_node: "cinnabar.we.replacecontainer",
           requires_positions: true,
           ..Default::default()
        }
    };
    commands.extend(misc::commands());
    commands
});

static ALIASES: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(|| {
    let mut aliases = map! {
        "undo" => "/undo",
        "redo" => "/redo",
        "/ext" => "/ex",
        "ex" => "/ex",
        "ext" => "/ex",
        "extinguish" => "/ex",
        "/extinguish" => "/ex",
        "schem" => "schematic",
        "schematics" => "schematic",
        "u" => "up",
        "desc" => "descend",
        "asc" => "ascend",
        "j" => "jumpto",
        "!" => "unstuck",
        "/ceil" => "ceil",
        "/thru" => "thru",
        "/unstuck" => "unstuck",
        "/jumpto" => "jumpto",
        "/j" => "jumpto",
        ";" => "/sel",
        "/desel" => "/sel",
        "/deselect" => "/sel",
        "/viz" => "/visualize",
        "/1" => "/pos1",
        "/2" => "/pos2",
        "/c" => "/copy",
        "/x" => "/cut",
        "/v" => "/paste",
        "/va" => "/paste -a",
        "/s" => "/stack",
        "/sa" => "/stack -a",
        "/e" => "/expand",
        "/r" => "/rotate",
        "/f" => "/flip",
        "/h1" => "/hpos1",
        "/h2" => "/hpos2",
        "/rs" => "/rstack",
        "/rc" => "/replacecontainer",
        "/rn" => "replacenear",
        "/replacenear" => "replacenear",
        "/re" => "/replace",
        "/rep" => "/replace",
        "/mv" => "/move",
        "br" => "brush",
        "/br" => "brush",
        "/brush" => "brush",
        "/mask" => "mask",
        "/smask" => "smask",
        "/mat" => "mat",
        "material" => "mat",
        "/material" => "mat",
        "/none" => "none",
        "/range" => "range",
        "/primary" => "primary",
        "/secondary" => "secondary",
        "/scroll" => "scroll",
        "/target" => "target",
        "/targetmask" => "targetmask",
        "/targetoffset" => "targetoffset",
        "/transform" => "transform",
        "/tool" => "tool",
        "/listbrush" => "listbrush",
        "/loadbrush" => "loadbrush",
        "/savebrush" => "savebrush",
        "/ss" => "/sphere",
        "/hs" => "/hsphere",
        "/cy" => "/cyl",
        "/hc" => "/hcyl"
    };
    aliases.extend(misc::aliases());
    aliases
});

pub fn ray_trace_block(
    world: &impl World,
    mut pos: PlayerPos,
    start_pitch: f64,
    start_yaw: f64,
    max_distance: f64,
) -> Option<BlockPos> {
    let check_distance = 0.2;

    // Player view height
    pos.y += 1.65;
    let rot_x = (start_yaw + 90.0) % 360.0;
    let rot_y = -start_pitch;
    let h = check_distance * rot_y.to_radians().cos();

    let offset_x = h * rot_x.to_radians().cos();
    let offset_y = check_distance * rot_y.to_radians().sin();
    let offset_z = h * rot_x.to_radians().sin();

    let mut current_distance = 0.0;

    while current_distance < max_distance {
        let block_pos = pos.block_pos();
        let block = world.get_block(block_pos);

        if !matches!(block, Block::Air) {
            return Some(block_pos);
        }

        pos.x += offset_x;
        pos.y += offset_y;
        pos.z += offset_z;
        current_distance += check_distance;
    }

    None
}

/// Where the two corners end up when the region grows (or shrinks, for `contract`) by `amount`.
/// A positive part of the amount moves the upper bound, a negative part the lower bound.
fn adjusted_corners(
    mut p1: BlockPos,
    mut p2: BlockPos,
    amount: BlockPos,
    contract: bool,
) -> (BlockPos, BlockPos) {
    fn axis_of(pos: &mut BlockPos, axis: u8) -> &mut i32 {
        match axis {
            0 => &mut pos.x,
            1 => &mut pos.y,
            2 => &mut pos.z,
            _ => unreachable!(),
        }
    }

    for axis in 0..=2 {
        let amount = *axis_of(&mut amount.clone(), axis);
        let c1 = axis_of(&mut p1, axis);
        let c2 = axis_of(&mut p2, axis);
        #[allow(clippy::comparison_chain)]
        if amount > 0 {
            if (*c1 > *c2) ^ contract {
                *c1 += amount;
            } else {
                *c2 += amount;
            }
        } else if amount < 0 {
            if (*c1 < *c2) ^ contract {
                *c1 += amount;
            } else {
                *c2 += amount;
            }
        }
    }
    (p1, p2)
}

fn set_corners(player: &mut Player, p1: BlockPos, p2: BlockPos) {
    if Some(p1) != player.first_position {
        player.worldedit_set_first_position(p1);
    }
    if Some(p2) != player.second_position {
        player.worldedit_set_second_position(p2);
    }
}

fn expand_selection(player: &mut Player, amount: BlockPos, contract: bool) {
    let (p1, p2) = adjusted_corners(
        player.first_position.unwrap(),
        player.second_position.unwrap(),
        amount,
        contract,
    );
    set_corners(player, p1, p2);
}

fn update(plot: &mut PlotWorld, first_pos: BlockPos, second_pos: BlockPos) {
    for_each_block_mut_optimized(plot, first_pos, second_pos, |plot, pos| {
        let block = plot.get_block(pos);
        cinnabar_redstone::update(block, plot, pos);
    });
}

pub fn add_command_completions(nodes: &mut Vec<CCommandsNode>) {
    type Parser = CDeclareCommandsNodeParser;
    for (name, command) in &*COMMANDS {
        if !command.normal_completions {
            // These commands have completions manually defined in plot::commands
            continue;
        }
        let command_idx = nodes.len() as i32;
        nodes[0].children.push(command_idx);
        nodes.push(CCommandsNode {
            flags: (CommandFlags::LITERAL | CommandFlags::EXECUTABLE).bits() as i8,
            children: Vec::new(),
            redirect_node: None,
            name: Some(name),
            parser: None,
            suggestions_type: None,
        });
        for arg in command.arguments {
            let parent_idx = nodes.len() - 1;
            let arg_idx = nodes.len() as i32;
            nodes[parent_idx].children.push(arg_idx);
            nodes.push(CCommandsNode {
                flags: (CommandFlags::ARGUMENT | CommandFlags::EXECUTABLE).bits() as i8,
                children: Vec::new(),
                redirect_node: None,
                name: Some(arg.name),
                parser: Some(match arg.argument_type {
                    ArgumentType::UnsignedInteger => Parser::Integer(0, i32::MAX),
                    ArgumentType::Integer => Parser::Integer(i32::MIN, i32::MAX),
                    _ => Parser::String(0),
                }),
                suggestions_type: None,
            });
        }
    }

    // Add completions for command aliases
    for (name, target) in ALIASES.iter() {
        let target = target.split_whitespace().next().unwrap();
        let target_idx = nodes
            .iter()
            .position(|node| {
                node.name == Some(target)
                    && !CommandFlags::from_bits_truncate(node.flags as u32)
                        .contains(CommandFlags::ARGUMENT)
            })
            .unwrap();
        let alias_idx = nodes.len();
        nodes[0].children.push(alias_idx as i32);
        nodes.push(CCommandsNode {
            name: Some(name),
            flags: (CommandFlags::LITERAL | CommandFlags::REDIRECT).bits() as i8,
            children: Vec::new(),
            redirect_node: Some(target_idx as i32),
            parser: None,
            suggestions_type: None,
        });
    }
}
