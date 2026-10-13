mod props;

pub use crate::generated::Block;
use crate::{generic, BlockDirection, BlockFacing, BlockProperty};
use cinnabar_proc_macros::BlockTransform;
pub use props::*;

#[derive(Clone, Copy, Debug)]
pub enum FlipDirection {
    FlipX,
    FlipZ,
}

#[derive(Clone, Copy, Debug)]
pub enum RotateAmt {
    Rotate90,
    Rotate180,
    Rotate270,
}

pub(crate) trait BlockTransform {
    fn rotate(&mut self, amt: crate::blocks::RotateAmt) {
        match amt {
            // ez
            RotateAmt::Rotate90 => self.rotate90(),
            RotateAmt::Rotate180 => {
                self.rotate90();
                self.rotate90();
            }
            RotateAmt::Rotate270 => {
                self.rotate90();
                self.rotate90();
                self.rotate90();
            }
        }
    }
    fn rotate90(&mut self);
    fn flip(&mut self, dir: crate::blocks::FlipDirection);
}

macro_rules! noop_block_transform {
    ($($ty:ty),*$(,)?) => {
        $(
            impl BlockTransform for $ty {
                fn rotate90(&mut self) {}
                fn flip(&mut self, _dir: crate::blocks::FlipDirection) {}
            }
        )*
    };
}

noop_block_transform!(
    u8,
    u32,
    bool,
    BlockFacing,
    TrapdoorHalf,
    LeverFace,
    ComparatorMode,
    Instrument,
    SlabType,
);

impl BlockTransform for BlockDirection {
    fn flip(&mut self, dir: FlipDirection) {
        match dir {
            FlipDirection::FlipX => match self {
                BlockDirection::East => *self = BlockDirection::West,
                BlockDirection::West => *self = BlockDirection::East,
                _ => {}
            },
            FlipDirection::FlipZ => match self {
                BlockDirection::North => *self = BlockDirection::South,
                BlockDirection::South => *self = BlockDirection::North,
                _ => {}
            },
        }
    }

    fn rotate90(&mut self) {
        *self = match self {
            BlockDirection::North => BlockDirection::East,
            BlockDirection::East => BlockDirection::South,
            BlockDirection::South => BlockDirection::West,
            BlockDirection::West => BlockDirection::North,
        }
    }
}

impl Block {
    pub fn is_sign(self) -> bool {
        matches!(
            self,
            Block::OakSign { .. }
                | Block::SpruceSign { .. }
                | Block::BirchSign { .. }
                | Block::AcaciaSign { .. }
                | Block::JungleSign { .. }
                | Block::DarkOakSign { .. }
                | Block::CrimsonSign { .. }
                | Block::WarpedSign { .. }
                | Block::BambooSign { .. }
                | Block::CherrySign { .. }
                | Block::MangroveSign { .. }
        )
    }

    pub fn is_wall_sign(self) -> bool {
        matches!(
            self,
            Block::OakWallSign { .. }
                | Block::SpruceWallSign { .. }
                | Block::BirchWallSign { .. }
                | Block::AcaciaWallSign { .. }
                | Block::JungleWallSign { .. }
                | Block::DarkOakWallSign { .. }
                | Block::CrimsonWallSign { .. }
                | Block::WarpedWallSign { .. }
                | Block::BambooWallSign { .. }
                | Block::CherryWallSign { .. }
                | Block::MangroveWallSign { .. }
        )
    }

    pub fn has_block_entity(self) -> bool {
        self.is_sign()
            || self.is_wall_sign()
            || matches!(
                self,
                Block::Comparator { .. }
                    | Block::Barrel { .. }
                    | Block::Furnace { .. }
                    | Block::Hopper { .. }
            )
            || self.is_chiseled_bookshelf()
            || self.is_sculk_sensor()
    }

    /// Whether the block is a sculk sensor or a calibrated sculk sensor
    pub fn is_sculk_sensor(self) -> bool {
        matches!(self, Block::Generic(id) if crate::generic::is_sculk_sensor(id))
    }

    pub fn is_chiseled_bookshelf(self) -> bool {
        matches!(self, Block::Generic(id) if crate::generic::state_name(id) == "minecraft:chiseled_bookshelf")
    }

    /// Whether placing a block at the position of this block replaces it
    pub fn can_place_block_in(self) -> bool {
        matches!(
            self.get_name(),
            "minecraft:air"
                | "minecraft:void_air"
                | "minecraft:cave_air"
                | "minecraft:water"
                | "minecraft:lava"
                | "minecraft:short_grass"
                | "minecraft:fern"
                | "minecraft:dead_bush"
                | "minecraft:seagrass"
                | "minecraft:tall_seagrass"
                | "minecraft:tall_grass"
                | "minecraft:large_fern"
        )
    }

    /// The `waterlogged` field of the dedicated variants that have one
    fn waterlogged_mut(&mut self) -> Option<&mut bool> {
        match self {
            Block::OakWallSign { waterlogged, .. }
            | Block::SpruceWallSign { waterlogged, .. }
            | Block::BirchWallSign { waterlogged, .. }
            | Block::AcaciaWallSign { waterlogged, .. }
            | Block::JungleWallSign { waterlogged, .. }
            | Block::DarkOakWallSign { waterlogged, .. }
            | Block::CrimsonWallSign { waterlogged, .. }
            | Block::WarpedWallSign { waterlogged, .. }
            | Block::BambooWallSign { waterlogged, .. }
            | Block::CherryWallSign { waterlogged, .. }
            | Block::MangroveWallSign { waterlogged, .. }
            | Block::OakSign { waterlogged, .. }
            | Block::SpruceSign { waterlogged, .. }
            | Block::BirchSign { waterlogged, .. }
            | Block::AcaciaSign { waterlogged, .. }
            | Block::JungleSign { waterlogged, .. }
            | Block::DarkOakSign { waterlogged, .. }
            | Block::CrimsonSign { waterlogged, .. }
            | Block::WarpedSign { waterlogged, .. }
            | Block::BambooSign { waterlogged, .. }
            | Block::CherrySign { waterlogged, .. }
            | Block::MangroveSign { waterlogged, .. }
            | Block::SeaPickle { waterlogged, .. }
            | Block::SmoothStoneSlab { waterlogged, .. }
            | Block::QuartzSlab { waterlogged, .. }
            | Block::IronTrapdoor { waterlogged, .. } => Some(waterlogged),
            _ => None,
        }
    }

