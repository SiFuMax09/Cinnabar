//! Commands of the FAWE command overview that deal with biomes, light, the super pickaxe,
//! entities, chunks, snapshots and the `/we` family, plus `//limit`, `//timeout` and
//! `//reorder`. The messages are the ones of FAWE.

use super::*;
use crate::plot::physics::Vec3;
use crate::plot::PLOT_BLOCK_HEIGHT;
use cinnabar_blocks::items::Item;
use std::collections::HashSet;
use std::sync::Mutex;

macro_rules! arg {
    ($name:literal, $type:ident, $desc:literal, $default:literal) => {
        ArgumentDescription {
            name: $name,
            argument_type: ArgumentType::$type,
            description: $desc,
            default: None,
            default_str: Some($default),
        }
    };
    ($name:literal, $type:ident, $desc:literal) => {
        ArgumentDescription {
            name: $name,
            argument_type: ArgumentType::$type,
            description: $desc,
            default: None,
            default_str: None,
        }
    };
}

macro_rules! flag_of {
    ($letter:literal, $desc:literal) => {
        FlagDescription {
            letter: $letter,
            argument_type: None,
            description: $desc,
        }
    };
}

/// The biome of the plots, which is the only one that exists
const PLOT_BIOME: &str = "cinnabar:plot";

/// The biomes of the game, which is what `/biomelist` shows and `//setbiome` knows
const BIOMES: &[&str] = &[
    "badlands",
    "bamboo_jungle",
    "basalt_deltas",
    "beach",
    "birch_forest",
    "cherry_grove",
    "cold_ocean",
    "crimson_forest",
    "dark_forest",
    "deep_cold_ocean",
    "deep_dark",
    "deep_frozen_ocean",
    "deep_lukewarm_ocean",
    "deep_ocean",
    "desert",
    "dripstone_caves",
    "end_barrens",
    "end_highlands",
    "end_midlands",
    "eroded_badlands",
    "flower_forest",
    "forest",
    "frozen_ocean",
    "frozen_peaks",
    "frozen_river",
    "grove",
    "ice_spikes",
    "jagged_peaks",
    "jungle",
    "lukewarm_ocean",
    "lush_caves",
    "mangrove_swamp",
    "meadow",
    "mushroom_fields",
    "nether_wastes",
    "ocean",
    "old_growth_birch_forest",
    "old_growth_pine_taiga",
    "old_growth_spruce_taiga",
    "pale_garden",
    "plains",
    "river",
    "savanna",
    "savanna_plateau",
    "small_end_islands",
    "snowy_beach",
    "snowy_plains",
    "snowy_slopes",
    "snowy_taiga",
    "soul_sand_valley",
    "sparse_jungle",
    "stony_peaks",
    "stony_shore",
    "sunflower_plains",
    "swamp",
    "taiga",
    "the_end",
    "the_void",
    "warm_ocean",
    "warped_forest",
    "windswept_forest",
    "windswept_gravelly_hills",
    "windswept_hills",
    "windswept_savanna",
    "wooded_badlands",
];

const BIOMES_PER_PAGE: usize = 10;

