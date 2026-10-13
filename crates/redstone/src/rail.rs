//! Rails, following the vanilla `BaseRailBlock`, `RailState`, `PoweredRailBlock` and
//! `DetectorRailBlock` classes.
//!
//! The shape of a rail is derived from the rails around it when it is placed, and rails that are
//! placed next to it are connected to it in the same way. Powered and activator rails are only
//! ever straight, detector rails report whether a minecart is on them.

use cinnabar_blocks::blocks::Block;
use cinnabar_blocks::{generic, BlockFace, BlockPos};
use cinnabar_world::World;

const RAIL: &str = "minecraft:rail";
const POWERED_RAIL: &str = "minecraft:powered_rail";
const DETECTOR_RAIL: &str = "minecraft:detector_rail";
const ACTIVATOR_RAIL: &str = "minecraft:activator_rail";

/// The offsets of the neighbours that rails connect to
const NORTH: (i32, i32, i32) = (0, 0, -1);
const SOUTH: (i32, i32, i32) = (0, 0, 1);
const EAST: (i32, i32, i32) = (1, 0, 0);
const WEST: (i32, i32, i32) = (-1, 0, 0);
const UP: (i32, i32, i32) = (0, 1, 0);

const MAX_POWER_SEARCH: u32 = 8;

fn offset(pos: BlockPos, (dx, dy, dz): (i32, i32, i32)) -> BlockPos {
    BlockPos::new(pos.x + dx, pos.y + dy, pos.z + dz)
}

fn add(a: (i32, i32, i32), b: (i32, i32, i32)) -> (i32, i32, i32) {
    (a.0 + b.0, a.1 + b.1, a.2 + b.2)
}

pub fn is_rail_name(name: &str) -> bool {
    matches!(name, RAIL | POWERED_RAIL | DETECTOR_RAIL | ACTIVATOR_RAIL)
}

pub fn is_rail(id: u32) -> bool {
    is_rail_name(generic::state_name(id))
}

/// Rails that can only be straight, which are the powered, detector and activator rails
fn is_straight_name(name: &str) -> bool {
    name != RAIL
}

fn is_rail_at(world: &impl World, pos: BlockPos) -> bool {
    matches!(world.get_block(pos), Block::Generic(id) if is_rail(id))
}

/// The positions the rail with the shape connects to, like `RailState.updateConnections`
pub fn connections(pos: BlockPos, shape: &str) -> Vec<BlockPos> {
    let (a, b) = match shape {
        "north_south" => (NORTH, SOUTH),
        "east_west" => (WEST, EAST),
        "ascending_east" => (WEST, add(EAST, UP)),
        "ascending_west" => (add(WEST, UP), EAST),
        "ascending_north" => (add(NORTH, UP), SOUTH),
        "ascending_south" => (NORTH, add(SOUTH, UP)),
        "south_east" => (EAST, SOUTH),
        "south_west" => (WEST, SOUTH),
        "north_west" => (WEST, NORTH),
        "north_east" => (EAST, NORTH),
        _ => (NORTH, SOUTH),
    };
    vec![offset(pos, a), offset(pos, b)]
}

/// A rail that is being reshaped, like `RailState`
struct RailState {
    pos: BlockPos,
    id: u32,
    straight: bool,
    connections: Vec<BlockPos>,
}

impl RailState {
    fn new(pos: BlockPos, id: u32) -> RailState {
        let shape = generic::get_property(id, "shape").unwrap_or("north_south");
        RailState {
            pos,
            id,
            straight: is_straight_name(generic::state_name(id)),
            connections: connections(pos, shape),
        }
    }

    /// The rail at the position, which can also be one block above or below it
    fn get(world: &impl World, pos: BlockPos) -> Option<RailState> {
        for candidate in [
            pos,
            pos.offset(BlockFace::Top),
            pos.offset(BlockFace::Bottom),
        ] {
            if let Block::Generic(id) = world.get_block(candidate)
                && is_rail(id)
            {
                return Some(RailState::new(candidate, id));
            }
        }
        None
    }

