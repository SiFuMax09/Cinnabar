//! This implements Sponge Schematic Specification ver. 2
//! https://github.com/SpongePowered/Schematic-Specification/blob/master/versions/schematic-2.md

use anyhow::{bail, Result};
use cinnabar_blocks::block_entities::BlockEntity;
use cinnabar_blocks::blocks::Block;
use cinnabar_blocks::BlockPos;
use cinnabar_world::{World, MC_DATA_VERSION};
use rustc_hash::FxHashMap;
use serde::Serialize;
use std::fs::{self, File};
use std::path::Path;

/// A cuboid of blocks together with the block entities in it.
///
/// The blocks are stored as raw state ids in `x`, then `z`, then `y` order (the same order the
/// Sponge format uses). A `u16` is enough because there are fewer than 2^15 block states.
#[derive(Clone, Debug)]
pub struct WorldEditClipboard {
    pub offset_x: i32,
    pub offset_y: i32,
    pub offset_z: i32,
    pub size_x: u32,
    pub size_y: u32,
    pub size_z: u32,
    pub data: Vec<u16>,
    pub block_entities: FxHashMap<BlockPos, BlockEntity>,
}

impl WorldEditClipboard {
    pub fn volume(&self) -> usize {
        self.size_x as usize * self.size_y as usize * self.size_z as usize
    }

    /// The index in `data` of the block at the position relative to the clipboard's corner
    pub fn index(&self, x: u32, y: u32, z: u32) -> usize {
        (y as usize * self.size_z as usize + z as usize) * self.size_x as usize + x as usize
    }

    pub fn get(&self, x: u32, y: u32, z: u32) -> u32 {
        self.data[self.index(x, y, z)] as u32
    }
}

/// Blocks that were renamed since older versions. Schematics are matched by name, so the old
/// names have to be translated. Only used when the name is not a block any more.
fn renamed_block(name: &str) -> &str {
    match name {
        "minecraft:chain" => "minecraft:iron_chain",
        other => other,
    }
}

/// Places the clipboard in any world with `pos` at the origin of the clipboard. The server has
/// its own faster implementation for plots, this is for tools and tests.
pub fn paste_clipboard<W: World>(
    world: &mut W,
    cb: &WorldEditClipboard,
    pos: BlockPos,
    ignore_air: bool,
) {
    let base = BlockPos::new(
        pos.x - cb.offset_x,
        pos.y - cb.offset_y,
        pos.z - cb.offset_z,
    );
    let mut i = 0;
    for y in 0..cb.size_y as i32 {
        for z in 0..cb.size_z as i32 {
            for x in 0..cb.size_x as i32 {
                let entry = cb.data[i] as u32;
                i += 1;
                if ignore_air && entry == 0 {
                    continue;
                }
                world.set_block_raw(BlockPos::new(base.x + x, base.y + y, base.z + z), entry);
            }
        }
    }

    // Send block changes before we send block entity data, otherwise it'll be ignored
    world.flush_block_changes();

    for (pos, block_entity) in &cb.block_entities {
        world.set_block_entity(base + *pos, block_entity.clone());
    }
}

macro_rules! nbt_as {
    // I'm not sure if path is the right type here.
    // It works though!
    ($e:expr, $p:path) => {
        match $e {
            $p(val) => val,
            _ => bail!(concat!("Could not parse nbt value as ", stringify!($p))),
        }
    };
}

/// Parses a palette entry like `minecraft:oak_stairs[facing=north,half=bottom]`. Blocks that don't
/// exist (any more) become air. Block states are matched by name and properties only, so
/// schematics from other versions keep working as long as the block still exists.
fn parse_block(str: &str) -> Block {
    let (name, properties) = match str.split_once('[') {
        Some((name, rest)) => (name, rest.strip_suffix(']').unwrap_or(rest)),
        None => (str, ""),
    };
    let mut block_name = name.trim().to_owned();
    if !block_name.contains(':') {
        block_name.insert_str(0, "minecraft:");
    }
    let mut block = Block::from_name(&block_name)
        .or_else(|| Block::from_name(renamed_block(&block_name)))
        .unwrap_or(Block::Air);
    if !properties.is_empty() {
        let properties = properties
            .split(',')
            .filter_map(|prop| prop.split_once('='))
            .collect();
        block.set_properties(properties);
    }
    block
}