pub(super) fn commands() -> Vec<(&'static str, WorldeditCommand)> {
    vec![
        (
            "/",
            WorldeditCommand {
                arguments: &[arg!("state", String, "on, off or nothing to toggle", "")],
                execute_fn: execute_toggle_super_pickaxe,
                description: "Toggle the super pickaxe function",
                permission_node: "worldedit.superpickaxe",
                mutates_world: false,
                ..Default::default()
            },
        ),
        (
            "sp",
            WorldeditCommand {
                arguments: &[
                    arg!("mode", String, "single, area or recur", ""),
                    arg!("radius", String, "The radius of area and recur", ""),
                ],
                execute_fn: execute_super_pickaxe_mode,
                description: "Super pickaxe modes: single, area <radius> and recur <radius>",
                permission_node: "worldedit.superpickaxe",
                mutates_world: false,
                ..Default::default()
            },
        ),
        (
            "/setbiome",
            WorldeditCommand {
                arguments: &[arg!("biome", String, "The biome type", "")],
                flags: &[flag_of!('p', "Use your current position")],
                execute_fn: execute_set_biome,
                description: "Sets the biome of your current block or region",
                permission_node: "worldedit.biome.set",
                mutates_world: false,
                ..Default::default()
            },
        ),
        (
            "biomelist",
            WorldeditCommand {
                arguments: &[arg!("page", String, "The page of the list", "1")],
                execute_fn: execute_biome_list,
                description: "Gets all biomes available",
                permission_node: "worldedit.biome.list",
                mutates_world: false,
                ..Default::default()
            },
        ),
        (
            "biomeinfo",
            WorldeditCommand {
                flags: &[
                    flag_of!('t', "Use the block you are looking at"),
                    flag_of!('p', "Use your current position"),
                ],
                execute_fn: execute_biome_info,
                description: "Get the biome of the targeted block",
                permission_node: "worldedit.biome.info",
                mutates_world: false,
                ..Default::default()
            },
        ),
        (
            "/setskylight",
            WorldeditCommand {
                arguments: &[arg!("value", String, "The light level", "")],
                execute_fn: execute_set_light,
                description: "Set sky lighting in a selection",
                permission_node: "worldedit.light.set",
                requires_positions: true,
                mutates_world: false,
                ..Default::default()
            },
        ),
        (
            "/setblocklight",
            WorldeditCommand {
                arguments: &[arg!("value", String, "The light level", "")],
                execute_fn: execute_set_light,
                description: "Set block lighting in a selection",
                permission_node: "worldedit.light.set",
                requires_positions: true,
                mutates_world: false,
                ..Default::default()
            },
        ),
        (
            "/removelight",
            WorldeditCommand {
                execute_fn: execute_remove_light,
                description: "Removes lighting in a selection",
                permission_node: "worldedit.light.remove",
                requires_positions: true,
                mutates_world: false,
                ..Default::default()
            },
        ),
        (
            "/getlighting",
            WorldeditCommand {
                execute_fn: execute_get_lighting,
                description: "Get the light at a position",
                permission_node: "worldedit.light.fix",
                mutates_world: false,
                ..Default::default()
            },
        ),
        (
            "/fixlighting",
            WorldeditCommand {
                execute_fn: execute_fix_lighting,
                description: "Get the light at a position",
                permission_node: "worldedit.light.fix",
                requires_positions: true,
                mutates_world: false,
                ..Default::default()
            },
        ),
        (
            "remove",
            WorldeditCommand {
                arguments: &[
                    arg!("type", String, "The type of entity to remove"),
                    arg!("radius", Integer, "The radius of the cuboid to remove from"),
                ],
                plot_fn: Some(execute_remove),
                description: "Remove all entities of a type",
                permission_node: "worldedit.remove",
                mutates_world: false,
                ..Default::default()
            },
        ),
        (
            "butcher",
            WorldeditCommand {
                arguments: &[arg!("radius", Integer, "Radius to kill mobs in", "-1")],
                flags: &[
                    flag_of!('p', "Also kill pets"),
                    flag_of!('l', "Currently does nothing"),
                    flag_of!('a', "Also kill animals"),
                    flag_of!('n', "Also kill NPCs"),
                    flag_of!('g', "Also kill golems"),
                    flag_of!('b', "Also kill ambient mobs"),
                    flag_of!('t', "Also kill mobs with name tags"),
                    flag_of!('f', "Also kill all friendly mobs (shortcut for -abgnpt)"),
                    flag_of!('r', "Also destroy armor stands"),
                    flag_of!('w', "Also kill water mobs"),
                ],
                plot_fn: Some(execute_butcher),
                description: "Kill all or nearby mobs",
                permission_node: "worldedit.butcher",
                mutates_world: false,
                ..Default::default()
            },
        ),
        (
            "/removenear",
            WorldeditCommand {
                arguments: &[
                    arg!("block", Mask, "The mask of blocks to remove"),
                    arg!(
                        "size",
                        UnsignedInteger,
                        "The radius of the square to remove in",
                        "50"
                    ),
                ],
                execute_fn: execute_remove_near,
                reads_world: true,
                description: "Remove blocks near you",
                permission_node: "worldedit.removenear",
                ..Default::default()
            },
        ),
        (
            "/removeabove",
            WorldeditCommand {
                arguments: &[
                    arg!(
                        "size",
                        UnsignedInteger,
                        "The apothem of the square to remove from",
                        "1"
                    ),
                    arg!(
                        "height",
                        UnsignedInteger,
                        "The maximum height to remove from",
                        "4096"
                    ),
                ],
                execute_fn: execute_remove_above,
                description: "Remove blocks above your head",
                permission_node: "worldedit.removeabove",
                ..Default::default()
            },
        ),
        (
            "/removebelow",
            WorldeditCommand {
                arguments: &[
                    arg!(
                        "size",
                        UnsignedInteger,
                        "The apothem of the square to remove from",
                        "1"
                    ),
                    arg!(
                        "height",
                        UnsignedInteger,
                        "The maximum height to remove from",
                        "4096"
                    ),
                ],
                execute_fn: execute_remove_below,
                description: "Remove blocks below you",
                permission_node: "worldedit.removebelow",
                ..Default::default()
            },
        ),
        (
            "/nbtinfo",
            WorldeditCommand {
                execute_fn: execute_nbt_info,
                description: "View NBT info for a block",
                permission_node: "worldedit.nbtinfo",
                mutates_world: false,
                ..Default::default()
            },
        ),
        (
            "/calc",
            WorldeditCommand {
                arguments: &[arg!("expression", String, "Expression to evaluate")],
                execute_fn: execute_calc,
                description: "Evaluate a mathematical expression",
                permission_node: "worldedit.calc",
                mutates_world: false,
                ..Default::default()
            },
        ),
        (
            "/searchitem",
            WorldeditCommand {
                arguments: &[arg!("query", String, "Item name to search for")],
                flags: &[
                    flag_of!('b', "Only search for blocks"),
                    flag_of!('i', "Only search for items"),
                ],
                execute_fn: execute_search_item,
                description: "Search for an item",
                permission_node: "worldedit.searchitem",
                mutates_world: false,
                ..Default::default()
            },
        ),
        (
            "/confirm",
            WorldeditCommand {
                execute_fn: execute_confirm,
                description: "Confirm a command",
                permission_node: "fawe.confirm",
                mutates_world: false,
                ..Default::default()
            },
        ),
        (
            "we",
            WorldeditCommand {
                arguments: &[
                    arg!("subcommand", String, "threads, version, help or reload", ""),
                    arg!("command", String, "The command to show help for", ""),
                ],
                execute_fn: execute_we,
                description: "WorldEdit commands",
                mutates_world: false,
                ..Default::default()
            },
        ),
        (
            "/cancel",
            WorldeditCommand {
                plot_fn: Some(execute_cancel),
                description: "Cancel your current command",
                permission_node: "fawe.cancel",
                mutates_world: false,
                ..Default::default()
            },
        ),
        (
            "chunkinfo",
            WorldeditCommand {
                execute_fn: execute_chunk_info,
                description: "Get information about the chunk you are inside",
                permission_node: "worldedit.chunkinfo",
                mutates_world: false,
                ..Default::default()
            },
        ),
        (
            "listchunks",
            WorldeditCommand {
                execute_fn: execute_list_chunks,
                description: "List chunks that your selection includes",
                permission_node: "worldedit.listchunks",
                requires_positions: true,
                mutates_world: false,
                ..Default::default()
            },
        ),
        (
            "delchunks",
            WorldeditCommand {
                execute_fn: execute_delete_chunks,
                description: "Delete chunks that your selection includes",
                permission_node: "worldedit.delchunks",
                requires_positions: true,
                mutates_world: false,
                ..Default::default()
            },
        ),
        (
            "snapshot",
            WorldeditCommand {
                arguments: &[
                    arg!("subcommand", String, "list, use, sel, before or after", ""),
                    arg!("value", String, "The snapshot, date or index", ""),
                ],
                execute_fn: execute_snapshot,
                description: "Snapshot commands",
                mutates_world: false,
                ..Default::default()
            },
        ),
        (
            "restore",
            WorldeditCommand {
                arguments: &[arg!("snapshot", String, "The snapshot to restore", "")],
                execute_fn: execute_restore,
                description: "Restore the selection from a snapshot",
                permission_node: "worldedit.snapshots.restore",
                requires_positions: true,
                mutates_world: false,
                ..Default::default()
            },
        ),
        (
            "/wea",
            WorldeditCommand {
                execute_fn: execute_wea,
                description: "Bypass region restrictions",
                permission_node: "fawe.admin",
                mutates_world: false,
                ..Default::default()
            },
        ),
        (
            "/wer",
            WorldeditCommand {
                execute_fn: execute_wer,
                description: "Select your current allowed region",
                permission_node: "fawe.worldeditregion",
                mutates_world: false,
                ..Default::default()
            },
        ),
        (
            "/limit",
            WorldeditCommand {
                arguments: &[arg!("limit", Integer, "The limit to set", "-1")],
                execute_fn: execute_limit,
                description: "Modify block change limit",
                permission_node: "worldedit.limit",
                mutates_world: false,
                ..Default::default()
            },
        ),
        (
            "/timeout",
            WorldeditCommand {
                arguments: &[arg!(
                    "timeout",
                    Integer,
                    "The timeout in milliseconds",
                    "-1"
                )],
                execute_fn: execute_timeout,
                description: "Modify evaluation timeout time.",
                permission_node: "worldedit.timeout",
                mutates_world: false,
                ..Default::default()
            },
        ),
        (
            "/reorder",
            WorldeditCommand {
                arguments: &[arg!(
                    "mode",
                    String,
                    "The reorder mode: multi, fast or none",
                    ""
                )],
                execute_fn: execute_reorder,
                description: "Sets the reorder mode of WorldEdit",
                permission_node: "worldedit.reorder",
                mutates_world: false,
                ..Default::default()
            },
        ),
    ]
}

