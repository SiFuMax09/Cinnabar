//! Minecarts that drive along rails.
//!
//! The track of a rail is the straight line between the two places where it connects to its
//! neighbours. Curves are approximated by the chord, which is close enough at the speeds of a
//! minecart. A cart that is not on a track falls like any other entity.

use super::physics::{Aabb, Vec3};
use cinnabar_blocks::blocks::Block;
use cinnabar_blocks::{generic, BlockPos};
use cinnabar_redstone::rail;
use cinnabar_world::World;

/// The height of the top of a flat rail above the bottom of its block
pub const RAIL_HEIGHT: f64 = 0.0625;
/// How far the cart can be from the track and still be on it
const ON_TRACK_DISTANCE: f64 = 0.6;
/// The acceleration of gravity per tick, for the entities that fall
pub const GRAVITY: f64 = 0.04;
/// What a slope adds to the horizontal speed of a cart per tick, downhill or against it uphill
const SLOPE_PULL: f64 = 0.0078125;
/// The speed a minecart can't go faster than, in blocks per tick
const MAX_SPEED: f64 = 0.4;
/// The push of a powered rail that is powered, in blocks per tick
const POWERED_BOOST: f64 = 0.06;
/// The share of the speed that is kept each tick on a track
const FRICTION: f64 = 0.96;
/// A cart slower than this stops completely, which the game does not need to do since its
/// movement is too small to see long before this
const MIN_SPEED: f64 = 0.003;
/// A cart on a powered rail that is not powered stops below this horizontal speed and is slowed to
/// half of it above
const BRAKE_STOP_SPEED: f64 = 0.03;
/// A cart on a powered rail needs this horizontal speed to be pushed by it; slower carts are
/// only launched when there is a block next to the rail
const BOOST_MIN_SPEED: f64 = 0.01;
/// The speed of a cart that is launched off a block next to a powered rail
const LAUNCH_SPEED: f64 = 0.02;

/// A rail with the points where its track starts and ends
#[derive(Clone, Copy, Debug)]
pub struct Track {
    pub rail: BlockPos,
    pub start: Vec3,
    pub end: Vec3,
    /// The positions of the neighbours of the rail the track connects to, the start first
    connections: [BlockPos; 2],
}

/// The point on the track of the rail where the connection to the block is
fn face_point(rail_pos: BlockPos, connection: BlockPos) -> Vec3 {
    // The track of a slope ends at the top of the block at its upper end
    let y = rail_pos.y.max(connection.y) as f64 + RAIL_HEIGHT;
    Vec3::new(
        rail_pos.x as f64 + 0.5 + 0.5 * (connection.x - rail_pos.x) as f64,
        y,
        rail_pos.z as f64 + 0.5 + 0.5 * (connection.z - rail_pos.z) as f64,
    )
}

impl Track {
    pub fn new(rail_pos: BlockPos, id: u32) -> Track {
        let shape = generic::get_property(id, "shape").unwrap_or("north_south");
        let connections = rail::connections(rail_pos, shape);
        let [first, second] = [connections[0], connections[1]];
        Track {
            rail: rail_pos,
            start: face_point(rail_pos, first),
            end: face_point(rail_pos, second),
            connections: [first, second],
        }
    }

    fn vector(&self) -> Vec3 {
        self.end - self.start
    }

    pub fn length(&self) -> f64 {
        self.vector().length()
    }

    /// The direction from the start to the end
    pub fn direction(&self) -> Vec3 {
        self.vector().normalized()
    }

    /// The distance along the track of the point closest to `pos`, and that distance squared
    fn project(&self, pos: Vec3) -> (f64, f64) {
        let length = self.length();
        let along = (pos - self.start).dot(self.direction()).clamp(0.0, length);
        let closest = self.start + self.direction() * along;
        (along, (pos - closest).length_squared())
    }

    /// The point on the track at the distance from the start
    fn point(&self, along: f64) -> Vec3 {
        self.start + self.direction() * along
    }

    /// Whether the point is one of the ends of the track
    fn end_at(&self, point: Vec3) -> Option<bool> {
        if (self.start - point).length() < 1e-4 {
            Some(true)
        } else if (self.end - point).length() < 1e-4 {
            Some(false)
        } else {
            None
        }
    }
}

