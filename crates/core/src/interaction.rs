use crate::config::CONFIG;
use crate::generic_placement;
use crate::player::{Gamemode, Player, PlayerPos};
use crate::plot::fluids;
use crate::plot::{PlotWorld, PLOT_BLOCK_HEIGHT};
use mchprs_blocks::block_entities::{BlockEntity, ContainerType, Inventory};
use mchprs_blocks::items::{Item, ItemStack};
use mchprs_blocks::{blocks::*, BlockDirection, BlockFacing};
use mchprs_blocks::{BlockFace, BlockPos};
use mchprs_network::packets::clientbound::{COpenSignEditor, ClientBoundPacket};
use mchprs_redstone as redstone;
use mchprs_utils::nbt_unwrap_val;
use mchprs_world::game_event::{block_center, dampens_vibrations, EventContext, GameEvent};
use mchprs_world::{storage::Chunk, PistonQueue, TickPriority, World};

/// A world where every block is air. Placement state computed against it ignores neighbours,
/// which is what a plot needs when the neighbouring blocks belong to another plot's thread.
#[derive(Default)]
pub struct NoBlocks {
    piston_queue: PistonQueue,
}

impl World for NoBlocks {
    fn get_block_raw(&self, _pos: BlockPos) -> u32 {
        0
    }

    fn set_block_raw(&mut self, _pos: BlockPos, _block: u32) -> bool {
        false
    }

    fn delete_block_entity(&mut self, _pos: BlockPos) {}

    fn get_block_entity(&self, _pos: BlockPos) -> Option<&BlockEntity> {
        None
    }

    fn set_block_entity(&mut self, _pos: BlockPos, _block_entity: BlockEntity) {}

    fn get_chunk(&self, _x: i32, _z: i32) -> Option<&Chunk> {
        None
    }

    fn get_chunk_mut(&mut self, _x: i32, _z: i32) -> Option<&mut Chunk> {
        None
    }

    fn schedule_tick(&mut self, _pos: BlockPos, _delay: u32, _priority: TickPriority) {}

    fn pending_tick_at(&mut self, _pos: BlockPos) -> bool {
        false
    }

    fn piston_queue(&mut self) -> &mut PistonQueue {
        &mut self.piston_queue
    }
}

/// Where a block was clicked
#[derive(Clone, Copy)]
pub struct BlockHit {
    pub face: BlockFace,
    /// The position of the click inside the block
    pub cursor: (f32, f32, f32),
}

pub fn on_use(
    block: Block,
    world: &mut impl World,
    player: &mut Player,
    pos: BlockPos,
    item_in_hand: Option<Item>,
    hit: BlockHit,
) -> ActionResult {
    if redstone::on_use(block, world, pos) {
        return ActionResult::Success;
    }

    match block {
        Block::SeaPickle {
            pickles,
            waterlogged,
        } => {
            if let Some(Item::SeaPickle) = item_in_hand
                && pickles < 4
            {
                world.set_block(
                    pos,
                    Block::SeaPickle {
                        pickles: pickles + 1,
                        waterlogged,
                    },
                );
            }
            ActionResult::Success
        }
        Block::EndPortalFrame { eye, facing } => {
            if let Some(Item::EnderEye) = item_in_hand
                && !eye
            {
                world.set_block(pos, Block::EndPortalFrame { eye: true, facing });
                redstone::update_surrounding_blocks(world, pos);
                return ActionResult::Success;
            }
            ActionResult::Pass
        }
        Block::Generic(id) => {
            if generic_placement::use_block(world, pos, id, item_in_hand, hit) {
                ActionResult::Success
            } else if open_container(block, world, player, pos) {
                ActionResult::Success
            } else {
                ActionResult::Pass
            }
        }
        b if b.has_block_entity() => {
            open_container(b, world, player, pos);
            ActionResult::Success
        }
        _ => ActionResult::Pass,
    }
}

/// Opens the screen of the container block for the player. Returns false if the block is none.
fn open_container(block: Block, world: &impl World, player: &mut Player, pos: BlockPos) -> bool {
    let Some(ty) = ContainerType::for_block(block) else {
        return false;
    };
    let inventory = match world.get_block_entity(pos) {
        Some(BlockEntity::Container { inventory, .. }) => {
            Inventory::from_entries(inventory, ty.num_slots() as usize)
        }
        _ => Inventory::new(ty.num_slots() as usize),
    };
    player.open_container(pos, &inventory, ty);
    true
}