pub(super) fn aliases() -> Vec<(&'static str, &'static str)> {
    vec![
        ("superpickaxe", "sp"),
        ("pickaxe", "sp"),
        ("snap", "snapshot"),
    ]
}

fn usage(player: &Player, text: &str) {
    player.send_error_message(&format!("Usage: {text}"));
}

fn parse_toggle(text: &str) -> Result<Option<bool>, String> {
    match text.to_ascii_lowercase().as_str() {
        "" => Ok(None),
        "on" | "true" | "yes" => Ok(Some(true)),
        "off" | "false" | "no" => Ok(Some(false)),
        _ => Err(format!("Invalid value '{text}'. Use on or off.")),
    }
}

// ---------------------------------------------------------------------------------------------
// Super pickaxe
// ---------------------------------------------------------------------------------------------

/// The most blocks around the clicked one that area and recursive mode reach
const MAX_SUPER_PICKAXE_SIZE: i32 = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PickaxeMode {
    Single,
    Area(i32),
    Recursive(i32),
}

#[derive(Clone, Copy, Debug)]
struct SuperPickaxe {
    enabled: bool,
    mode: PickaxeMode,
}

impl Default for SuperPickaxe {
    fn default() -> Self {
        SuperPickaxe {
            enabled: false,
            mode: PickaxeMode::Single,
        }
    }
}

#[derive(Default)]
struct SessionState {
    pickaxe: SuperPickaxe,
    bypass: bool,
    limit: Option<i32>,
    timeout: Option<i32>,
    reorder: Option<Reorder>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Reorder {
    Multi,
    Fast,
    None,
}

impl Reorder {
    fn name(self) -> &'static str {
        match self {
            Reorder::Multi => "multi",
            Reorder::Fast => "fast",
            Reorder::None => "none",
        }
    }
}

static SESSIONS: LazyLock<Mutex<HashMap<u128, SessionState>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn with_session<R>(uuid: u128, f: impl FnOnce(&mut SessionState) -> R) -> R {
    let mut map = SESSIONS.lock().unwrap_or_else(|e| e.into_inner());
    f(map.entry(uuid).or_default())
}

/// Whether the player turned on `//wea`
pub(super) fn is_bypassing(uuid: u128) -> bool {
    with_session(uuid, |s| s.bypass)
}