/// The track of the rail at the position, or the one below it. The cart is on the track that is
/// closest, if it is close enough.
pub fn find_track(world: &impl World, pos: Vec3) -> Option<(Track, f64)> {
    let block = pos.block_pos();
    let below = BlockPos::new(block.x, block.y - 1, block.z);
    let mut best: Option<(Track, f64, f64)> = None;
    for candidate in [block, below] {
        let Block::Generic(id) = world.get_block(candidate) else {
            continue;
        };
        if !rail::is_rail(id) {
            continue;
        }
        let track = Track::new(candidate, id);
        let (along, distance_squared) = track.project(pos);
        if distance_squared.sqrt() > ON_TRACK_DISTANCE {
            continue;
        }
        if best.is_none_or(|(_, _, best_distance)| distance_squared < best_distance) {
            best = Some((track, along, distance_squared));
        }
    }
    best.map(|(track, along, _)| (track, along))
}

/// The track that continues at the end of `track` where it is `at_end`
fn next_track(world: &impl World, track: &Track, at_end: bool) -> Option<Track> {
    let (connection, end) = if at_end {
        (track.connections[1], track.end)
    } else {
        (track.connections[0], track.start)
    };
    // The rail is either next to the connection or one block below it, when it goes down
    for candidate in [
        connection,
        BlockPos::new(connection.x, connection.y - 1, connection.z),
    ] {
        let Block::Generic(id) = world.get_block(candidate) else {
            continue;
        };
        if !rail::is_rail(id) {
            continue;
        }
        let next = Track::new(candidate, id);
        if next.rail != track.rail && next.end_at(end).is_some() {
            return Some(next);
        }
    }
    None
}

/// The hitbox of a minecart and of a player, which decide when a player pushes a cart
const CART_SIZE: (f64, f64) = (0.98, 0.7);
const PLAYER_SIZE: (f64, f64) = (0.6, 1.8);
/// The strength of the push two overlapping entities give each other per tick
const PUSH: f64 = 0.05;

/// The velocity a player gives a cart by overlapping with it, directed away from the player.
/// This is the push every entity gives the entities it overlaps with in the game; the track
/// then turns it into a movement along the rail.
pub fn push_from_player(cart: Vec3, player: Vec3) -> Option<Vec3> {
    let cart_box = Aabb::entity(cart, CART_SIZE.0, CART_SIZE.1);
    let player_box = Aabb::entity(player, PLAYER_SIZE.0, PLAYER_SIZE.1);
    let overlaps =
        (0..3).all(|a| cart_box.max[a] > player_box.min[a] && cart_box.min[a] < player_box.max[a]);
    if !overlaps {
        return None;
    }
    let (dx, dz) = (player.x - cart.x, player.z - cart.z);
    let distance = dx.abs().max(dz.abs());
    if distance < 0.01 {
        return None;
    }
    let distance = distance.sqrt();
    let strength = (1.0 / distance).min(1.0) * PUSH / distance;
    Some(Vec3::new(-dx * strength, 0.0, -dz * strength))
}

/// The share of a movement along the direction that is horizontal. The game keeps the horizontal
/// speed of a cart, so the speed along a track changes by this when the track changes its slope.
fn horizontal_share(direction: Vec3) -> f64 {
    (1.0 - direction.y * direction.y).sqrt().max(0.1)
}

/// The direction along a straight track a cart at rest on a powered rail is launched in, away from
/// a solid block at one of its ends
fn launch_direction(world: &impl World, track: &Track) -> Option<f64> {
    if track.direction().y.abs() > 1e-6 {
        return None;
    }
    let solid = |pos: BlockPos| world.get_block(pos).is_solid();
    if solid(track.connections[0]) {
        Some(1.0)
    } else if solid(track.connections[1]) {
        Some(-1.0)
    } else {
        None
    }
}

/// What the cart did in the tick
pub struct Movement {
    /// The rail the cart is on, when it is on a track
    pub rail: Option<BlockPos>,
    /// The yaw and pitch of the cart in degrees, as the client shows it
    pub yaw: f32,
    pub pitch: f32,
}

