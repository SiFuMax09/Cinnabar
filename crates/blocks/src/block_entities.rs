use crate::blocks::Block;
use crate::generic;
use crate::items::Item;
use cinnabar_proc_macros::protocol_id;
use cinnabar_utils::{map, nbt_unwrap_val};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::str::FromStr;

/// A single item in an inventory
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InventoryEntry {
    pub id: u32,
    pub slot: i8,
    pub count: i8,
    pub nbt: Option<Vec<u8>>,
}

#[derive(Default, Debug, Clone, Serialize, Deserialize)]
pub struct SignBlockEntity {
    pub front_rows: [String; 4],
    pub back_rows: [String; 4],
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ContainerType {
    Furnace,
    Barrel,
    Hopper,
    // New variants are only added at the end to keep saved plots readable
    Dispenser,
    Dropper,
    Chest,
    TrappedChest,
    ShulkerBox,
}

impl FromStr for ContainerType {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "barrel" => ContainerType::Barrel,
            "furnace" => ContainerType::Furnace,
            "hopper" => ContainerType::Hopper,
            "dispenser" => ContainerType::Dispenser,
            "dropper" => ContainerType::Dropper,
            "chest" => ContainerType::Chest,
            "trapped_chest" => ContainerType::TrappedChest,
            "shulker_box" => ContainerType::ShulkerBox,
            _ => return Err(()),
        })
    }
}

impl std::fmt::Display for ContainerType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ContainerType::Furnace => "minecraft:furnace",
            ContainerType::Barrel => "minecraft:barrel",
            ContainerType::Hopper => "minecraft:hopper",
            ContainerType::Dispenser => "minecraft:dispenser",
            ContainerType::Dropper => "minecraft:dropper",
            ContainerType::Chest => "minecraft:chest",
            ContainerType::TrappedChest => "minecraft:trapped_chest",
            ContainerType::ShulkerBox => "minecraft:shulker_box",
        })
    }
}

impl ContainerType {
    pub fn num_slots(self) -> u8 {
        match self {
            ContainerType::Furnace => 3,
            ContainerType::Barrel => 27,
            ContainerType::Hopper => 5,
            ContainerType::Dispenser | ContainerType::Dropper => 9,
            ContainerType::Chest | ContainerType::TrappedChest | ContainerType::ShulkerBox => 27,
        }
    }

    /// The name shown at the top of the screen
    pub fn title(self) -> &'static str {
        match self {
            ContainerType::Furnace => "Furnace",
            ContainerType::Barrel => "Barrel",
            ContainerType::Hopper => "Item Hopper",
            ContainerType::Dispenser => "Dispenser",
            ContainerType::Dropper => "Dropper",
            ContainerType::Chest => "Chest",
            ContainerType::TrappedChest => "Trapped Chest",
            ContainerType::ShulkerBox => "Shulker Box",
        }
    }

    pub fn window_type(self) -> i32 {
        // https://wiki.vg/Inventory
        match self {
            ContainerType::Furnace => protocol_id!("minecraft:menu", "minecraft:furnace"),
            ContainerType::Barrel => protocol_id!("minecraft:menu", "minecraft:generic_9x3"),
            ContainerType::Hopper => protocol_id!("minecraft:menu", "minecraft:hopper"),
            ContainerType::Dispenser | ContainerType::Dropper => {
                protocol_id!("minecraft:menu", "minecraft:generic_3x3")
            }
            ContainerType::Chest | ContainerType::TrappedChest => {
                protocol_id!("minecraft:menu", "minecraft:generic_9x3")
            }
            ContainerType::ShulkerBox => protocol_id!("minecraft:menu", "minecraft:shulker_box"),
        }
    }

    /// The container type a block holds its items in, if it is a container
    pub fn for_block(block: Block) -> Option<ContainerType> {
        match block {
            Block::Barrel { .. } => Some(ContainerType::Barrel),
            Block::Furnace { .. } => Some(ContainerType::Furnace),
            Block::Hopper { .. } => Some(ContainerType::Hopper),
            Block::Generic(id) => match generic::state_name(id) {
                "minecraft:dispenser" => Some(ContainerType::Dispenser),
                "minecraft:dropper" => Some(ContainerType::Dropper),
                "minecraft:chest" => Some(ContainerType::Chest),
                "minecraft:trapped_chest" => Some(ContainerType::TrappedChest),
                name if name.ends_with("shulker_box") => Some(ContainerType::ShulkerBox),
                _ => None,
            },
            _ => None,
        }
    }
}