fn get_sign_placement(
    context: &UseOnBlockContext<'_>,
    standard: fn(u8) -> Block,
    wall: fn(BlockDirection) -> Block,
) -> Block {
    let rotation = (((180.0 + context.player.yaw) * 16.0 / 360.0) + 0.5).floor() as u8 & 15;

    match context.block_face {
        BlockFace::Bottom => Block::Air,
        BlockFace::Top => standard(rotation),
        _ => wall(context.block_face.unwrap_direction()),
    }
}

pub fn get_state_for_placement(
    world: &impl World,
    pos: BlockPos,
    item: Item,
    context: &UseOnBlockContext<'_>,
) -> Block {
    let simple_block = item.get_simple_placement();
    macro_rules! sign_placement {
        ($standard_block:ident, $wall_block:ident) => {
            get_sign_placement(
                context,
                |rotation| Block::$standard_block {
                    waterlogged: false,
                    rotation,
                },
                |facing| Block::$wall_block {
                    waterlogged: false,
                    facing,
                },
            )
        };
    }
    let block = match item {
        Item::OakSign => sign_placement!(OakSign, OakWallSign),
        Item::SpruceSign => sign_placement!(SpruceSign, SpruceWallSign),
        Item::BirchSign => sign_placement!(BirchSign, BirchWallSign),
        Item::AcaciaSign => sign_placement!(AcaciaSign, AcaciaWallSign),
        Item::JungleSign => sign_placement!(JungleSign, JungleWallSign),
        Item::DarkOakSign => sign_placement!(DarkOakSign, DarkOakWallSign),
        Item::CrimsonSign => sign_placement!(CrimsonSign, CrimsonWallSign),
        Item::WarpedSign => sign_placement!(WarpedSign, WarpedWallSign),
        Item::BambooSign => sign_placement!(BambooSign, BambooWallSign),
        Item::CherrySign => sign_placement!(CherrySign, CherryWallSign),
        Item::MangroveSign => sign_placement!(MangroveSign, MangroveWallSign),
        Item::SeaPickle => Block::SeaPickle {
            pickles: 1,
            waterlogged: false,
        },
        Item::Furnace => Block::Furnace {
            facing: context.player.get_direction().opposite(),
            lit: false,
        },
        Item::OakPressurePlate => Block::OakPressurePlate { powered: false },
        Item::SprucePressurePlate => Block::SprucePressurePlate { powered: false },
        Item::BirchPressurePlate => Block::BirchPressurePlate { powered: false },
        Item::JunglePressurePlate => Block::JunglePressurePlate { powered: false },
        Item::AcaciaPressurePlate => Block::AcaciaPressurePlate { powered: false },
        Item::DarkOakPressurePlate => Block::DarkOakPressurePlate { powered: false },
        Item::MangrovePressurePlate => Block::MangrovePressurePlate { powered: false },
        Item::CherryPressurePlate => Block::CherryPressurePlate { powered: false },
        Item::BambooPressurePlate => Block::BambooPressurePlate { powered: false },
        Item::CrimsonPressurePlate => Block::CrimsonPressurePlate { powered: false },
        Item::WarpedPressurePlate => Block::WarpedPressurePlate { powered: false },
        Item::StonePressurePlate => Block::StonePressurePlate { powered: false },
        Item::PolishedBlackstonePressurePlate => {
            Block::PolishedBlackstonePressurePlate { powered: false }
        }
        Item::Lever => {
            let face = match context.block_face {
                BlockFace::Top => LeverFace::Floor,
                BlockFace::Bottom => LeverFace::Ceiling,
                _ => LeverFace::Wall,
            };
            let facing = if face == LeverFace::Wall {
                context.block_face.unwrap_direction()
            } else {
                context.player.get_direction()
            };
            Block::Lever {
                face,
                facing,
                powered: false,
            }
        }
        Item::RedstoneTorch => match context.block_face {
            BlockFace::Top | BlockFace::Bottom => Block::RedstoneTorch { lit: true },
            face => Block::RedstoneWallTorch {
                lit: true,
                facing: face.unwrap_direction(),
            },
        },
        Item::TripwireHook => match context.block_face {
            BlockFace::Bottom | BlockFace::Top => Block::Air,
            direction => Block::TripwireHook {
                facing: direction.unwrap_direction(),
                powered: false,
                attached: false,
            },
        },
        Item::StoneButton => {
            let face = match context.block_face {
                BlockFace::Top => LeverFace::Floor,
                BlockFace::Bottom => LeverFace::Ceiling,
                _ => LeverFace::Wall,
            };
            let facing = if face == LeverFace::Wall {
                context.block_face.unwrap_direction()
            } else {
                context.player.get_direction()
            };
            Block::StoneButton {
                face,
                facing,
                powered: false,
            }
        }
        Item::RedstoneLamp => Block::RedstoneLamp {
            lit: redstone::redstone_lamp_should_be_lit(world, pos),
        },
        Item::Hopper => Block::Hopper {
            // A powered hopper is locked by the first neighbor update
            enabled: true,
            facing: match context.block_face {
                BlockFace::North => HopperFacing::South,
                BlockFace::South => HopperFacing::North,
                BlockFace::West => HopperFacing::East,
                BlockFace::East => HopperFacing::West,
                _ => HopperFacing::Down,
            },
        },
        Item::Repeater => Block::Repeater(redstone::repeater::get_state_for_placement(
            world,
            pos,
            context.player.get_direction().opposite(),
        )),
        Item::Comparator => Block::Comparator(Comparator::new(
            context.player.get_direction().opposite(),
            ComparatorMode::Compare,
            false,
        )),
        Item::Redstone => Block::RedstoneWire(redstone::wire::get_state_for_placement(world, pos)),
        // TODO: Barrel facing
        Item::Barrel => Block::Barrel {
            facing: BlockFacing::Up,
            open: false,
        },
        Item::Target => Block::Target { power: 0 },
        Item::SmoothStoneSlab => Block::SmoothStoneSlab {
            ty: SlabType::Top,
            waterlogged: false,
        },
        Item::QuartzSlab => Block::QuartzSlab {
            ty: SlabType::Top,
            waterlogged: false,
        },
        Item::IronTrapdoor => match context.block_face {
            BlockFace::Bottom => Block::IronTrapdoor {
                facing: context.player.get_direction().opposite(),
                half: TrapdoorHalf::Top,
                powered: false,
                open: false,
                waterlogged: false,
            },
            BlockFace::Top => Block::IronTrapdoor {
                facing: context.player.get_direction().opposite(),
                half: TrapdoorHalf::Bottom,
                open: false,
                waterlogged: false,
                powered: false,
            },
            _ => Block::IronTrapdoor {
                facing: context.block_face.unwrap_direction(),
                half: if context.cursor_y > 0.5 {
                    TrapdoorHalf::Top
                } else {
                    TrapdoorHalf::Bottom
                },
                open: false,
                waterlogged: false,
                powered: false,
            },
        },
        Item::NoteBlock => Block::NoteBlock {
            instrument: Instrument::Harp,
            note: 0,
            powered: false,
        },
        Item::BoneBlock => Block::BoneBlock { axis: BlockAxis::Y },
        Item::HayBlock => Block::HayBlock { axis: BlockAxis::Y },
        Item::EndPortalFrame => Block::EndPortalFrame {
            eye: false,
            facing: context.player.get_direction().opposite(),
        },
        _ => generic_placement::state_for_placement(item, context),
    };
    let block = simple_block.unwrap_or(block);
    if is_valid_position(block, world, pos) {
        block
    } else {
        Block::Air
    }
}

