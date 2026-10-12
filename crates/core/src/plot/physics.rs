//! Collision and ray casting against the blocks of a world

use mchprs_blocks::generic;
use mchprs_blocks::{BlockFace, BlockPos};
use mchprs_world::World;
use std::ops::{Add, AddAssign, Mul, Sub};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vec3 {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl Vec3 {
    pub const ZERO: Vec3 = Vec3::new(0.0, 0.0, 0.0);

    pub const fn new(x: f64, y: f64, z: f64) -> Vec3 {
        Vec3 { x, y, z }
    }

    pub fn length(self) -> f64 {
        (self.x * self.x + self.y * self.y + self.z * self.z).sqrt()
    }

    pub fn normalized(self) -> Vec3 {
        let length = self.length();
        if length < 1e-9 {
            Vec3::ZERO
        } else {
            self * (1.0 / length)
        }
    }

    pub fn block_pos(self) -> BlockPos {
        BlockPos::new(
            self.x.floor() as i32,
            self.y.floor() as i32,
            self.z.floor() as i32,
        )
    }

    /// The direction a player looks into, for the rotation in degrees
    pub fn from_rotation(yaw: f32, pitch: f32) -> Vec3 {
        let (yaw, pitch) = ((yaw as f64).to_radians(), (pitch as f64).to_radians());
        Vec3::new(
            -yaw.sin() * pitch.cos(),
            -pitch.sin(),
            yaw.cos() * pitch.cos(),
        )
    }
}

impl Add for Vec3 {
    type Output = Vec3;
    fn add(self, other: Vec3) -> Vec3 {
        Vec3::new(self.x + other.x, self.y + other.y, self.z + other.z)
    }
}

impl AddAssign for Vec3 {
    fn add_assign(&mut self, other: Vec3) {
        *self = *self + other;
    }
}

impl Sub for Vec3 {
    type Output = Vec3;
    fn sub(self, other: Vec3) -> Vec3 {
        Vec3::new(self.x - other.x, self.y - other.y, self.z - other.z)
    }
}

impl Mul<f64> for Vec3 {
    type Output = Vec3;
    fn mul(self, factor: f64) -> Vec3 {
        Vec3::new(self.x * factor, self.y * factor, self.z * factor)
    }
}

/// An axis aligned box, as the minimum and maximum corner
#[derive(Clone, Copy, Debug)]
pub struct Aabb {
    pub min: [f64; 3],
    pub max: [f64; 3],
}

impl Aabb {
    /// The box of an entity with the position at the center of the bottom
    pub fn entity(pos: Vec3, width: f64, height: f64) -> Aabb {
        Aabb {
            min: [pos.x - width / 2.0, pos.y, pos.z - width / 2.0],
            max: [pos.x + width / 2.0, pos.y + height, pos.z + width / 2.0],
        }
    }

    fn offset(self, axis: usize, amount: f64) -> Aabb {
        let mut result = self;
        result.min[axis] += amount;
        result.max[axis] += amount;
        result
    }

    /// Whether the boxes overlap on the two axes other than `axis`
    fn overlaps_except(&self, other: &Aabb, axis: usize) -> bool {
        (0..3)
            .filter(|&a| a != axis)
            .all(|a| self.max[a] > other.min[a] && self.min[a] < other.max[a])
    }

    /// How far the box can move along the axis until it hits the other one
    fn clip(&self, other: &Aabb, axis: usize, delta: f64) -> f64 {
        if !self.overlaps_except(other, axis) {
            return delta;
        }
        if delta > 0.0 && self.max[axis] <= other.min[axis] {
            delta.min(other.min[axis] - self.max[axis])
        } else if delta < 0.0 && self.min[axis] >= other.max[axis] {
            delta.max(other.max[axis] - self.min[axis])
        } else {
            delta
        }
    }
}

/// The collision box of the block at the position
fn block_box(world: &impl World, pos: BlockPos) -> Option<Aabb> {
    let shape = generic::collision_box(world.get_block_raw(pos))?;
    let (x, y, z) = (pos.x as f64, pos.y as f64, pos.z as f64);
    Some(Aabb {
        min: [x + shape[0], y + shape[1], z + shape[2]],
        max: [x + shape[3], y + shape[4], z + shape[5]],
    })
}

/// Which axes the movement of an entity was stopped on
#[derive(Clone, Copy, Debug, Default)]
pub struct Collisions {
    pub x: bool,
    pub y: bool,
    pub z: bool,
}

/// Moves the box by `delta` as far as the blocks of the world allow it. Returns the movement
/// that is possible and the axes that were blocked.
pub fn move_box(world: &impl World, bb: Aabb, delta: Vec3) -> (Vec3, Collisions) {
    let wanted = [delta.x, delta.y, delta.z];

    let mut boxes = Vec::new();
    let min = |axis: usize| (bb.min[axis] + wanted[axis].min(0.0)).floor() as i32 - 1;
    let max = |axis: usize| (bb.max[axis] + wanted[axis].max(0.0)).floor() as i32 + 1;
    for x in min(0)..=max(0) {
        for y in min(1)..=max(1) {
            for z in min(2)..=max(2) {
                boxes.extend(block_box(world, BlockPos::new(x, y, z)));
            }
        }
    }

    // Vertical movement first, then the horizontal axis with the longer movement
    let order = if wanted[0].abs() < wanted[2].abs() {
        [1, 2, 0]
    } else {
        [1, 0, 2]
    };
    let mut bb = bb;
    let mut moved = [0.0; 3];
    for axis in order {
        let mut amount = wanted[axis];
        for other in &boxes {
            amount = bb.clip(other, axis, amount);
        }
        bb = bb.offset(axis, amount);
        moved[axis] = amount;
    }

    let blocked = |axis: usize| (moved[axis] - wanted[axis]).abs() > 1e-9;
    (
        Vec3::new(moved[0], moved[1], moved[2]),
        Collisions {
            x: blocked(0),
            y: blocked(1),
            z: blocked(2),
        },
    )
}

#[derive(Clone, Copy, Debug)]
pub struct RayHit {
    pub pos: BlockPos,
    pub face: BlockFace,
    pub point: Vec3,
}

/// Whether a block is water or lava, which can be targeted when ray casting for fluids. The water
/// of a waterlogged block counts too.
fn is_fluid(world: &impl World, pos: BlockPos) -> bool {
    let block = world.get_block(pos);
    block.is_waterlogged() || matches!(block.get_name(), "minecraft:water" | "minecraft:lava")
}

/// The distance along the ray where it enters the box, and the face it enters through
fn ray_box(origin: Vec3, dir: Vec3, bb: &Aabb) -> Option<(f64, BlockFace)> {
    let origin = [origin.x, origin.y, origin.z];
    let dir = [dir.x, dir.y, dir.z];
    let mut near = f64::NEG_INFINITY;
    let mut far = f64::INFINITY;
    let mut face = BlockFace::Top;
    for axis in 0..3 {
        if dir[axis].abs() < 1e-12 {
            if origin[axis] < bb.min[axis] || origin[axis] > bb.max[axis] {
                return None;
            }
            continue;
        }
        let (mut t1, mut t2) = (
            (bb.min[axis] - origin[axis]) / dir[axis],
            (bb.max[axis] - origin[axis]) / dir[axis],
        );
        // The face that the ray enters through belongs to the nearer plane
        let entering = if dir[axis] > 0.0 {
            [BlockFace::West, BlockFace::Bottom, BlockFace::North][axis]
        } else {
            [BlockFace::East, BlockFace::Top, BlockFace::South][axis]
        };
        if t1 > t2 {
            std::mem::swap(&mut t1, &mut t2);
        }
        if t1 > near {
            near = t1;
            face = entering;
        }
        far = far.min(t2);
        if near > far {
            return None;
        }
    }
    (far >= 0.0).then_some((near.max(0.0), face))
}

/// Finds the first block that the ray from `origin` into the direction `dir` hits within
/// `max_distance`. With `fluids`, water and lava count as full blocks.
pub fn raycast(
    world: &impl World,
    origin: Vec3,
    dir: Vec3,
    max_distance: f64,
    fluids: bool,
) -> Option<RayHit> {
    let dir = dir.normalized();
    let mut cell = origin.block_pos();
    let step = [
        if dir.x > 0.0 { 1 } else { -1 },
        if dir.y > 0.0 { 1 } else { -1 },
        if dir.z > 0.0 { 1 } else { -1 },
    ];
    let origin_arr = [origin.x, origin.y, origin.z];
    let dir_arr = [dir.x, dir.y, dir.z];
    let cell_arr = |cell: BlockPos| [cell.x, cell.y, cell.z];

    let mut t_max = [f64::INFINITY; 3];
    let mut t_delta = [f64::INFINITY; 3];
    for axis in 0..3 {
        if dir_arr[axis].abs() > 1e-12 {
            let next_boundary = if dir_arr[axis] > 0.0 {
                cell_arr(cell)[axis] as f64 + 1.0
            } else {
                cell_arr(cell)[axis] as f64
            };
            t_max[axis] = (next_boundary - origin_arr[axis]) / dir_arr[axis];
            t_delta[axis] = 1.0 / dir_arr[axis].abs();
        }
    }

    loop {
        let block_hit = block_box(world, cell).and_then(|bb| ray_box(origin, dir, &bb));
        let fluid_hit = if fluids && is_fluid(world, cell) {
            let (x, y, z) = (cell.x as f64, cell.y as f64, cell.z as f64);
            ray_box(
                origin,
                dir,
                &Aabb {
                    min: [x, y, z],
                    max: [x + 1.0, y + 1.0, z + 1.0],
                },
            )
        } else {
            None
        };
        // A waterlogged block has water in the whole cell, so the nearer of the two counts
        let hit = match (block_hit, fluid_hit) {
            (Some(block), Some(fluid)) => Some(if fluid.0 < block.0 { fluid } else { block }),
            (hit, None) | (None, hit) => hit,
        };
        if let Some((distance, face)) = hit
            && distance <= max_distance
        {
            return Some(RayHit {
                pos: cell,
                face,
                point: origin + dir * distance,
            });
        }

        // Continue with the next cell along the ray
        let axis = if t_max[0] < t_max[1] && t_max[0] < t_max[2] {
            0
        } else if t_max[1] < t_max[2] {
            1
        } else {
            2
        };
        if t_max[axis] > max_distance {
            return None;
        }
        match axis {
            0 => cell.x += step[0],
            1 => cell.y += step[1],
            _ => cell.z += step[2],
        }
        t_max[axis] += t_delta[axis];
    }
}