pub fn load_schematic(path: &Path) -> Result<WorldEditClipboard> {
    let mut file = File::open(path)?;
    let nbt = nbt::Blob::from_gzip_reader(&mut file)?;
    load_schematic_nbt(&nbt)
}

fn load_schematic_nbt(nbt: &nbt::Blob) -> Result<WorldEditClipboard> {
    let root = if nbt.content.contains_key("Schematic") {
        nbt_as!(&nbt["Schematic"], nbt::Value::Compound)
    } else {
        &nbt.content
    };

    let version = nbt_as!(root["Version"], nbt::Value::Int);
    match version {
        2 | 3 => load_schematic_sponge(root, version),
        _ => bail!("unknown schematic version: {}", version),
    }
}

fn read_block_container(
    nbt: &nbt::Map<String, nbt::Value>,
    version: i32,
    size_x: u32,
    size_y: u32,
    size_z: u32,
) -> Result<(Vec<u16>, FxHashMap<BlockPos, BlockEntity>)> {
    use nbt::Value;

    let nbt_palette = nbt_as!(&nbt["Palette"], Value::Compound);
    let mut palette: FxHashMap<u32, u16> = FxHashMap::default();
    for (k, v) in nbt_palette {
        let id = *nbt_as!(v, Value::Int) as u32;
        palette.insert(id, parse_block(k).get_id() as u16);
    }

    let data_name = match version {
        2 => "BlockData",
        3 => "Data",
        _ => unreachable!(),
    };
    let blocks: Vec<u8> = nbt_as!(&nbt[data_name], Value::ByteArray)
        .iter()
        .map(|b| *b as u8)
        .collect();

    // The Sponge format stores varints in x, z, y order which is what the clipboard uses too
    let volume = size_x as usize * size_y as usize * size_z as usize;
    let mut data = Vec::with_capacity(volume);
    let mut bytes = blocks.iter();
    while data.len() < volume {
        let mut blockstate_id = 0u32;
        // Max varint length is 5
        for varint_len in 0..5 {
            let Some(byte) = bytes.next() else {
                bail!("the block data of the schematic is too short");
            };
            blockstate_id |= ((byte & 127) as u32) << (varint_len * 7);
            if byte & 128 != 128 {
                break;
            }
        }
        let Some(entry) = palette.get(&blockstate_id) else {
            bail!("block data refers to a missing palette entry: {blockstate_id}");
        };
        data.push(*entry);
    }

    let mut parsed_block_entities = FxHashMap::default();
    if let Some(block_entities) = nbt.get("BlockEntities") {
        for block_entity in nbt_as!(block_entities, Value::List) {
            let val = nbt_as!(block_entity, Value::Compound);
            let pos_array = nbt_as!(&val["Pos"], Value::IntArray);
            let pos = BlockPos {
                x: pos_array[0],
                y: pos_array[1],
                z: pos_array[2],
            };
            let id = nbt_as!(&val.get("Id").unwrap_or_else(|| &val["id"]), Value::String);
            let data = match version {
                2 => val,
                3 => nbt_as!(&val["Data"], Value::Compound),
                _ => unreachable!(),
            };
            if let Some(parsed) = BlockEntity::from_nbt(id, data) {
                parsed_block_entities.insert(pos, parsed);
            }
        }
    }

    Ok((data, parsed_block_entities))
}

fn load_schematic_sponge(
    nbt: &nbt::Map<String, nbt::Value>,
    version: i32,
) -> Result<WorldEditClipboard> {
    use nbt::Value;

    let size_x = nbt_as!(nbt["Width"], Value::Short) as u32;
    let size_z = nbt_as!(nbt["Length"], Value::Short) as u32;
    let size_y = nbt_as!(nbt["Height"], Value::Short) as u32;

    let (offset_x, offset_y, offset_z) = match version {
        2 => {
            // Older versions of WorldEdit put the offset in Metadata
            // These offsets are optional but if present all must be present
            // Its important to check the WEOffset first as both can be present but only the WEOffset is correct
            if let Some(metadata) = nbt.get("Metadata")
                && let metadata = nbt_as!(metadata, Value::Compound)
                && let Some(offset_x) = metadata.get("WEOffsetX")
            {
                (
                    -nbt_as!(offset_x, Value::Int),
                    -nbt_as!(metadata["WEOffsetY"], Value::Int),
                    -nbt_as!(metadata["WEOffsetZ"], Value::Int),
                )
            } else if let Some(offset) = nbt.get("Offset") {
                let offset_array = nbt_as!(offset, Value::IntArray);
                (-offset_array[0], -offset_array[1], -offset_array[2])
            } else {
                (0, 0, 0)
            }
        }
        3 => {
            if let Some(offset_array) = nbt.get("Offset") {
                let offset_array = nbt_as!(offset_array, Value::IntArray);
                (-offset_array[0], -offset_array[1], -offset_array[2])
            } else {
                (0, 0, 0)
            }
        }
        _ => unreachable!(),
    };

    let (data, block_entities) = read_block_container(
        match version {
            2 => nbt,
            3 => nbt_as!(&nbt["Blocks"], Value::Compound),
            _ => unreachable!(),
        },
        version,
        size_x,
        size_y,
        size_z,
    )?;
    Ok(WorldEditClipboard {
        size_x,
        size_y,
        size_z,
        offset_x,
        offset_y,
        offset_z,
        data,
        block_entities,
    })
}