fn read_block_entity_tag(nbt: &nbt::Blob, block_id: &str) -> Option<BlockEntity> {
    if let nbt::Value::Compound(compound) = &nbt["BlockEntityTag"] {
        let id = match nbt.get("Id").or_else(|| nbt.get("id")) {
            Some(id) => nbt_unwrap_val!(id, nbt::Value::String),
            None => block_id,
        };
        return BlockEntity::from_nbt(id, compound);
    }

    None
}

/// Updates the blocks around a position where a piston changed a block
pub fn piston_notify<W: World>(world: &mut W, pos: BlockPos) {
    change_neighbors(world, pos);
    generic_placement::update_shapes_around(world, pos);
}

pub fn place_in_world(
    block: Block,
    world: &mut impl World,
    pos: BlockPos,
    nbt: &Option<nbt::Blob>,
) {
    if (block.has_block_entity() || ContainerType::for_block(block).is_some())
        && let Some(nbt) = nbt
        && let Some(block_entity) = read_block_entity_tag(nbt, block.get_name())
    {
        world.set_block_entity(pos, block_entity);
    };
    world.set_block(pos, block);
    if let Block::Generic(id) = block
        && mchprs_redstone::rail::is_rail(id)
    {
        mchprs_redstone::rail::on_placed(world, pos);
    }
    if let Block::Hopper { .. } = block {
        redstone::update(block, world, pos);
    }
    change_surrounding_blocks(world, pos);
    if let Block::RedstoneWire { .. } = block {
        redstone::update_wire_neighbors(world, pos);
    } else {
        redstone::update_surrounding_blocks(world, pos);
    }
    // Pistons check for power when they are placed
    if let Block::Generic(id) = block
        && redstone::piston::piston_base(id).is_some()
    {
        redstone::piston::check_if_extend(world, pos);
    }
}

