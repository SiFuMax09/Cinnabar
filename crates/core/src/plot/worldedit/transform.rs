//! Rotating and flipping clipboards.

use cinnabar_blocks::blocks::{Block, FlipDirection, RotateAmt};
use cinnabar_blocks::BlockPos;
use cinnabar_schematic::WorldEditClipboard;
use rustc_hash::FxHashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    X,
    Y,
    Z,
}

/// Rotates a point by a quarter turn around the origin. The directions are the ones WorldEdit
/// uses: a positive turn around Y goes clockwise seen from above (east to south).
fn rotate_point(axis: Axis, (x, y, z): (i32, i32, i32)) -> (i32, i32, i32) {
    match axis {
        Axis::Y => (-z, y, x),
        Axis::X => (x, z, -y),
        Axis::Z => (y, -x, z),
    }
}

fn set_property(id: u32, name: &str, value: &str) -> u32 {
    let mut block = Block::from_id(id);
    block.set_properties([(name, value)].into_iter().collect());
    // Setting a value the block can't have does nothing, then the block stays as it is
    block.get_id()
}

/// The state after a quarter turn around the axis
pub fn rotate_state(id: u32, axis: Axis) -> u32 {
    if axis == Axis::Y {
        let mut block = Block::from_id(id);
        block.rotate(RotateAmt::Rotate90);
        return block.get_id();
    }
    let props = Block::from_id(id).properties();
    let mut result = id;
    if let Some(facing) = props.get("facing") {
        let new = match (axis, facing.as_str()) {
            (Axis::X, "up") => "north",
            (Axis::X, "north") => "down",
            (Axis::X, "down") => "south",
            (Axis::X, "south") => "up",
            (Axis::Z, "up") => "east",
            (Axis::Z, "east") => "down",
            (Axis::Z, "down") => "west",
            (Axis::Z, "west") => "up",
            _ => facing.as_str(),
        };
        result = set_property(result, "facing", new);
    }
    if let Some(block_axis) = props.get("axis") {
        let new = match (axis, block_axis.as_str()) {
            (Axis::X, "y") => "z",
            (Axis::X, "z") => "y",
            (Axis::Z, "x") => "y",
            (Axis::Z, "y") => "x",
            _ => block_axis.as_str(),
        };
        result = set_property(result, "axis", new);
    }
    result
}

/// The state after mirroring along the axis
pub fn flip_state(id: u32, axis: Axis) -> u32 {
    match axis {
        Axis::X | Axis::Z => {
            let mut block = Block::from_id(id);
            block.flip(if axis == Axis::X {
                FlipDirection::FlipX
            } else {
                FlipDirection::FlipZ
            });
            block.get_id()
        }
        Axis::Y => {
            let props = Block::from_id(id).properties();
            let mut result = id;
            for name in ["facing", "half", "type"] {
                let Some(value) = props.get(name) else {
                    continue;
                };
                let swapped = match value.as_str() {
                    "up" => "down",
                    "down" => "up",
                    "top" => "bottom",
                    "bottom" => "top",
                    _ => continue,
                };
                result = set_property(result, name, swapped);
            }
            result
        }
    }
}

