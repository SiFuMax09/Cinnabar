//! Support for block states and items that have no dedicated variant in the generated `Block` and
//! `Item` enums.
//!
//! Only blocks that the server has special behavior for (redstone components and a few more) get
//! their own variant. Every other block is represented by [`Block::Generic`] holding its state
//! id, and the tables in this module describe it. The tables are generated from the Minecraft
//! data generator and a probe of the server, see `mc_data/gen_generic_blocks.py`.

use crate::blocks::{FlipDirection, RotateAmt};
use std::collections::HashMap;
use std::sync::LazyLock;

pub struct GenericBlock {
    pub name: &'static str,
    pub first_id: u32,
    pub default_id: u32,
    /// The properties in the order of the state ids (the last one changes the fastest) together
    /// with all values they can have.
    pub props: Vec<(&'static str, Vec<&'static str>)>,
    /// How much the state id changes when the value index of the property at the same position
    /// in `props` goes up by one.
    strides: Vec<u32>,
    num_states: u32,
}

impl GenericBlock {
    fn value_indices(&self, id: u32) -> impl Iterator<Item = usize> + '_ {
        let offset = id - self.first_id;
        self.props
            .iter()
            .zip(&self.strides)
            .map(move |((_, values), stride)| (offset / stride) as usize % values.len())
    }

    fn prop_index(&self, name: &str) -> Option<usize> {
        self.props.iter().position(|(prop, _)| *prop == name)
    }
}

static BLOCKS: LazyLock<Vec<GenericBlock>> = LazyLock::new(|| {
    let data = include_str!("../../../mc_data/generic_blocks.txt");
    data.lines()
        .map(|line| {
            let mut parts = line.splitn(4, ' ');
            let name = parts.next().unwrap();
            let first_id = parts.next().unwrap().parse().unwrap();
            let default_id = parts.next().unwrap().parse().unwrap();
            let props: Vec<(&str, Vec<&str>)> = parts
                .next()
                .unwrap_or("")
                .split(';')
                .filter(|spec| !spec.is_empty())
                .map(|spec| {
                    let (prop, values) = spec.split_once('=').unwrap();
                    (prop, values.split(',').collect())
                })
                .collect();
            let mut strides = vec![1; props.len()];
            for i in (0..props.len().saturating_sub(1)).rev() {
                strides[i] = strides[i + 1] * props[i + 1].1.len() as u32;
            }
            let num_states = props
                .iter()
                .map(|(_, values)| values.len() as u32)
                .product();
            GenericBlock {
                name,
                first_id,
                default_id,
                props,
                strides,
                num_states,
            }
        })
        .collect()
});

static BY_NAME: LazyLock<HashMap<&'static str, usize>> = LazyLock::new(|| {
    BLOCKS
        .iter()
        .enumerate()
        .map(|(idx, block)| (block.name, idx))
        .collect()
});

/// Two bytes per block state: flags and the note block instrument
static ATTRS: &[u8] = include_bytes!("../../../mc_data/block_attrs.bin");

/// Six bytes per block state, see `collision_box`
static COLLISION: &[u8] = include_bytes!("../../../mc_data/block_collision.bin");

const COLLISION_OFFSET: i32 = 16;

/// Two bytes per block state, see `explosion_resistance`
static RESISTANCE: &[u8] = include_bytes!("../../../mc_data/block_resistance.bin");

/// One byte per block state, see `push_reaction`
static PISTON: &[u8] = include_bytes!("../../../mc_data/block_piston.bin");
/// Two bytes per block state, see `light_emission`, `light_opacity` and `light_occluding_faces`
static LIGHT: &[u8] = include_bytes!("../../../mc_data/block_light.bin");

const FLAG_SOLID: u8 = 1;
const FLAG_STURDY_TOP: u8 = 2;

static ITEM_NAMES: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
    include_str!("../../../mc_data/item_names.txt")
        .lines()
        .collect()
});

static ITEM_IDS: LazyLock<HashMap<&'static str, u32>> = LazyLock::new(|| {
    ITEM_NAMES
        .iter()
        .enumerate()
        .map(|(id, name)| (*name, id as u32))
        .collect()
});