fn execute_toggle_super_pickaxe(ctx: CommandExecuteContext<'_>) {
    let requested = match parse_toggle(ctx.arguments[0].unwrap_string()) {
        Ok(requested) => requested,
        Err(message) => return ctx.player.send_error_message(&message),
    };
    let uuid = ctx.player.uuid;
    let (was, now) = with_session(uuid, |s| {
        let was = s.pickaxe.enabled;
        s.pickaxe.enabled = requested.unwrap_or(!was);
        (was, s.pickaxe.enabled)
    });
    let message = match (requested.is_some(), was == now, now) {
        (true, true, true) => "Super pickaxe already enabled.",
        (true, true, false) => "Super pickaxe already disabled.",
        (_, _, true) => "Super pickaxe enabled.",
        (_, _, false) => "Super pickaxe disabled.",
    };
    ctx.player.send_worldedit_message(message);
}

fn execute_super_pickaxe_mode(ctx: CommandExecuteContext<'_>) {
    let player = &*ctx.player;
    let mode = ctx.arguments[0].unwrap_string().to_ascii_lowercase();
    let radius_text = ctx.arguments[1].unwrap_string();
    let (permission, new_mode, text) = match mode.as_str() {
        "single" => (
            "worldedit.superpickaxe",
            PickaxeMode::Single,
            "Mode is now single. Left click with a pickaxe. // to disable.",
        ),
        "area" | "recur" | "recursive" => {
            let Ok(radius) = radius_text.parse::<i32>() else {
                return usage(player, &format!("/sp {mode} <radius>"));
            };
            if radius > MAX_SUPER_PICKAXE_SIZE || radius < 0 {
                return player
                    .send_error_message(&format!("Maximum range is {MAX_SUPER_PICKAXE_SIZE}."));
            }
            if mode == "area" {
                (
                    "worldedit.superpickaxe.area",
                    PickaxeMode::Area(radius),
                    "Mode is now area. Left click with a pickaxe. // to disable.",
                )
            } else {
                (
                    "worldedit.superpickaxe.recursive",
                    PickaxeMode::Recursive(radius),
                    "Mode is now recursive. Left click with a pickaxe. // to disable.",
                )
            }
        }
        _ => return usage(player, "/sp <single|area|recur> [radius]"),
    };
    if !player.has_permission(permission) {
        return player.send_no_permission_message();
    }
    with_session(player.uuid, |s| {
        s.pickaxe.mode = new_mode;
        s.pickaxe.enabled = true;
    });
    player.send_worldedit_message(text);
}

/// Handles the left click of a player with the super pickaxe turned on. Returns whether the
/// click was used, then the block is not broken the normal way.
pub fn super_pickaxe_click(plot: &mut Plot, player_idx: usize, pos: BlockPos) -> bool {
    let player = &plot.players[player_idx];
    let pickaxe = with_session(player.uuid, |s| s.pickaxe);
    if !pickaxe.enabled || !player.has_permission("worldedit.superpickaxe") {
        return false;
    }
    let holds_pickaxe = player.inventory[player.selected_slot as usize + 36]
        .as_ref()
        .is_some_and(|item| item.item_type.get_name().ends_with("_pickaxe"));
    if !holds_pickaxe {
        return false;
    }
    let owner = player.uuid;
    let gmask = player.worldedit_mask.clone();

    let breakable = |world: &PlotWorld, pos: BlockPos| {
        (0..PLOT_BLOCK_HEIGHT).contains(&pos.y)
            && Plot::in_plot_bounds(world.x, world.z, pos.x, pos.z)
            && !matches!(world.get_block(pos), Block::Air)
    };
    let mut positions = Vec::new();
    match pickaxe.mode {
        PickaxeMode::Single => positions.push(pos),
        PickaxeMode::Area(radius) => {
            for x in -radius..=radius {
                for y in -radius..=radius {
                    for z in -radius..=radius {
                        positions.push(BlockPos::new(pos.x + x, pos.y + y, pos.z + z));
                    }
                }
            }
        }
        PickaxeMode::Recursive(radius) => {
            let id = plot.world.get_block(pos).get_id();
            let mut seen = HashSet::new();
            let mut queue = VecDeque::new();
            seen.insert(pos);
            queue.push_back(pos);
            while let Some(current) = queue.pop_front() {
                positions.push(current);
                for (dx, dy, dz) in [
                    (1, 0, 0),
                    (-1, 0, 0),
                    (0, 1, 0),
                    (0, -1, 0),
                    (0, 0, 1),
                    (0, 0, -1),
                ] {
                    let next = BlockPos::new(current.x + dx, current.y + dy, current.z + dz);
                    if (next.x - pos.x).abs() > radius
                        || (next.y - pos.y).abs() > radius
                        || (next.z - pos.z).abs() > radius
                        || !breakable(&plot.world, next)
                        || plot.world.get_block(next).get_id() != id
                        || !seen.insert(next)
                    {
                        continue;
                    }
                    queue.push_back(next);
                }
            }
        }
    }
    positions.retain(|p| breakable(&plot.world, *p));
    positions.retain(|p| {
        plot.world
            .get_block(*p)
            .get_name()
            .trim_start_matches("minecraft:")
            != "bedrock"
    });
    if positions.is_empty() {
        return true;
    }
    let Ok(air) = Pattern::parse("air") else {
        return true;
    };
    let job = ops::positions_job(owner, gmask, positions, air, None, pos).quiet();
    plot.reset_redpiler();
    plot.queue_edit_job(job);
    plot.run_edit_jobs(SLICE_BUDGET);
    true
}