    fn has_connection(&self, pos: BlockPos) -> bool {
        // Only the horizontal position counts, the height of a slope is not part of it
        self.connections
            .iter()
            .any(|connection| connection.x == pos.x && connection.z == pos.z)
    }

    /// Drops the connections to rails that don't connect back, like `removeSoftConnections`
    fn remove_soft_connections(&mut self, world: &impl World) {
        let mut i = 0;
        while i < self.connections.len() {
            match RailState::get(world, self.connections[i]) {
                Some(rail) if rail.has_connection(self.pos) => {
                    self.connections[i] = rail.pos;
                    i += 1;
                }
                _ => {
                    self.connections.remove(i);
                }
            }
        }
    }

    fn can_connect_to(&self, pos: BlockPos) -> bool {
        self.has_connection(pos) || self.connections.len() != 2
    }

    fn has_neighbor_rail(&self, world: &impl World, pos: BlockPos) -> bool {
        match RailState::get(world, pos) {
            None => false,
            Some(mut neighbor) => {
                neighbor.remove_soft_connections(world);
                neighbor.can_connect_to(self.pos)
            }
        }
    }

    fn set_connections_from_shape(&mut self, shape: &str) {
        self.connections = connections(self.pos, shape);
    }

    /// Connects this rail to the rail at `other`, which then becomes the shape of the rail,
    /// like `RailState.connectTo`
    fn connect_to(&mut self, world: &mut impl World, other: BlockPos) {
        self.connections.push(other);
        let north = offset(self.pos, NORTH);
        let south = offset(self.pos, SOUTH);
        let west = offset(self.pos, WEST);
        let east = offset(self.pos, EAST);
        let n = self.has_connection(north);
        let s = self.has_connection(south);
        let w = self.has_connection(west);
        let e = self.has_connection(east);
        let mut shape = None;
        if n || s {
            shape = Some("north_south");
        }
        if w || e {
            shape = Some("east_west");
        }
        if !self.straight {
            if s && e && !n && !w {
                shape = Some("south_east");
            }
            if s && w && !n && !e {
                shape = Some("south_west");
            }
            if n && w && !s && !e {
                shape = Some("north_west");
            }
            if n && e && !s && !w {
                shape = Some("north_east");
            }
        }
        shape = self.slope_shape(world, shape, north, south, east, west);
        let shape = shape.unwrap_or("north_south");
        self.set_shape(world, shape);
    }

