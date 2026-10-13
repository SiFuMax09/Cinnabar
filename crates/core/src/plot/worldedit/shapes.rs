//! The geometry behind spheres, cylinders, lines, hollowing, filling and smoothing. Everything in
//! here is pure and works on plain numbers, so it can be tested without a world.

use cinnabar_blocks::BlockPos;
use std::collections::VecDeque;

/// Whether the block at the offset from the center is inside of a sphere of this radius
pub fn in_sphere(dx: i32, dy: i32, dz: i32, radius: f64) -> bool {
    let limit = (radius + 0.5) * (radius + 0.5);
    ((dx * dx + dy * dy + dz * dz) as f64) < limit
}

/// Whether the block is in the one block thick shell of the sphere
pub fn in_sphere_shell(dx: i32, dy: i32, dz: i32, radius: f64) -> bool {
    in_sphere(dx, dy, dz, radius)
        && [
            (1, 0, 0),
            (-1, 0, 0),
            (0, 1, 0),
            (0, -1, 0),
            (0, 0, 1),
            (0, 0, -1),
        ]
        .iter()
        .any(|(x, y, z)| !in_sphere(dx + x, dy + y, dz + z, radius))
}

pub fn in_cylinder(dx: i32, dz: i32, radius: f64) -> bool {
    in_sphere(dx, 0, dz, radius)
}

pub fn in_cylinder_shell(dx: i32, dz: i32, radius: f64) -> bool {
    in_cylinder(dx, dz, radius)
        && [(1, 0), (-1, 0), (0, 1), (0, -1)]
            .iter()
            .any(|(x, z)| !in_cylinder(dx + x, dz + z, radius))
}

/// The blocks on a straight line from `a` to `b`, both included
pub fn line_points(a: BlockPos, b: BlockPos) -> Vec<BlockPos> {
    let (dx, dy, dz) = (b.x - a.x, b.y - a.y, b.z - a.z);
    let steps = dx.abs().max(dy.abs()).max(dz.abs());
    if steps == 0 {
        return vec![a];
    }
    (0..=steps)
        .map(|i| {
            let t = i as f64 / steps as f64;
            BlockPos::new(
                a.x + (dx as f64 * t).round() as i32,
                a.y + (dy as f64 * t).round() as i32,
                a.z + (dz as f64 * t).round() as i32,
            )
        })
        .collect()
}

/// The blocks of the center of a box: one block per axis with an odd size and two for an even
/// size
pub fn center_boxes(min: BlockPos, max: BlockPos) -> (BlockPos, BlockPos) {
    let mid = |lo: i32, hi: i32| ((lo + hi).div_euclid(2), (lo + hi + 1).div_euclid(2));
    let (x0, x1) = mid(min.x, max.x);
    let (y0, y1) = mid(min.y, max.y);
    let (z0, z1) = mid(min.z, max.z);
    (BlockPos::new(x0, y0, z0), BlockPos::new(x1, y1, z1))
}

/// Disjoint boxes that cover the walls of the box (and the floor and ceiling if `faces`)
pub fn shell_boxes(min: BlockPos, max: BlockPos, faces: bool) -> Vec<(BlockPos, BlockPos)> {
    let mut boxes = Vec::new();
    let (mut y_lo, mut y_hi) = (min.y, max.y);
    if faces {
        boxes.push((min, BlockPos::new(max.x, min.y, max.z)));
        y_lo += 1;
        if max.y > min.y {
            boxes.push((BlockPos::new(min.x, max.y, min.z), max));
            y_hi -= 1;
        }
    }
    if y_lo > y_hi {
        return boxes;
    }
    boxes.push((
        BlockPos::new(min.x, y_lo, min.z),
        BlockPos::new(max.x, y_hi, min.z),
    ));
    if max.z > min.z {
        boxes.push((
            BlockPos::new(min.x, y_lo, max.z),
            BlockPos::new(max.x, y_hi, max.z),
        ));
    }
    if max.z - min.z >= 2 {
        boxes.push((
            BlockPos::new(min.x, y_lo, min.z + 1),
            BlockPos::new(min.x, y_hi, max.z - 1),
        ));
        if max.x > min.x {
            boxes.push((
                BlockPos::new(max.x, y_lo, min.z + 1),
                BlockPos::new(max.x, y_hi, max.z - 1),
            ));
        }
    }
    boxes
}