/// The block the state id belongs to, if it is a valid state id
pub fn block(id: u32) -> Option<&'static GenericBlock> {
    let idx = BLOCKS.partition_point(|block| block.first_id <= id);
    let block = &BLOCKS[idx.checked_sub(1)?];
    (id < block.first_id + block.num_states).then_some(block)
}

/// The range of state ids of the block with the name
fn state_range_or_empty(name: &str) -> std::ops::Range<u32> {
    state_range(name).unwrap_or(0..0)
}

static SCULK_SENSOR_IDS: LazyLock<std::ops::Range<u32>> =
    LazyLock::new(|| state_range_or_empty("minecraft:sculk_sensor"));
static CALIBRATED_SCULK_SENSOR_IDS: LazyLock<std::ops::Range<u32>> =
    LazyLock::new(|| state_range_or_empty("minecraft:calibrated_sculk_sensor"));

/// Whether the state is of a sculk sensor, which has no variant of its own in `Block`
pub fn is_sculk_sensor(id: u32) -> bool {
    SCULK_SENSOR_IDS.contains(&id) || CALIBRATED_SCULK_SENSOR_IDS.contains(&id)
}

pub fn is_calibrated_sculk_sensor(id: u32) -> bool {
    CALIBRATED_SCULK_SENSOR_IDS.contains(&id)
}

pub fn state_name(id: u32) -> &'static str {
    block(id).map_or("minecraft:air", |block| block.name)
}

/// The default state of a block, if there is a block with this name
pub fn default_state(name: &str) -> Option<u32> {
    BY_NAME.get(name).map(|&idx| BLOCKS[idx].default_id)
}

/// All properties of a state with their current values
pub fn properties(id: u32) -> Vec<(&'static str, &'static str)> {
    let Some(block) = block(id) else {
        return Vec::new();
    };
    block
        .props
        .iter()
        .zip(block.value_indices(id))
        .map(|((name, values), idx)| (*name, values[idx]))
        .collect()
}

/// The values the property can have for the block of this state
pub fn property_values(id: u32, name: &str) -> &'static [&'static str] {
    block(id)
        .and_then(|block| block.prop_index(name).map(|idx| &block.props[idx].1[..]))
        .unwrap_or(&[])
}

pub fn get_property(id: u32, name: &str) -> Option<&'static str> {
    properties(id)
        .into_iter()
        .find_map(|(prop, value)| (prop == name).then_some(value))
}

/// Returns the state with one property changed. Unknown properties and values are ignored.
pub fn with_property(id: u32, name: &str, value: &str) -> u32 {
    let Some(block) = block(id) else {
        return id;
    };
    let Some(prop_idx) = block.prop_index(name) else {
        return id;
    };
    let Some(new_idx) = block.props[prop_idx].1.iter().position(|v| *v == value) else {
        return id;
    };
    let stride = block.strides[prop_idx];
    let old_idx = block.value_indices(id).nth(prop_idx).unwrap();
    (id - old_idx as u32 * stride) + new_idx as u32 * stride
}

/// Changes the properties of a state, ignoring unknown properties and values
pub fn set_properties(mut id: u32, props: &HashMap<&str, &str>) -> u32 {
    for (name, value) in props {
        id = with_property(id, name, value);
    }
    id
}

fn flags(id: u32) -> u8 {
    ATTRS.get(id as usize * 2).copied().unwrap_or(0)
}

/// The bounding box of the collision shape of the block state, as
/// `[min_x, min_y, min_z, max_x, max_y, max_z]` relative to the block. It is `None` for blocks
/// that entities can walk through.
pub fn collision_box(id: u32) -> Option<[f64; 6]> {
    let bytes = COLLISION.get(id as usize * 6..id as usize * 6 + 6)?;
    if bytes[0] == 0xFF {
        return None;
    }
    let mut result = [0.0; 6];
    for (value, byte) in result.iter_mut().zip(bytes) {
        *value = (*byte as i32 - COLLISION_OFFSET) as f64 / 16.0;
    }
    Some(result)
}

/// How much of an explosion the block withstands. Blocks that can't be destroyed by explosions
/// are `f32::INFINITY`.
pub fn explosion_resistance(id: u32) -> f32 {
    let Some(bytes) = RESISTANCE.get(id as usize * 2..id as usize * 2 + 2) else {
        return 0.0;
    };
    match u16::from_le_bytes([bytes[0], bytes[1]]) {
        u16::MAX => f32::INFINITY,
        tenths => tenths as f32 / 10.0,
    }
}