// ---------------------------------------------------------------------------------------------
// Biomes
// ---------------------------------------------------------------------------------------------

fn execute_set_biome(ctx: CommandExecuteContext<'_>) {
    let player = &*ctx.player;
    let name = ctx.arguments[0].unwrap_string();
    if name.is_empty() {
        return usage(player, "//setbiome [-p] <biome>");
    }
    let short = name
        .strip_prefix("minecraft:")
        .unwrap_or(name)
        .to_ascii_lowercase();
    if !BIOMES.contains(&short.as_str()) {
        return player.send_error_message(&format!("Biome name '{name}' was not recognized."));
    }
    if !ctx.has_flag('p') && (player.first_position.is_none() || player.second_position.is_none()) {
        return player.send_error_message("Make a region selection first.");
    }
    player.send_error_message("Plots only have one biome, it can't be changed.");
}

fn execute_biome_list(ctx: CommandExecuteContext<'_>) {
    let player = &*ctx.player;
    let pages = BIOMES.len().div_ceil(BIOMES_PER_PAGE);
    let page = match ctx.arguments[0].unwrap_string().parse::<usize>() {
        Ok(page) if (1..=pages).contains(&page) => page,
        _ => return player.send_error_message("Invalid page number."),
    };
    let mut text = format!("Available Biomes (page {page}/{pages})");
    for biome in BIOMES
        .iter()
        .skip((page - 1) * BIOMES_PER_PAGE)
        .take(BIOMES_PER_PAGE)
    {
        text.push_str(&format!("\nminecraft:{biome}"));
    }
    player.send_worldedit_message(&text);
}

fn execute_biome_info(ctx: CommandExecuteContext<'_>) {
    let player = &*ctx.player;
    if ctx.has_flag('t') {
        let hit = ray_trace_block(
            ctx.plot,
            player.pos,
            player.pitch as f64,
            player.yaw as f64,
            300.0,
        );
        if hit.is_none() {
            return player.send_error_message("No block in sight!");
        }
        player.send_worldedit_message(&format!("Biomes at line of sight point: {PLOT_BIOME}"));
    } else if ctx.has_flag('p') {
        player.send_worldedit_message(&format!("Biomes at your position: {PLOT_BIOME}"));
    } else {
        if player.first_position.is_none() || player.second_position.is_none() {
            return player.send_error_message("Make a region selection first.");
        }
        player.send_worldedit_message(&format!("Biomes in your selection: {PLOT_BIOME}"));
    }
}

// ---------------------------------------------------------------------------------------------
// Light
// ---------------------------------------------------------------------------------------------

fn execute_set_light(ctx: CommandExecuteContext<'_>) {
    let value = ctx.arguments[0].unwrap_string();
    match value.parse::<i32>() {
        Ok(level) if (0..=15).contains(&level) => {}
        _ => return usage(ctx.player, "//setskylight|//setblocklight <0-15>"),
    }
    ctx.player.send_error_message(
        "Light is calculated from the blocks on plots, use light blocks to light an area.",
    );
}

fn execute_remove_light(ctx: CommandExecuteContext<'_>) {
    ctx.player.send_error_message(
        "Light is calculated from the blocks on plots, remove the light sources instead.",
    );
}

fn execute_get_lighting(ctx: CommandExecuteContext<'_>) {
    let pos = ctx.player.pos.block_pos();
    let light = (0..PLOT_BLOCK_HEIGHT)
        .contains(&pos.y)
        .then(|| ctx.plot.get_chunk(pos.x >> 4, pos.z >> 4))
        .flatten()
        .map(|chunk| {
            let (x, z) = ((pos.x & 15) as u32, (pos.z & 15) as u32);
            (
                chunk.light.block_light(x, pos.y as u32, z),
                chunk.light.sky_light(x, pos.y as u32, z),
            )
        });
    match light {
        Some((block, sky)) => ctx
            .player
            .send_worldedit_message(&format!("Light: {block} (block), {sky} (sky)")),
        None => ctx
            .player
            .send_error_message("You are outside of the plot."),
    }
}

fn selection_chunks(player: &Player) -> Vec<(i32, i32)> {
    let (a, b) = (
        player.first_position.unwrap(),
        player.second_position.unwrap(),
    );
    let (min, max) = (a.min(b), a.max(b));
    let mut chunks = Vec::new();
    for x in (min.x >> 4)..=(max.x >> 4) {
        for z in (min.z >> 4)..=(max.z >> 4) {
            chunks.push((x, z));
        }
    }
    chunks
}

fn execute_fix_lighting(ctx: CommandExecuteContext<'_>) {
    let chunks = selection_chunks(ctx.player).len();
    ctx.plot.initialize_light();
    ctx.player
        .send_worldedit_message(&format!("Lighting has been propagated in {chunks} chunks."));
}

// ---------------------------------------------------------------------------------------------
// Entities
// ---------------------------------------------------------------------------------------------

fn player_center(plot: &Plot, player_idx: usize) -> Vec3 {
    let pos = plot.players[player_idx].pos;
    Vec3::new(pos.x, pos.y, pos.z)
}