/// The slots of a container with the logic of moving items in and out of them
#[derive(Clone, Debug, PartialEq)]
pub struct Inventory {
    slots: Vec<Option<InventoryEntry>>,
}

impl Inventory {
    pub fn new(num_slots: usize) -> Inventory {
        Inventory {
            slots: vec![None; num_slots],
        }
    }

    pub fn from_entries(entries: &[InventoryEntry], num_slots: usize) -> Inventory {
        let mut inventory = Inventory::new(num_slots);
        for entry in entries {
            if let Some(slot) = inventory.slots.get_mut(entry.slot as usize)
                && entry.count > 0
            {
                *slot = Some(entry.clone());
            }
        }
        inventory
    }

    pub fn into_entries(self) -> Vec<InventoryEntry> {
        self.slots.into_iter().flatten().collect()
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.iter().all(Option::is_none)
    }

    pub fn get(&self, slot: usize) -> Option<&InventoryEntry> {
        self.slots.get(slot)?.as_ref()
    }

    /// Replaces the item in the slot. The entry is stored with the slot number it is put in.
    pub fn set(&mut self, slot: usize, entry: Option<InventoryEntry>) {
        self.slots[slot] = entry.map(|mut entry| {
            entry.slot = slot as i8;
            entry
        });
    }

    pub fn take(&mut self, slot: usize) -> Option<InventoryEntry> {
        self.slots[slot].take()
    }

    pub fn max_stack(entry: &InventoryEntry) -> i8 {
        Item::from_id(entry.id).max_stack_size().min(i8::MAX as u32) as i8
    }

    pub fn is_full(&self) -> bool {
        self.slots.iter().all(|slot| {
            slot.as_ref()
                .is_some_and(|e| e.count >= Inventory::max_stack(e))
        })
    }

    pub fn same_item(a: &InventoryEntry, b: &InventoryEntry) -> bool {
        a.id == b.id && a.nbt == b.nbt
    }

    /// Puts as much of the entry as fits into the slot. Returns what is left.
    pub fn insert_into_slot(
        &mut self,
        slot: usize,
        mut entry: InventoryEntry,
    ) -> Option<InventoryEntry> {
        match &mut self.slots[slot] {
            None => {
                let max = Inventory::max_stack(&entry);
                let moved = entry.count.min(max);
                let mut placed = entry.clone();
                placed.count = moved;
                placed.slot = slot as i8;
                entry.count -= moved;
                self.slots[slot] = Some(placed);
            }
            Some(current) if Inventory::same_item(current, &entry) => {
                let space = (Inventory::max_stack(current) - current.count).max(0);
                let moved = entry.count.min(space);
                current.count += moved;
                entry.count -= moved;
            }
            Some(_) => {}
        }
        (entry.count > 0).then_some(entry)
    }

    /// Puts as much of the entry as fits into the slots in the given order
    pub fn insert(
        &mut self,
        mut entry: InventoryEntry,
        slots: impl IntoIterator<Item = usize>,
    ) -> Option<InventoryEntry> {
        for slot in slots {
            entry = self.insert_into_slot(slot, entry)?;
        }
        Some(entry)
    }

    /// The signal strength a comparator reads from the container
    pub fn comparator_signal(&self) -> u8 {
        let mut fullness = 0.0f32;
        for entry in self.slots.iter().flatten() {
            fullness += entry.count as f32 / Inventory::max_stack(entry) as f32;
        }
        fullness /= self.slots.len() as f32;
        (fullness * 14.0).floor() as u8 + (fullness > 0.0) as u8
    }
}