#[derive(Serialize)]
struct Metadata {
    #[serde(rename = "WEOffsetX")]
    offset_x: i32,
    #[serde(rename = "WEOffsetY")]
    offset_y: i32,
    #[serde(rename = "WEOffsetZ")]
    offset_z: i32,
}

/// Used to serialize schematics in NBT. This cannot be used for deserialization because of
/// [a bug](https://github.com/PistonDevelopers/hematite_nbt/issues/45) in `hematite-nbt`.
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct Schematic {
    width: i16,
    length: i16,
    height: i16,
    palette: nbt::Blob,
    metadata: Metadata,
    #[serde(serialize_with = "nbt::i8_array")]
    block_data: Vec<i8>,
    block_entities: Vec<nbt::Blob>,
    version: i32,
    data_version: i32,
}

pub fn save_schematic(path: &Path, clipboard: &WorldEditClipboard) -> Result<()> {
    fs::create_dir_all(path.parent().unwrap())?;

    let mut file = File::create(path)?;
    let size_x = clipboard.size_x;
    let size_y = clipboard.size_y;
    let size_z = clipboard.size_z;
    let offset_x = -clipboard.offset_x;
    let offset_y = -clipboard.offset_y;
    let offset_z = -clipboard.offset_z;

    // Every state id only needs to be turned into a palette entry once
    let mut palette_index: FxHashMap<u16, usize> = FxHashMap::default();
    let mut pallette: Vec<String> = Vec::new();
    let mut data = Vec::with_capacity(clipboard.data.len());
    for entry in &clipboard.data {
        let mut idx = *palette_index.entry(*entry).or_insert_with(|| {
            let block = Block::from_id(*entry as u32);
            let name = block.get_name();
            let props = block.properties();
            let full_name = if !props.is_empty() {
                let mut props: Vec<_> = props.iter().collect();
                props.sort();
                let props_strs: Vec<String> = props
                    .iter()
                    .map(|(name, val)| format!("{}={}", name, val))
                    .collect();
                format!("{}[{}]", name, props_strs.join(","))
            } else {
                name.to_owned()
            };
            if let Some(idx) = pallette.iter().position(|s| *s == full_name) {
                idx
            } else {
                pallette.push(full_name);
                pallette.len() - 1
            }
        });

        loop {
            let mut temp = (idx & 0b0111_1111) as u8;
            idx >>= 7;
            if idx != 0 {
                temp |= 0b1000_0000;
            }
            data.push(temp as i8);
            if idx == 0 {
                break;
            }
        }
    }

    let mut encoded_pallete = nbt::Blob::new();
    for (i, entry) in pallette.iter().enumerate() {
        encoded_pallete.insert(entry, i as i32)?;
    }

    let mut block_entities = Vec::new();
    for (pos, block_entity) in &clipboard.block_entities {
        if let Some(mut blob) = block_entity.to_nbt(false) {
            blob.insert("Pos", nbt::Value::IntArray(vec![pos.x, pos.y, pos.z]))?;
            block_entities.push(blob);
        }
    }

    let metadata = Metadata {
        offset_x,
        offset_y,
        offset_z,
    };
    let schematic = Schematic {
        width: size_x as i16,
        length: size_z as i16,
        height: size_y as i16,
        block_data: data,
        block_entities,
        palette: encoded_pallete,
        metadata,
        version: 2,
        data_version: MC_DATA_VERSION,
    };
    nbt::to_gzip_writer(&mut file, &schematic, Some("Schematic"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "cinnabar_schematic_{}_{}",
            std::process::id(),
            name
        ))
    }

    #[test]
    fn parse_block_keeps_underscored_properties() {
        let block = parse_block(
            "minecraft:oak_stairs[facing=east,half=top,shape=inner_left,waterlogged=false]",
        );
        let props = block.properties();
        assert_eq!(block.get_name(), "minecraft:oak_stairs");
        assert_eq!(props["facing"], "east");
        assert_eq!(props["half"], "top");
        assert_eq!(props["shape"], "inner_left");
    }

    #[test]
    fn parse_block_without_namespace_and_unknown() {
        assert_eq!(parse_block("stone"), Block::Stone);
        assert_eq!(
            parse_block("minecraft:this_block_does_not_exist"),
            Block::Air
        );
    }

    #[test]
    fn parse_block_renamed_from_1_20_4() {
        assert_eq!(
            parse_block("minecraft:chain[axis=y,waterlogged=false]").get_name(),
            "minecraft:iron_chain"
        );
    }

    #[test]
    fn save_and_load_roundtrip() {
        let stairs = parse_block("oak_stairs[facing=south,half=top,shape=outer_right]");
        let blocks = [
            Block::Stone,
            Block::Air,
            stairs,
            Block::Glass,
            stairs,
            Block::Stone,
        ];
        let clipboard = WorldEditClipboard {
            offset_x: 1,
            offset_y: 0,
            offset_z: 2,
            size_x: 3,
            size_y: 1,
            size_z: 2,
            data: blocks.iter().map(|b| b.get_id() as u16).collect(),
            block_entities: FxHashMap::default(),
        };
        let path = temp_path("roundtrip.schem");
        save_schematic(&path, &clipboard).unwrap();
        let loaded = load_schematic(&path).unwrap();
        let _ = fs::remove_file(&path);
        assert_eq!(loaded.data, clipboard.data);
        assert_eq!((loaded.size_x, loaded.size_y, loaded.size_z), (3, 1, 2));
        assert_eq!(
            (loaded.offset_x, loaded.offset_y, loaded.offset_z),
            (1, 0, 2)
        );
    }

    /// A schematic like the ones WorldEdit wrote for 1.20.4: version 2, no `BlockEntities`
    #[test]
    fn load_old_sponge_v2_without_block_entities() {
        let mut palette = nbt::Blob::new();
        palette.insert("minecraft:air", 0).unwrap();
        palette
            .insert(
                "minecraft:oak_stairs[facing=north,half=bottom,shape=straight,waterlogged=false]",
                1,
            )
            .unwrap();
        palette
            .insert("minecraft:chain[axis=y,waterlogged=false]", 2)
            .unwrap();
        let mut root = nbt::Blob::new();
        root.insert("Version", 2).unwrap();
        root.insert("DataVersion", 3700).unwrap();
        root.insert("Width", nbt::Value::Short(3)).unwrap();
        root.insert("Height", nbt::Value::Short(1)).unwrap();
        root.insert("Length", nbt::Value::Short(1)).unwrap();
        root.insert("Palette", nbt::Value::Compound(palette.content))
            .unwrap();
        root.insert("BlockData", nbt::Value::ByteArray(vec![0, 1, 2]))
            .unwrap();

        let clipboard = load_schematic_nbt(&root).unwrap();
        assert_eq!(clipboard.data.len(), 3);
        let names: Vec<_> = clipboard
            .data
            .iter()
            .map(|id| Block::from_id(*id as u32).get_name())
            .collect();
        assert_eq!(
            names,
            [
                "minecraft:air",
                "minecraft:oak_stairs",
                "minecraft:iron_chain"
            ]
        );
        assert!(clipboard.block_entities.is_empty());
    }

    #[test]
    fn truncated_block_data_is_an_error() {
        let mut palette = nbt::Blob::new();
        palette.insert("minecraft:air", 0).unwrap();
        let mut root = nbt::Blob::new();
        root.insert("Version", 2).unwrap();
        root.insert("Width", nbt::Value::Short(2)).unwrap();
        root.insert("Height", nbt::Value::Short(1)).unwrap();
        root.insert("Length", nbt::Value::Short(1)).unwrap();
        root.insert("Palette", nbt::Value::Compound(palette.content))
            .unwrap();
        root.insert("BlockData", nbt::Value::ByteArray(vec![0]))
            .unwrap();
        assert!(load_schematic_nbt(&root).is_err());
    }
}