fn execute_remove(plot: &mut Plot, player_idx: usize, args: &[Argument], _flags: &[char]) {
    use super::super::entities::EntityKind;
    let kind = args[0].unwrap_string().to_ascii_lowercase();
    let radius = args[1].unwrap_int();
    if radius < -1 {
        return plot.players[player_idx]
            .send_error_message("Use -1 to remove all entities in loaded chunks");
    }
    let filter: fn(&EntityKind) -> bool = match kind
        .strip_prefix("minecraft:")
        .unwrap_or(&kind)
    {
        "all" => |_| true,
        "items" | "item" => |k| matches!(k, EntityKind::Item { .. }),
        "minecarts" | "minecart" => |k| matches!(k, EntityKind::Minecart { .. }),
        "tnt" => |k| matches!(k, EntityKind::PrimedTnt { .. }),
        "projectiles" | "arrows" | "ender_pearl" => |k| matches!(k, EntityKind::EnderPearl { .. }),
        "falling_block" => |k| matches!(k, EntityKind::FallingBlock { .. }),
        // Types that exist in the game but not on plots
        "paintings" | "itemframes" | "boats" | "xp" | "arrow" | "painting" | "item_frame"
        | "experience_orb" => |_| false,
        _ => {
            return plot.players[player_idx].send_error_message(
                "Acceptable types: projectiles, items, paintings, itemframes, boats, minecarts, tnt, xp, or all",
            )
        }
    };
    let center = player_center(plot, player_idx);
    let radius = (radius >= 0).then_some(radius as f64);
    let removed = plot.remove_entities_near(center, radius, filter);
    plot.players[player_idx]
        .send_worldedit_message(&format!("{removed} entities have been marked for removal."));
}

fn execute_butcher(plot: &mut Plot, player_idx: usize, args: &[Argument], _flags: &[char]) {
    // There are no mobs on plots, so nothing is ever killed
    let radius = args[0].unwrap_int();
    let player = &plot.players[player_idx];
    if radius < -1 {
        return player.send_error_message("Use -1 to remove all mobs in loaded chunks");
    }
    player.send_worldedit_message(&format!("0 mobs have been killed in a radius of {radius}."));
}

fn execute_cancel(plot: &mut Plot, player_idx: usize, _args: &[Argument], _flags: &[char]) {
    let uuid = plot.players[player_idx].uuid;
    let cancelled = plot.cancel_edit_jobs(uuid);
    plot.players[player_idx].send_worldedit_message(&format!("Cancelled {cancelled} edits."));
}

// ---------------------------------------------------------------------------------------------
// Removing blocks
// ---------------------------------------------------------------------------------------------

fn removal_job(
    ctx: &CommandExecuteContext<'_>,
    filter: Option<Mask>,
    min: BlockPos,
    max: BlockPos,
) -> EditJob {
    let air = Pattern::parse("air").expect("air is a pattern");
    ops::region_job(
        ctx.plot,
        ctx.player.uuid,
        ctx.player.worldedit_mask.clone(),
        filter,
        min,
        max,
        air,
        None,
        None,
    )
    .on_finish(|player, stats| {
        player.send_worldedit_message(&format!("{} blocks have been removed.", stats.blocks))
    })
}

fn execute_remove_near(ctx: CommandExecuteContext<'_>) {
    let size = ctx.arguments[1].unwrap_uint().clamp(1, 256) as i32;
    let filter = ctx.arguments[0].unwrap_mask().clone();
    let c = ctx.player.pos.block_pos();
    let job = removal_job(
        &ctx,
        Some(filter),
        BlockPos::new(c.x - size, c.y - size, c.z - size),
        BlockPos::new(c.x + size, c.y + size, c.z + size),
    );
    ctx.jobs.push_back(job);
}

fn height_argument(ctx: &CommandExecuteContext<'_>) -> i32 {
    // Without a height the whole world height is meant
    ctx.arguments[1]
        .unwrap_uint()
        .clamp(1, PLOT_BLOCK_HEIGHT as u32) as i32
}

fn execute_remove_above(ctx: CommandExecuteContext<'_>) {
    let size = ctx.arguments[0].unwrap_uint().max(1) as i32 - 1;
    let height = height_argument(&ctx);
    let c = ctx.player.pos.block_pos();
    let job = removal_job(
        &ctx,
        None,
        BlockPos::new(c.x - size, c.y, c.z - size),
        BlockPos::new(c.x + size, c.y + height - 1, c.z + size),
    );
    ctx.jobs.push_back(job);
}

fn execute_remove_below(ctx: CommandExecuteContext<'_>) {
    let size = ctx.arguments[0].unwrap_uint().max(1) as i32 - 1;
    let height = height_argument(&ctx);
    let c = ctx.player.pos.block_pos();
    let job = removal_job(
        &ctx,
        None,
        BlockPos::new(c.x - size, c.y - height + 1, c.z - size),
        BlockPos::new(c.x + size, c.y, c.z + size),
    );
    ctx.jobs.push_back(job);
}

// ---------------------------------------------------------------------------------------------
// Utility
// ---------------------------------------------------------------------------------------------

fn execute_nbt_info(ctx: CommandExecuteContext<'_>) {
    let player = &*ctx.player;
    let Some(pos) = ray_trace_block(
        ctx.plot,
        player.pos,
        player.pitch as f64,
        player.yaw as f64,
        300.0,
    ) else {
        return player.send_error_message("No block in sight!");
    };
    match ctx.plot.get_block_entity(pos) {
        Some(entity) => player.send_worldedit_message(&format!("{entity:?}")),
        None => player.send_error_message("Block has no NBT data."),
    }
}