pub fn destroy(block: Block, world: &mut impl World, pos: BlockPos) {
    if block.has_block_entity() || ContainerType::for_block(block).is_some() {
        world.delete_block_entity(pos);
        redstone::transport::update_comparators_around(world, pos);
    }

    generic_placement::remove_partner(world, block, pos);
    generic_placement::remove_piston_partner(world, block, pos);

    match block {
        Block::RedstoneWire { .. } => {
            world.set_block(pos, Block::Air);
            change_surrounding_blocks(world, pos);
            redstone::update_wire_neighbors(world, pos);
        }
        Block::Lever { face, facing, .. } => {
            world.set_block(pos, Block::Air);
            // This is a horrible idea, don't do this.
            // One day this will be fixed, but for now... too bad!
            match face {
                LeverFace::Ceiling => {
                    change_surrounding_blocks(world, pos.offset(BlockFace::Top));
                    redstone::update_surrounding_blocks(world, pos.offset(BlockFace::Top));
                }
                LeverFace::Floor => {
                    change_surrounding_blocks(world, pos.offset(BlockFace::Bottom));
                    redstone::update_surrounding_blocks(world, pos.offset(BlockFace::Bottom));
                }
                LeverFace::Wall => {
                    change_surrounding_blocks(world, pos.offset(facing.opposite().block_face()));
                    redstone::update_surrounding_blocks(
                        world,
                        pos.offset(facing.opposite().block_face()),
                    );
                }
            }
        }
        _ => {
            if block.is_waterlogged() {
                // The water stays where the block was
                world.set_block(pos, fluids::water_source());
                world.schedule_fluid_tick(pos);
            } else {
                world.set_block(pos, Block::Air);
            }
            change_surrounding_blocks(world, pos);
            redstone::update_surrounding_blocks(world, pos);
            if block.is_sculk_sensor() {
                // The block below could have been powered strongly by the sensor
                redstone::sculk::on_removed(world, pos);
            }
            generic_placement::after_remove(world, pos);
        }
    }
}

pub fn is_valid_position(block: Block, world: &impl World, pos: BlockPos) -> bool {
    if world.is_cursed() {
        return true;
    }

    if let Block::Generic(id) = block
        && mchprs_redstone::rail::is_rail(id)
    {
        return mchprs_redstone::rail::is_supported(world, pos, id);
    }

    let check_bottom = matches!(
        block,
        Block::RedstoneWire(_)
            | Block::Comparator(_)
            | Block::Repeater(_)
            | Block::RedstoneTorch { .. }
            | Block::Lever {
                face: LeverFace::Floor,
                ..
            }
            | Block::StoneButton {
                face: LeverFace::Floor,
                ..
            }
    ) || block.is_sign();

    let check_top = matches!(
        block,
        Block::Lever {
            face: LeverFace::Ceiling,
            ..
        } | Block::StoneButton {
            face: LeverFace::Ceiling,
            ..
        }
    );

    let check_parent = block.get_wall_sign_facing().or(match block {
        Block::TripwireHook { facing, .. } => Some(facing),
        Block::StoneButton {
            face: LeverFace::Wall,
            facing,
            ..
        } => Some(facing),
        Block::Lever {
            face: LeverFace::Wall,
            facing,
            ..
        } => Some(facing),
        _ => None,
    });

    if check_bottom {
        let bottom_block = world.get_block(pos.offset(BlockFace::Bottom));
        bottom_block.is_cube()
    } else if check_top {
        let top_block = world.get_block(pos.offset(BlockFace::Top));
        top_block.is_cube()
    } else if let Some(facing) = check_parent {
        let parent_block = world.get_block(pos.offset(facing.opposite().block_face()));
        parent_block.is_cube()
    } else {
        true
    }
}