/// What a piston does with a block it tries to move
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushReaction {
    Normal,
    /// The block breaks when it is pushed
    Destroy,
    /// The block can't be moved
    Block,
    /// The block can only be pushed by the piston that is directly attached to it, not by others
    /// in the same line
    PushOnly,
}

fn piston_attrs(id: u32) -> u8 {
    PISTON.get(id as usize).copied().unwrap_or(0)
}

pub fn push_reaction(id: u32) -> PushReaction {
    match piston_attrs(id) & 3 {
        0 => PushReaction::Normal,
        1 => PushReaction::Destroy,
        2 => PushReaction::Block,
        _ => PushReaction::PushOnly,
    }
}

/// Whether the block can't be broken, like bedrock
pub fn is_indestructible(id: u32) -> bool {
    piston_attrs(id) & 4 != 0
}

/// Whether the block state belongs to a block that has a block entity in Minecraft. Pistons
/// refuse to move those.
pub fn has_block_entity(id: u32) -> bool {
    piston_attrs(id) & 8 != 0
}

/// How much light the block state emits (0..=15)
pub fn light_emission(id: u32) -> u8 {
    LIGHT.get(id as usize * 2).map_or(0, |byte| byte & 0xF)
}

/// How much the block dims light passing through it (0..=15). Blocks with 0 let light and sky
/// light straight down pass without loss.
pub fn light_opacity(id: u32) -> u8 {
    LIGHT.get(id as usize * 2).map_or(0, |byte| byte >> 4)
}

/// A mask of the faces of the block whose shape fully covers the face, so that light can't pass
/// through them even if the block has no opacity. Bit n is the face in direction n in the order
/// down, up, north, south, west, east.
pub fn light_occluding_faces(id: u32) -> u8 {
    LIGHT.get(id as usize * 2 + 1).copied().unwrap_or(0)
}

/// Whether redstone power can pass through the block
pub fn is_solid(id: u32) -> bool {
    flags(id) & FLAG_SOLID != 0
}

/// Whether redstone can be placed on top of the block
pub fn is_cube(id: u32) -> bool {
    flags(id) & FLAG_STURDY_TOP != 0
}

/// Blocks that are full blocks but don't conduct redstone power, like glass
pub fn is_transparent(id: u32) -> bool {
    flags(id) & (FLAG_SOLID | FLAG_STURDY_TOP) == FLAG_STURDY_TOP
}

/// The index of the note block instrument the block below a note block selects, which is the
/// position in the `Instrument` enum
pub fn instrument_index(id: u32) -> usize {
    ATTRS.get(id as usize * 2 + 1).copied().unwrap_or(0) as usize
}

pub fn item_id(name: &str) -> Option<u32> {
    ITEM_IDS.get(name).copied()
}

pub fn item_name(id: u32) -> &'static str {
    ITEM_NAMES
        .get(id as usize)
        .copied()
        .unwrap_or("minecraft:air")
}