fn execute_calc(ctx: CommandExecuteContext<'_>) {
    let expression = ctx.arguments[0].unwrap_string();
    ctx.player.send_error_message(&format!(
        "'{expression}' could not be parsed as a valid expression: 'expressions are not supported'"
    ));
}

fn execute_search_item(ctx: CommandExecuteContext<'_>) {
    let player = &*ctx.player;
    let query = ctx.arguments[0].unwrap_string().to_ascii_lowercase();
    let (blocks_only, items_only) = (ctx.has_flag('b'), ctx.has_flag('i'));
    if blocks_only && items_only {
        return player
            .send_error_message("You cannot use both the 'b' and 'i' flags simultaneously.");
    }
    if query.len() <= 2 {
        return player.send_error_message("Enter a longer search string (len > 2).");
    }
    player.send_worldedit_message("(Please wait... searching items.)");
    let mut found = Vec::new();
    for id in 0..2500 {
        let item = Item::from_id(id);
        if item.get_id() != id {
            continue;
        }
        let name = item.get_name();
        if !name.to_ascii_lowercase().contains(&query) {
            continue;
        }
        let is_block = Block::from_name(name).is_some();
        if (blocks_only && !is_block) || (items_only && is_block) {
            continue;
        }
        found.push(format!("{name} (#{id})"));
    }
    if found.is_empty() {
        return player.send_error_message("No items found.");
    }
    player.send_worldedit_message(&found.join("\n"));
}

fn execute_confirm(ctx: CommandExecuteContext<'_>) {
    ctx.player
        .send_error_message("You have no actions pending confirmation.");
}

fn execute_we(ctx: CommandExecuteContext<'_>) {
    let sub = ctx.arguments[0].unwrap_string().to_ascii_lowercase();
    match sub.as_str() {
        "version" | "ver" => ctx.player.send_worldedit_message(&format!(
            "FAWE version:\n - Date unknown\n - Commit unknown\n - Build {}\n - Platform Cinnabar",
            env!("CARGO_PKG_VERSION")
        )),
        "reload" => {
            if !ctx.player.has_permission("worldedit.reload") {
                return ctx.player.send_no_permission_message();
            }
            ctx.player.send_worldedit_message("Configuration reloaded!");
        }
        "threads" => {
            if !ctx.player.has_permission("worldedit.threads") {
                return ctx.player.send_no_permission_message();
            }
            let threads = std::fs::read_dir("/proc/self/task").map_or(0, |dir| dir.count());
            ctx.player
                .send_worldedit_message(&format!("Threads: {threads}"));
        }
        "help" | "" => {
            if !ctx.player.has_permission("worldedit.help") {
                return ctx.player.send_no_permission_message();
            }
            let command = ctx.arguments[1].unwrap_string().clone();
            if command.is_empty() {
                let mut names: Vec<&str> = COMMANDS.keys().copied().collect();
                names.sort_unstable();
                let list: Vec<String> = names.iter().map(|n| format!("/{n}")).collect();
                return ctx
                    .player
                    .send_worldedit_message(&format!("Commands: {}", list.join(", ")));
            }
            let help_ctx = CommandExecuteContext {
                arguments: vec![Argument::String(command.trim_start_matches('/').to_owned())],
                ..ctx
            };
            execute_help(help_ctx);
        }
        _ => usage(ctx.player, "/we <threads|version|help|reload>"),
    }
}

// ---------------------------------------------------------------------------------------------
// Chunks and snapshots
// ---------------------------------------------------------------------------------------------

fn execute_chunk_info(ctx: CommandExecuteContext<'_>) {
    let pos = ctx.player.pos.block_pos();
    let (cx, cz) = (pos.x >> 4, pos.z >> 4);
    ctx.player.send_worldedit_message(&format!(
        "Chunk: {cx}, {cz}\nMcRegion: region/r.{}.{}.mca",
        cx >> 5,
        cz >> 5
    ));
}

fn execute_list_chunks(ctx: CommandExecuteContext<'_>) {
    let chunks = selection_chunks(ctx.player);
    let (a, b) = (
        ctx.player.first_position.unwrap(),
        ctx.player.second_position.unwrap(),
    );
    let mut text = format!(
        "Listing chunks for: ({}, {}, {}) - ({}, {}, {})",
        a.x, a.y, a.z, b.x, b.y, b.z
    );
    for (x, z) in chunks.iter().take(20) {
        text.push_str(&format!("\nChunk: {x}, {z}"));
    }
    if chunks.len() > 20 {
        text.push_str(&format!("\n... and {} more", chunks.len() - 20));
    }
    ctx.player.send_worldedit_message(&text);
}

fn execute_delete_chunks(ctx: CommandExecuteContext<'_>) {
    ctx.player
        .send_error_message("Chunks of plots can't be deleted.");
}

fn execute_snapshot(ctx: CommandExecuteContext<'_>) {
    let player = &*ctx.player;
    let sub = ctx.arguments[0].unwrap_string().to_ascii_lowercase();
    let permission = match sub.as_str() {
        "list" => "worldedit.snapshots.list",
        "use" | "sel" | "before" | "after" => "worldedit.snapshots.restore",
        _ => return usage(player, "/snapshot <list|use|sel|before|after>"),
    };
    if !player.has_permission(permission) {
        return player.send_no_permission_message();
    }
    player.send_error_message("Snapshot/backup restore is not configured.");
}

fn execute_restore(ctx: CommandExecuteContext<'_>) {
    ctx.player
        .send_error_message("Snapshot/backup restore is not configured.");
}