pub fn change(block: Block, world: &mut impl World, pos: BlockPos, direction: BlockFace) {
    if fluids::holds_fluid(block.get_id()) {
        world.schedule_fluid_tick(pos);
    }
    if generic_placement::is_gravity_block(block) {
        world.schedule_falling_block(pos);
    }
    if !is_valid_position(block, world, pos) {
        destroy(block, world, pos);
        return;
    }
    if let Block::Generic(id) = block
        && mchprs_redstone::rail::is_rail(id)
    {
        mchprs_redstone::rail::on_neighbor_changed(world, pos);
    }
    if let Block::RedstoneWire(wire) = block {
        let new_state = redstone::wire::on_neighbor_changed(wire, world, pos, direction);
        if world.set_block(pos, Block::RedstoneWire(new_state)) {
            redstone::update_wire_neighbors(world, pos);
        }
    }
}

/// Updates the blocks around a position where a block was changed by something other than a
/// player, like an entity
pub fn change_neighbors(world: &mut impl World, pos: BlockPos) {
    change_surrounding_blocks(world, pos);
    redstone::update_surrounding_blocks(world, pos);
}

fn change_surrounding_blocks(world: &mut impl World, pos: BlockPos) {
    for direction in &BlockFace::values() {
        let neighbor_pos = pos.offset(*direction);
        let block = world.get_block(neighbor_pos);
        change(block, world, neighbor_pos, *direction);

        // Also change diagonal blocks

        let up_pos = neighbor_pos.offset(BlockFace::Top);
        let up_block = world.get_block(up_pos);
        change(up_block, world, up_pos, *direction);

        let down_pos = neighbor_pos.offset(BlockFace::Bottom);
        let down_block = world.get_block(down_pos);
        change(down_block, world, down_pos, *direction);
    }
}

#[derive(PartialEq, Eq, Copy, Clone)]
pub enum ActionResult {
    Success,
    Pass,
}

impl ActionResult {
    pub fn is_success(self) -> bool {
        self == ActionResult::Success
    }
}

pub struct UseOnBlockContext<'a> {
    pub block_pos: BlockPos,
    pub block_face: BlockFace,
    pub player: &'a mut Player,
    pub cursor_x: f32,
    pub cursor_y: f32,
    pub cursor_z: f32,
    /// The positions of the other players, which blocks can't be placed in
    pub other_players: Vec<PlayerPos>,
}

/// Whether a block placed at the position would be inside of a player
fn collides_with_player(block: Block, pos: BlockPos, ctx: &UseOnBlockContext<'_>) -> bool {
    const HALF_WIDTH: f64 = 0.3;

    let Some(shape) = mchprs_blocks::generic::collision_box(block.get_id()) else {
        return false;
    };
    let [min_x, min_y, min_z, max_x, max_y, max_z] = shape;
    let (x, y, z) = (pos.x as f64, pos.y as f64, pos.z as f64);

    let players = std::iter::once((ctx.player.pos, ctx.player.crouching))
        .chain(ctx.other_players.iter().map(|pos| (*pos, false)));
    players.into_iter().any(|(player, crouching)| {
        let height = if crouching { 1.5 } else { 1.8 };
        player.x + HALF_WIDTH > x + min_x
            && player.x - HALF_WIDTH < x + max_x
            && player.y + height > y + min_y
            && player.y < y + max_y
            && player.z + HALF_WIDTH > z + min_z
            && player.z - HALF_WIDTH < z + max_z
    })
}