/// Builds a clipboard from the old one: `map_pos` says where a block goes and `map_state` what
/// it becomes. The new clipboard is moved so that its corner is at 0.
fn map_clipboard(
    cb: &WorldEditClipboard,
    map_pos: impl Fn((i32, i32, i32)) -> (i32, i32, i32),
    map_state: impl Fn(u32) -> u32,
) -> WorldEditClipboard {
    let max = (
        cb.size_x as i32 - 1,
        cb.size_y as i32 - 1,
        cb.size_z as i32 - 1,
    );
    let mut lo = (i32::MAX, i32::MAX, i32::MAX);
    let mut hi = (i32::MIN, i32::MIN, i32::MIN);
    for x in [0, max.0] {
        for y in [0, max.1] {
            for z in [0, max.2] {
                let p = map_pos((x, y, z));
                lo = (lo.0.min(p.0), lo.1.min(p.1), lo.2.min(p.2));
                hi = (hi.0.max(p.0), hi.1.max(p.1), hi.2.max(p.2));
            }
        }
    }
    let shift = |p: (i32, i32, i32)| (p.0 - lo.0, p.1 - lo.1, p.2 - lo.2);
    let size = (
        (hi.0 - lo.0 + 1) as u32,
        (hi.1 - lo.1 + 1) as u32,
        (hi.2 - lo.2 + 1) as u32,
    );
    let mut result = WorldEditClipboard {
        offset_x: 0,
        offset_y: 0,
        offset_z: 0,
        size_x: size.0,
        size_y: size.1,
        size_z: size.2,
        data: vec![0; cb.data.len()],
        block_entities: FxHashMap::default(),
    };
    // Clipboards have few different states, so every state is only transformed once
    let mut state_cache: FxHashMap<u16, u16> = FxHashMap::default();
    let mut i = 0;
    for y in 0..cb.size_y as i32 {
        for z in 0..cb.size_z as i32 {
            for x in 0..cb.size_x as i32 {
                let (nx, ny, nz) = shift(map_pos((x, y, z)));
                let old = cb.data[i];
                let new = *state_cache
                    .entry(old)
                    .or_insert_with(|| map_state(old as u32) as u16);
                let idx = result.index(nx as u32, ny as u32, nz as u32);
                result.data[idx] = new;
                i += 1;
            }
        }
    }
    let offset = shift(map_pos((cb.offset_x, cb.offset_y, cb.offset_z)));
    result.offset_x = offset.0;
    result.offset_y = offset.1;
    result.offset_z = offset.2;
    for (pos, entity) in &cb.block_entities {
        let (x, y, z) = shift(map_pos((pos.x, pos.y, pos.z)));
        result
            .block_entities
            .insert(BlockPos::new(x, y, z), entity.clone());
    }
    result
}

/// Rotates the clipboard by `quarter_turns` quarter turns (may be negative) around the axis
pub fn rotate_clipboard(
    cb: &WorldEditClipboard,
    axis: Axis,
    quarter_turns: i32,
) -> WorldEditClipboard {
    let turns = quarter_turns.rem_euclid(4);
    if turns == 0 {
        return cb.clone();
    }
    map_clipboard(
        cb,
        |mut p| {
            for _ in 0..turns {
                p = rotate_point(axis, p);
            }
            p
        },
        |mut id| {
            for _ in 0..turns {
                id = rotate_state(id, axis);
            }
            id
        },
    )
}