/// The state of a block that a piston is moving, which is shown as a `moving_piston` block until
/// the movement is done
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MovingPistonEntity {
    /// The state id of the block that arrives at the end of the movement
    pub moved_state: u32,
    /// The block entity of the moved block, which is given back when the movement is done
    pub moved_entity: Option<Box<BlockEntity>>,
    /// The id of the `BlockFace` of the piston
    pub facing: u8,
    pub extending: bool,
    /// Whether this is the piston head or the piston base itself and not a moved block
    pub source: bool,
    /// The number of game ticks the movement has been ticked. Vanilla moves by half a block per
    /// tick and puts the block down on the tick after that.
    pub ticks: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BlockEntity {
    Comparator {
        output_strength: u8,
    },
    Container {
        comparator_override: u8,
        inventory: Vec<InventoryEntry>,
        ty: ContainerType,
    },
    Sign(Box<SignBlockEntity>),
    /// The slot of a chiseled bookshelf that was interacted with last, which is what a
    /// comparator reads from it
    ChiseledBookshelf {
        last_interacted_slot: i8,
    },
    MovingPiston(Box<MovingPistonEntity>),
    /// The frequency of the last vibration a sculk sensor received, which is what a comparator
    /// reads from it while it is active
    SculkSensor {
        last_vibration_frequency: u8,
        calibrated: bool,
    },
    /// The book on a lectern and the page it is opened at. New variants go at the end to keep
    /// saved plots readable.
    Lectern {
        book: Option<InventoryEntry>,
        page: i32,
        /// The number of pages of the book, which is what the signal of a comparator is based on
        page_count: i32,
    },
}

/// The name of the data component that holds the content of a written book
pub const WRITTEN_BOOK_COMPONENT: &str = "minecraft:written_book_content";
/// The name of the data component that holds the content of a book and quill
pub const WRITABLE_BOOK_COMPONENT: &str = "minecraft:writable_book_content";

/// The number of pages of a book item. The server does not interpret item data components, it
/// only keeps the encoded form of the book components of the stack in the `Components` compound
/// of the item NBT, so the page count is read from that.
pub fn book_page_count(entry: &InventoryEntry) -> i32 {
    fn read_varint(data: &mut &[u8]) -> Option<i32> {
        let mut value = 0u32;
        for i in 0..5 {
            let (&byte, rest) = data.split_first()?;
            *data = rest;
            value |= ((byte & 0x7F) as u32) << (7 * i);
            if byte & 0x80 == 0 {
                return Some(value as i32);
            }
        }
        None
    }
    fn skip_string(data: &mut &[u8]) -> Option<()> {
        let len = read_varint(data)? as usize;
        *data = data.get(len..)?;
        Some(())
    }
    fn read_bool(data: &mut &[u8]) -> Option<bool> {
        let (&byte, rest) = data.split_first()?;
        *data = rest;
        Some(byte != 0)
    }

    let Some(bytes) = &entry.nbt else {
        return 0;
    };
    let Ok(blob) = nbt::Blob::from_reader(&mut std::io::Cursor::new(bytes)) else {
        return 0;
    };
    let Some(nbt::Value::Compound(components)) = blob.get("Components") else {
        return 0;
    };
    let raw = |name: &str| -> Option<Vec<u8>> {
        match components.get(name)? {
            nbt::Value::ByteArray(data) => Some(data.iter().map(|byte| *byte as u8).collect()),
            _ => None,
        }
    };
    if let Some(data) = raw(WRITABLE_BOOK_COMPONENT) {
        // The pages are the only field
        return read_varint(&mut data.as_slice()).unwrap_or(0);
    }
    if let Some(data) = raw(WRITTEN_BOOK_COMPONENT) {
        let mut data = data.as_slice();
        let count = (|| {
            // The title is a string that can come with a filtered version
            skip_string(&mut data)?;
            if read_bool(&mut data)? {
                skip_string(&mut data)?;
            }
            // The author and the generation
            skip_string(&mut data)?;
            read_varint(&mut data)?;
            read_varint(&mut data)
        })();
        return count.unwrap_or(0);
    }
    0
}

impl BlockEntity {
    /// The protocol id for the block entity
    pub fn ty(&self) -> i32 {
        macro_rules! block_entity_id {
            ($name:literal) => {
                protocol_id!("minecraft:block_entity_type", $name)
            };
        }
        match self {
            BlockEntity::Comparator { .. } => block_entity_id!("minecraft:comparator"),
            BlockEntity::Container { ty, .. } => match ty {
                ContainerType::Furnace => block_entity_id!("minecraft:furnace"),
                ContainerType::Barrel => block_entity_id!("minecraft:barrel"),
                ContainerType::Hopper => block_entity_id!("minecraft:hopper"),
                ContainerType::Dispenser => block_entity_id!("minecraft:dispenser"),
                ContainerType::Dropper => block_entity_id!("minecraft:dropper"),
                ContainerType::Chest => block_entity_id!("minecraft:chest"),
                ContainerType::TrappedChest => block_entity_id!("minecraft:trapped_chest"),
                ContainerType::ShulkerBox => block_entity_id!("minecraft:shulker_box"),
            },
            BlockEntity::Sign(_) => block_entity_id!("minecraft:sign"),
            BlockEntity::ChiseledBookshelf { .. } => {
                block_entity_id!("minecraft:chiseled_bookshelf")
            }
            BlockEntity::MovingPiston(_) => block_entity_id!("minecraft:piston"),
            BlockEntity::SculkSensor { calibrated, .. } => {
                if *calibrated {
                    block_entity_id!("minecraft:calibrated_sculk_sensor")
                } else {
                    block_entity_id!("minecraft:sculk_sensor")
                }
            }
            BlockEntity::Lectern { .. } => block_entity_id!("minecraft:lectern"),
        }
    }

    /// A lectern holding the book, opened at the first page
    pub fn lectern(book: Option<InventoryEntry>) -> BlockEntity {
        let page_count = book.as_ref().map_or(0, book_page_count);
        BlockEntity::Lectern {
            book,
            page: 0,
            page_count,
        }
    }

    /// A container with the comparator signal calculated from its items
    pub fn container(inventory: Vec<InventoryEntry>, ty: ContainerType) -> BlockEntity {
        let comparator_override =
            Inventory::from_entries(&inventory, ty.num_slots() as usize).comparator_signal();
        BlockEntity::Container {
            comparator_override,
            inventory,
            ty,
        }
    }

    /// Reads the item stack of a lectern book
    fn load_item(item: &HashMap<String, nbt::Value>) -> Option<InventoryEntry> {
        let count = nbt_get_int(item, "count").or_else(|| nbt_get_int(item, "Count"))?;
        let name = match nbt_case_insensitive(item, "id")? {
            nbt::Value::String(name) => name,
            _ => return None,
        };
        Some(InventoryEntry {
            id: Item::from_name(name)?.get_id(),
            slot: 0,
            count: count as i8,
            nbt: None,
        })
    }

    fn load_container(slots_nbt: &[nbt::Value], ty: ContainerType) -> Option<BlockEntity> {
        use nbt::Value;
        let mut inventory = Vec::new();
        for item in slots_nbt {
            let item_compound = nbt_unwrap_val!(item, Value::Compound);
            // Item stacks save their count as `count` since 1.20.5
            let count = nbt_get_int(item_compound, "Count")
                .or_else(|| nbt_get_int(item_compound, "count"))? as i8;
            let slot = nbt_get_int(item_compound, "Slot")? as i8;
            let namespaced_name =
                nbt_unwrap_val!(nbt_case_insensitive(item_compound, "Id")?, Value::String);
            let item_type = Item::from_name(namespaced_name);

            let mut blob = nbt::Blob::new();
            for (k, v) in item_compound {
                blob.insert(k, v.clone()).unwrap();
            }
            let mut data = Vec::new();
            blob.to_writer(&mut data).unwrap();

            let tag = match item_compound.get("tag") {
                Some(nbt::Value::Compound(map)) => {
                    let mut blob = nbt::Blob::new();
                    for (k, v) in map {
                        blob.insert(k, v.clone()).unwrap();
                    }

                    let mut data = Vec::new();
                    blob.to_writer(&mut data).unwrap();
                    Some(data)
                }
                _ => None,
            };
            inventory.push(InventoryEntry {
                slot,
                count,
                id: item_type.unwrap_or(Item::Redstone).get_id(),
                nbt: tag,
            });
        }
        Some(BlockEntity::container(inventory, ty))
    }

    pub fn from_nbt(id: &str, nbt: &HashMap<String, nbt::Value>) -> Option<BlockEntity> {
        use nbt::Value;
        match id.trim_start_matches("minecraft:") {
            "comparator" => Some(BlockEntity::Comparator {
                output_strength: *nbt_unwrap_val!(&nbt["OutputSignal"], Value::Int) as u8,
            }),
            "chiseled_bookshelf" => Some(BlockEntity::ChiseledBookshelf {
                last_interacted_slot: nbt_get_int(nbt, "last_interacted_slot").unwrap_or(-1) as i8,
            }),
            id @ ("sculk_sensor" | "calibrated_sculk_sensor") => Some(BlockEntity::SculkSensor {
                last_vibration_frequency: nbt_get_int(nbt, "last_vibration_frequency")
                    .unwrap_or(0)
                    .clamp(0, 15) as u8,
                calibrated: id == "calibrated_sculk_sensor",
            }),
            "lectern" => {
                let book = match nbt.get("Book") {
                    Some(Value::Compound(item)) => BlockEntity::load_item(item),
                    _ => None,
                };
                let mut lectern = BlockEntity::lectern(book);
                if let BlockEntity::Lectern {
                    page, page_count, ..
                } = &mut lectern
                {
                    // Books saved by the game keep their pages in components, which are not read
                    *page_count = (*page_count).max(1);
                    *page = nbt_get_int(nbt, "Page")
                        .unwrap_or(0)
                        .clamp(0, *page_count - 1);
                }
                Some(lectern)
            }
            "furnace" => BlockEntity::load_container(
                nbt_unwrap_val!(&nbt["Items"], Value::List),
                ContainerType::Furnace,
            ),
            "barrel" => BlockEntity::load_container(
                nbt_unwrap_val!(&nbt["Items"], Value::List),
                ContainerType::Barrel,
            ),
            "hopper" => BlockEntity::load_container(
                nbt_unwrap_val!(&nbt["Items"], Value::List),
                ContainerType::Hopper,
            ),
            name @ ("dispenser" | "dropper" | "chest" | "trapped_chest" | "shulker_box") => {
                BlockEntity::load_container(
                    nbt_unwrap_val!(nbt.get("Items")?, Value::List),
                    name.parse().ok()?,
                )
            }
            "sign" => {
                let sign = if nbt.contains_key("Text1") {
                    // This is the pre-1.20 encoding
                    SignBlockEntity {
                        front_rows: [
                            // This cloning is really dumb
                            nbt_unwrap_val!(nbt["Text1"].clone(), Value::String),
                            nbt_unwrap_val!(nbt["Text2"].clone(), Value::String),
                            nbt_unwrap_val!(nbt["Text3"].clone(), Value::String),
                            nbt_unwrap_val!(nbt["Text4"].clone(), Value::String),
                        ],
                        back_rows: Default::default(),
                    }
                } else {
                    let get_side = |side| {
                        let messages =
                            nbt_unwrap_val!(&nbt[side], Value::Compound).get("messages")?;
                        let mut messages = nbt_unwrap_val!(messages, Value::List).iter().cloned();
                        Some([
                            nbt_unwrap_val!(messages.next()?, Value::String),
                            nbt_unwrap_val!(messages.next()?, Value::String),
                            nbt_unwrap_val!(messages.next()?, Value::String),
                            nbt_unwrap_val!(messages.next()?, Value::String),
                        ])
                    };
                    SignBlockEntity {
                        front_rows: get_side("front_text")?,
                        back_rows: get_side("back_text")?,
                    }
                };
                Some(BlockEntity::Sign(Box::new(sign)))
            }
            _ => None,
        }
    }

    pub fn to_nbt(&self, sign_only: bool) -> Option<nbt::Blob> {
        if sign_only && !matches!(self, BlockEntity::Sign(_)) {
            return None;
        }

        use nbt::Value;
        match self {
            // Clients animate the movement from the block event of the piston
            BlockEntity::MovingPiston(_) => None,
            BlockEntity::ChiseledBookshelf {
                last_interacted_slot,
            } => Some(nbt::Blob::with_content(map! {
                "last_interacted_slot" => Value::Int(*last_interacted_slot as i32),
                "id" => Value::String("minecraft:chiseled_bookshelf".to_owned())
            })),
            BlockEntity::SculkSensor {
                last_vibration_frequency,
                calibrated,
            } => Some(nbt::Blob::with_content(map! {
                "last_vibration_frequency" => Value::Int(*last_vibration_frequency as i32),
                "id" => Value::String(
                    if *calibrated {
                        "minecraft:calibrated_sculk_sensor"
                    } else {
                        "minecraft:sculk_sensor"
                    }
                    .to_owned()
                )
            })),
            // Clients don't need the book, the state of the block shows whether there is one
            BlockEntity::Lectern { .. } if sign_only => None,
            BlockEntity::Lectern { book, page, .. } => {
                let mut content = map! {
                    "Page" => Value::Int(*page),
                    "id" => Value::String("minecraft:lectern".to_owned())
                };
                if let Some(book) = book {
                    content.insert(
                        "Book".to_owned(),
                        Value::Compound(map! {
                            "count" => Value::Int(book.count as i32),
                            "id" => Value::String(Item::from_id(book.id).get_name().to_owned())
                        }),
                    );
                }
                Some(nbt::Blob::with_content(content))
            }
            BlockEntity::Sign(sign) => Some({
                // Text components are sent as NBT over the network, while they are saved as JSON
                let text = |row: &String| {
                    if sign_only {
                        sign_row_to_nbt(row)
                    } else {
                        Value::String(row.clone())
                    }
                };
                let front = sign.front_rows.iter().map(text);
                let back = sign.back_rows.iter().map(text);
                nbt::Blob::with_content(map! {
                    "is_waxed" => Value::Byte(0),
                    "front_text" => Value::Compound(map! {
                        "has_glowing_text" => Value::Byte(0),
                        "color" => Value::String("black".into()),
                        "messages" => Value::List(front.collect())
                    }),
                    "back_text" => Value::Compound(map! {
                        "has_glowing_text" => Value::Byte(0),
                        "color" => Value::String("black".into()),
                        "messages" => Value::List(back.collect())
                    }),
                    "id" => Value::String("minecraft:sign".to_owned())
                })
            }),
            BlockEntity::Comparator { output_strength } => Some({
                nbt::Blob::with_content(map! {
                    "OutputSignal" => Value::Int(*output_strength as i32),
                    "id" => Value::String("minecraft:comparator".to_owned())
                })
            }),
            BlockEntity::Container { inventory, ty, .. } => Some({
                let mut items = Vec::new();
                for entry in inventory {
                    let nbt = map! {
                        "Count" => nbt::Value::Byte(entry.count),
                        "id" => nbt::Value::String(Item::from_id(entry.id).get_name().to_owned()),
                        "Slot" => nbt::Value::Byte(entry.slot)
                    };
                    // TODO: item nbt data in containers
                    // if let Some(tag) = &entry.nbt {
                    //     let blob = nbt::Blob::from_reader(&mut Cursor::new(tag)).unwrap();
                    // }
                    items.push(nbt::Value::Compound(nbt));
                }
                nbt::Blob::with_content(map! {
                    "id" => Value::String(ty.to_string()),
                    "Items" => Value::List(items)
                })
            }),
        }
    }
}

/// Converts a line of a sign saved as a JSON text component to the NBT format of text components
fn sign_row_to_nbt(row: &str) -> nbt::Value {
    match serde_json::from_str::<serde_json::Value>(row) {
        Ok(serde_json::Value::String(text)) => nbt::Value::String(text),
        Ok(serde_json::Value::Object(object)) => match object.get("text") {
            Some(serde_json::Value::String(text)) => nbt::Value::String(text.clone()),
            _ => nbt::Value::String(String::new()),
        },
        _ => nbt::Value::String(row.to_owned()),
    }
}

fn nbt_case_insensitive<'a>(
    compound: &'a HashMap<String, nbt::Value>,
    name: &str,
) -> Option<&'a nbt::Value> {
    // It's not really case insensitive, this is really just checking both cases of the first character.
    compound
        .get(name)
        .or_else(|| compound.get(&name.to_lowercase()))
}