/// For hollowing: how many layers a solid block is away from the surface. Blocks next to air or
/// next to the edge of the box have the distance 0. `solid` is in the clipboard layout. Air has
/// the distance 255 and solid blocks that are more than `max_distance` deep 254.
pub fn surface_distances(solid: &[bool], size: (usize, usize, usize), max_distance: u8) -> Vec<u8> {
    let (sx, sy, sz) = size;
    let index = |x: usize, y: usize, z: usize| (y * sz + z) * sx + x;
    let mut dist = vec![u8::MAX; solid.len()];
    let mut queue = VecDeque::new();
    let neighbours = |x: usize, y: usize, z: usize| {
        [
            (x.wrapping_sub(1), y, z),
            (x + 1, y, z),
            (x, y.wrapping_sub(1), z),
            (x, y + 1, z),
            (x, y, z.wrapping_sub(1)),
            (x, y, z + 1),
        ]
    };
    for y in 0..sy {
        for z in 0..sz {
            for x in 0..sx {
                let i = index(x, y, z);
                if !solid[i] {
                    continue;
                }
                let exposed = neighbours(x, y, z).iter().any(|&(nx, ny, nz)| {
                    nx >= sx || ny >= sy || nz >= sz || !solid[index(nx, ny, nz)]
                });
                if exposed {
                    dist[i] = 0;
                    queue.push_back((x, y, z));
                }
            }
        }
    }
    while let Some((x, y, z)) = queue.pop_front() {
        let d = dist[index(x, y, z)];
        if d >= max_distance {
            continue;
        }
        for (nx, ny, nz) in neighbours(x, y, z) {
            if nx >= sx || ny >= sy || nz >= sz {
                continue;
            }
            let ni = index(nx, ny, nz);
            if solid[ni] && dist[ni] == u8::MAX {
                dist[ni] = d + 1;
                queue.push_back((nx, ny, nz));
            }
        }
    }
    // Solid blocks the search did not reach are deeper than max_distance
    for (d, solid) in dist.iter_mut().zip(solid) {
        if *solid && *d == u8::MAX {
            *d = u8::MAX - 1;
        }
    }
    dist
}

/// Finds the air blocks a fill from `origin` reaches: it spreads sideways and downwards through
/// blocks for which `is_air` is true, at most `radius` blocks away horizontally and `depth`
/// blocks below the origin.
pub fn flood_fill(
    origin: BlockPos,
    radius: i32,
    depth: i32,
    mut is_air: impl FnMut(BlockPos) -> bool,
) -> Vec<BlockPos> {
    let width = (2 * radius + 1) as usize;
    let height = depth.max(1) as usize;
    let index = |p: BlockPos| {
        (((origin.y - p.y) as usize * width) + (p.z - origin.z + radius) as usize) * width
            + (p.x - origin.x + radius) as usize
    };
    let mut visited = vec![false; width * width * height];
    let mut result = Vec::new();
    let mut queue = VecDeque::new();
    if !is_air(origin) {
        return result;
    }
    visited[index(origin)] = true;
    queue.push_back(origin);
    while let Some(pos) = queue.pop_front() {
        result.push(pos);
        for next in [
            BlockPos::new(pos.x + 1, pos.y, pos.z),
            BlockPos::new(pos.x - 1, pos.y, pos.z),
            BlockPos::new(pos.x, pos.y, pos.z + 1),
            BlockPos::new(pos.x, pos.y, pos.z - 1),
            BlockPos::new(pos.x, pos.y - 1, pos.z),
        ] {
            let (dx, dz) = (next.x - origin.x, next.z - origin.z);
            if dx * dx + dz * dz > radius * radius || origin.y - next.y >= depth || next.y < 0 {
                continue;
            }
            let i = index(next);
            if !visited[i] {
                visited[i] = true;
                if is_air(next) {
                    queue.push_back(next);
                }
            }
        }
    }
    result
}