/// How many items of this kind fit in a stack. The data of the items without a dedicated variant
/// has no stack sizes, so they are derived from the names the way Minecraft registers them.
pub fn item_max_stack(id: u32) -> u32 {
    let name = item_name(id).trim_start_matches("minecraft:");
    const SINGLE_SUFFIXES: &[&str] = &[
        "_sword",
        "_pickaxe",
        "_axe",
        "_shovel",
        "_hoe",
        "_helmet",
        "_chestplate",
        "_leggings",
        "_boots",
        "_boat",
        "_raft",
        "_minecart",
        "_shulker_box",
        "_bed",
        "_spear",
        "_horse_armor",
        "_nautilus_armor",
        "_harness",
        "_pottery_sherd",
        "_bundle",
    ];
    const SINGLE: &[&str] = &[
        "bow",
        "crossbow",
        "trident",
        "shield",
        "elytra",
        "shears",
        "flint_and_steel",
        "fishing_rod",
        "saddle",
        "carrot_on_a_stick",
        "warped_fungus_on_a_stick",
        "cake",
        "written_book",
        "writable_book",
        "enchanted_book",
        "totem_of_undying",
        "minecart",
        "potion",
        "splash_potion",
        "lingering_potion",
        "mace",
        "brush",
        "spyglass",
        "compass",
        "recovery_compass",
        "clock",
        "shulker_box",
    ];
    const SIXTEEN_SUFFIXES: &[&str] = &["_sign", "_hanging_sign", "_banner"];
    const SIXTEEN: &[&str] = &[
        "ender_pearl",
        "snowball",
        "egg",
        "bucket",
        "honey_bottle",
        "armor_stand",
        "wind_charge",
    ];
    if name.starts_with("music_disc_")
        || name.ends_with("_bucket") && name != "bucket"
        || SINGLE.contains(&name)
        || SINGLE_SUFFIXES.iter().any(|suffix| name.ends_with(suffix))
    {
        1
    } else if SIXTEEN.contains(&name)
        || SIXTEEN_SUFFIXES.iter().any(|suffix| name.ends_with(suffix))
    {
        16
    } else {
        64
    }
}

/// Whether the item places a block with the same name
pub fn item_is_block(id: u32) -> bool {
    BY_NAME.contains_key(item_name(id))
}

const DIRECTIONS: [&str; 4] = ["north", "east", "south", "west"];

fn rotate_direction(dir: &str, quarter_turns: usize) -> &str {
    match DIRECTIONS.iter().position(|d| *d == dir) {
        Some(idx) => DIRECTIONS[(idx + quarter_turns) % 4],
        None => dir,
    }
}

fn flip_direction(dir: &str, flip: FlipDirection) -> &str {
    match (flip, dir) {
        (FlipDirection::FlipX, "east") => "west",
        (FlipDirection::FlipX, "west") => "east",
        (FlipDirection::FlipZ, "north") => "south",
        (FlipDirection::FlipZ, "south") => "north",
        _ => dir,
    }
}

/// Applies `f` to every direction in a value like `ascending_east` or `south_west`
fn map_directions(value: &str, f: impl Fn(&str) -> String) -> String {
    value.split('_').map(f).collect::<Vec<_>>().join("_")
}

fn swap_left_right(value: &str) -> String {
    match value {
        "left" => "right".to_owned(),
        "right" => "left".to_owned(),
        _ => map_directions(value, |part| match part {
            "left" => "right".to_owned(),
            "right" => "left".to_owned(),
            other => other.to_owned(),
        }),
    }
}

/// Transforms a state by remapping its properties: `f` gets the name and the value of every
/// property and returns the new value for the property at `new name`
fn transform(
    id: u32,
    rename: impl Fn(&str) -> String,
    value_of: impl Fn(&str, &str) -> String,
) -> u32 {
    let props = properties(id);
    let mut result = id;
    for (name, value) in &props {
        // The value that ends up in `name` comes from the property that is renamed to `name`
        let (source_name, source_value) = props
            .iter()
            .find(|(source, _)| rename(source) == *name)
            .map_or((*name, *value), |(source, value)| (*source, *value));
        let new_value = value_of(source_name, source_value);
        result = with_property(result, name, &new_value);
    }
    result
}

pub fn rotate(id: u32, amt: RotateAmt) -> u32 {
    let turns = match amt {
        RotateAmt::Rotate90 => 1,
        RotateAmt::Rotate180 => 2,
        RotateAmt::Rotate270 => 3,
    };
    transform(
        id,
        // Properties named after a direction (fences, walls, panes, ...) move around
        |name| rotate_direction(name, turns).to_owned(),
        |name, value| match name {
            "axis" if turns % 2 == 1 => match value {
                "x" => "z".to_owned(),
                "z" => "x".to_owned(),
                other => other.to_owned(),
            },
            "rotation" => value
                .parse::<usize>()
                .map_or(value.to_owned(), |rot| ((rot + 4 * turns) % 16).to_string()),
            // Directions can appear in values too, like `facing=north` and `shape=north_south`
            _ => map_directions(value, |part| rotate_direction(part, turns).to_owned()),
        },
    )
}