    /// Turns a flat north-south or east-west shape into a slope when the rail above the
    /// connection is a rail, like `RailState`
    fn slope_shape(
        &self,
        world: &impl World,
        shape: Option<&'static str>,
        north: BlockPos,
        south: BlockPos,
        east: BlockPos,
        west: BlockPos,
    ) -> Option<&'static str> {
        let mut shape = shape;
        if shape == Some("north_south") {
            if is_rail_at(world, offset(north, UP)) {
                shape = Some("ascending_north");
            }
            if is_rail_at(world, offset(south, UP)) {
                shape = Some("ascending_south");
            }
        }
        if shape == Some("east_west") {
            if is_rail_at(world, offset(east, UP)) {
                shape = Some("ascending_east");
            }
            if is_rail_at(world, offset(west, UP)) {
                shape = Some("ascending_west");
            }
        }
        shape
    }

    fn set_shape(&mut self, world: &mut impl World, shape: &'static str) {
        self.id = generic::with_property(self.id, "shape", shape);
        world.set_block(self.pos, Block::Generic(self.id));
    }

    /// Computes the shape from the rails around it and updates the rails it connects to, like
    /// `RailState.place`
    fn place(
        &mut self,
        world: &mut impl World,
        has_signal: bool,
        first: bool,
        default_shape: &'static str,
    ) {
        let north = offset(self.pos, NORTH);
        let south = offset(self.pos, SOUTH);
        let west = offset(self.pos, WEST);
        let east = offset(self.pos, EAST);
        let n = self.has_neighbor_rail(world, north);
        let s = self.has_neighbor_rail(world, south);
        let w = self.has_neighbor_rail(world, west);
        let e = self.has_neighbor_rail(world, east);

        let north_or_south = n || s;
        let west_or_east = w || e;
        let mut shape = match (north_or_south, west_or_east) {
            (true, false) => Some("north_south"),
            (false, true) => Some("east_west"),
            _ => None,
        };

        let south_and_east = s && e;
        let south_and_west = s && w;
        let north_and_east = n && e;
        let north_and_west = n && w;
        if !self.straight {
            if south_and_east && !n && !w {
                shape = Some("south_east");
            }
            if south_and_west && !n && !e {
                shape = Some("south_west");
            }
            if north_and_west && !s && !e {
                shape = Some("north_west");
            }
            if north_and_east && !s && !w {
                shape = Some("north_east");
            }
        }

        if shape.is_none() {
            if north_or_south && west_or_east {
                shape = Some(default_shape);
            } else if north_or_south {
                shape = Some("north_south");
            } else if west_or_east {
                shape = Some("east_west");
            }

            if !self.straight {
                // With a signal the curves prefer the south side, without one the north side
                let curves: [(bool, &'static str); 4] = if has_signal {
                    [
                        (south_and_east, "south_east"),
                        (south_and_west, "south_west"),
                        (north_and_east, "north_east"),
                        (north_and_west, "north_west"),
                    ]
                } else {
                    [
                        (north_and_west, "north_west"),
                        (north_and_east, "north_east"),
                        (south_and_west, "south_west"),
                        (south_and_east, "south_east"),
                    ]
                };
                for (matches, curve) in curves {
                    if matches {
                        shape = Some(curve);
                    }
                }
            }
        }

        let shape = self.slope_shape(world, shape, north, south, east, west);
        let shape = shape.unwrap_or(default_shape);
        self.set_connections_from_shape(shape);
        self.id = generic::with_property(self.id, "shape", shape);

        if first || world.get_block_raw(self.pos) != self.id {
            world.set_block(self.pos, Block::Generic(self.id));
            for i in 0..self.connections.len() {
                let connection = self.connections[i];
                if let Some(mut neighbor) = RailState::get(world, connection) {
                    neighbor.remove_soft_connections(world);
                    if neighbor.can_connect_to(self.pos) {
                        neighbor.connect_to(world, self.pos);
                    }
                }
            }
        }
    }
}

/// Reshapes the rail at the position, like `BaseRailBlock.updateDir`
fn update_dir(world: &mut impl World, pos: BlockPos, id: u32, first: bool) {
    let has_signal = redstone_has_signal(world, pos);
    let shape = generic::get_property(id, "shape").unwrap_or("north_south");
    let shape = generic_shape(shape);
    RailState::new(pos, id).place(world, has_signal, first, shape);
}

fn generic_shape(shape: &str) -> &'static str {
    const SHAPES: [&str; 10] = [
        "north_south",
        "east_west",
        "ascending_east",
        "ascending_west",
        "ascending_north",
        "ascending_south",
        "south_east",
        "south_west",
        "north_west",
        "north_east",
    ];
    SHAPES
        .iter()
        .copied()
        .find(|known| *known == shape)
        .unwrap_or("north_south")
}

fn redstone_has_signal(world: &impl World, pos: BlockPos) -> bool {
    super::redstone_lamp_should_be_lit(world, pos)
}

/// Called when a rail was placed at the position, like `BaseRailBlock.onPlace`
pub fn on_placed(world: &mut impl World, pos: BlockPos) {
    let Block::Generic(id) = world.get_block(pos) else {
        return;
    };
    if !is_rail(id) {
        return;
    }
    update_dir(world, pos, id, true);
    if !is_straight_name(generic::state_name(id)) {
        return;
    }
    update_powered(world, pos);
}

/// Called when a block next to a rail changed. Rails that lost their support are removed by the
/// caller, which knows how to destroy blocks.
pub fn on_neighbor_changed(world: &mut impl World, pos: BlockPos) {
    let Block::Generic(id) = world.get_block(pos) else {
        return;
    };
    if !is_rail(id) {
        return;
    }
    let name = generic::state_name(id);
    if name == POWERED_RAIL || name == ACTIVATOR_RAIL {
        update_powered(world, pos);
    }
}