fn nbt_get_int(compound: &HashMap<String, nbt::Value>, name: &str) -> Option<i32> {
    let value = nbt_case_insensitive(compound, name)?;
    match value {
        nbt::Value::Byte(val) => Some(*val as i32),
        nbt::Value::Int(val) => Some(*val),
        _ => None,
    }
}

/// The block entity the client needs to render the block, for blocks that have no block entity
/// of their own on the server until they hold something. Returns the protocol id of the type and
/// its name. Chests are only drawn by the client when it has a block entity for them.
pub fn render_block_entity(state: u32) -> Option<(i32, nbt::Blob)> {
    use std::ops::Range;
    use std::sync::OnceLock;
    static RANGES: OnceLock<[(Range<u32>, i32, &'static str); 3]> = OnceLock::new();
    let ranges = RANGES.get_or_init(|| {
        let range = |name| generic::state_range(name).unwrap();
        [
            (
                range("minecraft:chest"),
                protocol_id!("minecraft:block_entity_type", "minecraft:chest"),
                "minecraft:chest",
            ),
            (
                range("minecraft:trapped_chest"),
                protocol_id!("minecraft:block_entity_type", "minecraft:trapped_chest"),
                "minecraft:trapped_chest",
            ),
            (
                range("minecraft:ender_chest"),
                protocol_id!("minecraft:block_entity_type", "minecraft:ender_chest"),
                "minecraft:ender_chest",
            ),
        ]
    });
    ranges
        .iter()
        .find(|(range, ..)| range.contains(&state))
        .map(|(_, ty, name)| {
            (
                *ty,
                nbt::Blob::with_content(map! {
                    "id" => nbt::Value::String((*name).to_owned())
                }),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn book_with(component: &str, data: Vec<i8>) -> InventoryEntry {
        let mut components = HashMap::new();
        components.insert(component.to_owned(), nbt::Value::ByteArray(data));
        let mut blob = nbt::Blob::new();
        blob.insert("Components", nbt::Value::Compound(components))
            .unwrap();
        let mut bytes = Vec::new();
        blob.to_writer(&mut bytes).unwrap();
        InventoryEntry {
            id: 0,
            slot: 0,
            count: 1,
            nbt: Some(bytes),
        }
    }

    #[test]
    fn page_count_of_a_writable_book() {
        // Three pages, the first one with a filtered version
        let data = vec![3, 1, b'a' as i8, 1, 1, b'b' as i8, 0, 1, b'c' as i8, 0];
        let entry = book_with(WRITABLE_BOOK_COMPONENT, data);
        assert_eq!(book_page_count(&entry), 3);
    }

    #[test]
    fn page_count_of_a_written_book() {
        // Title "ab" without a filtered version, author "c", generation 1 and two pages
        let data = vec![2, b'a' as i8, b'b' as i8, 0, 1, b'c' as i8, 1, 2, 8, 0, 0];
        let entry = book_with(WRITTEN_BOOK_COMPONENT, data);
        assert_eq!(book_page_count(&entry), 2);
    }

    #[test]
    fn page_count_without_book_components() {
        let entry = InventoryEntry {
            id: 0,
            slot: 0,
            count: 1,
            nbt: None,
        };
        assert_eq!(book_page_count(&entry), 0);
    }
}