    /// Whether the block can hold water by being waterlogged. Double slabs can't.
    pub fn can_be_waterlogged(self) -> bool {
        match self {
            Block::Generic(id) => {
                !generic::property_values(id, "waterlogged").is_empty()
                    && generic::get_property(id, "type") != Some("double")
            }
            Block::SmoothStoneSlab { ty, .. } | Block::QuartzSlab { ty, .. } => {
                ty != SlabType::Double
            }
            _ => {
                let mut block = self;
                block.waterlogged_mut().is_some()
            }
        }
    }

    pub fn is_waterlogged(self) -> bool {
        match self {
            Block::Generic(id) => generic::get_property(id, "waterlogged") == Some("true"),
            _ => {
                let mut block = self;
                block
                    .waterlogged_mut()
                    .is_some_and(|waterlogged| *waterlogged)
            }
        }
    }

    /// The block with its water set, or `None` if the block can't be waterlogged
    pub fn with_waterlogged(mut self, waterlogged: bool) -> Option<Block> {
        if !self.can_be_waterlogged() {
            return None;
        }
        if let Block::Generic(id) = &mut self {
            let value = if waterlogged { "true" } else { "false" };
            *id = generic::with_property(*id, "waterlogged", value);
        } else if let Some(field) = self.waterlogged_mut() {
            *field = waterlogged;
        }
        Some(self)
    }

    pub(crate) fn complex_rotate(&mut self, amt: RotateAmt) {
        match self {
            Block::RedstoneWire(wire) => wire.rotate(amt),
            _ => unreachable!(),
        }
    }

    pub(crate) fn complex_flip(&mut self, dir: FlipDirection) {
        match self {
            Block::RedstoneWire(wire) => wire.flip(dir),
            _ => unreachable!(),
        }
    }

    pub fn get_sign_rotation(self) -> Option<u8> {
        Some(match self {
            Block::OakSign { rotation, .. }
            | Block::SpruceSign { rotation, .. }
            | Block::BirchSign { rotation, .. }
            | Block::AcaciaSign { rotation, .. }
            | Block::JungleSign { rotation, .. }
            | Block::DarkOakSign { rotation, .. }
            | Block::CrimsonSign { rotation, .. }
            | Block::WarpedSign { rotation, .. } => rotation,
            _ => return None,
        })
    }

    pub fn get_wall_sign_facing(self) -> Option<BlockDirection> {
        Some(match self {
            Block::OakWallSign { facing, .. }
            | Block::SpruceWallSign { facing, .. }
            | Block::BirchWallSign { facing, .. }
            | Block::AcaciaWallSign { facing, .. }
            | Block::JungleWallSign { facing, .. }
            | Block::DarkOakWallSign { facing, .. }
            | Block::CrimsonWallSign { facing, .. }
            | Block::WarpedWallSign { facing, .. } => facing,
            _ => return None,
        })
    }

    pub(crate) fn is_solid_dynamic(self) -> bool {
        match self {
            Block::SmoothStoneSlab { ty, .. } | Block::QuartzSlab { ty, .. } => {
                ty == SlabType::Double
            }
            _ => unreachable!(),
        }
    }

    pub(crate) fn is_transparent_dynamic(self) -> bool {
        match self {
            Block::SmoothStoneSlab { ty, .. } | Block::QuartzSlab { ty, .. } => {
                ty != SlabType::Double
            }
            _ => unreachable!(),
        }
    }

    pub(crate) fn is_cube_dynamic(self) -> bool {
        match self {
            Block::SmoothStoneSlab { ty, .. } | Block::QuartzSlab { ty, .. } => ty == SlabType::Top,
            _ => unreachable!(),
        }
    }

    pub fn get_pressure_plate_powered(&mut self) -> Option<&mut bool> {
        match self {
            Block::OakPressurePlate { powered }
            | Block::SprucePressurePlate { powered }
            | Block::BirchPressurePlate { powered }
            | Block::JunglePressurePlate { powered }
            | Block::AcaciaPressurePlate { powered }
            | Block::DarkOakPressurePlate { powered }
            | Block::MangrovePressurePlate { powered }
            | Block::CherryPressurePlate { powered }
            | Block::BambooPressurePlate { powered }
            | Block::CrimsonPressurePlate { powered }
            | Block::WarpedPressurePlate { powered }
            | Block::StonePressurePlate { powered }
            | Block::PolishedBlackstonePressurePlate { powered } => Some(powered),
            _ => None,
        }
    }
}

#[test]
fn repeater_id_test() {
    let original = Block::Repeater(Repeater::new(3, BlockDirection::West, true, false));
    let id = original.get_id();
    assert_eq!(id, 7075);
    let new = Block::from_id(id);
    assert_eq!(new, original);
}

#[test]
fn comparator_id_test() {
    let original = Block::Comparator(Comparator::new(
        BlockDirection::West,
        ComparatorMode::Subtract,
        false,
    ));
    let id = original.get_id();
    assert_eq!(id, 11274);
    let new = Block::from_id(id);
    assert_eq!(new, original);
}