/// Whether the rail has a solid block below it, and for slopes one at the side of the upper end.
/// Rails without support are removed, like `BaseRailBlock.shouldBeRemoved`.
pub fn is_supported(world: &impl World, pos: BlockPos, id: u32) -> bool {
    if !world.get_block(offset(pos, (0, -1, 0))).is_cube() {
        return false;
    }
    let side = match generic::get_property(id, "shape") {
        Some("ascending_east") => EAST,
        Some("ascending_west") => WEST,
        Some("ascending_north") => NORTH,
        Some("ascending_south") => SOUTH,
        _ => return true,
    };
    world.get_block(offset(pos, side)).is_cube()
}

/// Whether the rail at the position is a powered or activator rail that is powered
fn powered_rail_is_powered(id: u32) -> bool {
    generic::get_property(id, "powered") == Some("true")
}

/// Powers a powered or activator rail when it is receiving redstone power, directly or through
/// the powered rails next to it, like `PoweredRailBlock.updateState`
fn update_powered(world: &mut impl World, pos: BlockPos) {
    let Block::Generic(id) = world.get_block(pos) else {
        return;
    };
    let name = generic::state_name(id);
    if name != POWERED_RAIL && name != ACTIVATOR_RAIL {
        return;
    }
    let was_powered = powered_rail_is_powered(id);
    let shape = generic::get_property(id, "shape").unwrap_or("north_south");
    let should_power = super::redstone_lamp_should_be_lit(world, pos)
        || find_powered_signal(world, pos, shape, true, 0)
        || find_powered_signal(world, pos, shape, false, 0);
    if should_power != was_powered {
        let value = if should_power { "true" } else { "false" };
        world.set_block(
            pos,
            Block::Generic(generic::with_property(id, "powered", value)),
        );
        // Like a block update, this lets the rails next to it (which can be powered through it)
        // and the block below (which is powered by it) know about the change
        for face in &BlockFace::values() {
            let neighbor = pos.offset(*face);
            super::update(world.get_block(neighbor), world, neighbor);
        }
    }
}

/// Searches along the powered rails from the position for redstone power, like
/// `PoweredRailBlock.findPoweredRailSignal`
fn find_powered_signal(
    world: &impl World,
    pos: BlockPos,
    shape: &str,
    forward: bool,
    depth: u32,
) -> bool {
    if depth >= MAX_POWER_SEARCH {
        return false;
    }
    let (mut x, mut y, mut z) = (pos.x, pos.y, pos.z);
    let mut check_below = true;
    let mut dir = shape;
    match shape {
        "north_south" => {
            if forward {
                z += 1;
            } else {
                z -= 1;
            }
        }
        "east_west" => {
            if forward {
                x -= 1;
            } else {
                x += 1;
            }
        }
        "ascending_east" => {
            if forward {
                x -= 1;
            } else {
                x += 1;
                y += 1;
                check_below = false;
            }
            dir = "east_west";
        }
        "ascending_west" => {
            if forward {
                x -= 1;
                y += 1;
                check_below = false;
            } else {
                x += 1;
            }
            dir = "east_west";
        }
        "ascending_north" => {
            if forward {
                z += 1;
            } else {
                z -= 1;
                y += 1;
                check_below = false;
            }
            dir = "north_south";
        }
        "ascending_south" => {
            if forward {
                z += 1;
                y += 1;
                check_below = false;
            } else {
                z -= 1;
            }
            dir = "north_south";
        }
        _ => return false,
    }
    is_same_rail_with_power(world, BlockPos::new(x, y, z), forward, depth, dir)
        || check_below
            && is_same_rail_with_power(world, BlockPos::new(x, y - 1, z), forward, depth, dir)
}