pub fn flip_clipboard(cb: &WorldEditClipboard, axis: Axis) -> WorldEditClipboard {
    map_clipboard(
        cb,
        |(x, y, z)| match axis {
            Axis::X => (-x, y, z),
            Axis::Y => (x, -y, z),
            Axis::Z => (x, y, -z),
        },
        |id| flip_state(id, axis),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(spec: &str) -> u16 {
        let (name, props) = spec.split_once('[').unwrap_or((spec, ""));
        let mut block = Block::from_name(&format!("minecraft:{name}")).unwrap();
        let props = props.trim_end_matches(']');
        block.set_properties(props.split(',').filter_map(|p| p.split_once('=')).collect());
        block.get_id() as u16
    }

    fn clipboard(size: (u32, u32, u32), data: Vec<u16>) -> WorldEditClipboard {
        WorldEditClipboard {
            offset_x: 0,
            offset_y: 0,
            offset_z: 0,
            size_x: size.0,
            size_y: size.1,
            size_z: size.2,
            data,
            block_entities: FxHashMap::default(),
        }
    }

    fn numbered(size: (u32, u32, u32)) -> WorldEditClipboard {
        let volume = (size.0 * size.1 * size.2) as usize;
        clipboard(size, (0..volume).map(|i| 1 + (i % 7) as u16).collect())
    }

    #[test]
    fn four_quarter_turns_are_the_identity_on_every_axis() {
        let cb = numbered((2, 3, 4));
        for axis in [Axis::X, Axis::Y, Axis::Z] {
            let mut rotated = cb.clone();
            for _ in 0..4 {
                rotated = rotate_clipboard(&rotated, axis, 1);
            }
            assert_eq!(rotated.data, cb.data, "{axis:?}");
            assert_eq!((rotated.size_x, rotated.size_y, rotated.size_z), (2, 3, 4));
        }
    }

    #[test]
    fn rotation_swaps_the_sizes() {
        let cb = numbered((2, 3, 4));
        let y = rotate_clipboard(&cb, Axis::Y, 1);
        assert_eq!((y.size_x, y.size_y, y.size_z), (4, 3, 2));
        let x = rotate_clipboard(&cb, Axis::X, 1);
        assert_eq!((x.size_x, x.size_y, x.size_z), (2, 4, 3));
        let z = rotate_clipboard(&cb, Axis::Z, -1);
        assert_eq!((z.size_x, z.size_y, z.size_z), (3, 2, 4));
        assert_eq!(rotate_clipboard(&cb, Axis::Y, 2).size_x, 2);
    }

    #[test]
    fn rotating_forth_and_back_is_the_identity() {
        let cb = numbered((3, 2, 5));
        for axis in [Axis::X, Axis::Y, Axis::Z] {
            let back = rotate_clipboard(&rotate_clipboard(&cb, axis, 1), axis, -1);
            assert_eq!(back.data, cb.data);
        }
    }

    #[test]
    fn y_rotation_moves_east_to_south() {
        // A line of blocks along +x
        let cb = clipboard((3, 1, 1), vec![1, 2, 3]);
        let rotated = rotate_clipboard(&cb, Axis::Y, 1);
        assert_eq!((rotated.size_x, rotated.size_z), (1, 3));
        // The first block ends at the north end, the line runs along +z
        assert_eq!(rotated.get(0, 0, 0), 1);
        assert_eq!(rotated.get(0, 0, 2), 3);
    }

    #[test]
    fn x_rotation_moves_up_to_north() {
        // A column along +y
        let cb = clipboard((1, 3, 1), vec![1, 2, 3]);
        let rotated = rotate_clipboard(&cb, Axis::X, 1);
        assert_eq!((rotated.size_y, rotated.size_z), (1, 3));
        // Up is north (-z), so the top block is at the lowest z
        assert_eq!(rotated.get(0, 0, 0), 3);
        assert_eq!(rotated.get(0, 0, 2), 1);
    }

    #[test]
    fn z_rotation_moves_up_to_east() {
        let cb = clipboard((1, 3, 1), vec![1, 2, 3]);
        let rotated = rotate_clipboard(&cb, Axis::Z, 1);
        assert_eq!((rotated.size_x, rotated.size_y), (3, 1));
        assert_eq!(rotated.get(0, 0, 0), 1);
        assert_eq!(rotated.get(2, 0, 0), 3);
    }

    #[test]
    fn the_origin_moves_with_the_blocks() {
        let mut cb = numbered((3, 1, 1));
        cb.offset_x = 2;
        let rotated = rotate_clipboard(&cb, Axis::Y, 1);
        // Block 2 of the line is at z=2 now
        assert_eq!(
            (rotated.offset_x, rotated.offset_y, rotated.offset_z),
            (0, 0, 2)
        );
    }

    #[test]
    fn block_states_are_rotated() {
        let stairs = id("oak_stairs[facing=north,half=bottom,shape=straight,waterlogged=false]");
        let cb = clipboard((1, 1, 1), vec![stairs]);
        let rotated = rotate_clipboard(&cb, Axis::Y, 1);
        let block = Block::from_id(rotated.data[0] as u32);
        assert_eq!(block.properties()["facing"], "east");

        let log = id("oak_log[axis=y]");
        let rotated = rotate_clipboard(&clipboard((1, 1, 1), vec![log]), Axis::X, 1);
        assert_eq!(
            Block::from_id(rotated.data[0] as u32).properties()["axis"],
            "z"
        );

        let piston = id("observer[facing=up,powered=false]");
        let rotated = rotate_clipboard(&clipboard((1, 1, 1), vec![piston]), Axis::X, 1);
        assert_eq!(
            Block::from_id(rotated.data[0] as u32).properties()["facing"],
            "north"
        );
        let rotated = rotate_clipboard(&clipboard((1, 1, 1), vec![piston]), Axis::Z, 1);
        assert_eq!(
            Block::from_id(rotated.data[0] as u32).properties()["facing"],
            "east"
        );
    }

    #[test]
    fn flipping_is_an_involution() {
        let cb = numbered((3, 4, 5));
        for axis in [Axis::X, Axis::Y, Axis::Z] {
            let twice = flip_clipboard(&flip_clipboard(&cb, axis), axis);
            assert_eq!(twice.data, cb.data, "{axis:?}");
        }
        let flipped = flip_clipboard(&clipboard((3, 1, 1), vec![1, 2, 3]), Axis::X);
        assert_eq!(flipped.data, vec![3, 2, 1]);
    }

    #[test]
    fn flip_y_swaps_halves() {
        let stairs = id("oak_stairs[facing=north,half=bottom,shape=straight,waterlogged=false]");
        let flipped = flip_clipboard(&clipboard((1, 1, 1), vec![stairs]), Axis::Y);
        let props = Block::from_id(flipped.data[0] as u32).properties();
        assert_eq!(props["half"], "top");
        assert_eq!(props["facing"], "north");
    }
}