// ---------------------------------------------------------------------------------------------
// Options
// ---------------------------------------------------------------------------------------------

fn execute_wea(ctx: CommandExecuteContext<'_>) {
    let bypassing = with_session(ctx.player.uuid, |s| {
        s.bypass = !s.bypass;
        s.bypass
    });
    ctx.player.send_worldedit_message(if bypassing {
        "Currently bypassing FAWE restriction."
    } else {
        "Your FAWE edits are now restricted."
    });
}

fn execute_wer(ctx: CommandExecuteContext<'_>) {
    use crate::plot::PLOT_WIDTH;
    let size = PLOT_WIDTH * 16;
    let min = BlockPos::new(ctx.plot.x * size, 0, ctx.plot.z * size);
    let max = BlockPos::new(min.x + size - 1, PLOT_BLOCK_HEIGHT - 1, min.z + size - 1);
    ctx.player.worldedit_set_first_position(min);
    ctx.player.worldedit_set_second_position(max);
}

/// The block change limit a player may set
const MAX_LIMIT: i32 = 100_000_000;
const DEFAULT_TIMEOUT: i32 = 3000;
const MAX_TIMEOUT: i32 = 30_000;

fn execute_limit(ctx: CommandExecuteContext<'_>) {
    let requested = ctx.arguments[0].unwrap_int();
    let limit = if requested == -1 {
        MAX_LIMIT
    } else {
        requested
    };
    if limit > MAX_LIMIT {
        return ctx
            .player
            .send_error_message(&format!("Your maximum allowable limit is {MAX_LIMIT}."));
    }
    if limit < 1 {
        return usage(ctx.player, "//limit [limit]");
    }
    with_session(ctx.player.uuid, |s| s.limit = Some(limit));
    let mut text = format!("Block change limit set to {limit}.");
    if limit != MAX_LIMIT {
        text.push_str(" (Use //limit to go back to the default.)");
    }
    ctx.player.send_worldedit_message(&text);
}

fn execute_timeout(ctx: CommandExecuteContext<'_>) {
    let requested = ctx.arguments[0].unwrap_int();
    let timeout = if requested == -1 {
        DEFAULT_TIMEOUT
    } else {
        requested
    };
    if timeout > MAX_TIMEOUT {
        return ctx.player.send_error_message(&format!(
            "Your maximum allowable timeout is {MAX_TIMEOUT}ms."
        ));
    }
    if timeout < 1 {
        return usage(ctx.player, "//timeout [timeout]");
    }
    with_session(ctx.player.uuid, |s| s.timeout = Some(timeout));
    let mut text = format!("Timeout time set to {timeout}ms.");
    if timeout != DEFAULT_TIMEOUT {
        text.push_str(" (Use //timeout to go back to the default.)");
    }
    ctx.player.send_worldedit_message(&text);
}

fn execute_reorder(ctx: CommandExecuteContext<'_>) {
    let text = ctx.arguments[0].unwrap_string().to_ascii_lowercase();
    let uuid = ctx.player.uuid;
    if text.is_empty() {
        let mode = with_session(uuid, |s| s.reorder.unwrap_or(Reorder::Fast));
        return ctx
            .player
            .send_worldedit_message(&format!("The reorder mode is {}", mode.name()));
    }
    let mode = match text.as_str() {
        "multi" => Reorder::Multi,
        "fast" => Reorder::Fast,
        "none" => Reorder::None,
        _ => return usage(ctx.player, "//reorder [multi|fast|none]"),
    };
    with_session(uuid, |s| s.reorder = Some(mode));
    ctx.player
        .send_worldedit_message(&format!("The reorder mode is now {}", mode.name()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_aliases_resolve() {
        for (alias, target) in aliases() {
            assert!(
                commands().iter().any(|(name, _)| *name == target),
                "{alias}"
            );
        }
    }

    #[test]
    fn biome_names_are_sorted_and_unique() {
        let mut sorted = BIOMES.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted, BIOMES);
    }

    #[test]
    fn toggles_parse() {
        assert_eq!(parse_toggle(""), Ok(None));
        assert_eq!(parse_toggle("ON"), Ok(Some(true)));
        assert_eq!(parse_toggle("off"), Ok(Some(false)));
        assert!(parse_toggle("maybe").is_err());
    }

    #[test]
    fn plot_commands_exist() {
        for name in [
            "/",
            "sp",
            "/setbiome",
            "biomelist",
            "biomeinfo",
            "/setskylight",
            "/setblocklight",
            "/getlighting",
            "/removelight",
            "/fixlighting",
            "remove",
            "butcher",
            "/removenear",
            "/removeabove",
            "/removebelow",
            "/nbtinfo",
            "/calc",
            "/searchitem",
            "/confirm",
            "we",
            "/cancel",
            "chunkinfo",
            "listchunks",
            "delchunks",
            "snapshot",
            "restore",
            "/wea",
            "/wer",
            "/limit",
            "/timeout",
            "/reorder",
        ] {
            assert!(COMMANDS.contains_key(name), "{name}");
        }
    }

    #[test]
    fn defaults_of_the_commands_parse() {
        for (name, command) in commands() {
            for arg in command.arguments {
                if let Some(default) = arg.default_str {
                    let ok = match arg.argument_type {
                        ArgumentType::UnsignedInteger => default.parse::<u32>().is_ok(),
                        ArgumentType::Integer => default.parse::<i32>().is_ok(),
                        _ => true,
                    };
                    assert!(ok, "{name} {}", arg.name);
                }
            }
        }
    }
}