fn is_same_rail_with_power(
    world: &impl World,
    pos: BlockPos,
    forward: bool,
    depth: u32,
    dir: &str,
) -> bool {
    let Block::Generic(id) = world.get_block(pos) else {
        return false;
    };
    if generic::state_name(id) != POWERED_RAIL {
        return false;
    }
    let my_shape = generic::get_property(id, "shape").unwrap_or("north_south");
    let perpendicular = match dir {
        "east_west" => matches!(
            my_shape,
            "north_south" | "ascending_north" | "ascending_south"
        ),
        _ => matches!(my_shape, "east_west" | "ascending_east" | "ascending_west"),
    };
    if perpendicular {
        return false;
    }
    if !powered_rail_is_powered(id) {
        return false;
    }
    super::redstone_lamp_should_be_lit(world, pos)
        || find_powered_signal(world, pos, my_shape, forward, depth + 1)
}

/// Called for the rails in the redstone update, like the neighbour updates of the powered rails
pub fn on_redstone_update(world: &mut impl World, pos: BlockPos, id: u32) {
    let name = generic::state_name(id);
    if name == POWERED_RAIL || name == ACTIVATOR_RAIL {
        update_powered(world, pos);
    }
}

/// Whether the rail is a detector rail, whatever its state is
pub fn is_detector(id: u32) -> bool {
    generic::state_name(id) == DETECTOR_RAIL
}

/// Whether the detector rail is pressed by a minecart, which is the signal it gives
pub fn is_detector_powered(id: u32) -> bool {
    generic::state_name(id) == DETECTOR_RAIL && powered_rail_is_powered(id)
}

/// Sets the state of a detector rail when a minecart enters or leaves it. Returns whether the
/// state changed.
pub fn set_detector_powered(world: &mut impl World, pos: BlockPos, powered: bool) -> bool {
    let Block::Generic(id) = world.get_block(pos) else {
        return false;
    };
    if generic::state_name(id) != DETECTOR_RAIL || powered_rail_is_powered(id) == powered {
        return false;
    }
    let value = if powered { "true" } else { "false" };
    world.set_block(
        pos,
        Block::Generic(generic::with_property(id, "powered", value)),
    );
    super::update_surrounding_blocks(world, pos);
    super::update_surrounding_blocks(world, pos.offset(BlockFace::Bottom));
    true
}