/// returns true if cancelled
pub fn use_item_on_block(
    item: &ItemStack,
    world: &mut PlotWorld,
    ctx: UseOnBlockContext<'_>,
) -> bool {
    let use_pos = ctx.block_pos;
    let use_block = world.get_block(use_pos);
    let block_pos = ctx.block_pos.offset(ctx.block_face);
    let can_place = item.item_type.is_block() && world.get_block(block_pos).can_place_block_in();

    if !ctx.player.crouching
        && on_use(
            use_block,
            world,
            ctx.player,
            ctx.block_pos,
            Some(item.item_type),
            BlockHit {
                face: ctx.block_face,
                cursor: (ctx.cursor_x, ctx.cursor_y, ctx.cursor_z),
            },
        )
        .is_success()
    {
        return false;
    }

    if !ctx.player.crouching
        && generic_placement::try_make_double_slab(world, item.item_type, use_pos, ctx.block_face)
    {
        return false;
    }

    if can_place && (0..PLOT_BLOCK_HEIGHT).contains(&block_pos.y) {
        let block = get_state_for_placement(world, block_pos, item.item_type, &ctx);

        if !CONFIG.block_in_hitbox && collides_with_player(block, block_pos, &ctx) {
            return true;
        }

        if (block.is_sign() || block.is_wall_sign())
            && !item
                .nbt
                .as_ref()
                .is_some_and(|blob| blob.content.contains_key("BlockEntityTag"))
        {
            let open_sign_editor = COpenSignEditor {
                pos_x: block_pos.x,
                pos_y: block_pos.y,
                pos_z: block_pos.z,
                // TODO: editing back text
                is_front_text: true,
            }
            .encode();
            ctx.player.client.send_packet(&open_sign_editor);
        }

        let block = fluids::waterlog_on_place(world, block_pos, block);
        place_in_world(block, world, block_pos, &item.nbt);
        generic_placement::after_place(world, block_pos);
        world.game_event(
            GameEvent::BlockPlace,
            block_center(block_pos),
            EventContext {
                sneaking: ctx.player.crouching,
                spectator: matches!(ctx.player.gamemode, Gamemode::Spectator),
                dampened: dampens_vibrations(block.get_name()),
            },
        );
        false
    } else {
        true
    }
}

#[cfg(test)]
mod rail_tests {
    use super::*;
    use mchprs_world::testing::TestWorld;

    fn floor_world() -> TestWorld {
        let mut world = TestWorld::new(2, 2, 2);
        let stone = mchprs_blocks::generic::default_state("minecraft:stone").unwrap();
        for x in 0..32 {
            for z in 0..32 {
                world.set_block_raw(BlockPos::new(x, 0, z), stone);
            }
        }
        world
    }

    fn rail(shape: &str) -> Block {
        let id = mchprs_blocks::generic::default_state("minecraft:rail").unwrap();
        Block::Generic(mchprs_blocks::generic::with_property(id, "shape", shape))
    }

    fn shape_at(world: &TestWorld, pos: BlockPos) -> Option<&'static str> {
        match world.get_block(pos) {
            Block::Generic(id) => mchprs_blocks::generic::get_property(id, "shape"),
            _ => None,
        }
    }

    #[test]
    fn placed_rails_connect_and_break_without_support() {
        let mut world = floor_world();
        place_in_world(
            rail("north_south"),
            &mut world,
            BlockPos::new(5, 1, 5),
            &None,
        );
        place_in_world(rail("east_west"), &mut world, BlockPos::new(6, 1, 5), &None);
        assert_eq!(shape_at(&world, BlockPos::new(5, 1, 5)), Some("east_west"));
        assert_eq!(shape_at(&world, BlockPos::new(6, 1, 5)), Some("east_west"));

        // Removing the support of the rail removes the rail
        world.set_block_raw(BlockPos::new(5, 0, 5), 0);
        change_neighbors(&mut world, BlockPos::new(5, 0, 5));
        assert_eq!(world.get_block(BlockPos::new(5, 1, 5)), Block::Air);
        assert!(shape_at(&world, BlockPos::new(6, 1, 5)).is_some());
    }
}
