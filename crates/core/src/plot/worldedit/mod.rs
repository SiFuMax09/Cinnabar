//! [Worldedit](https://github.com/EngineHub/WorldEdit) and [RedstoneTools](https://github.com/paulikauro/RedstoneTools) implementation

#[cfg(test)]
mod bench;
mod brush;
mod execute;
mod history;
mod job;
mod ops;
mod pattern;
mod session;
mod shapes;
mod transform;

use super::commands::CommandFlags;
use super::{Plot, PlotWorld};
use crate::player::{PacketSender, Player, PlayerPos};
use execute::*;
use mchprs_blocks::block_entities::ContainerType;
use mchprs_blocks::blocks::Block;
use mchprs_blocks::{BlockFacing, BlockPos};
use mchprs_network::packets::clientbound::{CCommandsNode, CDeclareCommandsNodeParser};
use mchprs_utils::map;
use mchprs_world::{for_each_block_mut_optimized, World};
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::LazyLock;

pub use brush::{Brush, BrushKind};
pub use history::History;
pub use job::{EditJob, SLICE_BUDGET};
pub use pattern::{Mask, Pattern, PatternParseError};

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

    let wea = player.has_permission("plots.worldedit.bypass");
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

    let total_arg_count = args.len();
    let mut arg_iter = args.iter().copied().enumerate();
    let mut args = Vec::new();

    while let Some((i, arg)) = arg_iter.next() {
        // `-5` is a number, not a flag
        if arg.starts_with('-') && arg[1..].starts_with(|c: char| c.is_ascii_alphabetic()) {
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
                    arg_iter.next();
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
        if arg.is_none() {
            return Argument::get_default(player, desc);
        }
        let arg = arg.unwrap();
        let arg_type = desc.argument_type;
        match arg_type {
            ArgumentType::Direction => {
                let player_facing = player.get_facing();
                Ok(Argument::Direction(match arg {
                    "me" => player_facing,
                    "u" | "up" => BlockFacing::Up,
                    "d" | "down" => BlockFacing::Down,
                    "l" | "left" => player_facing.rotate_ccw(),
                    "r" | "right" => player_facing.rotate(),
                    _ => return Err(ArgumentParseError::new(arg_type, "unknown direction")),
                }))
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

                let mut base_dir = arg;
                if arg.len() > 1 && matches!(arg.chars().last(), Some('u' | 'd')) {
                    match arg.chars().last().unwrap() {
                        'u' => vec.y += 1,
                        'd' => vec.y -= 1,
                        _ => unreachable!(),
                    }
                    base_dir = &arg[..1];
                }

                let facing = match base_dir {
                    "u" | "up" => BlockFacing::Up,
                    "d" | "down" => BlockFacing::Down,
                    "l" | "left" => player_facing.rotate_ccw(),
                    "r" | "right" => player_facing.rotate(),
                    _ => return Err(ArgumentParseError::new(arg_type, "unknown direction")),
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
    ($name:literal, $type:ident, $desc:literal) => {
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
    jobs: &'a mut VecDeque<EditJob>,
}

impl CommandExecuteContext<'_> {
    fn has_flag(&self, c: char) -> bool {
        self.flags.contains(&c)
    }
}

struct WorldeditCommand {
    arguments: &'static [ArgumentDescription],
    flags: &'static [FlagDescription],
    requires_positions: bool,
    requires_clipboard: bool,
    execute_fn: fn(CommandExecuteContext<'_>),
    description: &'static str,
    permission_node: &'static str,
    mutates_world: bool,
    /// Whether the command looks at the blocks of the world when it is run. Then the edits that
    /// are still queued have to be finished first.
    reads_world: bool,
    normal_completions: bool,
}

impl Default for WorldeditCommand {
    fn default() -> Self {
        Self {
            arguments: &[],
            flags: &[],
            execute_fn: execute_unimplemented,
            description: "",
            requires_clipboard: false,
            requires_positions: false,
            permission_node: "",
            mutates_world: true,
            reads_world: false,
            normal_completions: true,
        }
    }
}

static COMMANDS: LazyLock<HashMap<&'static str, WorldeditCommand>> = LazyLock::new(|| {
    map! {
        "/undo" => WorldeditCommand {
            arguments: &[
                argument!("times", UnsignedInteger, "Number of edits to undo")
            ],
            execute_fn: execute_undo,
            description: "Undoes the last actions (from history)",
            permission_node: "worldedit.history.undo",
            ..Default::default()
        },
        "/redo" => WorldeditCommand {
            arguments: &[
                argument!("times", UnsignedInteger, "Number of edits to redo")
            ],
            execute_fn: execute_redo,
            description: "Redoes the last undone actions (from history)",
            permission_node: "worldedit.history.redo",
            ..Default::default()
        },
        "/move" => WorldeditCommand {
            arguments: &[
                argument!("count", UnsignedInteger, "The distance to move"),
                argument!("direction", Direction, "The direction to move"),
                argument_str!("replace", Pattern, "The pattern to leave behind", "air")
            ],
            requires_positions: true,
            reads_world: true,
            execute_fn: execute_move,
            description: "Move the contents of the selection",
            flags: &[
                flag!('a', None, "Ignore air blocks"),
                flag!('s', None, "Shift the selection to the target location")
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
        "/replacenear" => WorldeditCommand {
            arguments: &[
                argument!("size", UnsignedInteger, "The radius of the cube around you"),
                argument!("from", Mask, "The mask representing blocks to replace"),
                argument!("to", Pattern, "The pattern of blocks to replace with")
            ],
            execute_fn: execute_replace_near,
            description: "Replace nearby blocks",
            permission_node: "worldedit.region.replacenear",
            ..Default::default()
        },
        "/sphere" => WorldeditCommand {
            arguments: &[
                argument!("pattern", Pattern, "The pattern of blocks to place"),
                argument!("radius", UnsignedInteger, "The radius of the sphere")
            ],
            flags: &[flag!('r', None, "Raise the sphere so it rests on the ground")],
            execute_fn: execute_sphere,
            description: "Generate a filled sphere",
            permission_node: "worldedit.generation.sphere",
            ..Default::default()
        },
        "/hsphere" => WorldeditCommand {
            arguments: &[
                argument!("pattern", Pattern, "The pattern of blocks to place"),
                argument!("radius", UnsignedInteger, "The radius of the sphere")
            ],
            flags: &[flag!('r', None, "Raise the sphere so it rests on the ground")],
            execute_fn: execute_hsphere,
            description: "Generate a hollow sphere",
            permission_node: "worldedit.generation.sphere",
            ..Default::default()
        },
        "/cyl" => WorldeditCommand {
            arguments: &[
                argument!("pattern", Pattern, "The pattern of blocks to place"),
                argument!("radius", UnsignedInteger, "The radius of the cylinder"),
                argument!("height", UnsignedInteger, "The height of the cylinder")
            ],
            execute_fn: execute_cyl,
            description: "Generate a filled cylinder that goes up from your feet",
            permission_node: "worldedit.generation.cylinder",
            ..Default::default()
        },
        "/hcyl" => WorldeditCommand {
            arguments: &[
                argument!("pattern", Pattern, "The pattern of blocks to place"),
                argument!("radius", UnsignedInteger, "The radius of the cylinder"),
                argument!("height", UnsignedInteger, "The height of the cylinder")
            ],
            execute_fn: execute_hcyl,
            description: "Generate a hollow cylinder that goes up from your feet",
            permission_node: "worldedit.generation.cylinder",
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
                argument!("iterations", UnsignedInteger, "The number of smoothing passes", 4)
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
        "/chunk" => WorldeditCommand {
            execute_fn: execute_chunk,
            description: "Select the chunk you are in",
            permission_node: "worldedit.selection.chunk",
            mutates_world: false,
            ..Default::default()
        },
        "/brush" => WorldeditCommand {
            arguments: &[
                argument!("type", String, "sphere, cylinder, smooth, clipboard or none"),
                argument_str!("arg1", String, "sphere/cylinder: pattern, smooth: size", ""),
                argument_str!("arg2", String, "sphere/cylinder: size, smooth: iterations", ""),
                argument_str!("arg3", String, "cylinder: height", "")
            ],
            flags: &[
                flag!('h', None, "Create hollow shapes"),
                flag!('a', None, "Skip air blocks of the clipboard")
            ],
            execute_fn: execute_brush,
            description: "Bind a brush to the item in your hand, use it with a right click",
            permission_node: "worldedit.brush.options",
            mutates_world: false,
            ..Default::default()
        },
        "/size" => WorldeditCommand {
            arguments: &[
                argument!("size", UnsignedInteger, "The new size of the brush in your hand")
            ],
            execute_fn: execute_size,
            description: "Change the size of the brush in your hand",
            permission_node: "worldedit.brush.options.size",
            mutates_world: false,
            ..Default::default()
        },
        "/mat" => WorldeditCommand {
            arguments: &[
                argument!("pattern", Pattern, "The new pattern of the brush in your hand")
            ],
            execute_fn: execute_mat,
            description: "Change the pattern of the brush in your hand",
            permission_node: "worldedit.brush.options.material",
            mutates_world: false,
            ..Default::default()
        },
        "/mask" => WorldeditCommand {
            arguments: &[
                argument_str!("mask", String, "The mask, none removes it", "none")
            ],
            execute_fn: execute_mask,
            description: "Set the mask of the brush in your hand",
            permission_node: "worldedit.brush.options.mask",
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
        "/none" => WorldeditCommand {
            execute_fn: execute_none,
            description: "Unbind the brush from the item in your hand",
            permission_node: "worldedit.brush.none",
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
            permission_node: "worldedit.navigation.up",
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
            execute_fn: execute_pos1,
            description: "Set position 1",
            permission_node: "worldedit.selection.pos",
            mutates_world: false,
            ..Default::default()
        },
        "/pos2" => WorldeditCommand {
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
            execute_fn: execute_sel,
            description: "Choose a region selector",
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
            permission_node: "worldedit.region.stack",
            ..Default::default()
        },
        "/replace" => WorldeditCommand {
            arguments: &[
                argument!("from", Mask, "The mask representng blocks to replace"),
                argument!("to", Pattern, "The pattern of blocks to replace with")
            ],
            requires_positions: true,
            execute_fn: execute_replace,
            description: "Replace all blocks in a selection with another",
            permission_node: "worldedit.region.replace",
            ..Default::default()
        },
        "/copy" => WorldeditCommand {
            requires_positions: true,
            execute_fn: execute_copy,
            reads_world: true,
            description: "Copy the selection to the clipboard",
            permission_node: "worldedit.clipboard.copy",
            mutates_world: false,
            ..Default::default()
        },
        "/cut" => WorldeditCommand {
            requires_positions: true,
            execute_fn: execute_cut,
            reads_world: true,
            description: "Cut the selection to the clipboard",
            permission_node: "worldedit.clipboard.cut",
            ..Default::default()
        },
        "/paste" => WorldeditCommand {
            requires_clipboard: true,
            execute_fn: execute_paste,
            description: "Paste the clipboard's contents",
            flags: &[
                flag!('a', None, "Skip air blocks"),
                flag!('u', None, "Also update all affected blocks"),
                flag!('s', None, "Select the volume of the pasted in schematic"),
            ],
            permission_node: "worldedit.clipboard.paste",
            ..Default::default()
        },
        "/stack" => WorldeditCommand {
            arguments: &[
                argument!("count", UnsignedInteger, "# of copies to stack"),
                argument!("direction", Direction, "The direction to stack")
            ],
            requires_positions: true,
            execute_fn: execute_stack,
            reads_world: true,
            description: "Repeat the contents of the selection",
            flags: &[
                flag!('a', None, "Ignore air blocks")
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
            execute_fn: execute_load,
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
            requires_clipboard: true,
            execute_fn: execute_save,
            description: "Save a schematic file from the clipboard",
            permission_node: "worldedit.clipboard.save",
            mutates_world: false,
            ..Default::default()
        },
        "/expand" => WorldeditCommand {
            arguments: &[
                argument!("amount", UnsignedInteger, "Amount to expand the selection by"),
                argument!("direction", Direction, "Direction to expand")
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
                argument!("direction", Direction, "Direction to contract")
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
            permission_node: "mchprs.we.update",
            requires_positions: false,
            flags: &[
                flag!('p', None, "Update the entire plot"),
            ],
            ..Default::default()
        },
        "/help" => WorldeditCommand {
            arguments: &[
                argument!("command", String, "Command to retrieve help for"),
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
           permission_node: "mchprs.we.replacecontainer",
           requires_positions: true,
           ..Default::default()
        }
    }
});

static ALIASES: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(|| {
    map! {
        "u" => "up",
        "desc" => "descend",
        "asc" => "ascend",
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
        "/rn" => "/replacenear",
        "/br" => "/brush",
        "/ss" => "/sphere",
        "/hs" => "/hsphere",
        "/cy" => "/cyl",
        "/hc" => "/hcyl"
    }
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

fn expand_selection(player: &mut Player, amount: BlockPos, contract: bool) {
    let mut p1 = player.first_position.unwrap();
    let mut p2 = player.second_position.unwrap();

    fn get_pos_axis(pos: &mut BlockPos, axis: u8) -> &mut i32 {
        match axis {
            0 => &mut pos.x,
            1 => &mut pos.y,
            2 => &mut pos.z,
            _ => unreachable!(),
        }
    }

    let mut expand_axis = |axis: u8| {
        let amount = *get_pos_axis(&mut amount.clone(), axis);
        let p1 = get_pos_axis(&mut p1, axis);
        let p2 = get_pos_axis(&mut p2, axis);
        #[allow(clippy::comparison_chain)]
        if amount > 0 {
            if (p1 > p2) ^ contract {
                *p1 += amount;
            } else {
                *p2 += amount;
            }
        } else if amount < 0 {
            if (p1 < p2) ^ contract {
                *p1 += amount;
            } else {
                *p2 += amount;
            }
        }
    };

    for axis in 0..=2 {
        expand_axis(axis);
    }

    if Some(p1) != player.first_position {
        player.worldedit_set_first_position(p1);
    }
    if Some(p2) != player.second_position {
        player.worldedit_set_second_position(p2);
    }
}

fn update(plot: &mut PlotWorld, first_pos: BlockPos, second_pos: BlockPos) {
    for_each_block_mut_optimized(plot, first_pos, second_pos, |plot, pos| {
        let block = plot.get_block(pos);
        mchprs_redstone::update(block, plot, pos);
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