/// The shape a rail placed by a player gets, which is east-west when the player looks east or
/// west, like `BaseRailBlock.getStateForPlacement`
pub fn placement_shape(looking_east_west: bool) -> &'static str {
    if looking_east_west {
        "east_west"
    } else {
        "north_south"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cinnabar_world::testing::TestWorld;

    /// A world with a stone floor at y = 0
    fn floor_world() -> TestWorld {
        let mut world = TestWorld::new(2, 2, 2);
        let stone = generic::default_state("minecraft:stone").unwrap();
        for x in 0..32 {
            for z in 0..32 {
                world.set_block_raw(BlockPos::new(x, 0, z), stone);
            }
        }
        world
    }

    /// Places a block of the name and runs the rail logic for it, like a player does
    fn place(world: &mut TestWorld, pos: BlockPos, name: &str) {
        world.set_block_raw(pos, generic::default_state(name).unwrap());
        if is_rail_name(name) {
            on_placed(world, pos);
        }
    }

    fn shape(world: &TestWorld, pos: BlockPos) -> &'static str {
        let id = world.get_block_raw(pos);
        generic::get_property(id, "shape").unwrap()
    }

    fn powered(world: &TestWorld, pos: BlockPos) -> bool {
        generic::get_property(world.get_block_raw(pos), "powered") == Some("true")
    }

    fn at(x: i32, y: i32, z: i32) -> BlockPos {
        BlockPos::new(x, y, z)
    }

    #[test]
    fn straight_rails_connect() {
        let mut world = floor_world();
        place(&mut world, at(5, 1, 5), "minecraft:rail");
        place(&mut world, at(6, 1, 5), "minecraft:rail");
        assert_eq!(shape(&world, at(6, 1, 5)), "east_west");
        assert_eq!(shape(&world, at(5, 1, 5)), "east_west");

        place(&mut world, at(5, 1, 6), "minecraft:rail");
        assert_eq!(shape(&world, at(5, 1, 6)), "north_south");
    }

    #[test]
    fn single_rail_uses_default_shape() {
        let mut world = floor_world();
        place(&mut world, at(5, 1, 5), "minecraft:rail");
        assert_eq!(shape(&world, at(5, 1, 5)), "north_south");
    }

    #[test]
    fn curve_connects_east_and_south() {
        let mut world = floor_world();
        place(&mut world, at(6, 1, 5), "minecraft:rail");
        place(&mut world, at(5, 1, 6), "minecraft:rail");
        place(&mut world, at(5, 1, 5), "minecraft:rail");
        assert_eq!(shape(&world, at(5, 1, 5)), "south_east");
    }

    #[test]
    fn slope_goes_up_to_the_rail_above() {
        let mut world = floor_world();
        world.set_block_raw(
            at(6, 1, 5),
            generic::default_state("minecraft:stone").unwrap(),
        );
        place(&mut world, at(6, 2, 5), "minecraft:rail");
        place(&mut world, at(5, 1, 5), "minecraft:rail");
        assert_eq!(shape(&world, at(5, 1, 5)), "ascending_east");
    }

    #[test]
    fn slope_needs_support_at_its_upper_side() {
        let mut world = floor_world();
        place(&mut world, at(5, 1, 5), "minecraft:rail");
        place(&mut world, at(6, 2, 5), "minecraft:rail");
        let id = world.get_block_raw(at(5, 1, 5));
        world.set_block_raw(
            at(5, 1, 5),
            generic::with_property(id, "shape", "ascending_east"),
        );
        assert!(!is_supported(
            &world,
            at(5, 1, 5),
            world.get_block_raw(at(5, 1, 5))
        ));
        world.set_block_raw(
            at(6, 1, 5),
            generic::default_state("minecraft:stone").unwrap(),
        );
        assert!(is_supported(
            &world,
            at(5, 1, 5),
            world.get_block_raw(at(5, 1, 5))
        ));
    }

    #[test]
    fn powered_rail_is_straight() {
        let mut world = floor_world();
        place(&mut world, at(6, 1, 5), "minecraft:rail");
        place(&mut world, at(5, 1, 6), "minecraft:powered_rail");
        place(&mut world, at(5, 1, 5), "minecraft:powered_rail");
        assert_eq!(shape(&world, at(5, 1, 5)), "north_south");
    }

    #[test]
    fn powered_rail_follows_redstone_block() {
        let mut world = floor_world();
        place(&mut world, at(5, 1, 5), "minecraft:powered_rail");
        assert!(!powered(&world, at(5, 1, 5)));
        place(&mut world, at(5, 1, 6), "minecraft:redstone_block");
        update_powered(&mut world, at(5, 1, 5));
        assert!(powered(&world, at(5, 1, 5)));
    }

    #[test]
    fn powered_rails_power_each_other_up_to_eight_rails() {
        let mut world = floor_world();
        for x in 5..=14 {
            place(&mut world, at(x, 1, 5), "minecraft:powered_rail");
        }
        place(&mut world, at(4, 1, 5), "minecraft:redstone_block");
        update_powered(&mut world, at(5, 1, 5));
        // Every rail searches up to eight rails for a source, and the rails powered by it
        // update their neighbours in turn, which reaches the rail nine after the source
        for x in 5..=13 {
            assert!(
                powered(&world, at(x, 1, 5)),
                "rail at {x} should be powered"
            );
        }
        assert!(!powered(&world, at(14, 1, 5)));
    }

    #[test]
    fn detector_rail_powers_the_block_above() {
        let mut world = floor_world();
        place(&mut world, at(5, 1, 5), "minecraft:detector_rail");
        assert!(!is_detector_powered(world.get_block_raw(at(5, 1, 5))));
        assert!(set_detector_powered(&mut world, at(5, 1, 5), true));
        assert!(is_detector_powered(world.get_block_raw(at(5, 1, 5))));
        assert!(!set_detector_powered(&mut world, at(5, 1, 5), true));
    }
}