/// Smooths a height map (`width` columns per row) with a gaussian kernel. Heights below zero
/// stand for "no ground" and are left alone.
pub fn smooth_heights(heights: &[i32], width: usize, iterations: u32) -> Vec<i32> {
    const KERNEL: [[f32; 5]; 5] = [
        [1.0, 4.0, 7.0, 4.0, 1.0],
        [4.0, 16.0, 26.0, 16.0, 4.0],
        [7.0, 26.0, 41.0, 26.0, 7.0],
        [4.0, 16.0, 26.0, 16.0, 4.0],
        [1.0, 4.0, 7.0, 4.0, 1.0],
    ];
    let depth = heights.len() / width;
    let mut current = heights.to_vec();
    for _ in 0..iterations {
        let mut next = current.clone();
        for z in 0..depth {
            for x in 0..width {
                if current[z * width + x] < 0 {
                    continue;
                }
                let (mut sum, mut weights) = (0.0, 0.0);
                for (kz, row) in KERNEL.iter().enumerate() {
                    for (kx, weight) in row.iter().enumerate() {
                        let (nx, nz) = (x as i32 + kx as i32 - 2, z as i32 + kz as i32 - 2);
                        if nx < 0 || nz < 0 || nx >= width as i32 || nz >= depth as i32 {
                            continue;
                        }
                        let h = current[nz as usize * width + nx as usize];
                        if h >= 0 {
                            sum += *weight * h as f32;
                            weights += *weight;
                        }
                    }
                }
                next[z * width + x] = (sum / weights).round() as i32;
            }
        }
        current = next;
    }
    current
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sphere_counts() {
        let count = |r: i32| {
            let mut n = 0;
            for x in -r..=r {
                for y in -r..=r {
                    for z in -r..=r {
                        n += in_sphere(x, y, z, r as f64) as usize;
                    }
                }
            }
            n
        };
        assert_eq!(count(0), 1);
        // The same shape WorldEdit makes: everything closer than radius + 0.5
        assert_eq!(count(1), 19);
        assert!(count(5) > 600 && count(5) < 800);
    }

    #[test]
    fn sphere_shell_has_no_inside() {
        assert!(!in_sphere_shell(0, 0, 0, 4.0));
        assert!(in_sphere_shell(4, 0, 0, 4.0));
        assert!(in_sphere_shell(0, 0, 0, 0.0));
    }

    #[test]
    fn cylinder_is_flat_sphere() {
        assert!(in_cylinder(3, 0, 3.0));
        assert!(!in_cylinder(3, 3, 3.0));
        assert!(in_cylinder_shell(3, 0, 3.0));
        assert!(!in_cylinder_shell(0, 0, 3.0));
    }

    #[test]
    fn line_includes_both_ends_and_is_connected() {
        let a = BlockPos::new(0, 0, 0);
        let b = BlockPos::new(10, 3, -5);
        let points = line_points(a, b);
        assert_eq!(points.first(), Some(&a));
        assert_eq!(points.last(), Some(&b));
        assert_eq!(points.len(), 11);
        for pair in points.windows(2) {
            let d = pair[1] - pair[0];
            assert!(d.x.abs() <= 1 && d.y.abs() <= 1 && d.z.abs() <= 1);
        }
        assert_eq!(line_points(a, a), vec![a]);
    }

    #[test]
    fn center_of_odd_and_even_sizes() {
        let (a, b) = center_boxes(BlockPos::new(0, 0, 0), BlockPos::new(4, 3, 4));
        assert_eq!(a, BlockPos::new(2, 1, 2));
        assert_eq!(b, BlockPos::new(2, 2, 2));
        let (a, b) = center_boxes(BlockPos::new(-3, 0, 0), BlockPos::new(-1, 0, 0));
        assert_eq!((a.x, b.x), (-2, -2));
    }

    fn volume((min, max): (BlockPos, BlockPos)) -> i64 {
        ((max.x - min.x + 1) * (max.y - min.y + 1) * (max.z - min.z + 1)) as i64
    }

    #[test]
    fn shell_boxes_are_disjoint_and_cover_the_shell() {
        for (sx, sy, sz) in [
            (5, 5, 5),
            (1, 1, 1),
            (2, 3, 2),
            (1, 4, 6),
            (3, 1, 3),
            (6, 2, 1),
        ] {
            for faces in [false, true] {
                let min = BlockPos::new(10, 20, 30);
                let max = BlockPos::new(10 + sx - 1, 20 + sy - 1, 30 + sz - 1);
                let boxes = shell_boxes(min, max, faces);
                let mut seen = std::collections::HashSet::new();
                for (lo, hi) in &boxes {
                    for x in lo.x..=hi.x {
                        for y in lo.y..=hi.y {
                            for z in lo.z..=hi.z {
                                assert!(seen.insert((x, y, z)), "overlap in {sx}x{sy}x{sz}");
                            }
                        }
                    }
                }
                let mut expected = 0;
                for x in min.x..=max.x {
                    for y in min.y..=max.y {
                        for z in min.z..=max.z {
                            let wall = x == min.x || x == max.x || z == min.z || z == max.z;
                            let cap = y == min.y || y == max.y;
                            if wall || (faces && cap) {
                                expected += 1;
                                assert!(seen.contains(&(x, y, z)), "missing {x},{y},{z}");
                            }
                        }
                    }
                }
                assert_eq!(seen.len(), expected);
                assert_eq!(
                    boxes.iter().copied().map(volume).sum::<i64>(),
                    expected as i64
                );
            }
        }
    }

    #[test]
    fn hollow_distances() {
        // 5x5x5 solid cube: only the center block is not on the surface
        let solid = vec![true; 125];
        let dist = surface_distances(&solid, (5, 5, 5), 10);
        assert_eq!(dist[(2 * 5 + 2) * 5 + 2], 2);
        assert_eq!(dist[0], 0);
        let inner = dist.iter().filter(|d| **d > 0).count();
        assert_eq!(inner, 27);
        let deep = dist.iter().filter(|d| **d > 1).count();
        assert_eq!(deep, 1);
    }

    #[test]
    fn hollow_air_is_untouched() {
        let mut solid = vec![true; 27];
        solid[13] = false;
        let dist = surface_distances(&solid, (3, 3, 3), 10);
        assert_eq!(dist[13], u8::MAX);
        // The block above the hole is exposed
        assert_eq!(dist[13 + 9], 0);
    }

    #[test]
    fn fill_spreads_sideways_and_down() {
        // A hole shaped like a 3x3 pit that is 2 deep below y=5, with walls around
        let is_air = |p: BlockPos| p.y >= 4 && p.x.abs() <= 1 && p.z.abs() <= 1;
        let filled = flood_fill(BlockPos::new(0, 5, 0), 10, 10, is_air);
        assert_eq!(filled.len(), 9 * 2);
        // Radius limits it
        let filled = flood_fill(BlockPos::new(0, 5, 0), 0, 10, is_air);
        assert_eq!(filled.len(), 2);
        // Depth limits it
        let filled = flood_fill(BlockPos::new(0, 5, 0), 10, 1, is_air);
        assert_eq!(filled.len(), 9);
        // Starting in a solid block does nothing
        assert!(flood_fill(BlockPos::new(5, 5, 5), 3, 3, is_air).is_empty());
    }

    #[test]
    fn smoothing_flattens_a_spike() {
        let mut heights = vec![10; 11 * 11];
        heights[5 * 11 + 5] = 20;
        let smoothed = smooth_heights(&heights, 11, 1);
        assert!(smoothed[5 * 11 + 5] < 20);
        assert!(smoothed[5 * 11 + 5] > 10);
        let flat = smooth_heights(&vec![7; 25], 5, 3);
        assert!(flat.iter().all(|h| *h == 7));
    }

    #[test]
    fn smoothing_ignores_missing_ground() {
        let heights = vec![5, -1, 5, 5];
        let smoothed = smooth_heights(&heights, 2, 2);
        assert_eq!(smoothed[1], -1);
        assert_eq!(smoothed[0], 5);
    }
}
