//! Minecarts that drive along rails.
//!
//! The track of a rail is the straight line between the two places where it connects to its
//! neighbours. Curves are approximated by the chord, which is close enough at the speeds of a
//! minecart. A cart that is not on a track falls like any other entity.

use super::physics::Vec3;
use mchprs_blocks::blocks::Block;
use mchprs_blocks::{generic, BlockPos};
use mchprs_redstone::rail;
use mchprs_world::World;

/// The height of the top of a flat rail above the bottom of its block
pub const RAIL_HEIGHT: f64 = 0.0625;
/// How far the cart can be from the track and still be on it
const ON_TRACK_DISTANCE: f64 = 0.6;
/// The acceleration of gravity per tick
pub const GRAVITY: f64 = 0.04;
/// The speed a minecart can't go faster than, in blocks per tick
const MAX_SPEED: f64 = 0.4;
/// The push of a powered rail that is powered, in blocks per tick
const POWERED_BOOST: f64 = 0.06;
/// The share of the speed that is kept each tick on a track
const FRICTION: f64 = 0.96;
/// Carts that are braked by a powered rail that is not powered stop below this speed
const MIN_SPEED: f64 = 0.01;

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
    // The velocity along the track, positive towards the end, with the pull of gravity on slopes
    let along_speed = (vel.dot(unit) - GRAVITY * unit.y).clamp(-MAX_SPEED, MAX_SPEED);
    let mut direction = if along_speed < 0.0 { -1.0 } else { 1.0 };
    let mut speed = along_speed.abs();

    if let Block::Generic(id) = world.get_block(track.rail)
        && generic::state_name(id) == "minecraft:powered_rail"
    {
        if generic::get_property(id, "powered") == Some("true") {
            speed = (speed + POWERED_BOOST).min(MAX_SPEED);
        } else {
            // Unpowered powered rails brake the cart
            speed *= 0.5;
            if speed < MIN_SPEED {
                speed = 0.0;
            }
        }
    }
    speed *= FRICTION;
    if speed < MIN_SPEED {
        speed = 0.0;
    }

    // Move along the track. At the end of a rail the cart continues on the next one, which is
    // at most one rail away since the speed is below the length of a rail.
    let target = along + speed * direction;
    let length = track.length();
    if !(0.0..=length).contains(&target) {
        let at_end = target > length;
        let overshoot = if at_end { target - length } else { -target };
        match next_track(world, &track, at_end) {
            Some(next) => {
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
    let direction_vec = track.direction() * direction;
    *vel = direction_vec * speed;

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
    use mchprs_world::testing::TestWorld;

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
            mchprs_redstone::rail::on_placed(world, *pos);
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

    #[test]
    fn powered_rail_boosts_the_cart() {
        let mut world = floor_world();
        lay_rails(
            &mut world,
            &[
                (at(5, 1, 5), "minecraft:powered_rail"),
                (at(6, 1, 5), "minecraft:powered_rail"),
                (at(7, 1, 5), "minecraft:powered_rail"),
                (at(8, 1, 5), "minecraft:rail"),
            ],
        );
        // The rails are powered by the redstone around them, which is not part of this test
        for x in 5..=7 {
            let id = world.get_block_raw(at(x, 1, 5));
            world.set_block_raw(at(x, 1, 5), generic::with_property(id, "powered", "true"));
        }
        let mut pos = Vec3::new(5.5, 1.0 + RAIL_HEIGHT, 5.5);
        let mut vel = Vec3::ZERO;
        run(&world, &mut pos, &mut vel, 10);
        assert!(
            vel.x > 0.1,
            "the powered rail should push the cart, speed {vel:?}"
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