/// Moves the cart along the track it is on. Returns `None` when the cart is not on a track,
/// in which case the caller moves it like a falling entity.
pub fn drive(world: &impl World, pos: &mut Vec3, vel: &mut Vec3) -> Option<Movement> {
    let (mut track, mut along) = find_track(world, *pos)?;
    let unit = track.direction();
    // The velocity along the track, positive towards the end, with the pull of the slope. The
    // game adds the pull to the horizontal speed, which is only part of the speed along a slope.
    let horizontal = horizontal_share(unit);
    let pull = if unit.y.abs() > 1e-6 {
        SLOPE_PULL / horizontal * unit.y.signum()
    } else {
        0.0
    };
    // Only the distance of a tick is limited to the maximum speed, the speed itself is not
    let along_speed = vel.dot(unit) - pull;
    let mut direction = if along_speed < 0.0 { -1.0 } else { 1.0 };
    let mut speed = along_speed.abs();

    // Whether the powered rail the cart starts the tick on is powered
    let powered = match world.get_block(track.rail) {
        Block::Generic(id) if generic::state_name(id) == "minecraft:powered_rail" => {
            Some(generic::get_property(id, "powered") == Some("true"))
        }
        _ => None,
    };
    if powered == Some(false) {
        // Unpowered powered rails brake the cart
        speed = if speed * horizontal < BRAKE_STOP_SPEED {
            0.0
        } else {
            speed * 0.5
        };
    }
    let launch_track = track;

    // Move along the track. At the end of a rail the cart continues on the next one, which is
    // at most one rail away since the speed is below the length of a rail.
    let target = along + speed.min(MAX_SPEED) * direction;
    let length = track.length();
    if !(0.0..=length).contains(&target) {
        let at_end = target > length;
        let overshoot = if at_end { target - length } else { -target };
        match next_track(world, &track, at_end) {
            Some(next) => {
                // The horizontal speed and distance carry over to a track with another slope
                let scale =
                    horizontal_share(track.direction()) / horizontal_share(next.direction());
                speed *= scale;
                let overshoot = overshoot * scale;
                // Entering the next track at its start means moving along its direction
                let entered_at_start =
                    next.end_at(if at_end { track.end } else { track.start }) == Some(true);
                if entered_at_start {
                    along = overshoot;
                    direction = 1.0;
                } else {
                    along = next.length() - overshoot;
                    direction = -1.0;
                }
                track = next;
            }
            None => {
                // The end of the track stops the cart
                along = if at_end { length } else { 0.0 };
                speed = 0.0;
            }
        }
    } else {
        along = target;
    }

    *pos = track.point(along);
    // The friction slows the cart down after it moved
    speed *= FRICTION;
    if powered == Some(true) {
        // The powered rail pushes the cart after the move, a cart that is almost at rest is
        // only launched off a block next to the rail
        let share = horizontal_share(track.direction());
        if speed * share > BOOST_MIN_SPEED {
            speed += POWERED_BOOST / share;
        } else if let Some(launch) = launch_direction(world, &launch_track) {
            direction = launch;
            speed = LAUNCH_SPEED;
        }
    }
    let direction_vec = track.direction() * direction;
    *vel = direction_vec * if speed < MIN_SPEED { 0.0 } else { speed };

    let horizontal = (direction_vec.x * direction_vec.x + direction_vec.z * direction_vec.z).sqrt();
    let yaw = (-direction_vec.x).atan2(direction_vec.z).to_degrees() as f32;
    let pitch = (-direction_vec.y).atan2(horizontal).to_degrees() as f32;
    Some(Movement {
        rail: Some(track.rail),
        yaw,
        pitch,
    })
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

    /// Places the rails with their connections, like a player does
    fn lay_rails(world: &mut TestWorld, rails: &[(BlockPos, &str)]) {
        for (pos, name) in rails {
            world.set_block_raw(*pos, generic::default_state(name).unwrap());
            cinnabar_redstone::rail::on_placed(world, *pos);
        }
    }

    fn at(x: i32, y: i32, z: i32) -> BlockPos {
        BlockPos::new(x, y, z)
    }

    /// Runs the cart for the number of ticks and returns its positions
    fn run(world: &TestWorld, pos: &mut Vec3, vel: &mut Vec3, ticks: u32) -> Vec<Vec3> {
        let mut positions = Vec::new();
        for _ in 0..ticks {
            if drive(world, pos, vel).is_none() {
                break;
            }
            positions.push(*pos);
        }
        positions
    }

    fn rail_line(xs: std::ops::RangeInclusive<i32>) -> Vec<(BlockPos, &'static str)> {
        xs.map(|x| (at(x, 1, 5), "minecraft:rail")).collect()
    }

    #[test]
    fn cart_moves_along_a_straight_track() {
        let mut world = floor_world();
        lay_rails(&mut world, &rail_line(5..=12));
        let mut pos = Vec3::new(5.5, 1.0 + RAIL_HEIGHT, 5.5);
        let mut vel = Vec3::new(0.2, 0.0, 0.0);
        let positions = run(&world, &mut pos, &mut vel, 3);
        assert_eq!(positions.len(), 3);
        // The cart stays on the centre line of the track
        for p in &positions {
            assert!((p.z - 5.5).abs() < 1e-9);
            assert!((p.y - (1.0 + RAIL_HEIGHT)).abs() < 1e-9);
        }
        assert!(positions[2].x > positions[0].x);
    }

    #[test]
    fn cart_follows_a_curve() {
        let mut world = floor_world();
        // An east-west line that turns south at x = 5
        lay_rails(
            &mut world,
            &[
                (at(3, 1, 5), "minecraft:rail"),
                (at(4, 1, 5), "minecraft:rail"),
                (at(5, 1, 6), "minecraft:rail"),
                (at(5, 1, 7), "minecraft:rail"),
                (at(5, 1, 5), "minecraft:rail"),
            ],
        );
        assert_eq!(
            generic::get_property(world.get_block_raw(at(5, 1, 5)), "shape"),
            Some("south_west")
        );
        let mut pos = Vec3::new(3.5, 1.0 + RAIL_HEIGHT, 5.5);
        let mut vel = Vec3::new(0.3, 0.0, 0.0);
        let positions = run(&world, &mut pos, &mut vel, 14);
        let last = *positions.last().unwrap();
        assert!(
            last.z > 6.0,
            "the cart should have turned south, it is at {last:?}"
        );
        assert!(vel.z > 0.0 && vel.x.abs() < 1e-6, "velocity {vel:?}");
    }

    #[test]
    fn cart_climbs_and_rolls_back_on_a_slope() {
        let mut world = floor_world();
        // Stone under the upper end of the slope, and a rail at the top of it
        world.set_block_raw(
            at(6, 1, 5),
            generic::default_state("minecraft:stone").unwrap(),
        );
        lay_rails(
            &mut world,
            &[
                (at(4, 1, 5), "minecraft:rail"),
                (at(5, 1, 5), "minecraft:rail"),
                (at(6, 2, 5), "minecraft:rail"),
                (at(7, 2, 5), "minecraft:rail"),
            ],
        );
        assert_eq!(
            generic::get_property(world.get_block_raw(at(5, 1, 5)), "shape"),
            Some("ascending_east")
        );

        // Going up with little speed, gravity slows the cart down until it rolls back
        let mut pos = Vec3::new(5.1, 1.0 + RAIL_HEIGHT, 5.5);
        let mut vel = Vec3::new(0.3, 0.0, 0.0);
        let positions = run(&world, &mut pos, &mut vel, 40);
        let highest = positions.iter().map(|p| p.y).fold(f64::MIN, f64::max);
        assert!(
            highest > 1.4,
            "the cart should climb the slope, highest {highest}"
        );
        let last = *positions.last().unwrap();
        assert!(
            last.y < highest - 0.1,
            "the cart should roll back down, at {last:?}"
        );
    }

    #[test]
    fn cart_stops_at_the_end_of_the_track() {
        let mut world = floor_world();
        lay_rails(&mut world, &rail_line(5..=6));
        let mut pos = Vec3::new(5.5, 1.0 + RAIL_HEIGHT, 5.5);
        let mut vel = Vec3::new(0.4, 0.0, 0.0);
        let positions = run(&world, &mut pos, &mut vel, 10);
        let last = *positions.last().unwrap();
        // The track ends at the east side of the last rail
        assert!((last.x - 7.0).abs() < 1e-9);
        assert_eq!(vel, Vec3::ZERO);
    }

    /// A single powered rail at x = 5 followed by plain rails, with the powered state set
    fn powered_rail_world(powered: bool) -> TestWorld {
        let mut world = floor_world();
        let mut rails = vec![(at(5, 1, 5), "minecraft:powered_rail")];
        for x in 6..=30 {
            rails.push((at(x, 1, 5), "minecraft:rail"));
        }
        lay_rails(&mut world, &rails);
        let id = world.get_block_raw(at(5, 1, 5));
        let state = if powered { "true" } else { "false" };
        world.set_block_raw(at(5, 1, 5), generic::with_property(id, "powered", state));
        world
    }

    #[test]
    fn powered_rail_boosts_a_moving_cart() {
        let world = powered_rail_world(true);
        let mut pos = Vec3::new(5.5, 1.0 + RAIL_HEIGHT, 5.5);
        let mut vel = Vec3::new(0.02, 0.0, 0.0);
        run(&world, &mut pos, &mut vel, 10);
        assert!(
            vel.x > 0.1,
            "the powered rail should push the cart, speed {vel:?}"
        );
    }

    #[test]
    fn cart_at_rest_on_a_powered_rail_stays_without_a_block_next_to_it() {
        let world = powered_rail_world(true);
        let mut pos = Vec3::new(5.5, 1.0 + RAIL_HEIGHT, 5.5);
        let mut vel = Vec3::ZERO;
        run(&world, &mut pos, &mut vel, 10);
        assert_eq!(vel, Vec3::ZERO);
        assert!((pos.x - 5.5).abs() < 1e-9);
    }

    #[test]
    fn cart_is_launched_off_a_block_next_to_a_powered_rail() {
        let mut world = powered_rail_world(true);
        // The block at the west end of the track: the cart is pushed east, away from it
        let stone = generic::default_state("minecraft:stone").unwrap();
        world.set_block_raw(at(4, 1, 5), stone);
        let mut pos = Vec3::new(5.5, 1.0 + RAIL_HEIGHT, 5.5);
        let mut vel = Vec3::ZERO;
        run(&world, &mut pos, &mut vel, 20);
        assert!(
            pos.x > 6.0,
            "the cart should have been launched, at {pos:?}"
        );
    }

    #[test]
    fn cart_rolls_as_far_after_a_powered_rail_as_in_the_game() {
        let world = powered_rail_world(true);
        // Launched from the middle of one powered rail the cart rolls about 7.5 blocks beyond it
        let mut pos = Vec3::new(5.5, 1.0 + RAIL_HEIGHT, 5.5);
        let mut vel = Vec3::new(0.02, 0.0, 0.0);
        run(&world, &mut pos, &mut vel, 400);
        let beyond = pos.x - 6.0;
        assert!(
            (6.8..8.2).contains(&beyond),
            "the cart rolled {beyond} blocks beyond the rail"
        );
    }

    #[test]
    fn unpowered_powered_rail_brakes_the_cart() {
        let mut world = floor_world();
        lay_rails(
            &mut world,
            &[
                (at(5, 1, 5), "minecraft:powered_rail"),
                (at(6, 1, 5), "minecraft:powered_rail"),
            ],
        );
        let mut pos = Vec3::new(5.5, 1.0 + RAIL_HEIGHT, 5.5);
        let mut vel = Vec3::new(0.3, 0.0, 0.0);
        run(&world, &mut pos, &mut vel, 10);
        assert_eq!(vel, Vec3::ZERO);
    }

    #[test]
    fn player_pushes_the_cart_away() {
        let cart = Vec3::new(0.5, 1.0625, 0.5);
        let push = push_from_player(cart, Vec3::new(0.0, 1.0, 0.5)).unwrap();
        assert!(push.x > 0.0 && push.z.abs() < 1e-9 && push.y == 0.0);
        let push = push_from_player(cart, Vec3::new(0.5, 1.0, 1.0)).unwrap();
        assert!(push.z < 0.0 && push.x.abs() < 1e-9);
    }

    #[test]
    fn distant_or_misplaced_players_do_not_push() {
        let cart = Vec3::new(0.5, 1.0625, 0.5);
        // Too far away, above the cart and exactly in its middle
        assert!(push_from_player(cart, Vec3::new(3.0, 1.0, 0.5)).is_none());
        assert!(push_from_player(cart, Vec3::new(0.5, 3.0, 0.5)).is_none());
        assert!(push_from_player(cart, cart).is_none());
    }

    #[test]
    fn pushed_cart_rolls_along_the_track() {
        let mut world = floor_world();
        lay_rails(&mut world, &rail_line(0..=8));
        let mut pos = Vec3::new(4.5, 1.0 + RAIL_HEIGHT, 5.5);
        let mut vel = push_from_player(pos, Vec3::new(4.0, 1.0, 5.5)).unwrap();
        // The cart is pushed from the side as well: only the part along the rail counts
        vel.z += 0.03;
        let positions = run(&world, &mut pos, &mut vel, 10);
        let last = positions.last().unwrap();
        assert!(
            last.x > 4.6,
            "the cart should have rolled away from the player: {last:?}"
        );
        assert!((last.z - 5.5).abs() < 1e-6);
    }

    #[test]
    fn cart_released_on_a_slope_rolls_as_far_as_in_the_game() {
        let mut world = floor_world();
        let mut rails = vec![(at(6, 1, 5), "minecraft:rail")];
        for x in 0..=5 {
            rails.push((at(x, 1, 5), "minecraft:rail"));
        }
        lay_rails(&mut world, &rails);
        let slope =
            generic::with_property(world.get_block_raw(at(6, 1, 5)), "shape", "ascending_east");
        world.set_block_raw(at(6, 1, 5), slope);
        // Released at the top of a slope that is one block high; the wiki measures about 2.8 blocks
        let mut pos = Vec3::new(6.99, 2.0 + RAIL_HEIGHT, 5.5);
        let mut vel = Vec3::ZERO;
        let start = pos.x;
        run(&world, &mut pos, &mut vel, 400);
        let traveled = start - pos.x;
        assert!(
            (2.3..4.0).contains(&traveled),
            "the cart rolled {traveled} blocks"
        );
    }

    #[test]
    fn cart_coasts_as_far_as_the_friction_allows() {
        let mut world = floor_world();
        lay_rails(&mut world, &rail_line(0..=30));
        // An empty cart keeps 96% of its speed each tick, so it rolls 0.2 / (1 - 0.96) = 5 blocks
        let mut pos = Vec3::new(1.5, 1.0 + RAIL_HEIGHT, 5.5);
        let mut vel = Vec3::new(0.2, 0.0, 0.0);
        let start = pos.x;
        run(&world, &mut pos, &mut vel, 300);
        let traveled = pos.x - start;
        assert!(
            (4.6..5.1).contains(&traveled),
            "the cart rolled {traveled} blocks"
        );
    }

    #[test]
    fn cart_off_the_track_is_not_driven() {
        let world = floor_world();
        let mut pos = Vec3::new(5.5, 1.0, 5.5);
        let mut vel = Vec3::new(0.1, 0.0, 0.0);
        assert!(drive(&world, &mut pos, &mut vel).is_none());
        assert_eq!(pos, Vec3::new(5.5, 1.0, 5.5));
    }

    #[test]
    fn yaw_faces_the_direction_of_the_track() {
        let mut world = floor_world();
        lay_rails(&mut world, &rail_line(5..=9));
        let mut pos = Vec3::new(5.5, 1.0 + RAIL_HEIGHT, 5.5);
        let mut vel = Vec3::new(0.2, 0.0, 0.0);
        let movement = drive(&world, &mut pos, &mut vel).unwrap();
        // Driving east is a yaw of -90 degrees in the game
        assert!((movement.yaw + 90.0).abs() < 1e-3, "yaw {}", movement.yaw);
        assert!(movement.pitch.abs() < 1e-3);
    }
}