pub fn flip(id: u32, dir: FlipDirection) -> u32 {
    transform(
        id,
        |name| flip_direction(name, dir).to_owned(),
        |name, value| match name {
            "rotation" => value.parse::<usize>().map_or(value.to_owned(), |rot| {
                let mirrored = match dir {
                    FlipDirection::FlipX => 16 - rot,
                    FlipDirection::FlipZ => 8 + 16 - rot,
                };
                (mirrored % 16).to_string()
            }),
            // Mirroring swaps the sides of stairs, doors and chests
            "shape" | "hinge" | "type" if value.contains("left") || value.contains("right") => {
                swap_left_right(value)
            }
            _ => map_directions(value, |part| flip_direction(part, dir).to_owned()),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_roundtrip() {
        for block in BLOCKS.iter() {
            for id in block.first_id..block.first_id + block.num_states {
                let mut new_id = block.default_id;
                for (name, value) in properties(id) {
                    new_id = with_property(new_id, name, value);
                }
                assert_eq!(new_id, id, "{}", block.name);
            }
        }
    }

    #[test]
    fn log_axis() {
        let id = default_state("minecraft:oak_log").unwrap();
        assert_eq!(get_property(id, "axis"), Some("y"));
        let rotated = rotate(with_property(id, "axis", "x"), RotateAmt::Rotate90);
        assert_eq!(get_property(rotated, "axis"), Some("z"));
        assert!(is_solid(id));
        assert!(is_cube(id));
    }

    #[test]
    fn stairs_transform() {
        let mut id = default_state("minecraft:oak_stairs").unwrap();
        id = with_property(id, "facing", "north");
        id = with_property(id, "shape", "inner_left");
        let rotated = rotate(id, RotateAmt::Rotate90);
        assert_eq!(get_property(rotated, "facing"), Some("east"));
        assert_eq!(get_property(rotated, "shape"), Some("inner_left"));
        let flipped = flip(id, FlipDirection::FlipX);
        assert_eq!(get_property(flipped, "facing"), Some("north"));
        assert_eq!(get_property(flipped, "shape"), Some("inner_right"));
    }

    #[test]
    fn pane_sides_rotate() {
        let mut id = default_state("minecraft:glass_pane").unwrap();
        id = with_property(id, "north", "true");
        let rotated = rotate(id, RotateAmt::Rotate90);
        assert_eq!(get_property(rotated, "east"), Some("true"));
        assert_eq!(get_property(rotated, "north"), Some("false"));
    }

    #[test]
    fn light_properties() {
        let id = |name| default_state(name).unwrap();
        assert_eq!(light_emission(id("minecraft:glowstone")), 15);
        assert_eq!(light_emission(id("minecraft:torch")), 14);
        assert_eq!(light_emission(id("minecraft:stone")), 0);
        assert_eq!(light_opacity(id("minecraft:stone")), 15);
        assert_eq!(light_opacity(id("minecraft:glass")), 0);
        assert_eq!(light_opacity(id("minecraft:air")), 0);
        assert_eq!(light_opacity(id("minecraft:water")), 1);
        assert_eq!(light_opacity(id("minecraft:oak_leaves")), 1);
        assert!(light_occluding_faces(id("minecraft:oak_stairs")).count_ones() >= 1);
    }

    #[test]
    fn glass_is_not_solid() {
        let id = default_state("minecraft:glass").unwrap();
        assert!(!is_solid(id));
        assert!(is_cube(id));
        assert!(is_transparent(id));
    }
}

#[cfg(test)]
mod item_tests {
    use super::*;
    use crate::items::Item;

    #[test]
    fn block_items_are_placeable() {
        for name in [
            "minecraft:oak_fence_gate",
            "minecraft:chiseled_bookshelf",
            "minecraft:oak_log",
            "minecraft:oak_stairs",
        ] {
            let id = item_id(name).unwrap();
            let item = Item::from_id(id);
            assert!(item.is_block(), "{name} {item:?}");
            assert_eq!(item.get_name(), name);
            assert!(default_state(name).is_some());
        }
    }
}

/// The state ids of a block, if there is a block with this name
pub fn state_range(name: &str) -> Option<std::ops::Range<u32>> {
    let block = &BLOCKS[*BY_NAME.get(name)?];
    Some(block.first_id..block.first_id + block.num_states)
}
